use crate::common::config::integration_app_config_envelope;
use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult, origin_port,
};
use crate::environments::ReadyCheckOptions;
use error_stack::{Report, ResultExt as _};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::{NamedTempFile, TempDir};

const APS_RUNNER_PROXY_MANIFEST: &str =
    include_str!("../../fixtures/configs/spin-aps-runner-proxy.toml");
const WASM_PLACEHOLDER: &str = "__APS_RUNNER_PROXY_WASM__";

pub struct SpinRuntime;

impl RuntimeEnvironment for SpinRuntime {
    fn id(&self) -> &'static str {
        "spin"
    }

    fn spawn(&self, _wasm_path: &Path) -> TestResult<RuntimeProcess> {
        Err(Report::new(TestError::RuntimeSpawn)
            .attach("Spin is available only in the dedicated APS proxy corpus for now"))
    }

    fn spawn_aps_runner_proxy(
        &self,
        wasm_path: &Path,
        fixture_url: &str,
    ) -> TestResult<RuntimeProcess> {
        let port = super::find_available_port()?;
        let app_config = integration_app_config_envelope(origin_port())?;
        let manifest = generated_manifest(wasm_path)?;
        let state_directory = tempfile::tempdir()
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to create temporary Spin state directory")?;
        let listen = format!("127.0.0.1:{port}");

        let mut command = Command::new("spin");
        command
            .args(["up", "--from"])
            .arg(manifest.path())
            .args([
                "--variable",
                &format!("v_trusted_x5fserver_x5fconfig={app_config}"),
            ])
            .args([
                "--variable",
                &format!("aps_runner_proxy_test_endpoint={fixture_url}"),
            ])
            .arg("--state-dir")
            .arg(state_directory.path())
            .args(["--listen", &listen])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to spawn Spin APS runner proxy artifact")?;
        super::register_process_group(&mut child)?;

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.is_empty() {
                        log::debug!("spin: {line}");
                    }
                }
            });
        }

        let handle = SpinHandle {
            child,
            _manifest: manifest,
            _state_directory: state_directory,
        };
        let base_url = format!("http://{listen}");
        super::wait_for_http_ready(
            &base_url,
            self.health_check_path(),
            ReadyCheckOptions {
                max_attempts: 120,
                interval: Duration::from_millis(500),
                fallback_to_root: true,
                timeout_error: TestError::RuntimeNotReady,
                timeout_message: format!("Spin runtime at {base_url} not ready after 60s"),
            },
        )?;
        Ok(RuntimeProcess {
            inner: Box::new(handle),
            base_url,
        })
    }

    fn health_check_path(&self) -> &str {
        "/health"
    }
}

fn generated_manifest(wasm_path: &Path) -> TestResult<NamedTempFile> {
    if APS_RUNNER_PROXY_MANIFEST.matches(WASM_PLACEHOLDER).count() != 1 {
        return Err(Report::new(TestError::RuntimeSpawn)
            .attach("Spin APS proxy manifest must contain one WASM placeholder"));
    }
    let wasm_path = wasm_path
        .canonicalize()
        .change_context(TestError::RuntimeSpawn)?;
    let wasm_path = wasm_path.to_str().ok_or_else(|| {
        Report::new(TestError::RuntimeSpawn).attach("Spin WASM path is not UTF-8")
    })?;
    let rendered = APS_RUNNER_PROXY_MANIFEST.replace(WASM_PLACEHOLDER, wasm_path);
    let _: toml::Value = toml::from_str(&rendered)
        .change_context(TestError::RuntimeSpawn)
        .attach("generated Spin APS proxy manifest is invalid")?;
    let mut output = tempfile::Builder::new()
        .suffix(".toml")
        .tempfile()
        .change_context(TestError::RuntimeSpawn)
        .attach("failed to create temporary Spin APS proxy manifest")?;
    output
        .write_all(rendered.as_bytes())
        .change_context(TestError::RuntimeSpawn)
        .attach("failed to write Spin APS proxy manifest")?;
    Ok(output)
}

struct SpinHandle {
    child: Child,
    _manifest: NamedTempFile,
    _state_directory: TempDir,
}

impl RuntimeProcessHandle for SpinHandle {}

impl Drop for SpinHandle {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
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
    fn manifest_has_one_wasm_placeholder_and_required_test_variables() {
        assert_eq!(
            APS_RUNNER_PROXY_MANIFEST.matches(WASM_PLACEHOLDER).count(),
            1
        );
        assert!(APS_RUNNER_PROXY_MANIFEST.contains("aps_runner_proxy_test_endpoint"));
        assert!(APS_RUNNER_PROXY_MANIFEST.contains("v_trusted_x5fserver_x5fconfig"));
        assert!(!APS_RUNNER_PROXY_MANIFEST.contains("https://*:*"));
    }
}
