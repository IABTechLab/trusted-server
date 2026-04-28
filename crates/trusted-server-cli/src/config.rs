use std::fs;
use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt};
use serde::Serialize;
use trusted_server_core::runtime_config::LoadedRuntimeConfig;

use crate::error::CliError;

pub const DEFAULT_CONFIG_PATH: &str = "trusted-server.toml";
pub const STARTER_CONFIG_TEMPLATE: &str = include_str!("../../../trusted-server.example.toml");

#[derive(Debug)]
pub struct ValidatedConfig {
    pub path: PathBuf,
    pub loaded: LoadedRuntimeConfig,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ValidateConfigJson {
    pub valid: bool,
    pub path: String,
    pub config_hash: Option<String>,
    pub errors: Vec<String>,
}

pub fn resolve_config_path(path: Option<&Path>) -> Result<PathBuf, Report<CliError>> {
    let candidate = match path {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => std::env::current_dir()
            .change_context(CliError::Io)?
            .join(path),
        None => std::env::current_dir()
            .change_context(CliError::Io)?
            .join(DEFAULT_CONFIG_PATH),
    };

    Ok(candidate)
}

pub fn ensure_writable_path(path: &Path, force: bool) -> Result<(), Report<CliError>> {
    if path.exists() && !force {
        return Err(Report::new(CliError::Io).attach(format!(
            "refusing to overwrite existing file `{}`; re-run with --force",
            path.display()
        )));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).change_context(CliError::Io)?;
    }

    Ok(())
}

pub fn write_starter_config(path: &Path, force: bool) -> Result<(), Report<CliError>> {
    ensure_writable_path(path, force)?;
    fs::write(path, STARTER_CONFIG_TEMPLATE).change_context(CliError::Io)
}

pub fn load_validated_config(path: Option<&Path>) -> Result<ValidatedConfig, Report<CliError>> {
    let resolved_path = resolve_config_path(path)?;

    let original_toml = fs::read_to_string(&resolved_path).map_err(|error| {
        let hint = format!(
            "failed to read config `{}`: {error}. Hint: run `ts config init` or pass `--config <path>`.",
            resolved_path.display()
        );
        Report::new(CliError::Configuration).attach(hint)
    })?;

    let loaded = trusted_server_core::runtime_config::load_runtime_config(&original_toml)
        .change_context(CliError::Configuration)
        .attach(format!("while validating `{}`", resolved_path.display()))?;

    Ok(ValidatedConfig {
        path: resolved_path,
        loaded,
    })
}

pub fn validate_config_json(path: Option<&Path>) -> ValidateConfigJson {
    match load_validated_config(path) {
        Ok(validated) => ValidateConfigJson {
            valid: true,
            path: validated.path.display().to_string(),
            config_hash: Some(validated.loaded.config_hash),
            errors: Vec::new(),
        },
        Err(error) => {
            let resolved_path = resolve_config_path(path)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
            ValidateConfigJson {
                valid: false,
                path: resolved_path,
                config_hash: None,
                errors: vec![format!("{error:?}")],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_config_json_reports_success_for_example_config() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join(DEFAULT_CONFIG_PATH);
        fs::write(&path, STARTER_CONFIG_TEMPLATE).expect("should write starter config");

        let response = validate_config_json(Some(&path));

        assert!(response.valid, "should report valid example config");
        assert!(
            response.config_hash.is_some(),
            "should include config hash for valid config"
        );
    }

    #[test]
    fn validate_config_json_reports_missing_file() {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join("missing.toml");

        let response = validate_config_json(Some(&path));

        assert!(!response.valid, "should report invalid for missing file");
        assert_eq!(response.config_hash, None, "should not have hash");
    }
}
