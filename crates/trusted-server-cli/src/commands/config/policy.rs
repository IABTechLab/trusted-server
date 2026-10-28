//! The permission policy files `ts config push`, `ts config validate` and
//! `ts config diff` take with `--permissions`.
//!
//! A domain's settings name the policy file they run in
//! `[permissions] policy`, by its stem. These commands check the named file
//! and write its SHA-256 into `[permissions] sha256`, and for the adapters
//! that read the policy from the blob also the file itself into
//! `[permissions] text`, before `EdgeZero` builds the envelope. `EdgeZero`
//! reads the settings from a path, so they reach it through a temporary file
//! under the system temp folder, loaded with the `TRUSTED_SERVER__` overlay
//! already applied and handed over with `no_env`, so the overlay is applied
//! once and the digest is taken after it.

use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use edgezero_cli::args::{ConfigDiffArgs, ConfigPushArgs, ConfigValidateArgs};
use edgezero_core::app::AXUM_ADAPTER;
use edgezero_core::app_config::{self, AppConfigLoadOptions};
use tempfile::TempPath;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::permissions::{PermissionMaps, policy_sha256};
use trusted_server_core::settings::{PermissionsConfig, Settings};

use crate::app_config::{ResolvedAppConfig, resolve_app_config};

/// The extension a permission policy file carries.
const POLICY_FILE_EXTENSION: &str = "yaml";

/// The settings table that names the policy.
const PERMISSIONS_TABLE: &str = "permissions";

/// The policy file's SHA-256, which only the push writes.
const SHA256_KEY: &str = "sha256";

/// The policy file itself, which only the push writes.
const TEXT_KEY: &str = "text";

/// What `ts config validate` says when it is given no policy file.
const POLICY_NOT_CHECKED: &str =
    "the policy was not checked, because no --permissions file was given";

/// The `--permissions` flag.
#[derive(Debug, Default, clap::Args)]
pub struct PolicyFileArgs {
    /// A permission policy file, which the settings name by its stem in
    /// `[permissions] policy`. Repeat for each file.
    #[arg(long = "permissions", value_name = "FILE")]
    pub files: Vec<PathBuf>,
}

/// The arguments of `ts config push`.
#[derive(Debug, clap::Args)]
pub struct PushArgs {
    #[command(flatten)]
    pub edgezero: ConfigPushArgs,
    #[command(flatten)]
    pub policy: PolicyFileArgs,
}

/// The arguments of `ts config validate`.
#[derive(Debug, clap::Args)]
pub struct ValidateArgs {
    #[command(flatten)]
    pub edgezero: ConfigValidateArgs,
    #[command(flatten)]
    pub policy: PolicyFileArgs,
}

/// The arguments of `ts config diff`.
#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    #[command(flatten)]
    pub edgezero: ConfigDiffArgs,
    #[command(flatten)]
    pub policy: PolicyFileArgs,
}

/// Pushes the settings with the policy file they name, which the push
/// always requires.
///
/// # Errors
///
/// Returns a user-facing string when the policy check or `EdgeZero`'s push
/// fails.
pub fn run_push(args: PushArgs) -> Result<(), String> {
    run_push_with(
        args,
        edgezero_cli::run_config_push_typed::<TrustedServerAppConfig>,
    )
}

/// [`run_push`] with `EdgeZero`'s push passed in, so a test can read the
/// settings it is handed.
fn run_push_with(
    args: PushArgs,
    push: impl FnOnce(&ConfigPushArgs) -> Result<(), String>,
) -> Result<(), String> {
    let mut edgezero = args.edgezero;
    let prepared = prepare(
        &edgezero.manifest,
        edgezero.app_config.as_deref(),
        edgezero.no_env,
        &args.policy.files,
        PolicyText::for_adapter(&edgezero.adapter),
    )?;
    edgezero.app_config = Some(prepared.file.to_path_buf());
    edgezero.no_env = true;
    push(&edgezero)?;
    log::info!(
        "permissions: policy `{}`, sha256 {}",
        prepared.policy,
        prepared.sha256
    );
    Ok(())
}

