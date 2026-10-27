//! Reviewed implicit-credential plans and explicit provisioning.

mod fastly;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal as _, Read as _};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use edgezero_core::canonical_form::canonical_data_sha256;
use serde::Serialize;
use serde_json::{Value, json};
use trusted_server_core::secret_bindings::{
    BINDING_ABI_VERSION, CompiledBinding, NativeBinding, SECRET_STORE_ID, compile_bindings,
    render_native_with_mapping, validate_credential,
};

use crate::app_config::{AppConfigArgs, load_settings};
use crate::run::RunOutcome;

const PLAN_VERSION: u32 = 1;
const NATIVE_MAPPING_VERSION: &str = "trusted-server-native-v1";

/// Native runtime selected for credential planning and presence checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Adapter {
    /// Fastly secret-store keys and reviewed direct writes.
    Fastly,
    /// Axum process-environment credential bindings.
    Axum,
    /// Cloudflare Worker secret bindings.
    Cloudflare,
    /// Spin application and component variables.
    Spin,
}

impl Adapter {
    fn name(self) -> &'static str {
        match self {
            Self::Fastly => "fastly",
            Self::Axum => "axum",
            Self::Cloudflare => "cloudflare",
            Self::Spin => "spin",
        }
    }
}

/// Existing deployment selection and unresolved app-config inputs.
#[derive(Debug, Args)]
pub(crate) struct TargetArgs {
    #[command(flatten)]
    config: AppConfigArgs,
    /// Native runtime whose actual Trusted Server secret mapping to render.
    #[arg(long, value_enum)]
    adapter: Adapter,
    /// Existing Fastly service selection, as for managed deployment.
    #[arg(long)]
    service_id: Option<String>,
}

/// Arguments for rendering a secret-free native binding plan.
#[derive(Debug, Args)]
pub(crate) struct PlanArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// Write secret-free JSON metadata to this file; otherwise print it.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Arguments for checking reviewed-plan agreement and native presence.
#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// Complete previously reviewed plan. A plan is not authorization.
    #[arg(long)]
    plan: PathBuf,
}

/// Arguments for one explicitly approved Fastly credential write.
#[derive(Debug, Args)]
pub(crate) struct SetArgs {
    /// Active fixed purpose selector, for example tinybird.auction.
    role: String,
    #[command(flatten)]
    target: TargetArgs,
    /// Complete previously reviewed plan. Recompiled before credential input.
    #[arg(long)]
    plan: PathBuf,
    /// Tool-generated entry ID from the reviewed plan for a repeated purpose.
    #[arg(long)]
    binding: Option<String>,
    /// Read exact credential bytes from stdin, without trimming. Requires --yes.
    #[arg(long, requires = "yes")]
    stdin: bool,
    /// Approve the displayed operation noninteractively; does not authorize a plan.
    #[arg(long)]
    yes: bool,
}

/// Review credential bindings, check presence, or provision one approved value.
#[derive(Debug, Subcommand)]
pub(crate) enum SecretsCommand {
    /// Compile active roles and render secret-free native identities and declarations.
    ///
    /// Every owner remains in the plan. Only the first identical native request
    /// carries declaration snippets. Merge Spin component assignments under one header.
    Plan(PlanArgs),
    /// Check reviewed inputs and presence only, never runtime credential validity.
    Check(CheckArgs),
    /// Create or replace a reviewed Fastly binding through hidden input or stdin.
    Set(SetArgs),
}

/// Read-only target resolution and narrowly scoped writes. Tests replace transport,
/// not the compiler, plan comparison, selection, or command dispatch.
trait Provider {
    fn resources(&self, target: &TargetArgs) -> Result<Value, String>;
    fn exists(&self, resources: &Value, key: &str) -> Result<bool, String>;
    fn write(
        &self,
        resources: &Value,
        key: &str,
        value: &[u8],
        replace: bool,
    ) -> Result<(), String>;
    fn max_secret_bytes(&self) -> usize;
}

