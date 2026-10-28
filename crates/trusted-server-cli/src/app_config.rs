//! Shared effective Trusted Server app-config loading for the `ts` CLI.
//!
//! Both the static `ts config ad-templates ...` commands and the browser-backed
//! `ts audit ad-templates verify` command load the same effective app config
//! through [`load_settings`], so config-path resolution and the `EdgeZero`
//! environment overlay stay consistent across command families.

use std::path::{Path, PathBuf};

use clap::Args;
use edgezero_core::app_config::{self, AppConfigLoadOptions};
use edgezero_core::manifest::ManifestLoader;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::settings::Settings;

/// Shared local app-config flags accepted by every config/audit ad-template command.
#[derive(Clone, Debug, Args)]
pub struct AppConfigArgs {
    /// Path to `trusted-server.toml`. Defaults to `<app.name>.toml` beside `edgezero.toml`.
    #[arg(long)]
    pub app_config: Option<PathBuf>,
    /// Path to `edgezero.toml`.
    #[arg(long, default_value = "edgezero.toml")]
    pub manifest: PathBuf,
    /// Skip app-config environment overlay.
    #[arg(long)]
    pub no_env: bool,
}

/// Registers the modules a stock build ships with deploy validation.
///
/// [`TrustedServerAppConfig`] validates through a trait that takes no
/// arguments, so every path that validates settings calls this first. The
/// first call wins and a later one changes nothing.
pub(crate) fn register_stock_modules() {
    trusted_server_core::config::register_deploy_integrations(trusted_server_modules::builders());
}

/// Effective settings plus the resolved app-config path they were loaded from.
#[derive(Debug)]
pub struct LoadedSettings {
    /// The `trusted-server.toml` path the settings were loaded from.
    pub app_config_path: PathBuf,
    /// The deserialized effective settings.
    pub settings: Settings,
}

/// Loads the effective Trusted Server settings described by `args`.
///
/// Resolves the app-config path from `args` (or the manifest's `<app.name>.toml`
/// default), applies the `EdgeZero` environment overlay unless `no_env` is set, and
/// returns the deserialized [`Settings`].
///
/// # Errors
///
/// Returns a user-facing string when the manifest cannot be loaded, has no
/// `[app].name`, or the resolved app-config file cannot be read or parsed. When an
/// explicit `--app-config` path is given and is missing, the error names that
/// exact path rather than silently falling back.
pub fn load_settings(args: &AppConfigArgs) -> Result<LoadedSettings, String> {
    load_settings_with_env_overlay(args, !args.no_env)
}

/// Loads Trusted Server settings from the resolved app-config file without
/// applying environment overlays.
///
/// Mutating commands use this path so environment-only values are never
/// persisted into the operator-owned TOML file.
///
/// # Errors
///
/// Returns the same path-resolution, read, and parse errors as
/// [`load_settings`].
#[cfg(test)]
pub(crate) fn load_file_settings(args: &AppConfigArgs) -> Result<LoadedSettings, String> {
    load_settings_with_env_overlay(args, false)
}

/// Resolves the operator-owned app-config path without deserializing settings.
///
/// Mutating recovery commands use this when the existing config may already be
/// invalid but still needs a narrowly scoped structural repair.
///
/// # Errors
///
/// Returns a user-facing string when the manifest cannot be loaded or has no
/// `[app].name` and no explicit config path was supplied.
pub fn resolve_app_config_file(args: &AppConfigArgs) -> Result<PathBuf, String> {
    if let Some(path) = &args.app_config {
        return Ok(path.clone());
    }
    let manifest_loader = ManifestLoader::from_path(&args.manifest)
        .map_err(|err| format!("failed to load {}: {err}", args.manifest.display()))?;
    let app_name = manifest_loader.manifest().app.name.clone().ok_or_else(|| {
        format!(
            "{} has no [app].name; cannot resolve trusted-server.toml",
            args.manifest.display()
        )
    })?;
    Ok(resolve_app_config_path(None, &args.manifest, &app_name))
}

/// The app-config file a command reads and the app name its environment
/// overlay is read under.
#[derive(Debug)]
pub(crate) struct ResolvedAppConfig {
    /// The `trusted-server.toml` path.
    pub(crate) path: PathBuf,
    /// The manifest's `[app].name`.
    pub(crate) app_name: String,
}

/// Resolves the app-config file `app_config` names, or `<app.name>.toml`
/// beside `manifest`, and the manifest's app name.
///
/// # Errors
///
/// Returns a user-facing string when the manifest cannot be loaded or has no
/// `[app].name`.
pub(crate) fn resolve_app_config(
    manifest: &Path,
    app_config: Option<&Path>,
) -> Result<ResolvedAppConfig, String> {
    let manifest_loader = ManifestLoader::from_path(manifest)
        .map_err(|err| format!("failed to load {}: {err}", manifest.display()))?;
    let app_name = manifest_loader.manifest().app.name.clone().ok_or_else(|| {
        format!(
            "{} has no [app].name; cannot resolve trusted-server.toml",
            manifest.display()
        )
    })?;
    Ok(ResolvedAppConfig {
        path: resolve_app_config_path(app_config, manifest, &app_name),
        app_name,
    })
}

fn load_settings_with_env_overlay(
    args: &AppConfigArgs,
    env_overlay: bool,
) -> Result<LoadedSettings, String> {
    let resolved = resolve_app_config(&args.manifest, args.app_config.as_deref())?;

    let mut opts = AppConfigLoadOptions::default();
    opts.env_overlay = env_overlay;
    register_stock_modules();
    let app_config = app_config::deserialize_app_config_with_options::<TrustedServerAppConfig>(
        &resolved.path,
        &resolved.app_name,
        &opts,
    )
    .map_err(|err| format!("failed to load {}: {err}", resolved.path.display()))?;

    Ok(LoadedSettings {
        app_config_path: resolved.path,
        settings: app_config.into_settings(),
    })
}

fn resolve_app_config_path(
    explicit: Option<&Path>,
    manifest_path: &Path,
    app_name: &str,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    let file_name = format!("{app_name}.toml");
    if let Some(parent) = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        parent.join(file_name)
    } else {
        PathBuf::from(file_name)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn explicit_missing_app_config_does_not_fall_back() {
        let temp = TempDir::new().expect("should create temp dir");
        let manifest_path = temp.path().join("edgezero.toml");
        fs::write(&manifest_path, "[app]\nname = \"trusted-server\"\n")
            .expect("should write manifest");
        let missing_path = temp.path().join("missing.toml");

        let args = AppConfigArgs {
            app_config: Some(missing_path.clone()),
            manifest: manifest_path,
            no_env: true,
        };

        let err = load_settings(&args).expect_err("should reject missing explicit config");
        assert!(
            err.contains(missing_path.to_string_lossy().as_ref()),
            "error should mention the explicit missing path"
        );
    }
}