/// Validates the settings, and with `--permissions` the policy file they
/// name.
///
/// # Errors
///
/// Returns a user-facing string when the policy check or `EdgeZero`'s
/// validation fails.
pub fn run_validate(args: ValidateArgs) -> Result<(), String> {
    run_validate_with(
        args,
        edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>,
    )
}

/// [`run_validate`] with `EdgeZero`'s validation passed in, so a test can
/// read the settings it is handed.
fn run_validate_with(
    args: ValidateArgs,
    validate: impl FnOnce(&ConfigValidateArgs) -> Result<(), String>,
) -> Result<(), String> {
    let mut edgezero = args.edgezero;
    if args.policy.files.is_empty() {
        load_settings(
            &edgezero.manifest,
            edgezero.app_config.as_deref(),
            edgezero.no_env,
        )?;
        validate(&edgezero)?;
        log::warn!("permissions: {POLICY_NOT_CHECKED}");
        return Ok(());
    }
    // Validation writes nothing, so the file itself is left out.
    let prepared = prepare(
        &edgezero.manifest,
        edgezero.app_config.as_deref(),
        edgezero.no_env,
        &args.policy.files,
        PolicyText::Omitted,
    )?;
    edgezero.app_config = Some(prepared.file.to_path_buf());
    edgezero.no_env = true;
    validate(&edgezero)?;
    log::info!(
        "permissions: policy `{}` checked, sha256 {}",
        prepared.policy,
        prepared.sha256
    );
    Ok(())
}

/// Diffs the settings, as the push would write them, against the store.
///
/// The stored settings carry the policy file's SHA-256, and on every adapter
/// but Axum the file itself, so settings that name a policy are compared
/// only with the file `--permissions` supplies.
///
/// # Errors
///
/// Returns a user-facing string when the settings name a policy and no
/// `--permissions` file is given, or when the policy check or `EdgeZero`'s
/// diff fails.
pub fn run_diff(args: DiffArgs) -> Result<edgezero_cli::DiffExit, String> {
    let mut edgezero = args.edgezero;
    if args.policy.files.is_empty() {
        let loaded = load_settings(
            &edgezero.manifest,
            edgezero.app_config.as_deref(),
            edgezero.no_env,
        )?;
        if let Some(policy) = loaded.settings.permissions.policy.as_deref() {
            return Err(format!(
                "the settings name the permission policy `{policy}`, and a push writes \
                 that file's SHA-256 into the stored settings, so `ts config diff` needs \
                 the file to compare them. Supply it with --permissions <file>"
            ));
        }
        let exit = edgezero_cli::run_config_diff_typed::<TrustedServerAppConfig>(&edgezero)?;
        log::warn!(
            "permissions: the settings name no permission policy, which `ts config push` refuses"
        );
        return Ok(exit);
    }
    let prepared = prepare(
        &edgezero.manifest,
        edgezero.app_config.as_deref(),
        edgezero.no_env,
        &args.policy.files,
        PolicyText::for_adapter(&edgezero.adapter),
    )?;
    edgezero.app_config = Some(prepared.file.to_path_buf());
    edgezero.no_env = true;
    edgezero_cli::run_config_diff_typed::<TrustedServerAppConfig>(&edgezero)
}

/// Whether the settings handed to `EdgeZero` carry the policy file itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyText {
    /// For the adapters that read the policy from the blob.
    Included,
    /// For Axum, which reads the file from its folder, and for validation.
    Omitted,
}

impl PolicyText {
    fn for_adapter(adapter: &str) -> Self {
        if adapter.eq_ignore_ascii_case(AXUM_ADAPTER) {
            Self::Omitted
        } else {
            Self::Included
        }
    }
}

