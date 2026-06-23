use std::env;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser as _;
use edgezero_adapter::registry::{
    self as adapter_registry, AdapterAction, AdapterPushContext, ResolvedStoreId,
};
use edgezero_core::env_config::EnvConfig;
use edgezero_core::manifest::{Manifest, ManifestLoader, ResolvedEnvironment};

use crate::error::{cli_error, report_error, CliResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleCommand {
    AuthLogin,
    AuthLogout,
    AuthStatus,
    Build,
    Deploy,
    Provision,
    Serve,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigPushRequest {
    pub adapter: String,
    pub manifest: PathBuf,
    pub store: String,
    pub local: bool,
    pub dry_run: bool,
    pub runtime_config: Option<PathBuf>,
    pub entries: Vec<(String, String)>,
    pub config_hash: String,
}

pub trait EdgeZeroDelegate {
    fn run_lifecycle(
        &mut self,
        command: LifecycleCommand,
        adapter: &str,
        passthrough: &[String],
    ) -> CliResult<()>;

    fn push_config(&mut self, request: &ConfigPushRequest, out: &mut dyn Write) -> CliResult<()>;
}

#[derive(Default)]
pub struct ProductionEdgeZeroDelegate;

impl EdgeZeroDelegate for ProductionEdgeZeroDelegate {
    fn run_lifecycle(
        &mut self,
        command: LifecycleCommand,
        adapter: &str,
        passthrough: &[String],
    ) -> CliResult<()> {
        match command {
            LifecycleCommand::Provision => run_edgezero_provision(adapter, passthrough),
            other => run_edgezero_lifecycle(other, adapter, passthrough),
        }
    }

    fn push_config(&mut self, request: &ConfigPushRequest, out: &mut dyn Write) -> CliResult<()> {
        push_config_entries(request, out)
    }
}

fn run_edgezero_provision(adapter: &str, passthrough: &[String]) -> CliResult<()> {
    let mut argv = vec![
        "edgezero".to_string(),
        "provision".to_string(),
        "--adapter".to_string(),
        adapter.to_string(),
    ];
    argv.extend(passthrough.iter().cloned());
    let parsed = edgezero_cli::args::Args::try_parse_from(argv).map_err(|error| {
        report_error(format!(
            "[edgezero] failed to parse provision args: {error}"
        ))
    })?;
    let edgezero_cli::args::Command::Provision(args) = parsed.cmd else {
        return cli_error("internal error: parsed EdgeZero command was not provision");
    };
    edgezero_cli::run_provision(&args).map_err(|error| report_error(format!("[edgezero] {error}")))
}

fn run_edgezero_lifecycle(
    command: LifecycleCommand,
    adapter_name: &str,
    passthrough: &[String],
) -> CliResult<()> {
    let manifest = load_manifest_optional()?;
    ensure_adapter_defined(adapter_name, manifest.as_ref())?;

    if let Some(loader) = &manifest {
        if let Some(command_text) = manifest_command(loader.manifest(), adapter_name, command) {
            let manifest = loader.manifest();
            let root = manifest.root().unwrap_or_else(|| Path::new("."));
            let environment = manifest.environment_for(adapter_name);
            let adapter_bind = adapter_bind_from_manifest(manifest, adapter_name);
            return run_shell(command_text, root, &environment, adapter_bind, passthrough);
        }
    }

    let adapter = adapter_registry::get_adapter(adapter_name).ok_or_else(|| {
        let available = adapter_registry::registered_adapters();
        report_error(if available.is_empty() {
            format!("adapter `{adapter_name}` is not registered in this build")
        } else {
            format!(
                "adapter `{}` is not registered (available: {})",
                adapter_name,
                available.join(", ")
            )
        })
    })?;

    adapter
        .execute(adapter_action(command), passthrough)
        .map_err(|error| report_error(format!("[edgezero] {error}")))
}

fn adapter_action(command: LifecycleCommand) -> AdapterAction {
    match command {
        LifecycleCommand::AuthLogin => AdapterAction::AuthLogin,
        LifecycleCommand::AuthLogout => AdapterAction::AuthLogout,
        LifecycleCommand::AuthStatus => AdapterAction::AuthStatus,
        LifecycleCommand::Build => AdapterAction::Build,
        LifecycleCommand::Deploy => AdapterAction::Deploy,
        LifecycleCommand::Serve => AdapterAction::Serve,
        LifecycleCommand::Provision => AdapterAction::Build,
    }
}

fn manifest_command<'manifest>(
    manifest: &'manifest Manifest,
    adapter_name: &str,
    command: LifecycleCommand,
) -> Option<&'manifest str> {
    let (_canonical, cfg) = manifest.adapter_entry(adapter_name)?;
    match command {
        LifecycleCommand::AuthLogin => cfg.commands.auth_login.as_deref(),
        LifecycleCommand::AuthLogout => cfg.commands.auth_logout.as_deref(),
        LifecycleCommand::AuthStatus => cfg.commands.auth_status.as_deref(),
        LifecycleCommand::Build => cfg.commands.build.as_deref(),
        LifecycleCommand::Deploy => cfg.commands.deploy.as_deref(),
        LifecycleCommand::Serve => cfg.commands.serve.as_deref(),
        LifecycleCommand::Provision => None,
    }
}

