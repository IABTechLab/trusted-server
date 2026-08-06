use crate::common::config::cloudflare_config_json;
use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult, origin_port,
};
use error_stack::{Report, ResultExt as _};
#[cfg(feature = "aps-runner-proxy")]
use std::io::Write as _;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tempfile::{NamedTempFile, TempDir};

/// Cloudflare Workers runtime via `wrangler dev`.
///
/// In CI the bundle is pre-built and restored from artifacts; wrangler is
/// installed in the job. Locally, build the bundle first:
///
/// ```sh
/// cd crates/trusted-server-adapter-cloudflare && bash build.sh
/// ```
///
/// Then run the ignored tests with `-- --ignored test_wordpress_cloudflare`.
///
/// Set `CLOUDFLARE_WRANGLER_DIR` to override the default crate root path.
pub struct CloudflareWorkers;

/// Fallback port when dynamic allocation fails.
const CLOUDFLARE_DEFAULT_PORT: u16 = 8787;
const CI_CONFIG_TEMPLATE: &str = "wrangler.ci.toml";
const GENERATED_CI_CONFIG: &str = "wrangler.integration.generated.toml";
const TRUSTED_SERVER_CONFIG_PLACEHOLDER: &str = "TRUSTED_SERVER_CONFIG = \"{}\"";
#[cfg(feature = "aps-runner-proxy")]
const APS_RUNNER_PROXY_CONFIG_TEMPLATE: &str = "wrangler.aps-runner-proxy.toml";
#[cfg(feature = "aps-runner-proxy")]
const APS_RUNNER_PROXY_FIXTURE_CONFIG: &str =
    include_str!("../../fixtures/configs/cloudflare-aps-runner-proxy-fixture.toml");
#[cfg(feature = "aps-runner-proxy")]
const APS_RUNNER_PROXY_ENDPOINT_PLACEHOLDER: &str =
    "APS_RUNNER_PROXY_TEST_ENDPOINT = \"__APS_RUNNER_PROXY_TEST_ENDPOINT__\"";

fn write_generated_ci_config(wrangler_dir: &Path) -> TestResult<String> {
    let template_path = wrangler_dir.join(CI_CONFIG_TEMPLATE);
    let template = std::fs::read_to_string(&template_path)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to read Cloudflare CI wrangler config at {}",
            template_path.display()
        ))?;
    let config_json = cloudflare_config_json(origin_port())?;
    let generated = inject_cloudflare_config(&template, &config_json)?;
    let output_path = wrangler_dir.join(GENERATED_CI_CONFIG);
    std::fs::write(&output_path, generated)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to write generated Cloudflare CI wrangler config at {}",
            output_path.display()
        ))?;
    Ok(GENERATED_CI_CONFIG.to_string())
}

fn inject_cloudflare_config(template: &str, config_json: &str) -> TestResult<String> {
    let placeholder_count = template.matches(TRUSTED_SERVER_CONFIG_PLACEHOLDER).count();
    if placeholder_count != 1 {
        return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
            "Cloudflare CI wrangler config must contain exactly one `{TRUSTED_SERVER_CONFIG_PLACEHOLDER}` placeholder, found {placeholder_count}"
        )));
    }

    Ok(template.replace(
        TRUSTED_SERVER_CONFIG_PLACEHOLDER,
        &format!("TRUSTED_SERVER_CONFIG = '''{config_json}'''"),
    ))
}

#[cfg(feature = "aps-runner-proxy")]
fn validate_loopback_fixture_url(fixture_url: &str) -> TestResult<()> {
    let url = reqwest::Url::parse(fixture_url)
        .change_context(TestError::RuntimeSpawn)
        .attach("Cloudflare APS proxy fixture URL is invalid")?;
    let is_loopback = url
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback());
    if url.scheme() != "http"
        || !is_loopback
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Report::new(TestError::RuntimeSpawn).attach(
            "Cloudflare APS proxy fixture URL must be explicit loopback HTTP without credentials, query, or fragment",
        ));
    }
    Ok(())
}

#[cfg(feature = "aps-runner-proxy")]
fn write_temporary_config(directory: &Path, contents: &str) -> TestResult<NamedTempFile> {
    let _: toml::Value = toml::from_str(contents)
        .change_context(TestError::RuntimeSpawn)
        .attach("generated Cloudflare APS proxy Wrangler config is invalid")?;
    let mut config = tempfile::Builder::new()
        .prefix(".aps-runner-proxy-")
        .suffix(".toml")
        .tempfile_in(directory)
        .change_context(TestError::RuntimeSpawn)
        .attach("failed to create temporary Cloudflare APS proxy Wrangler config")?;
    config
        .write_all(contents.as_bytes())
        .change_context(TestError::RuntimeSpawn)
        .attach("failed to write temporary Cloudflare APS proxy Wrangler config")?;
    Ok(config)
}