/// The settings `EdgeZero` is handed, with the policy they name.
#[derive(Debug)]
struct PreparedSettings {
    /// The settings file, closed so `EdgeZero` can read it, and removed
    /// when this is dropped.
    file: TempPath,
    /// The policy's name.
    policy: String,
    /// The SHA-256 of the policy file.
    sha256: String,
}

/// A policy file `--permissions` supplied, read and checked.
#[derive(Debug)]
struct PolicyFile {
    name: String,
    path: PathBuf,
    text: String,
}

/// The settings a command reads, loaded with the overlay unless `no_env` is
/// set.
struct LoadedSettings {
    resolved: ResolvedAppConfig,
    options: AppConfigLoadOptions,
    settings: Settings,
}

/// Loads the settings with the `TRUSTED_SERVER__` overlay unless `no_env` is
/// set, and refuses settings that set what only the push writes.
fn load_settings(
    manifest: &Path,
    app_config: Option<&Path>,
    no_env: bool,
) -> Result<LoadedSettings, String> {
    let resolved = resolve_app_config(manifest, app_config)?;
    let mut options = AppConfigLoadOptions::default();
    options.env_overlay = !no_env;
    let settings = app_config::deserialize_app_config_with_options::<TrustedServerAppConfig>(
        &resolved.path,
        &resolved.app_name,
        &options,
    )
    .map_err(|error| format!("failed to load {}: {error}", resolved.path.display()))?
    .into_settings();
    refuse_fields_the_push_writes(&settings.permissions)?;
    Ok(LoadedSettings {
        resolved,
        options,
        settings,
    })
}

/// Loads the settings, checks the policy files, and writes the settings with
/// the named file's SHA-256, and its text when `text` asks for it, to a
/// temporary file.
fn prepare(
    manifest: &Path,
    app_config: Option<&Path>,
    no_env: bool,
    files: &[PathBuf],
    text: PolicyText,
) -> Result<PreparedSettings, String> {
    let LoadedSettings {
        resolved,
        options,
        settings,
    } = load_settings(manifest, app_config, no_env)?;
    let mut document =
        app_config::load_app_config_raw_with_options(&resolved.path, &resolved.app_name, &options)
            .map_err(|error| format!("failed to load {}: {error}", resolved.path.display()))?;

    let files = read_policy_files(files)?;
    let policy = named_policy(&settings.permissions, &files)?;
    let sha256 = policy_sha256(&policy.text);

    let table = document
        .get_mut(PERMISSIONS_TABLE)
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| {
            format!(
                "{} names a policy outside a `[{PERMISSIONS_TABLE}]` table",
                resolved.path.display()
            )
        })?;
    table.insert(SHA256_KEY.to_owned(), toml::Value::String(sha256.clone()));
    if text == PolicyText::Included {
        table.insert(
            TEXT_KEY.to_owned(),
            toml::Value::String(policy.text.clone()),
        );
    }

    Ok(PreparedSettings {
        file: write_settings(&document)?,
        policy: policy.name.clone(),
        sha256,
    })
}

/// Refuses settings that set what only the push writes.
fn refuse_fields_the_push_writes(permissions: &PermissionsConfig) -> Result<(), String> {
    for (key, set) in [
        (SHA256_KEY, permissions.sha256.is_some()),
        (TEXT_KEY, permissions.text.is_some()),
    ] {
        if set {
            return Err(format!(
                "`[{PERMISSIONS_TABLE}] {key}` is written by `ts config push` from the file \
                 --permissions supplies, so neither the settings nor the TRUSTED_SERVER__ \
                 environment overlay may set it"
            ));
        }
    }
    Ok(())
}