trait Interaction {
    fn interactive(&self) -> bool;
    fn line(&mut self) -> Result<String, String>;
    fn hidden(&mut self) -> Result<Vec<u8>, String>;
    fn stdin(&mut self, max_bytes: usize) -> Result<Vec<u8>, String>;
}

struct Terminal;

impl Interaction for Terminal {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    fn line(&mut self) -> Result<String, String> {
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|_| "could not read confirmation".to_owned())?;
        Ok(line)
    }

    fn hidden(&mut self) -> Result<Vec<u8>, String> {
        rpassword::prompt_password("Credential, input hidden: ")
            .map(String::into_bytes)
            .map_err(|_| "credential input cancelled or unavailable".to_owned())
    }

    fn stdin(&mut self, max_bytes: usize) -> Result<Vec<u8>, String> {
        let mut value = Vec::new();
        io::stdin()
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut value)
            .map_err(|_| "could not read credential input".to_owned())?;
        Ok(value)
    }
}

#[derive(Serialize)]
struct PlanEntry<'a> {
    entry_id: String,
    compiled: &'a CompiledBinding,
    /// Later owners retain native identity metadata but share the first owner's declarations.
    native: NativeBinding,
}

struct Prepared {
    document: Value,
    bindings: trusted_server_core::secret_bindings::BindingPlan,
    resources: Value,
    native_by_key: BTreeMap<String, NativeBinding>,
}

fn entry_id(binding: &CompiledBinding) -> Result<String, String> {
    let metadata = serde_json::to_value(binding)
        .map_err(|_| "could not encode compiled binding".to_owned())?;
    Ok(format!("binding_{}", canonical_data_sha256(&metadata)))
}

fn coalesce_native_declarations<'a>(
    requests: &mut BTreeMap<String, (&'a CompiledBinding, NativeBinding)>,
    compiled: &'a CompiledBinding,
    native: &mut NativeBinding,
) -> Result<(), String> {
    if let Some((previous, previous_native)) = requests.get(&native.native_name) {
        // The owning schema path is intentionally excluded. Every context,
        // target, store/key pair and full native rendering must still match.
        if !previous.same_authorization(compiled) || previous_native != &*native {
            return Err("distinct credential contexts collide in the native namespace".to_owned());
        }
        native.declarations.clear();
    } else {
        requests.insert(native.native_name.clone(), (compiled, native.clone()));
    }
    Ok(())
}