#[cfg(feature = "aps-runner-proxy")]
fn generated_aps_runner_proxy_configs(
    wrangler_dir: &Path,
    fixture_url: &str,
) -> TestResult<(NamedTempFile, NamedTempFile)> {
    validate_loopback_fixture_url(fixture_url)?;

    let main_template_path = wrangler_dir.join(APS_RUNNER_PROXY_CONFIG_TEMPLATE);
    let main_template = std::fs::read_to_string(&main_template_path)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to read Cloudflare APS proxy config at {}",
            main_template_path.display()
        ))?;
    let config_json = cloudflare_config_json(origin_port())?;
    let main_config = inject_cloudflare_config(&main_template, &config_json)?;

    let placeholder_count = APS_RUNNER_PROXY_FIXTURE_CONFIG
        .matches(APS_RUNNER_PROXY_ENDPOINT_PLACEHOLDER)
        .count();
    if placeholder_count != 1 {
        return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
            "Cloudflare APS fixture config must contain one endpoint placeholder, found {placeholder_count}"
        )));
    }
    let endpoint = toml::Value::String(fixture_url.to_string()).to_string();
    let fixture_config = APS_RUNNER_PROXY_FIXTURE_CONFIG.replace(
        APS_RUNNER_PROXY_ENDPOINT_PLACEHOLDER,
        &format!("APS_RUNNER_PROXY_TEST_ENDPOINT = {endpoint}"),
    );
    let fixture_config_directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/configs");

    Ok((
        write_temporary_config(wrangler_dir, &main_config)?,
        write_temporary_config(&fixture_config_directory, &fixture_config)?,
    ))
}

impl RuntimeEnvironment for CloudflareWorkers {
    fn id(&self) -> &'static str {
        "cloudflare"
    }

    fn spawn(&self, _wasm_path: &Path) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        let config = if std::env::var("CI").is_ok() {
            write_generated_ci_config(&wrangler_dir)?
        } else {
            "wrangler.toml".to_string()
        };

        let port = super::find_available_port().unwrap_or(CLOUDFLARE_DEFAULT_PORT);

        #[cfg(unix)]
        let child = {
            use std::os::unix::process::CommandExt as _;
            Command::new("wrangler")
                .args([
                    "dev",
                    "--config",
                    config.as_str(),
                    "--port",
                    &port.to_string(),
                    "--ip",
                    "127.0.0.1",
                ])
                .current_dir(&wrangler_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .change_context(TestError::RuntimeSpawn)
                .attach(format!(
                    "Failed to spawn `wrangler dev` in {}. \
                     Ensure wrangler is installed (`npm install -g wrangler`) \
                     and the bundle is pre-built (`bash build.sh` in that directory).",
                    wrangler_dir.display()
                ))?
        };

        #[cfg(not(unix))]
        let child = Command::new("wrangler")
            .args([
                "dev",
                "--config",
                config.as_str(),
                "--port",
                &port.to_string(),
                "--ip",
                "127.0.0.1",
            ])
            .current_dir(&wrangler_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach(format!(
                "Failed to spawn `wrangler dev` in {}. \
                 Ensure wrangler is installed (`npm install -g wrangler`) \
                 and the bundle is pre-built (`bash build.sh` in that directory).",
                wrangler_dir.display()
            ))?;

        let mut child = child;
        super::register_process_group(&mut child)?;
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.is_empty() {
                        log::debug!("cloudflare: {line}");
                    }
                }
            });
        }

        let handle = CloudflareHandle {
            child,
            _configs: Vec::new(),
            _state_directory: None,
        };
        let base_url = format!("http://127.0.0.1:{port}");

        super::wait_for_ready(&base_url, self.health_check_path(), true)?;

        Ok(RuntimeProcess {
            inner: Box::new(handle),
            base_url,
        })
    }

    #[cfg(feature = "aps-runner-proxy")]
    fn spawn_aps_runner_proxy(
        &self,
        _wasm_path: &Path,
        fixture_url: &str,
    ) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        let (main_config, fixture_config) =
            generated_aps_runner_proxy_configs(&wrangler_dir, fixture_url)?;
        let state_directory = tempfile::tempdir()
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to create temporary Cloudflare APS proxy state directory")?;
        let port = super::find_available_port()?;

        let mut command = Command::new("wrangler");
        command
            .arg("dev")
            .arg("--config")
            .arg(main_config.path())
            .arg("--config")
            .arg(fixture_config.path())
            .args(["--port", &port.to_string(), "--ip", "127.0.0.1"])
            .arg("--persist-to")
            .arg(state_directory.path())
            .args(["--local", "--log-level", "info"])
            .env(
                "WRANGLER_LOG_PATH",
                state_directory.path().join("wrangler.log"),
            )
            .current_dir(&wrangler_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach(format!(
                "failed to spawn Cloudflare APS proxy Worker in {}",
                wrangler_dir.display()
            ))?;
        super::register_process_group(&mut child)?;

        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.is_empty() {
                        log::debug!("cloudflare APS proxy: {line}");
                    }
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.is_empty() {
                        log::debug!("cloudflare APS proxy: {line}");
                    }
                }
            });
        }

        let handle = CloudflareHandle {
            child,
            _configs: vec![main_config, fixture_config],
            _state_directory: Some(state_directory),
        };
        let base_url = format!("http://127.0.0.1:{port}");
        super::wait_for_http_ready(
            &base_url,
            trusted_server_core::integrations::aps::APS_RENDERER_V1_ROUTE,
            super::ReadyCheckOptions {
                // Wrangler performs noticeably more startup work for the
                // two-Worker service-binding fixture than the other local
                // runtimes. Keep this process-readiness allowance independent
                // from the APS proxy's strict upstream deadlines.
                max_attempts: 120,
                interval: std::time::Duration::from_millis(500),
                fallback_to_root: false,
                timeout_error: TestError::RuntimeNotReady,
                timeout_message: format!(
                    "Cloudflare APS runtime at {base_url} not ready after 60s"
                ),
            },
        )?;

        Ok(RuntimeProcess {
            inner: Box::new(handle),
            base_url,
        })
    }

    fn health_check_path(&self) -> &str {
        "/.well-known/trusted-server.json"
    }
}

