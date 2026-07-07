use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrebidBundleConfig {
    pub adapters: Vec<String>,
    pub user_id_modules: Option<Vec<String>>,
    pub external_bundle_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrebidBundleGenerateRequest {
    pub js_lib_dir: PathBuf,
    pub out_dir: PathBuf,
    pub adapters: Vec<String>,
    pub user_id_modules: Option<Vec<String>>,
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

        let args = npm_prebid_bundle_args(request);

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

fn npm_prebid_bundle_args(request: &PrebidBundleGenerateRequest) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "build:prebid-external".to_string(),
        "--".to_string(),
        "--adapters".to_string(),
        request.adapters.join(","),
    ];
    if let Some(user_id_modules) = &request.user_id_modules {
        args.push("--user-id-modules".to_string());
        args.push(user_id_modules.join(","));
    }
    args.push("--out".to_string());
    args.push(request.out_dir.display().to_string());
    args
}

#[derive(Debug, Deserialize)]
struct PrebidBundleManifest {
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
        adapters: config.adapters,
        user_id_modules: config.user_id_modules,
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

    let adapters = read_required_string_array(
        bundle,
        "adapters",
        "integrations.prebid.bundle.adapters",
        config_path,
    )?;
    if adapters.is_empty() {
        return cli_error(format!(
            "{} must define at least one integrations.prebid.bundle.adapters entry",
            config_path.display()
        ));
    }

    let user_id_modules = read_optional_string_array(
        bundle,
        "user_id_modules",
        "integrations.prebid.bundle.user_id_modules",
        config_path,
    )?;
    if matches!(user_id_modules.as_ref(), Some(modules) if modules.is_empty()) {
        return cli_error(format!(
            "{} integrations.prebid.bundle.user_id_modules must not be empty when present",
            config_path.display()
        ));
    }

    let external_bundle_url = prebid
        .get("external_bundle_url")
        .and_then(toml::Value::as_str)
        .map(str::to_string);

    Ok(PrebidBundleConfig {
        adapters,
        user_id_modules,
        external_bundle_url,
    })
}

fn read_required_string_array(
    table: &toml::Value,
    key: &str,
    field_name: &str,
    config_path: &Path,
) -> CliResult<Vec<String>> {
    let value = table.get(key).ok_or_else(|| {
        report_error(format!(
            "{} is missing required {field_name}",
            config_path.display()
        ))
    })?;
    read_string_array(value, field_name, config_path)
}

fn read_optional_string_array(
    table: &toml::Value,
    key: &str,
    field_name: &str,
    config_path: &Path,
) -> CliResult<Option<Vec<String>>> {
    table
        .get(key)
        .map(|value| read_string_array(value, field_name, config_path))
        .transpose()
}