fn prepare(target: &TargetArgs, provider: &dyn Provider) -> Result<Prepared, String> {
    if target.adapter != Adapter::Fastly && target.service_id.is_some() {
        return Err("--service-id is supported only with --adapter fastly".to_owned());
    }
    let loaded = load_settings(&target.config)
        .map_err(|_| "could not load effective unresolved app configuration".to_owned())?;
    let bindings = compile_bindings(&loaded.settings)
        .map_err(|_| "could not compile active credential bindings".to_owned())?;
    let effective = serde_json::to_value(&loaded.settings)
        .map_err(|_| "could not encode effective unresolved app configuration".to_owned())?;
    let spin_component = fastly::spin_component(target)?;
    let runtime_store = fastly::runtime_secret_store(target)?;
    let mut native_requests = BTreeMap::new();
    let mut native_by_key = BTreeMap::new();
    let entries = bindings
        .entries()
        .iter()
        .map(|binding| {
            let mut native = render_native_with_mapping(
                target.adapter.name(),
                SECRET_STORE_ID,
                binding.key(),
                &runtime_store,
            )
            .map_err(|_| "could not render native credential binding".to_owned())?;
            if let Some(component) = &spin_component {
                let header = format!(
                    "[component.{}.variables]",
                    toml::Value::String(component.clone())
                );
                for declaration in &mut native.declarations {
                    *declaration =
                        declaration.replace("[component.trusted-server.variables]", &header);
                }
            }
            coalesce_native_declarations(&mut native_requests, binding, &mut native)?;
            native_by_key
                .entry(binding.key().to_owned())
                .or_insert_with(|| native.clone());
            Ok(PlanEntry {
                entry_id: entry_id(binding)?,
                compiled: binding,
                native,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    fastly::validate_native_declarations(
        target,
        &entries
            .iter()
            .map(|entry| &entry.native)
            .collect::<Vec<_>>(),
    )?;
    let resources = provider.resources(target)?;
    let document = json!({
        "plan_version": PLAN_VERSION,
        "compiler_version": env!("CARGO_PKG_VERSION"),
        "binding_abi": BINDING_ABI_VERSION,
        "native_mapping_version": NATIVE_MAPPING_VERSION,
        "adapter": target.adapter,
        "effective_config_sha256": canonical_data_sha256(&effective),
        "environment_overlay": !target.config.no_env,
        "resources": resources,
        "entries": entries,
    });
    Ok(Prepared {
        document,
        bindings,
        resources,
        native_by_key,
    })
}

fn verify_plan(path: &PathBuf, fresh: &Value) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|_| "could not read reviewed binding plan".to_owned())?;
    let reviewed: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "reviewed binding plan is not valid JSON".to_owned())?;
    if &reviewed != fresh {
        return Err("reviewed plan does not match current config, overlays, versions, native mappings or deployment resources; generate and review a new plan".to_owned());
    }
    Ok(())
}

fn select_binding<'a>(
    args: &SetArgs,
    prepared: &'a Prepared,
    interaction: &mut dyn Interaction,
    out: &mut dyn io::Write,
) -> Result<&'a CompiledBinding, String> {
    let candidates = prepared
        .bindings
        .entries()
        .iter()
        .filter(|binding| binding.descriptor_id() == args.role)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err("purpose is unknown or inactive in the current configuration".to_owned());
    }
    if let Some(id) = &args.binding {
        return candidates
            .into_iter()
            .find(|binding| entry_id(binding).is_ok_and(|candidate| &candidate == id))
            .ok_or_else(|| {
                "--binding must identify an active entry of this purpose in the reviewed plan"
                    .to_owned()
            });
    }
    if let [binding] = candidates.as_slice() {
        return Ok(binding);
    }
    if !interaction.interactive() || args.stdin || args.yes {
        return Err("repeated purpose requires an explicit tool-generated --binding entry ID from the reviewed plan".to_owned());
    }
    for binding in &candidates {
        writeln!(
            out,
            "{} {}",
            entry_id(binding)?,
            metadata(binding.subject())?
        )
        .map_err(output_error)?;
    }
    writeln!(out, "Enter the complete binding entry ID:").map_err(output_error)?;
    out.flush().map_err(output_error)?;
    let choice = interaction.line()?;
    candidates
        .into_iter()
        .find(|binding| entry_id(binding).is_ok_and(|id| id == choice.trim()))
        .ok_or_else(|| "no listed binding selected; write cancelled".to_owned())
}

fn metadata(value: &(impl Serialize + ?Sized)) -> Result<String, String> {
    serde_json::to_string(value).map_err(|_| "could not encode reviewed metadata".to_owned())
}

fn output_error(_: io::Error) -> String {
    "could not write command output".to_owned()
}

