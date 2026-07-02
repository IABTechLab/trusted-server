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
    let manifest_loader = ManifestLoader::from_path(&args.manifest)
        .map_err(|err| format!("failed to load {}: {err}", args.manifest.display()))?;
    let app_name = manifest_loader.manifest().app.name.clone().ok_or_else(|| {
        format!(
            "{} has no [app].name; cannot resolve trusted-server.toml",
            args.manifest.display()
        )
    })?;
    let app_config_path =
        resolve_app_config_path(args.app_config.as_deref(), &args.manifest, &app_name);

    let mut opts = AppConfigLoadOptions::default();
    opts.env_overlay = !args.no_env;
    let app_config = app_config::deserialize_app_config_with_options::<TrustedServerAppConfig>(
        &app_config_path,
        &app_name,
        &opts,
    )
    .map_err(|err| format!("failed to load {}: {err}", app_config_path.display()))?;

    Ok(LoadedSettings {
        app_config_path,
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