fn read_string_array(
    value: &toml::Value,
    field_name: &str,
    config_path: &Path,
) -> CliResult<Vec<String>> {
    let Some(items) = value.as_array() else {
        return cli_error(format!(
            "{} {field_name} must be an array of non-empty strings",
            config_path.display()
        ));
    };

    let mut strings = Vec::with_capacity(items.len());
    for item in items {
        let Some(raw) = item.as_str() else {
            return cli_error(format!(
                "{} {field_name} must be an array of non-empty strings",
                config_path.display()
            ));
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return cli_error(format!(
                "{} {field_name} must not contain empty strings",
                config_path.display()
            ));
        }
        strings.push(trimmed.to_string());
    }

    Ok(strings)
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

[integrations.prebid.bundle]
adapters = ["rubicon", "kargo"]
user_id_modules = ["sharedIdSystem", "uid2IdSystem"]
"#
        .to_string()
    }

    #[test]
    fn bundle_config_loader_accepts_valid_settings() {
        let (_temp, path) = write_config(&valid_config());

        let config = load_bundle_config(&path).expect("should load bundle config");

        assert_eq!(config.adapters, ["rubicon", "kargo"]);
        assert_eq!(
            config.user_id_modules,
            Some(vec![
                "sharedIdSystem".to_string(),
                "uid2IdSystem".to_string()
            ])
        );
        assert_eq!(
            config.external_bundle_url.as_deref(),
            Some("https://assets.example.com/prebid/trusted-prebid-old.js")
        );
    }

    #[test]
    fn bundle_config_loader_allows_missing_user_id_modules() {
        let (_temp, path) = write_config(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"

[integrations.prebid.bundle]
adapters = ["rubicon"]
"#,
        );

        let config = load_bundle_config(&path).expect("should load bundle config");

        assert_eq!(config.adapters, ["rubicon"]);
        assert_eq!(config.user_id_modules, None);
    }

    #[test]
    fn bundle_config_loader_rejects_missing_prebid_block() {
        let (_temp, path) = write_config("[publisher]\ndomain = \"example.com\"\n");

        let error = load_bundle_config(&path).expect_err("should reject missing prebid block");

        assert!(
            error.to_string().contains("missing [integrations.prebid]"),
            "error should explain missing prebid block: {error:?}"
        );
    }

    #[test]
    fn bundle_config_loader_rejects_missing_bundle_block() {
        let (_temp, path) = write_config(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"
"#,
        );

        let error = load_bundle_config(&path).expect_err("should reject missing bundle block");

        assert!(
            error
                .to_string()
                .contains("missing [integrations.prebid.bundle]"),
            "error should explain missing bundle block: {error:?}"
        );
    }

    #[test]
    fn bundle_config_loader_rejects_empty_adapters() {
        let (_temp, path) = write_config(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"

[integrations.prebid.bundle]
adapters = []
"#,
        );

        let error = load_bundle_config(&path).expect_err("should reject empty adapters");

        assert!(
            error.to_string().contains("at least one"),
            "error should explain empty adapters: {error:?}"
        );
    }

    #[test]
    fn bundle_config_loader_rejects_malformed_adapters() {
        let (_temp, path) = write_config(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"

[integrations.prebid.bundle]
adapters = ["rubicon", 123]
"#,
        );

        let error = load_bundle_config(&path).expect_err("should reject malformed adapters");

        assert!(
            error.to_string().contains("array of non-empty strings"),
            "error should explain malformed adapters: {error:?}"
        );
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
    fn npm_prebid_bundle_args_include_user_id_modules_when_configured() {
        let request = PrebidBundleGenerateRequest {
            js_lib_dir: PathBuf::from("crates/trusted-server-js/lib"),
            out_dir: PathBuf::from("/tmp/prebid"),
            adapters: vec!["rubicon".to_string(), "kargo".to_string()],
            user_id_modules: Some(vec!["sharedIdSystem".to_string()]),
        };

        assert_eq!(
            npm_prebid_bundle_args(&request),
            [
                "run",
                "build:prebid-external",
                "--",
                "--adapters",
                "rubicon,kargo",
                "--user-id-modules",
                "sharedIdSystem",
                "--out",
                "/tmp/prebid",
            ],
            "should pass configured adapters, user ID modules, and output path"
        );
    }

    #[test]
    fn npm_prebid_bundle_args_omit_user_id_modules_when_not_configured() {
        let request = PrebidBundleGenerateRequest {
            js_lib_dir: PathBuf::from("crates/trusted-server-js/lib"),
            out_dir: PathBuf::from("/tmp/prebid"),
            adapters: vec!["rubicon".to_string()],
            user_id_modules: None,
        };

        assert_eq!(
            npm_prebid_bundle_args(&request),
            [
                "run",
                "build:prebid-external",
                "--",
                "--adapters",
                "rubicon",
                "--out",
                "/tmp/prebid",
            ],
            "should omit user ID module flag so the JS generator uses its default preset"
        );
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
                fs::write(
                    request.out_dir.join("manifest.json"),
                    serde_json::json!({
                        "prebidVersion": "10.26.0",
                        "adapters": request.adapters,
                        "userIdModules": request.user_id_modules.clone().unwrap_or_default(),
                        "sha256": "b".repeat(64),
                        "sri": "sha384-test",
                        "filename": format!("trusted-prebid-{}.js", "b".repeat(64))
                    })
                    .to_string(),
                )
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
        assert_eq!(generator.generate_calls[0].adapters, ["rubicon", "kargo"]);

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
