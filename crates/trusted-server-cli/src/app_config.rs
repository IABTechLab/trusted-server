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
use trusted_server_core::integrations::IntegrationBuilder;
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

/// The module builders the `ts` tool links, in the order their request
/// hooks run, which is the order the adapters link them in. The tool
/// validates a deployment's settings against each module's own rules and
/// writes a configuration from what each module says of itself. None offers a
/// loading cache, as in each adapter's list, so the tool refuses a document
/// that names one.
#[must_use]
pub fn linked_builders() -> Vec<IntegrationBuilder> {
    vec![
        // Prebid registers from the auction plan, and what it registers runs
        // ahead of the modules below.
        trusted_server_auction_prebid::builder(),
        trusted_server_testing_testlight::builder(),
        trusted_server_framework_nextjs::builder(),
        trusted_server_audience_permutive::builder(),
        trusted_server_identity_lockr::builder(),
        trusted_server_cmp_didomi::builder(),
        trusted_server_cmp_inmobi::builder(),
        trusted_server_cmp_sourcepoint::builder(),
        trusted_server_cmp_osano::builder(),
        trusted_server_tag_google_tag::builder(),
        trusted_server_tag_google_tag_manager::builder(),
        trusted_server_bot_protection_datadome::builder(),
        trusted_server_ad_tag_google::builder(),
        trusted_server_ad_tag_google::diagnostics::builder(),
        trusted_server_ads_txt_iabtl::builder(),
        trusted_server_headers_geo::builder(),
        // Run by `[shared.51degrees]` rather than selected by a section.
        trusted_server_middleware_51degrees::builder(),
        // Implementations `[demand]` and `[ad-server]` can name, which no
        // section selects.
        trusted_server_auction_protocol_openrtb::builder(),
        trusted_server_auction_prebid_server::builder(),
        trusted_server_auction_aps::builder(),
        trusted_server_ad_server_iabtl::builder(),
    ]
}

/// Registers the modules the tool links with deploy validation.
///
/// [`TrustedServerAppConfig`] validates through a trait that takes no
/// arguments, so every path that validates settings calls this first. The
/// first call wins and a later one changes nothing.
pub(crate) fn register_linked_modules() {
    trusted_server_core::config::register_deploy_integrations(linked_builders());
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
    register_linked_modules();
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

    /// The tool links the modules each adapter links, in the order their hooks
    /// run, so it validates a deployment's settings as they do. A module is
    /// named by what its section selects, and an implementation by what
    /// `[demand]` or `[ad-server]` names.
    #[test]
    fn the_linked_modules_are_offered_in_hook_order() {
        let names: Vec<&str> = linked_builders()
            .iter()
            .map(|builder| {
                builder
                    .module_name()
                    .or_else(|| builder.demand().map(|demand| demand.id))
                    .or_else(|| builder.adserver().map(|adserver| adserver.id))
                    .unwrap_or_else(|| builder.id())
            })
            .collect();

        assert_eq!(
            names,
            [
                "auction.prebid",
                "testing.testlight",
                "framework.nextjs",
                "audience.permutive",
                "identity.lockr",
                "cmp.didomi",
                "cmp.inmobi",
                "cmp.sourcepoint",
                "cmp.osano",
                "tag.google-tag",
                "tag.google-tag-manager",
                "bot-protection.datadome",
                "ad-tag.google",
                "ad-tag.google.diagnostics",
                "ads-txt.iabtl",
                "headers.geo",
                "51degrees",
                "auction-protocol.openrtb",
                "auction.prebid-server",
                "auction.aps",
                "ad-server.iabtl",
            ],
            "should offer the linked modules in the order their hooks run"
        );
    }

    /// The tool links no loading cache module, so `ts config validate`
    /// refuses a document that names one, as a stock adapter does.
    #[test]
    fn the_stock_build_links_no_loading_cache() {
        let offering: Vec<&str> = linked_builders()
            .iter()
            .filter(|builder| builder.loading_cache().is_some())
            .map(IntegrationBuilder::id)
            .collect();

        assert!(
            offering.is_empty(),
            "a stock build should link no loading cache, and links {offering:?}"
        );
    }

    /// `ts config validate` validates with the builders the tool links, so a
    /// document whose `[cache.<name>]` names a loading cache module is
    /// refused, and the refusal names the table and says the tool has none.
    #[test]
    fn deploy_validation_refuses_a_loading_cache_module_the_tool_does_not_link() {
        // Deploy validation refuses the example's placeholder addresses as
        // insecure defaults, so the test gives addresses of its own.
        let document = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../trusted-server.example.toml"
        ))
        .replace("domain = \"example.com\"", "domain = \"publisher.example\"")
        .replace(
            "cookie_domain = \".example.com\"",
            "cookie_domain = \".publisher.example\"",
        )
        .replace(
            "url = \"https://origin.example.com\"",
            "url = \"https://origin.publisher.example\"",
        );
        let mut settings =
            Settings::from_toml(&document).expect("should parse the example settings");
        settings.cache.robots_txt.module = Some("51degrees".to_owned());

        let error = trusted_server_core::config::validate_settings_for_deploy_with(
            &settings,
            &linked_builders(),
        )
        .expect_err("a loading cache module the tool does not link should be refused");

        let message = format!("{error:?}");
        assert!(
            message.contains(
                "[cache.robots-txt] module names `51degrees`, which no linked crate offers a \
                 loading cache from. The loading cache modules this build has are: none"
            ),
            "should name the table and say the tool has no loading cache: {message}"
        );
    }
}