/// Reads every file `--permissions` supplied and checks that it is a valid
/// policy, whether or not the settings name it.
fn read_policy_files(paths: &[PathBuf]) -> Result<Vec<PolicyFile>, String> {
    let mut files: Vec<PolicyFile> = Vec::with_capacity(paths.len());
    for path in paths {
        let name = policy_name(path)?;
        let bytes = fs::read(path).map_err(|error| {
            format!(
                "failed to read the permission policy file {}: {error}",
                path.display()
            )
        })?;
        let text = String::from_utf8(bytes).map_err(|_| {
            format!(
                "the permission policy file {} is not UTF-8 text",
                path.display()
            )
        })?;
        PermissionMaps::from_yaml(&text).map_err(|error| {
            format!(
                "the permission policy file {} is not valid: {error}",
                path.display()
            )
        })?;
        if let Some(other) = files.iter().find(|file| file.name == name) {
            return Err(format!(
                "the permission policy files {} and {} both supply the policy `{name}`",
                other.path.display(),
                path.display()
            ));
        }
        files.push(PolicyFile {
            name,
            path: path.clone(),
            text,
        });
    }
    Ok(files)
}

/// The policy a file supplies, which is its stem.
fn policy_name(path: &Path) -> Result<String, String> {
    if path.extension().and_then(OsStr::to_str) != Some(POLICY_FILE_EXTENSION) {
        return Err(format!(
            "the permission policy file {} must end `.{POLICY_FILE_EXTENSION}`",
            path.display()
        ));
    }
    path.file_stem()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "the permission policy file {} has no name before `.{POLICY_FILE_EXTENSION}`",
                path.display()
            )
        })
}

/// The supplied file the settings name.
fn named_policy<'a>(
    permissions: &PermissionsConfig,
    files: &'a [PolicyFile],
) -> Result<&'a PolicyFile, String> {
    let Some(policy) = permissions.policy.as_deref() else {
        return Err(format!(
            "the settings name no permission policy, so name the publisher's policy file \
             by its stem in `[{PERMISSIONS_TABLE}] policy` and supply the file with \
             --permissions <file>"
        ));
    };
    files
        .iter()
        .find(|file| file.name == policy)
        .ok_or_else(|| {
            let supplied: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
            format!(
                "the settings name the permission policy `{policy}`, which no --permissions \
             file supplies (supplied: {})",
                if supplied.is_empty() {
                    "none".to_owned()
                } else {
                    supplied.join(", ")
                }
            )
        })
}

