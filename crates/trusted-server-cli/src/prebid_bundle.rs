use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Deserializer, Serialize};
use toml_edit::{DocumentMut, Item, table, value};

pub(crate) type CliResult<T> = Result<T, String>;

const NODE_MODULES_MISSING_HELP: &str = "Prebid bundling dependencies are missing. Run `cd crates/trusted-server-js/lib && npm ci`, then retry `ts prebid bundle`.";

#[derive(Debug, clap::Args)]
pub(crate) struct PrebidBundleArgs {
    /// Trusted Server config path.
    #[arg(long, default_value = "trusted-server.toml")]
    pub config: PathBuf,
    /// Local output directory for generated Prebid bundle artifacts.
    #[arg(long, default_value = "dist/prebid")]
    pub out: PathBuf,
}

fn report_error(message: impl Into<String>) -> String {
    message.into()
}

fn cli_error<T>(message: impl Into<String>) -> CliResult<T> {
    Err(message.into())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct PrebidModuleName(String);

impl PrebidModuleName {
    fn new(value: String) -> CliResult<Self> {
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return cli_error(format!(
                "invalid Prebid module stem {value:?}; use the exact upstream filename without .js"
            ));
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PrebidModuleName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrebidBundleModules {
    pub bidder: Vec<PrebidModuleName>,
    #[serde(default)]
    pub user_id: Option<Vec<PrebidModuleName>>,
    #[serde(default)]
    pub analytics: Option<Vec<PrebidModuleName>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrebidBundleSection {
    modules: PrebidBundleModules,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrebidBundleConfig {
    pub modules: PrebidBundleModules,
    pub external_bundle_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrebidBundleGenerateRequest {
    pub js_lib_dir: PathBuf,
    pub out_dir: PathBuf,
    pub modules: PrebidBundleModules,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrebidBundleModuleRequest<'a> {
    bidder: &'a [PrebidModuleName],
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<&'a [PrebidModuleName]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analytics: Option<&'a [PrebidModuleName]>,
}

pub(crate) trait PrebidBundleGenerator {
    fn generate(
        &mut self,
        request: &PrebidBundleGenerateRequest,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> CliResult<()>;
}

#[derive(Default)]
pub(crate) struct NpmPrebidBundleGenerator;

impl PrebidBundleGenerator for NpmPrebidBundleGenerator {
    fn generate(
        &mut self,
        request: &PrebidBundleGenerateRequest,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> CliResult<()> {
        ensure_local_build_prerequisites(&request.js_lib_dir)?;

        let args = npm_prebid_bundle_args(request)?;

        let output = Command::new("npm")
            .args(&args)
            .current_dir(&request.js_lib_dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                report_error(format!(
                    "failed to run Prebid bundle generator with npm: {error}"
                ))
            })?;

        if !output.stdout.is_empty() {
            out.write_all(&output.stdout).map_err(|error| {
                report_error(format!("failed to forward generator stdout: {error}"))
            })?;
        }

        if !output.stderr.is_empty() {
            err.write_all(&output.stderr).map_err(|error| {
                report_error(format!("failed to forward generator stderr: {error}"))
            })?;
        }

        if output.status.success() {
            Ok(())
        } else {
            cli_error(format!(
                "Prebid bundle generator exited with status {}",
                output.status
            ))
        }
    }
}

fn npm_prebid_bundle_args(request: &PrebidBundleGenerateRequest) -> CliResult<Vec<String>> {
    let modules = PrebidBundleModuleRequest {
        bidder: &request.modules.bidder,
        user_id: request.modules.user_id.as_deref(),
        analytics: request.modules.analytics.as_deref(),
    };
    let modules_json = serde_json::to_string(&modules).map_err(|error| {
        report_error(format!(
            "failed to serialize Prebid module request: {error}"
        ))
    })?;

    Ok(vec![
        "run".to_string(),
        "build:prebid-external".to_string(),
        "--".to_string(),
        "--modules-json".to_string(),
        modules_json,
        "--out".to_string(),
        request.out_dir.display().to_string(),
    ])
}

#[derive(Debug, Deserialize)]
struct PrebidBundleManifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    sha256: String,
    sri: String,
    filename: String,
}

pub(crate) fn run_bundle(
    args: &PrebidBundleArgs,
    generator: &mut dyn PrebidBundleGenerator,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<()> {
    let config = load_bundle_config(&args.config)?;
    let current_dir = env::current_dir()
        .map_err(|error| report_error(format!("failed to read current directory: {error}")))?;
    let js_lib_dir = find_js_lib_dir(&current_dir)?;
    let out_dir = resolve_output_dir(&current_dir, &args.out);
    ensure_output_dir_writable(&out_dir)?;

    let request = PrebidBundleGenerateRequest {
        js_lib_dir,
        out_dir: out_dir.clone(),
        modules: config.modules,
    };

    generator.generate(&request, out, err)?;

    let manifest_path = out_dir.join("manifest.json");
    let manifest = load_manifest(&manifest_path)?;
    patch_config_metadata(&args.config, &manifest.sha256, &manifest.sri)?;

    writeln!(
        out,
        "Built Prebid bundle: {}",
        out_dir.join(&manifest.filename).display()
    )
    .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    writeln!(out, "Manifest: {}", manifest_path.display())
        .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    writeln!(out, "Updated config: {}", args.config.display())
        .map_err(|error| report_error(format!("failed to write command output: {error}")))?;

    let bundle_filename = manifest.filename.as_str();
    if config.external_bundle_url.is_none() {
        writeln!(
            out,
            "Next: upload {bundle_filename} and set integrations.prebid.external_bundle_url to its HTTPS URL."
        )
    } else {
        writeln!(
            out,
            "Next: upload {bundle_filename} and update integrations.prebid.external_bundle_url if the hosted filename changed."
        )
    }
    .map_err(|error| report_error(format!("failed to write command output: {error}")))?;

    Ok(())
}

pub(crate) fn load_bundle_config(config_path: &Path) -> CliResult<PrebidBundleConfig> {
    let contents = fs::read_to_string(config_path).map_err(|error| {
        report_error(format!(
            "missing {}: run `ts config init` or pass --config <path>: {error}",
            config_path.display()
        ))
    })?;
    let root: toml::Value = toml::from_str(&contents).map_err(|error| {
        report_error(format!(
            "invalid TOML in {}: {error}",
            config_path.display()
        ))
    })?;

    let prebid = root
        .get("integrations")
        .and_then(|integrations| integrations.get("prebid"))
        .ok_or_else(|| {
            report_error(format!(
                "{} is missing [integrations.prebid]",
                config_path.display()
            ))
        })?;
    let bundle = prebid.get("bundle").ok_or_else(|| {
        report_error(format!(
            "{} is missing [integrations.prebid.bundle]",
            config_path.display()
        ))
    })?;

    let bundle_table = bundle.as_table().ok_or_else(|| {
        report_error(format!(
            "{} integrations.prebid.bundle must be a TOML table",
            config_path.display()
        ))
    })?;
    for (removed, replacement) in [
        ("adapters", "integrations.prebid.bundle.modules.bidder"),
        (
            "user_id_modules",
            "integrations.prebid.bundle.modules.user_id",
        ),
        (
            "analytics_adapters",
            "integrations.prebid.bundle.modules.analytics",
        ),
    ] {
        if bundle_table.contains_key(removed) {
            return cli_error(format!(
                "integrations.prebid.bundle.{removed} is no longer supported; configure exact module stems under {replacement}"
            ));
        }
    }

    let section: PrebidBundleSection = bundle.clone().try_into().map_err(|error| {
        report_error(format!(
            "{} has invalid integrations.prebid.bundle configuration: {error}",
            config_path.display()
        ))
    })?;
    validate_bundle_modules(&section.modules, config_path)?;

    let external_bundle_url = prebid
        .get("external_bundle_url")
        .and_then(toml::Value::as_str)
        .map(str::to_string);

    Ok(PrebidBundleConfig {
        modules: section.modules,
        external_bundle_url,
    })
}

fn validate_bundle_modules(modules: &PrebidBundleModules, config_path: &Path) -> CliResult<()> {
    if modules.bidder.is_empty() {
        return cli_error(format!(
            "{} integrations.prebid.bundle.modules.bidder must contain at least one module stem",
            config_path.display()
        ));
    }

    let selections = [
        (
            "integrations.prebid.bundle.modules.bidder",
            Some(modules.bidder.as_slice()),
        ),
        (
            "integrations.prebid.bundle.modules.user_id",
            modules.user_id.as_deref(),
        ),
        (
            "integrations.prebid.bundle.modules.analytics",
            modules.analytics.as_deref(),
        ),
    ];
    let mut owners: Vec<(&str, &str)> = Vec::new();
    for (field, names) in selections {
        for name in names.unwrap_or_default() {
            if let Some((_, previous_field)) = owners
                .iter()
                .find(|(previous_name, _)| *previous_name == name.as_str())
            {
                return cli_error(format!(
                    "{} {field} repeats module stem {:?} already selected by {previous_field}",
                    config_path.display(),
                    name.as_str()
                ));
            }
            owners.push((name.as_str(), field));
        }
    }

    Ok(())
}

fn ensure_local_build_prerequisites(js_lib_dir: &Path) -> CliResult<()> {
    which::which("npm").map_err(|error| {
        report_error(format!(
            "npm is required to build the Prebid bundle but was not found on PATH: {error}"
        ))
    })?;

    ensure_file_exists(
        &js_lib_dir.join("package.json"),
        "Prebid bundle package manifest",
    )?;
    ensure_file_exists(
        &js_lib_dir.join("build-prebid-external.mjs"),
        "Prebid external bundle generator",
    )?;

    let node_modules = js_lib_dir.join("node_modules");
    if !node_modules.is_dir() {
        return cli_error(NODE_MODULES_MISSING_HELP);
    }

    Ok(())
}

fn ensure_file_exists(path: &Path, description: &str) -> CliResult<()> {
    if path.is_file() {
        Ok(())
    } else {
        cli_error(format!("missing {description}: {}", path.display()))
    }
}

fn find_js_lib_dir(start: &Path) -> CliResult<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join("crates/trusted-server-js/lib");
        if is_js_lib_dir(&candidate) {
            return Ok(candidate);
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest_dir
        .join("../..")
        .join("crates/trusted-server-js/lib");
    if is_js_lib_dir(&candidate) {
        return candidate.canonicalize().map_err(|error| {
            report_error(format!(
                "failed to resolve JS library directory {}: {error}",
                candidate.display()
            ))
        });
    }

    cli_error(
        "failed to locate crates/trusted-server-js/lib; run `ts prebid bundle` from the Trusted Server repository",
    )
}

fn is_js_lib_dir(path: &Path) -> bool {
    path.join("package.json").is_file() && path.join("build-prebid-external.mjs").is_file()
}

fn resolve_output_dir(current_dir: &Path, out_dir: &Path) -> PathBuf {
    if out_dir.is_absolute() {
        out_dir.to_path_buf()
    } else {
        current_dir.join(out_dir)
    }
}

fn ensure_output_dir_writable(out_dir: &Path) -> CliResult<()> {
    if out_dir.exists() && !out_dir.is_dir() {
        return cli_error(format!(
            "Prebid bundle output path {} exists but is not a directory",
            out_dir.display()
        ));
    }

    fs::create_dir_all(out_dir).map_err(|error| {
        report_error(format!(
            "failed to create Prebid bundle output directory {}: {error}",
            out_dir.display()
        ))
    })?;

    let probe = out_dir.join(format!(
        ".ts-prebid-bundle-write-test-{}",
        std::process::id()
    ));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| {
            report_error(format!(
                "Prebid bundle output directory {} is not writable: {error}",
                out_dir.display()
            ))
        })?;
    fs::remove_file(&probe).map_err(|error| {
        report_error(format!(
            "failed to remove Prebid bundle output probe {}: {error}",
            probe.display()
        ))
    })?;

    Ok(())
}

fn load_manifest(path: &Path) -> CliResult<PrebidBundleManifest> {
    let contents = fs::read_to_string(path).map_err(|error| {
        report_error(format!(
            "failed to read generated Prebid manifest {}: {error}",
            path.display()
        ))
    })?;
    let manifest: PrebidBundleManifest = serde_json::from_str(&contents).map_err(|error| {
        report_error(format!(
            "failed to parse generated Prebid manifest {}: {error}",
            path.display()
        ))
    })?;

    if manifest.schema_version != 1 {
        return cli_error(format!(
            "generated Prebid manifest {} uses unsupported schemaVersion {}; expected 1",
            path.display(),
            manifest.schema_version
        ));
    }
    if manifest.filename.trim().is_empty() {
        return cli_error(format!(
            "generated Prebid manifest {} is missing filename",
            path.display()
        ));
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return cli_error(format!(
            "generated Prebid manifest {} has invalid sha256",
            path.display()
        ));
    }
    if !manifest.sri.starts_with("sha384-") {
        return cli_error(format!(
            "generated Prebid manifest {} has invalid sri",
            path.display()
        ));
    }

    Ok(manifest)
}

fn patch_config_metadata(config_path: &Path, sha256: &str, sri: &str) -> CliResult<()> {
    let contents = fs::read_to_string(config_path).map_err(|error| {
        report_error(format!(
            "failed to read config {} for metadata update: {error}",
            config_path.display()
        ))
    })?;
    let mut document = contents.parse::<DocumentMut>().map_err(|error| {
        report_error(format!(
            "failed to parse config {} for metadata update: {error}",
            config_path.display()
        ))
    })?;

    if !document.contains_key("integrations") {
        document.insert("integrations", table());
    }
    let integrations = table_like_mut(
        document
            .get_mut("integrations")
            .expect("should have integrations table"),
        "integrations",
        config_path,
    )?;

    if !integrations.contains_key("prebid") {
        integrations.insert("prebid", table());
    }
    let prebid = table_like_mut(
        integrations
            .get_mut("prebid")
            .expect("should have prebid table"),
        "integrations.prebid",
        config_path,
    )?;

    prebid.insert("external_bundle_sha256", value(sha256));
    prebid.insert("external_bundle_sri", value(sri));

    write_atomic(config_path, &document.to_string())
}

fn table_like_mut<'a>(
    item: &'a mut Item,
    field_name: &str,
    config_path: &Path,
) -> CliResult<&'a mut dyn toml_edit::TableLike> {
    item.as_table_like_mut().ok_or_else(|| {
        report_error(format!(
            "{} {field_name} must be a TOML table",
            config_path.display()
        ))
    })
}

fn write_atomic(path: &Path, contents: &str) -> CliResult<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        report_error(format!(
            "failed to create config parent directory {}: {error}",
            parent.display()
        ))
    })?;

    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("trusted-server.toml");
    let tmp_path = parent.join(format!(".{filename}.tmp-{}", std::process::id()));

    fs::write(&tmp_path, contents).map_err(|error| {
        report_error(format!(
            "failed to write temporary config {}: {error}",
            tmp_path.display()
        ))
    })?;
    fs::rename(&tmp_path, path).map_err(|error| {
        let _ = fs::remove_file(&tmp_path);
        report_error(format!(
            "failed to replace config {} with {}: {error}",
            path.display(),
            tmp_path.display()
        ))
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        fs::write(&path, contents).expect("should write config");
        (temp, path)
    }

    fn valid_config() -> String {
        r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"
external_bundle_url = "https://assets.example.com/prebid/trusted-prebid-old.js"

[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter", "kargoBidAdapter"]
user_id = ["sharedIdSystem", "uid2IdSystem"]
analytics = ["atsAnalyticsAdapter"]
"#
        .to_string()
    }

    fn module_names(names: &[&str]) -> Vec<PrebidModuleName> {
        names
            .iter()
            .map(|name| PrebidModuleName::new((*name).to_string()).expect("should be valid module"))
            .collect()
    }

    fn names(modules: &[PrebidModuleName]) -> Vec<&str> {
        modules.iter().map(PrebidModuleName::as_str).collect()
    }

    #[test]
    fn bundle_config_loader_accepts_valid_settings() {
        let (_temp, path) = write_config(&valid_config());

        let config = load_bundle_config(&path).expect("should load bundle config");

        assert_eq!(
            names(&config.modules.bidder),
            ["rubiconBidAdapter", "kargoBidAdapter"]
        );
        assert_eq!(
            names(
                config
                    .modules
                    .user_id
                    .as_deref()
                    .expect("should have User ID modules")
            ),
            ["sharedIdSystem", "uid2IdSystem"]
        );
        assert_eq!(
            names(
                config
                    .modules
                    .analytics
                    .as_deref()
                    .expect("should have analytics modules")
            ),
            ["atsAnalyticsAdapter"]
        );
        assert_eq!(
            config.external_bundle_url.as_deref(),
            Some("https://assets.example.com/prebid/trusted-prebid-old.js")
        );
    }

    #[test]
    fn bundle_config_loader_preserves_omitted_and_empty_optional_lists() {
        let (_omitted_temp, omitted_path) = write_config(
            r#"
[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
"#,
        );
        let omitted = load_bundle_config(&omitted_path).expect("should load omitted lists");
        assert_eq!(omitted.modules.user_id, None);
        assert_eq!(omitted.modules.analytics, None);

        let (_empty_temp, empty_path) = write_config(
            r#"
[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
user_id = []
analytics = []
"#,
        );
        let empty = load_bundle_config(&empty_path).expect("should load empty lists");
        assert_eq!(empty.modules.user_id, Some(Vec::new()));
        assert_eq!(empty.modules.analytics, Some(Vec::new()));
    }

    #[test]
    fn bundle_config_loader_rejects_missing_prebid_block() {
        let (_temp, path) = write_config("[publisher]\ndomain = \"example.com\"\n");

        let error = load_bundle_config(&path).expect_err("should reject missing prebid block");

        assert!(
            error.contains("missing [integrations.prebid]"),
            "error should explain missing prebid block: {error:?}"
        );
    }

    #[test]
    fn bundle_config_loader_rejects_missing_bundle_or_modules() {
        for (contents, expected) in [
            (
                "[integrations.prebid]\nenabled = true\n",
                "missing [integrations.prebid.bundle]",
            ),
            ("[integrations.prebid.bundle]\n", "missing field `modules`"),
        ] {
            let (_temp, path) = write_config(contents);
            let error = load_bundle_config(&path).expect_err("should reject missing table");
            assert!(
                error.contains(expected),
                "error should contain {expected:?}: {error:?}"
            );
        }
    }

    #[test]
    fn bundle_config_loader_rejects_empty_or_malformed_bidder_lists() {
        for (contents, expected) in [
            (
                "[integrations.prebid.bundle.modules]\nbidder = []\n",
                "must contain at least one",
            ),
            (
                "[integrations.prebid.bundle.modules]\nbidder = [\"rubiconBidAdapter\", 123]\n",
                "invalid type",
            ),
            (
                "[integrations.prebid.bundle.modules]\nbidder = \"rubiconBidAdapter\"\n",
                "invalid type",
            ),
        ] {
            let (_temp, path) = write_config(contents);
            let error = load_bundle_config(&path).expect_err("should reject bidder list");
            assert!(
                error.contains(expected),
                "error should contain {expected:?}: {error:?}"
            );
        }
    }

    #[test]
    fn bundle_config_loader_rejects_invalid_module_stems() {
        for stem in [
            "",
            " ",
            "rubiconBidAdapter.js",
            "../rubiconBidAdapter",
            "group/rubiconBidAdapter",
            "group\\rubiconBidAdapter",
            "https://example.com/adapter",
            "rubiconBidAdapter'",
            "rubicon\nBidAdapter",
        ] {
            let contents = format!("[integrations.prebid.bundle.modules]\nbidder = [{stem:?}]\n");
            let (_temp, path) = write_config(&contents);
            let error = load_bundle_config(&path).expect_err("should reject invalid stem");
            assert!(
                error.contains("invalid Prebid module stem"),
                "error should reject {stem:?}: {error:?}"
            );
        }
    }

    #[test]
    fn bundle_config_loader_rejects_duplicates_within_and_across_kinds() {
        for contents in [
            r#"
[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter", "rubiconBidAdapter"]
"#,
            r#"
[integrations.prebid.bundle.modules]
bidder = ["exampleModule"]
analytics = ["exampleModule"]
"#,
        ] {
            let (_temp, path) = write_config(contents);
            let error = load_bundle_config(&path).expect_err("should reject duplicate module");
            assert!(
                error.contains("repeats module stem"),
                "error should identify duplicate: {error:?}"
            );
        }
    }

    #[test]
    fn bundle_config_loader_rejects_removed_fields_in_fixed_order() {
        let (_temp, path) = write_config(
            r#"
[integrations.prebid.bundle]
adapters = ["rubicon"]
user_id_modules = ["sharedIdSystem"]
analytics_adapters = ["atsAnalyticsAdapter"]

[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
"#,
        );

        let error = load_bundle_config(&path).expect_err("should reject removed field");

        assert!(error.contains("bundle.adapters is no longer supported"));
        assert!(error.contains("bundle.modules.bidder"));
    }

    #[test]
    fn bundle_config_loader_reports_each_removed_field_replacement() {
        for (field, replacement) in [
            ("adapters", "bundle.modules.bidder"),
            ("user_id_modules", "bundle.modules.user_id"),
            ("analytics_adapters", "bundle.modules.analytics"),
        ] {
            let contents = format!(
                r#"
[integrations.prebid.bundle]
{field} = ["exampleModule"]

[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
"#
            );
            let (_temp, path) = write_config(&contents);

            let error = load_bundle_config(&path).expect_err("should reject removed field");

            assert!(
                error.contains(&format!("bundle.{field} is no longer supported")),
                "error should name removed field: {error:?}"
            );
            assert!(
                error.contains(replacement),
                "error should name {replacement}: {error:?}"
            );
        }
    }

    #[test]
    fn bundle_config_loader_rejects_unknown_module_kinds() {
        let (_temp, path) = write_config(
            r#"
[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
real_time_data = ["exampleRtdProvider"]
"#,
        );

        let error = load_bundle_config(&path).expect_err("should reject unknown kind");

        assert!(error.contains("unknown field `real_time_data`"));
        assert!(error.contains("integrations.prebid.bundle"));
    }

    #[test]
    fn output_dir_validation_rejects_existing_file() {
        let temp = tempfile::TempDir::new().expect("should create temp dir");
        let out_path = temp.path().join("prebid");
        fs::write(&out_path, "not a directory").expect("should write file");

        let error =
            ensure_output_dir_writable(&out_path).expect_err("should reject output path file");

        assert!(
            error.to_string().contains("not a directory"),
            "error should explain invalid output path: {error:?}"
        );
    }

    #[test]
    fn output_dir_validation_creates_writable_directory() {
        let temp = tempfile::TempDir::new().expect("should create temp dir");
        let out_path = temp.path().join("dist/prebid");

        ensure_output_dir_writable(&out_path).expect("should create output dir");

        assert!(out_path.is_dir(), "should create output directory");
    }

    #[test]
    fn npm_prebid_bundle_args_serialize_one_typed_module_request() {
        let request = PrebidBundleGenerateRequest {
            js_lib_dir: PathBuf::from("crates/trusted-server-js/lib"),
            out_dir: PathBuf::from("/tmp/prebid"),
            modules: PrebidBundleModules {
                bidder: module_names(&["rubiconBidAdapter", "kargoBidAdapter"]),
                user_id: Some(module_names(&["sharedIdSystem"])),
                analytics: Some(module_names(&["atsAnalyticsAdapter"])),
            },
        };

        assert_eq!(
            npm_prebid_bundle_args(&request).expect("should serialize module request"),
            [
                "run",
                "build:prebid-external",
                "--",
                "--modules-json",
                r#"{"bidder":["rubiconBidAdapter","kargoBidAdapter"],"userId":["sharedIdSystem"],"analytics":["atsAnalyticsAdapter"]}"#,
                "--out",
                "/tmp/prebid",
            ],
            "should pass one JSON argument and the output path"
        );
    }

    #[test]
    fn npm_prebid_bundle_args_distinguish_omitted_and_empty_lists() {
        for (user_id, analytics, expected_json) in [
            (None, None, r#"{"bidder":["rubiconBidAdapter"]}"#),
            (
                Some(Vec::new()),
                Some(Vec::new()),
                r#"{"bidder":["rubiconBidAdapter"],"userId":[],"analytics":[]}"#,
            ),
        ] {
            let request = PrebidBundleGenerateRequest {
                js_lib_dir: PathBuf::from("crates/trusted-server-js/lib"),
                out_dir: PathBuf::from("/tmp/prebid"),
                modules: PrebidBundleModules {
                    bidder: module_names(&["rubiconBidAdapter"]),
                    user_id,
                    analytics,
                },
            };
            let args = npm_prebid_bundle_args(&request).expect("should serialize module request");

            assert_eq!(args[3], "--modules-json");
            assert_eq!(args[4], expected_json);
            assert!(!args.iter().any(|arg| arg == "--adapters"));
            assert!(!args.iter().any(|arg| arg == "--user-id-modules"));
        }
    }

    #[test]
    fn patch_config_metadata_writes_hash_and_sri() {
        let (_temp, path) = write_config(&valid_config());
        let sha256 = "a".repeat(64);
        let sri = "sha384-abc";

        patch_config_metadata(&path, &sha256, sri).expect("should patch config metadata");

        let contents = fs::read_to_string(&path).expect("should read patched config");
        let value: toml::Value = toml::from_str(&contents).expect("should parse patched config");
        let prebid = value
            .get("integrations")
            .and_then(|integrations| integrations.get("prebid"))
            .expect("should have prebid table");
        assert_eq!(
            prebid
                .get("external_bundle_url")
                .and_then(toml::Value::as_str),
            Some("https://assets.example.com/prebid/trusted-prebid-old.js"),
            "should preserve external bundle URL"
        );
        assert_eq!(
            prebid
                .get("external_bundle_sha256")
                .and_then(toml::Value::as_str),
            Some(sha256.as_str()),
            "should write sha256"
        );
        assert_eq!(
            prebid
                .get("external_bundle_sri")
                .and_then(toml::Value::as_str),
            Some(sri),
            "should write SRI"
        );
    }

    struct FakeGenerator {
        generate_error: Option<String>,
        generate_calls: Vec<PrebidBundleGenerateRequest>,
        write_manifest: bool,
        manifest_schema: Option<serde_json::Value>,
    }

    impl PrebidBundleGenerator for FakeGenerator {
        fn generate(
            &mut self,
            request: &PrebidBundleGenerateRequest,
            out: &mut dyn Write,
            err: &mut dyn Write,
        ) -> CliResult<()> {
            self.generate_calls.push(request.clone());

            out.write_all(b"generator stdout\n")
                .expect("should capture generator stdout");
            err.write_all(b"generator stderr\n")
                .expect("should capture generator stderr");

            if self.write_manifest {
                fs::create_dir_all(&request.out_dir).expect("should create output dir");
                let mut manifest = serde_json::json!({
                    "prebidVersion": "10.26.0",
                    "modules": {
                        "bidder": request.modules.bidder,
                        "userId": request.modules.user_id,
                        "analytics": request.modules.analytics,
                    },
                    "runtimeCodes": {
                        "bidder": ["rubicon"],
                        "analytics": ["atsAnalytics"],
                    },
                    "sha256": "b".repeat(64),
                    "sri": "sha384-test",
                    "filename": format!("trusted-prebid-{}.js", "b".repeat(64))
                });
                if let Some(schema) = &self.manifest_schema {
                    manifest
                        .as_object_mut()
                        .expect("should be manifest object")
                        .insert("schemaVersion".to_string(), schema.clone());
                }
                fs::write(request.out_dir.join("manifest.json"), manifest.to_string())
                    .expect("should write fake manifest");
            }

            if let Some(error) = &self.generate_error {
                cli_error(error.clone())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn run_bundle_forwards_generator_output_to_stdio() {
        let (_temp, config_path) = write_config(&valid_config());
        let _out_root = tempfile::tempdir().expect("should create temp dir");
        let out_dir = _out_root.path().join("prebid");

        let mut generator = FakeGenerator {
            generate_error: None,
            generate_calls: Vec::new(),
            write_manifest: true,
            manifest_schema: Some(serde_json::json!(1)),
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = PrebidBundleArgs {
            config: config_path,
            out: out_dir.clone(),
        };

        run_bundle(&args, &mut generator, &mut out, &mut err).expect("should run bundle command");

        let output = String::from_utf8(out).expect("stdout should be valid utf8");
        assert!(output.contains("generator stdout"));
        assert!(
            output.contains(&format!(
                "Next: upload trusted-prebid-{}.js and update integrations.prebid.external_bundle_url",
                "b".repeat(64)
            )),
            "should tell operators which content-addressed filename to host: {output}"
        );
        let stderr = String::from_utf8(err).expect("stderr should be valid utf8");
        assert!(stderr.contains("generator stderr"));

        assert_eq!(generator.generate_calls.len(), 1);
        assert_eq!(
            names(&generator.generate_calls[0].modules.bidder),
            ["rubiconBidAdapter", "kargoBidAdapter"]
        );

        let patched = fs::read_to_string(&args.config).expect("should read patched config");
        assert!(patched.contains(&format!("external_bundle_sha256 = \"{}\"", "b".repeat(64))));
        assert!(patched.contains("external_bundle_sri = \"sha384-test\""));
    }

    #[test]
    fn run_bundle_does_not_patch_config_when_generation_fails() {
        let (_temp, config_path) = write_config(&valid_config());
        let original_config =
            fs::read_to_string(&config_path).expect("should read baseline config");
        let _out_root = tempfile::tempdir().expect("should create temp dir");
        let out_dir = _out_root.path().join("prebid");

        let mut generator = FakeGenerator {
            generate_error: Some("builder failed".to_string()),
            generate_calls: Vec::new(),
            write_manifest: false,
            manifest_schema: None,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = PrebidBundleArgs {
            config: config_path,
            out: out_dir,
        };

        let error = run_bundle(&args, &mut generator, &mut out, &mut err)
            .expect_err("should propagate generator failure");

        assert!(error.to_string().contains("builder failed"));
        assert!(fs::read_to_string(&args.config).expect("should read config") == original_config);
    }

    #[test]
    fn load_manifest_rejects_missing_or_unsupported_schema_versions() {
        for (schema, expected) in [
            (None, "schemaVersion"),
            (Some(serde_json::json!(0)), "unsupported schemaVersion 0"),
            (Some(serde_json::json!(2)), "unsupported schemaVersion 2"),
            (Some(serde_json::json!("1")), "invalid type"),
        ] {
            let temp = tempfile::tempdir().expect("should create temp dir");
            let path = temp.path().join("manifest.json");
            let mut manifest = serde_json::json!({
                "sha256": "b".repeat(64),
                "sri": "sha384-test",
                "filename": format!("trusted-prebid-{}.js", "b".repeat(64))
            });
            if let Some(schema) = schema {
                manifest
                    .as_object_mut()
                    .expect("should be manifest object")
                    .insert("schemaVersion".to_string(), schema);
            }
            fs::write(&path, manifest.to_string()).expect("should write manifest");

            let error = load_manifest(&path).expect_err("should reject manifest schema");

            assert!(
                error.contains(expected),
                "error should contain {expected:?}: {error:?}"
            );
        }
    }

    #[test]
    fn run_bundle_does_not_patch_config_when_manifest_schema_is_invalid() {
        for schema in [
            None,
            Some(serde_json::json!(0)),
            Some(serde_json::json!(2)),
            Some(serde_json::json!("1")),
        ] {
            let (_temp, config_path) = write_config(&valid_config());
            let original_config =
                fs::read_to_string(&config_path).expect("should read baseline config");
            let out_root = tempfile::tempdir().expect("should create temp dir");
            let mut generator = FakeGenerator {
                generate_error: None,
                generate_calls: Vec::new(),
                write_manifest: true,
                manifest_schema: schema,
            };
            let args = PrebidBundleArgs {
                config: config_path,
                out: out_root.path().join("prebid"),
            };

            run_bundle(&args, &mut generator, &mut Vec::new(), &mut Vec::new())
                .expect_err("should reject invalid manifest schema");

            assert_eq!(
                fs::read_to_string(&args.config).expect("should read unchanged config"),
                original_config,
                "manifest failure should leave config unchanged"
            );
        }
    }

    #[test]
    fn missing_node_modules_fails_with_npm_ci_instruction() {
        let temp = tempfile::TempDir::new().expect("should create temp dir");
        fs::write(temp.path().join("package.json"), "{}").expect("should write package manifest");
        fs::write(temp.path().join("build-prebid-external.mjs"), "")
            .expect("should write generator");

        let error = ensure_local_build_prerequisites(temp.path())
            .expect_err("should reject missing node modules");

        assert!(
            error.to_string().contains("npm ci"),
            "error should instruct npm ci: {error:?}"
        );
    }
}