fn load_manifest_optional() -> CliResult<Option<ManifestLoader>> {
    let (path, explicit) = env::var("EDGEZERO_MANIFEST").map_or_else(
        |_| (PathBuf::from("edgezero.toml"), false),
        |raw| (PathBuf::from(raw), true),
    );

    match ManifestLoader::from_path(&path) {
        Ok(loader) => Ok(Some(loader)),
        Err(error) if error.kind() == ErrorKind::NotFound && !explicit => Ok(None),
        Err(error) => cli_error(format!("failed to load {}: {error}", path.display())),
    }
}

fn ensure_adapter_defined(
    adapter_name: &str,
    manifest_loader: Option<&ManifestLoader>,
) -> CliResult<()> {
    let Some(loader) = manifest_loader else {
        return Ok(());
    };
    if loader.manifest().adapter_entry(adapter_name).is_some() {
        return Ok(());
    }
    let available: Vec<String> = loader.manifest().adapters.keys().cloned().collect();
    if available.is_empty() {
        cli_error(format!(
            "adapter `{adapter_name}` is not configured in edgezero.toml (no adapters defined)"
        ))
    } else {
        cli_error(format!(
            "adapter `{}` is not configured in edgezero.toml (available: {})",
            adapter_name,
            available.join(", ")
        ))
    }
}

fn run_shell(
    command_text: &str,
    cwd: &Path,
    environment: &ResolvedEnvironment,
    adapter_bind: (Option<String>, Option<u16>),
    passthrough: &[String],
) -> CliResult<()> {
    let full_command = if passthrough.is_empty() {
        command_text.to_string()
    } else {
        format!("{} {}", command_text, shell_join(passthrough))
    };
    let mut command = Command::new("sh");
    command.arg("-c").arg(&full_command).current_dir(cwd);

    apply_adapter_bind(adapter_bind, &mut command);
    apply_environment(environment, &mut command)?;

    let status = command.status().map_err(|error| {
        report_error(format!(
            "failed to run EdgeZero command `{command_text}`: {error}"
        ))
    })?;

    if status.success() {
        Ok(())
    } else {
        cli_error(format!(
            "EdgeZero command `{command_text}` exited with status {status}"
        ))
    }
}

fn adapter_bind_from_manifest(
    manifest: &Manifest,
    adapter_name: &str,
) -> (Option<String>, Option<u16>) {
    let Some((_canonical, cfg)) = manifest.adapter_entry(adapter_name) else {
        return (None, None);
    };
    (cfg.adapter.host.clone(), cfg.adapter.port)
}

fn apply_adapter_bind(adapter_bind: (Option<String>, Option<u16>), command: &mut Command) {
    let (host, port) = adapter_bind;
    if let Some(host) = host {
        if env::var_os("EDGEZERO__ADAPTER__HOST").is_none() {
            command.env("EDGEZERO__ADAPTER__HOST", host);
        }
    }
    if let Some(port) = port {
        if env::var_os("EDGEZERO__ADAPTER__PORT").is_none() {
            command.env("EDGEZERO__ADAPTER__PORT", port.to_string());
        }
    }
}

fn apply_environment(environment: &ResolvedEnvironment, command: &mut Command) -> CliResult<()> {
    for binding in &environment.variables {
        if let Some(value) = &binding.value {
            if env::var_os(&binding.env).is_none() {
                command.env(&binding.env, value);
            }
        }
    }

    let missing: Vec<String> = environment
        .secrets
        .iter()
        .filter(|binding| env::var_os(&binding.env).is_none())
        .map(|binding| format!("{} (env `{}`)", binding.name, binding.env))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        cli_error(format!(
            "EdgeZero command requires the following secrets to be set: {}",
            missing.join(", ")
        ))
    }
}