/// Writes `document` to a temporary file under the system temp folder.
fn write_settings(document: &toml::Value) -> Result<TempPath, String> {
    let rendered = toml::to_string(document)
        .map_err(|error| format!("failed to write the settings for EdgeZero: {error}"))?;
    let mut file = tempfile::Builder::new()
        .prefix("trusted-server-")
        .suffix(".toml")
        .tempfile()
        .map_err(|error| format!("failed to create a temporary settings file: {error}"))?;
    file.write_all(rendered.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|error| format!("failed to write a temporary settings file: {error}"))?;
    Ok(file.into_temp_path())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::commands::config::init::EXAMPLE_CONFIG;

    /// A valid policy whose top node grants everything.
    const OPEN_POLICY: &str = "groups:\n  open:\n    default: granted\nrules:\n  group: open\n  jurisdiction: non-regulated\n";

    /// A valid policy whose top node grants nothing without a signal.
    const STRICT_POLICY: &str = "groups:\n  strict:\n    default: requires_signal\nrules:\n  group: strict\n  jurisdiction: gdpr\n";

    /// Settings that name the policy `standard`.
    const NAMES_STANDARD: &str = "[permissions]\npolicy = \"standard\"\n";

    /// A folder holding a manifest whose app is `app_name` and the example
    /// settings with `permissions` appended.
    struct Project {
        folder: TempDir,
    }

    impl Project {
        fn new(app_name: &str, permissions: &str) -> Self {
            let folder = tempfile::tempdir().expect("should create a project folder");
            fs::write(
                folder.path().join("edgezero.toml"),
                format!("[app]\nname = \"{app_name}\"\n"),
            )
            .expect("should write the manifest");
            fs::write(
                folder.path().join("trusted-server.toml"),
                format!("{EXAMPLE_CONFIG}\n{permissions}"),
            )
            .expect("should write the settings");
            Self { folder }
        }

        fn policy_file(&self, file_name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.folder.path().join(file_name);
            fs::write(&path, bytes).expect("should write a policy file");
            path
        }

        fn manifest(&self) -> PathBuf {
            self.folder.path().join("edgezero.toml")
        }

        fn settings(&self) -> PathBuf {
            self.folder.path().join("trusted-server.toml")
        }

        fn prepare(
            &self,
            files: &[PathBuf],
            no_env: bool,
            text: PolicyText,
        ) -> Result<PreparedSettings, String> {
            prepare(
                &self.manifest(),
                Some(&self.settings()),
                no_env,
                files,
                text,
            )
        }
    }

    /// The `[permissions]` table of the settings file at `path`.
    fn read_permissions(path: Option<&Path>) -> toml::Table {
        let path = path.expect("should hand EdgeZero a settings file");
        let document: toml::Table =
            toml::from_str(&fs::read_to_string(path).expect("should read the handed settings"))
                .expect("should parse the handed settings");
        document[PERMISSIONS_TABLE]
            .as_table()
            .expect("should carry the permissions table")
            .clone()
    }

    /// The `[permissions]` table of the settings `EdgeZero` is handed.
    fn handed_permissions(prepared: &PreparedSettings) -> toml::Table {
        read_permissions(Some(&prepared.file))
    }

    #[test]
    fn the_push_writes_the_digest_and_the_exact_text_of_the_named_file() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        // Line endings a checkout could convert, so the bytes must survive.
        let text = OPEN_POLICY.replace('\n', "\r\n");
        let standard = project.policy_file("standard.yaml", text.as_bytes());

        let prepared = project
            .prepare(&[standard], true, PolicyText::Included)
            .expect("should prepare the settings");

        let permissions = handed_permissions(&prepared);
        assert_eq!(prepared.policy, "standard");
        assert_eq!(prepared.sha256, policy_sha256(&text));
        assert_eq!(
            permissions[SHA256_KEY].as_str(),
            Some(policy_sha256(&text).as_str()),
            "should write the SHA-256 of the file's exact bytes"
        );
        assert_eq!(
            permissions[TEXT_KEY].as_str(),
            Some(text.as_str()),
            "should write the file byte for byte"
        );
        assert_eq!(permissions["policy"].as_str(), Some("standard"));
    }

    #[test]
    fn axum_and_validation_leave_the_text_out() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        let standard = project.policy_file("standard.yaml", OPEN_POLICY.as_bytes());

        let mut push = PushArgs {
            edgezero: ConfigPushArgs::default(),
            policy: PolicyFileArgs {
                files: vec![standard.clone()],
            },
        };
        push.edgezero.adapter = "axum".to_owned();
        push.edgezero.manifest = project.manifest();
        push.edgezero.app_config = Some(project.settings());
        push.edgezero.no_env = true;
        let mut pushed = None;
        run_push_with(push, |handed| {
            pushed = Some(read_permissions(handed.app_config.as_deref()));
            Ok(())
        })
        .expect("should push");

        let mut validate = ValidateArgs {
            edgezero: ConfigValidateArgs::default(),
            policy: PolicyFileArgs {
                files: vec![standard],
            },
        };
        validate.edgezero.manifest = project.manifest();
        validate.edgezero.app_config = Some(project.settings());
        validate.edgezero.no_env = true;
        let mut validated = None;
        run_validate_with(validate, |handed| {
            validated = Some(read_permissions(handed.app_config.as_deref()));
            Ok(())
        })
        .expect("should validate");

        for (command, permissions) in [("push to Axum", pushed), ("validate", validated)] {
            let permissions = permissions.expect("should hand settings to EdgeZero");
            assert!(permissions.contains_key(SHA256_KEY), "{command}");
            assert!(
                !permissions.contains_key(TEXT_KEY),
                "{command} should leave the file out: {permissions:?}"
            );
        }
        assert_eq!(PolicyText::for_adapter("fastly"), PolicyText::Included);
        assert_eq!(PolicyText::for_adapter("Axum"), PolicyText::Omitted);
    }

    #[test]
    fn validation_without_a_file_refuses_a_digest_the_settings_set() {
        let project = Project::new(
            "trusted-server",
            &format!("{NAMES_STANDARD}{SHA256_KEY} = \"0000\"\n"),
        );
        let mut validate = ValidateArgs {
            edgezero: ConfigValidateArgs::default(),
            policy: PolicyFileArgs::default(),
        };
        validate.edgezero.manifest = project.manifest();
        validate.edgezero.app_config = Some(project.settings());
        validate.edgezero.no_env = true;

        let error = run_validate_with(validate, |_| {
            Err("EdgeZero should not be asked to validate".to_owned())
        })
        .expect_err("a digest the settings set should be refused");

        assert!(
            error.contains("`[permissions] sha256`"),
            "should name the field: {error}"
        );
    }

    #[test]
    fn a_diff_without_the_file_is_refused_when_the_settings_name_a_policy() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        let mut diff = DiffArgs {
            edgezero: ConfigDiffArgs::default(),
            policy: PolicyFileArgs::default(),
        };
        diff.edgezero.adapter = "fastly".to_owned();
        diff.edgezero.manifest = project.manifest();
        diff.edgezero.app_config = Some(project.settings());
        diff.edgezero.no_env = true;

        let error = run_diff(diff).expect_err("the diff should be refused");

        assert!(
            error.contains("`standard`") && error.contains("--permissions <file>"),
            "should name the policy and how to supply its file: {error}"
        );
    }

    #[test]
    fn the_digest_is_taken_after_the_environment_overlay() {
        // An app name of its own, so no other test reads the variable.
        let project = Project::new("policy-overlay-probe", NAMES_STANDARD);
        let files = [
            project.policy_file("standard.yaml", OPEN_POLICY.as_bytes()),
            project.policy_file("strict.yaml", STRICT_POLICY.as_bytes()),
        ];

        temp_env::with_var(
            "POLICY_OVERLAY_PROBE__PERMISSIONS__POLICY",
            Some("strict"),
            || {
                let overlaid = project
                    .prepare(&files, false, PolicyText::Included)
                    .expect("should prepare the overlaid settings");
                assert_eq!(
                    overlaid.policy, "strict",
                    "the overlay should choose the policy"
                );
                assert_eq!(overlaid.sha256, policy_sha256(STRICT_POLICY));
                let handed = handed_permissions(&overlaid);
                assert_eq!(
                    handed["policy"].as_str(),
                    Some("strict"),
                    "EdgeZero should be handed the policy the overlay chose"
                );
                assert_eq!(
                    handed[SHA256_KEY].as_str(),
                    Some(policy_sha256(STRICT_POLICY).as_str()),
                    "EdgeZero should be handed the digest of that policy's file"
                );

                let plain = project
                    .prepare(&files, true, PolicyText::Included)
                    .expect("should prepare the settings without the overlay");
                assert_eq!(plain.policy, "standard", "--no-env should skip the overlay");
                assert_eq!(plain.sha256, policy_sha256(OPEN_POLICY));
                assert_eq!(
                    handed_permissions(&plain)["policy"].as_str(),
                    Some("standard"),
                    "EdgeZero should be handed the policy the file names"
                );
            },
        );
    }

    #[test]
    fn the_example_settings_as_written_name_no_policy_and_are_refused() {
        let project = Project::new("trusted-server", "");
        let standard = project.policy_file("standard.yaml", OPEN_POLICY.as_bytes());

        let error = project
            .prepare(&[standard], true, PolicyText::Included)
            .expect_err("settings naming no policy should be refused");

        assert!(
            error.contains("name no permission policy")
                && error.contains("`[permissions] policy`")
                && error.contains("--permissions <file>"),
            "should say what is missing and how to supply it: {error}"
        );
    }

    #[test]
    fn a_policy_no_supplied_file_has_is_refused() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        let strict = project.policy_file("strict.yaml", STRICT_POLICY.as_bytes());

        let error = project
            .prepare(&[strict], true, PolicyText::Included)
            .expect_err("a policy no file supplies should be refused");
        assert!(
            error.contains("`standard`") && error.contains("supplied: strict"),
            "should name the policy and the files given: {error}"
        );

        let error = project
            .prepare(&[], true, PolicyText::Included)
            .expect_err("a push without a file should be refused");
        assert!(error.contains("supplied: none"), "{error}");
    }

    #[test]
    fn a_digest_or_text_the_settings_set_is_refused() {
        for (key, value) in [(SHA256_KEY, "\"0000\""), (TEXT_KEY, "\"groups: {}\"")] {
            let project = Project::new(
                "trusted-server",
                &format!("{NAMES_STANDARD}{key} = {value}\n"),
            );
            let standard = project.policy_file("standard.yaml", OPEN_POLICY.as_bytes());

            let error = project
                .prepare(&[standard], true, PolicyText::Included)
                .expect_err("a field only the push writes should be refused");

            assert!(
                error.contains(&format!("`[permissions] {key}`"))
                    && error.contains("TRUSTED_SERVER__"),
                "should name the field and both ways it could be set: {error}"
            );
        }
    }

    #[test]
    fn a_file_that_is_not_a_yaml_policy_in_utf8_text_is_refused() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        let cases = [
            (
                project.policy_file("standard.yml", OPEN_POLICY.as_bytes()),
                "must end `.yaml`",
            ),
            (
                project.policy_file("standard.yaml", &[0xff, 0xfe, 0x00]),
                "is not UTF-8 text",
            ),
            (
                project.policy_file("broken.yaml", b"groups: [\n"),
                "is not valid",
            ),
        ];

        for (path, reason) in cases {
            let error = project
                .prepare(std::slice::from_ref(&path), true, PolicyText::Included)
                .expect_err("the file should be refused");
            assert!(
                error.contains(reason) && error.contains(&path.display().to_string()),
                "should name {} and say `{reason}`: {error}",
                path.display()
            );
        }
    }

    #[test]
    fn every_supplied_file_is_checked_whether_or_not_it_is_named() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        let files = [
            project.policy_file("standard.yaml", OPEN_POLICY.as_bytes()),
            project.policy_file("unused.yaml", b"rules: {}\n"),
        ];

        let error = project
            .prepare(&files, true, PolicyText::Included)
            .expect_err("an invalid file should be refused even when no setting names it");

        assert!(error.contains("unused.yaml"), "{error}");
    }

    #[test]
    fn two_files_that_supply_one_policy_are_refused() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        for folder in ["a", "b"] {
            fs::create_dir(project.folder.path().join(folder)).expect("should create a folder");
        }
        let files = [
            project.policy_file("a/standard.yaml", OPEN_POLICY.as_bytes()),
            project.policy_file("b/standard.yaml", STRICT_POLICY.as_bytes()),
        ];

        let error = project
            .prepare(&files, true, PolicyText::Included)
            .expect_err("two files with one name should be refused");

        assert!(
            error.contains("both supply the policy `standard`"),
            "{error}"
        );
    }

    #[test]
    fn the_settings_handed_over_are_removed_when_done() {
        let project = Project::new("trusted-server", NAMES_STANDARD);
        let standard = project.policy_file("standard.yaml", OPEN_POLICY.as_bytes());

        let prepared = project
            .prepare(&[standard], true, PolicyText::Included)
            .expect("should prepare the settings");
        let path = prepared.file.to_path_buf();

        assert!(
            path.exists(),
            "the settings should exist while EdgeZero reads them"
        );
        assert!(
            path.starts_with(std::env::temp_dir()),
            "the settings should be in the system temp folder: {}",
            path.display()
        );
        drop(prepared);
        assert!(!path.exists(), "the settings should be removed");
    }
}
