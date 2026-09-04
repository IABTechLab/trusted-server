use std::fs;
use std::io::Write;
use std::path::PathBuf;

pub(crate) const EXAMPLE_CONFIG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../trusted-server.example.toml"
));

#[derive(Debug, clap::Args)]
pub struct ConfigInitArgs {
    /// Target app-config path.
    #[arg(
        long = "app-config",
        alias = "config",
        default_value = "trusted-server.toml"
    )]
    pub app_config: PathBuf,
    /// Overwrite an existing target file.
    #[arg(long)]
    pub force: bool,
}

pub fn run_config_init(args: &ConfigInitArgs) -> Result<(), String> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    run_config_init_with_writer(args, &mut out)
}

fn run_config_init_with_writer(args: &ConfigInitArgs, out: &mut dyn Write) -> Result<(), String> {
    if args.app_config.exists() && !args.force {
        return Err(format!(
            "{} already exists; pass --force to overwrite",
            args.app_config.display()
        ));
    }

    if let Some(parent) = args
        .app_config
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create parent directory {}: {error}",
                parent.display()
            )
        })?;
    }

    fs::write(&args.app_config, EXAMPLE_CONFIG).map_err(|error| {
        format!(
            "failed to write config {}: {error}",
            args.app_config.display()
        )
    })?;
    writeln!(out, "Initialized config at {}", args.app_config.display())
        .map_err(|error| format!("failed to write command output: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_writes_default_config_and_refuses_overwrite() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("trusted-server.toml");
        let mut out = Vec::new();

        run_config_init_with_writer(
            &ConfigInitArgs {
                app_config: path.clone(),
                force: false,
            },
            &mut out,
        )
        .expect("should initialize config");
        assert!(path.exists(), "should write config file");

        let config = fs::read_to_string(&path).expect("should read initialized config");
        let parsed = toml::from_str::<toml::Value>(&config).expect("config should parse as TOML");
        assert!(
            parsed["integrations"]["js_asset_proxy"]
                .get("cache_ttl_seconds")
                .is_none(),
            "JS asset proxy should preserve upstream cache policy by default"
        );
        assert!(
            config.contains(
                "[integrations.js_asset_proxy]\nenabled = false\n# Uncomment to override upstream cache headers for every asset below.\n# This replaces upstream directives, including private and no-store.\n# Use only when each asset's bytes are identical for every visitor.\n# cache_ttl_seconds = 3600\n# Asset fetches use a fixed TrustedServer/1.0 User-Agent. Do not proxy assets\n# that vary by browser User-Agent or use integrity hashes for UA-specific bytes."
            ),
            "initialized config should document the optional cache override"
        );

        let err = run_config_init_with_writer(
            &ConfigInitArgs {
                app_config: path,
                force: false,
            },
            &mut Vec::new(),
        )
        .expect_err("should refuse overwrite");
        assert!(
            err.contains("already exists"),
            "error should mention existing file"
        );
    }

    #[test]
    fn init_creates_parent_directories() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("nested/config/trusted-server.toml");

        run_config_init_with_writer(
            &ConfigInitArgs {
                app_config: path.clone(),
                force: false,
            },
            &mut Vec::new(),
        )
        .expect("should initialize nested config");

        assert!(path.exists(), "should write nested config file");
    }
}