fn execute(
    command: &SecretsCommand,
    provider: &dyn Provider,
    interaction: &mut dyn Interaction,
    out: &mut dyn io::Write,
) -> Result<RunOutcome, String> {
    match command {
        SecretsCommand::Plan(args) => {
            let prepared = prepare(&args.target, provider)?;
            let mut text = serde_json::to_vec_pretty(&prepared.document)
                .map_err(|_| "could not encode secret-free plan".to_owned())?;
            text.push(b'\n');
            if let Some(path) = &args.out {
                fs::write(path, text).map_err(|_| "could not write secret-free plan".to_owned())?;
            } else {
                out.write_all(&text).map_err(output_error)?;
            }
            Ok(RunOutcome::Success)
        }
        SecretsCommand::Check(args) => {
            let prepared = prepare(&args.target, provider)?;
            verify_plan(&args.plan, &prepared.document)?;
            let mut missing = false;
            let mut unsupported = false;
            for binding in prepared.bindings.entries() {
                let native = prepared
                    .native_by_key
                    .get(binding.key())
                    .ok_or_else(|| "compiled native identity is unavailable".to_owned())?;
                let status = match args.target.adapter {
                    Adapter::Fastly => Some(provider.exists(&prepared.resources, binding.key())?),
                    Adapter::Axum => Some(std::env::var_os(&native.native_name).is_some()),
                    Adapter::Cloudflare | Adapter::Spin => None,
                };
                let label = match status {
                    Some(true) => "present",
                    Some(false) => {
                        missing = true;
                        "missing"
                    }
                    None => {
                        unsupported = true;
                        "unsupported"
                    }
                };
                writeln!(out, "{} {} {label}", entry_id(binding)?, native.native_name)
                    .map_err(output_error)?;
            }
            writeln!(out, "Presence only. Credential validity, delivery, TLS and retained runtime state were not checked.")
                .map_err(output_error)?;
            if unsupported {
                return Err("native presence checks are unsupported for this adapter; generated declarations do not prove provider binding existence".to_owned());
            }
            Ok(if missing {
                RunOutcome::AssertionFailed
            } else {
                RunOutcome::Success
            })
        }
        SecretsCommand::Set(args) => {
            // This must precede config loading, provider access, and any interaction.
            if args.target.adapter != Adapter::Fastly {
                return Err("direct secret set is supported only for Fastly; use the reviewed native identities and your runtime's secure provisioning tools".to_owned());
            }
            if args.stdin && !args.yes {
                return Err("--stdin requires --yes".to_owned());
            }
            let prepared = prepare(&args.target, provider)?;
            verify_plan(&args.plan, &prepared.document)?;
            let binding = select_binding(args, &prepared, interaction, out)?;
            let replace = provider.exists(&prepared.resources, binding.key())?;
            writeln!(
                out,
                "Purpose: {}\nSubject: {}\nExport: {}\nDeployment: {}\nAction: {}",
                binding.descriptor_id(),
                metadata(binding.subject())?,
                metadata(binding.export())?,
                metadata(&prepared.resources)?,
                if replace { "replace" } else { "create" }
            )
            .map_err(output_error)?;
            if !args.yes {
                if !interaction.interactive() {
                    return Err(
                        "hidden input requires a terminal; automation must use --stdin --yes"
                            .to_owned(),
                    );
                }
                writeln!(out, "Type yes to approve this exact operation:").map_err(output_error)?;
                out.flush().map_err(output_error)?;
                if interaction.line()?.trim() != "yes" {
                    return Err("secret write cancelled".to_owned());
                }
            }
            if !args.stdin && !interaction.interactive() {
                return Err("hidden credential input requires a terminal".to_owned());
            }
            let value = if args.stdin {
                interaction.stdin(provider.max_secret_bytes())?
            } else {
                interaction.hidden()?
            };
            if value.len() > provider.max_secret_bytes() {
                return Err("credential exceeds the provider value-size limit".to_owned());
            }
            validate_credential(binding.scope(), &value)
                .map_err(|_| "credential representation is invalid for the selected purpose; contents withheld".to_owned())?;
            provider.write(&prepared.resources, binding.key(), &value, replace)?;
            writeln!(out, "Storage confirmed: {}. No deployment, activation, links or runtime refresh performed.",
                if replace { "replaced existing binding" } else { "created binding" })
                .map_err(output_error)?;
            Ok(RunOutcome::Success)
        }
    }
}

/// Execute a credential command with native transport and terminal interaction.
///
/// # Errors
/// Returns a sanitized error for invalid inputs, plan drift, unsupported operations,
/// unavailable interaction, or failed provider operations.
pub(crate) fn run(command: &SecretsCommand) -> Result<RunOutcome, String> {
    let mut out = io::stdout().lock();
    execute(
        command,
        &fastly::NativeProvider { transport: None },
        &mut Terminal,
        &mut out,
    )
}