fn shell_escape(arg: &str) -> String {
    if arg.is_empty() {
        "''".to_string()
    } else if arg
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "._-/:=@".contains(ch))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\"'\"'"))
    }
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_escape(arg.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_config_entries(request: &ConfigPushRequest, out: &mut dyn Write) -> CliResult<()> {
    let manifest_loader = ManifestLoader::from_path(&request.manifest).map_err(|error| {
        report_error(format!(
            "failed to load {}: {error}",
            request.manifest.display()
        ))
    })?;
    ensure_adapter_defined(&request.adapter, Some(&manifest_loader))?;
    let manifest = manifest_loader.manifest();
    let (_canonical, adapter_cfg) = manifest.adapter_entry(&request.adapter).ok_or_else(|| {
        report_error(format!(
            "adapter `{}` is not declared in {}",
            request.adapter,
            request.manifest.display()
        ))
    })?;

    let adapter = adapter_registry::get_adapter(&request.adapter).ok_or_else(|| {
        report_error(format!(
            "adapter `{}` is declared in {} but not registered in this build",
            request.adapter,
            request.manifest.display()
        ))
    })?;

    let declaration = manifest.stores.config.as_ref().ok_or_else(|| {
        report_error("manifest has no `[stores.config]` section; declare it before pushing config")
    })?;
    if !declaration.ids.iter().any(|id| id == &request.store) {
        return cli_error(format!(
            "--store={:?} is not in [stores.config].ids ({:?})",
            request.store, declaration.ids
        ));
    }

    let env_config = EnvConfig::from_env();
    let store = ResolvedStoreId::new(
        request.store.clone(),
        env_config.store_name("config", &request.store),
    );
    let manifest_root = request
        .manifest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut push_context = AdapterPushContext::new().with_local(request.local);
    if let Some(path) = request.runtime_config.as_deref() {
        push_context = push_context.with_runtime_config_path(path);
    }
    if let Some(deploy_cmd) = adapter_cfg.commands.deploy.as_deref() {
        push_context = push_context.with_manifest_adapter_deploy_cmd(deploy_cmd);
    }

    let lines = if request.local {
        adapter.push_config_entries_local(
            manifest_root,
            adapter_cfg.adapter.manifest.as_deref(),
            adapter_cfg.adapter.component.as_deref(),
            &store,
            &request.entries,
            &push_context,
            request.dry_run,
        )
    } else {
        adapter.push_config_entries(
            manifest_root,
            adapter_cfg.adapter.manifest.as_deref(),
            adapter_cfg.adapter.component.as_deref(),
            &store,
            &request.entries,
            &push_context,
            request.dry_run,
        )
    }
    .map_err(|error| report_error(format!("[edgezero] {error}")))?;

    if request.dry_run {
        writeln!(
            out,
            "Config push dry run: {} blob -> {} ({})",
            request.entries.len(),
            request.store,
            request.config_hash
        )
        .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    } else {
        writeln!(
            out,
            "Config pushed: {} blob -> {} ({})",
            request.entries.len(),
            request.store,
            request.config_hash
        )
        .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    }
    for line in lines {
        writeln!(out, "{line}")
            .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[derive(Default)]
    pub struct FakeEdgeZeroDelegate {
        pub lifecycle_calls: Vec<(LifecycleCommand, String, Vec<String>)>,
        pub push_calls: Vec<ConfigPushRequest>,
    }

    impl EdgeZeroDelegate for FakeEdgeZeroDelegate {
        fn run_lifecycle(
            &mut self,
            command: LifecycleCommand,
            adapter: &str,
            passthrough: &[String],
        ) -> CliResult<()> {
            self.lifecycle_calls
                .push((command, adapter.to_string(), passthrough.to_vec()));
            Ok(())
        }

        fn push_config(
            &mut self,
            request: &ConfigPushRequest,
            out: &mut dyn Write,
        ) -> CliResult<()> {
            self.push_calls.push(request.clone());
            writeln!(out, "fake push").map_err(|error| {
                report_error(format!("failed to write fake push output: {error}"))
            })?;
            Ok(())
        }
    }
}