impl CloudflareWorkers {
    /// Resolve the Cloudflare adapter crate root.
    ///
    /// Respects `CLOUDFLARE_WRANGLER_DIR` for CI overrides; falls back to
    /// the path relative to this crate's `CARGO_MANIFEST_DIR`.
    fn wrangler_dir(&self) -> PathBuf {
        if let Ok(dir) = std::env::var("CLOUDFLARE_WRANGLER_DIR") {
            return PathBuf::from(dir);
        }
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/trusted-server-adapter-cloudflare")
    }
}

struct CloudflareHandle {
    child: Child,
    _configs: Vec<NamedTempFile>,
    _state_directory: Option<TempDir>,
}

impl RuntimeProcessHandle for CloudflareHandle {}

impl Drop for CloudflareHandle {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // wrangler dev spawns workerd as a grandchild. Killing only the
            // parent leaves workerd orphaned, holding the port and fds until
            // the OS runner cleanup pass. Signal the whole process group so
            // both wrangler and workerd are terminated together.
            let pgid = self.child.id() as libc::pid_t;
            unsafe {
                libc::killpg(pgid, libc::SIGTERM);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_cloudflare_config_replaces_single_placeholder() {
        let template = format!("[vars]\n{TRUSTED_SERVER_CONFIG_PLACEHOLDER}\n");

        let generated = inject_cloudflare_config(&template, r#"{"app_config":"blob"}"#)
            .expect("should inject Cloudflare config");

        assert!(
            generated.contains("TRUSTED_SERVER_CONFIG = '''{\"app_config\":\"blob\"}'''"),
            "should inject generated config JSON"
        );
        assert!(
            !generated.contains(TRUSTED_SERVER_CONFIG_PLACEHOLDER),
            "should remove placeholder"
        );
    }

    #[test]
    fn inject_cloudflare_config_rejects_missing_placeholder() {
        let result = inject_cloudflare_config("[vars]\n", r#"{"app_config":"blob"}"#);

        assert!(result.is_err(), "should reject missing placeholder");
    }

    #[test]
    fn inject_cloudflare_config_rejects_duplicate_placeholders() {
        let template = format!(
            "[vars]\n{TRUSTED_SERVER_CONFIG_PLACEHOLDER}\n{TRUSTED_SERVER_CONFIG_PLACEHOLDER}\n"
        );

        let result = inject_cloudflare_config(&template, r#"{"app_config":"blob"}"#);

        assert!(result.is_err(), "should reject duplicate placeholders");
    }

    #[test]
    #[cfg(feature = "aps-runner-proxy")]
    fn aps_fixture_config_has_one_private_endpoint_placeholder() {
        assert_eq!(
            APS_RUNNER_PROXY_FIXTURE_CONFIG
                .matches(APS_RUNNER_PROXY_ENDPOINT_PLACEHOLDER)
                .count(),
            1,
            "should define exactly one private fixture endpoint"
        );
        assert!(
            !APS_RUNNER_PROXY_FIXTURE_CONFIG.contains("https://*:*"),
            "should not grant wildcard outbound access"
        );
    }

    #[test]
    #[cfg(feature = "aps-runner-proxy")]
    fn aps_fixture_url_rejects_non_loopback_targets() {
        assert!(
            validate_loopback_fixture_url("https://example.com/prebid-creative.js").is_err(),
            "should reject a public fixture target"
        );
        assert!(
            validate_loopback_fixture_url("http://127.0.0.1:1234/prebid-creative.js").is_ok(),
            "should accept an explicit loopback fixture target"
        );
    }
}
