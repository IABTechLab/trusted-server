use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use error_stack::{Report, ResultExt};

use crate::config::ValidatedConfig;
use crate::error::CliError;

pub const FASTLY_LOCAL_MANIFEST: &str = "fastly.local.toml";
const EMBEDDED_FASTLY_TEMPLATE: &str = include_str!("../../../fastly.toml");

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Adapter {
    #[default]
    Fastly,
}

pub fn render_local_fastly_manifest(template: &str, canonical_toml: &str) -> String {
    let escaped = serde_json::to_string(canonical_toml).expect("should encode canonical TOML");
    let mut rendered = template.to_string();
    rendered.push('\n');
    rendered.push_str("[local_server.config_stores.ts_config_store]\n");
    rendered.push_str("    format = \"inline-toml\"\n");
    rendered.push_str("[local_server.config_stores.ts_config_store.contents]\n");
    rendered.push_str(&format!("    ts-config = {escaped}\n"));
    rendered
}

pub fn write_local_fastly_manifest(
    project_dir: &Path,
    canonical_toml: &str,
) -> Result<PathBuf, Report<CliError>> {
    let output_path = project_dir.join(FASTLY_LOCAL_MANIFEST);
    let template_path = project_dir.join("fastly.toml");
    let template =
        fs::read_to_string(&template_path).unwrap_or_else(|_| EMBEDDED_FASTLY_TEMPLATE.to_string());
    fs::write(
        &output_path,
        render_local_fastly_manifest(&template, canonical_toml),
    )
    .change_context(CliError::Development)?;
    Ok(output_path)
}

pub fn run_fastly_dev(
    project_dir: &Path,
    fastly_env: &str,
    passthrough_args: &[String],
) -> Result<ExitStatus, Report<CliError>> {
    let mut args = vec![
        "compute".to_string(),
        "serve".to_string(),
        "--dir".to_string(),
        project_dir.display().to_string(),
    ];
    if !passthrough_args
        .iter()
        .any(|arg| arg == "--env" || arg.strip_prefix("--env=").is_some())
    {
        args.push(format!("--env={fastly_env}"));
    }
    args.extend(passthrough_args.iter().cloned());

    let has_skip_build = passthrough_args.iter().any(|arg| arg == "--skip-build");
    let has_file = passthrough_args
        .iter()
        .any(|arg| arg == "--file" || arg.strip_prefix("--file=").is_some());

    if has_skip_build && !has_file {
        let target_dir = cargo_target_dir(project_dir);
        let release_path =
            target_dir.join("wasm32-wasip1/release/trusted-server-adapter-fastly.wasm");
        let debug_path = target_dir.join("wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm");
        let wasm_path = if release_path.exists() {
            release_path
        } else if debug_path.exists() {
            debug_path
        } else {
            return Err(Report::new(CliError::Development).attach(
                "--skip-build was passed but no built Wasm binary was found. Hint: run `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`.",
            ));
        };
        args.push("--file".to_string());
        args.push(wasm_path.display().to_string());
    }

    Command::new("fastly")
        .args(&args)
        .status()
        .change_context(CliError::Development)
        .attach("failed to launch `fastly compute serve`")
}

fn cargo_target_dir(project_dir: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| project_dir.join("target"))
}

pub fn run_dev_command(
    adapter: Adapter,
    validated: &ValidatedConfig,
    fastly_env: &str,
    passthrough_args: &[String],
) -> Result<ExitStatus, Report<CliError>> {
    match adapter {
        Adapter::Fastly => {
            let project_dir = std::env::current_dir().change_context(CliError::Io)?;
            write_local_fastly_manifest(&project_dir, &validated.loaded.canonical_toml)?;
            run_fastly_dev(&project_dir, fastly_env, passthrough_args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_manifest_embeds_runtime_config_store() {
        let rendered = render_local_fastly_manifest(
            EMBEDDED_FASTLY_TEMPLATE,
            "[publisher]\ndomain = \"example.com\"\n",
        );

        assert!(
            rendered.contains("[local_server.config_stores.ts_config_store]"),
            "should add app config store section"
        );
        assert!(
            rendered.contains("ts-config = \"[publisher]\\ndomain = \\\"example.com\\\"\\n\""),
            "should embed canonical TOML under ts-config"
        );
    }

    #[test]
    fn cargo_target_dir_defaults_to_project_target() {
        temp_env::with_var_unset("CARGO_TARGET_DIR", || {
            let project_dir = Path::new("/repo");

            assert_eq!(
                cargo_target_dir(project_dir),
                PathBuf::from("/repo/target"),
                "should default to the project target directory"
            );
        });
    }

    #[test]
    fn cargo_target_dir_honors_environment_override() {
        temp_env::with_var("CARGO_TARGET_DIR", Some("/tmp/cargo-target"), || {
            assert_eq!(
                cargo_target_dir(Path::new("/repo")),
                PathBuf::from("/tmp/cargo-target"),
                "should honor CARGO_TARGET_DIR for --skip-build lookup"
            );
        });
    }
}
