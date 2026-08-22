use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult,
};
use error_stack::{Report, ResultExt as _};
use std::ffi::OsString;
#[cfg(feature = "aps-runner-proxy")]
use std::io::Write as _;
use std::io::{BufRead as _, BufReader};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use tempfile::NamedTempFile;
#[cfg(feature = "aps-runner-proxy")]
use trusted_server_core::integrations::aps::{
    APS_RUNNER_BLOCKING_READ_TIMEOUT, APS_RUNNER_FIRST_BYTE_TIMEOUT,
};

/// Fastly Compute runtime using Viceroy local simulator.
///
/// Spawns a `viceroy` child process with the WASM binary and the
/// generated Viceroy config (runtime resources plus Trusted Server app-config
/// blob).
pub struct FastlyViceroy;

impl RuntimeEnvironment for FastlyViceroy {
    fn id(&self) -> &'static str {
        "fastly"
    }

    fn spawn(&self, wasm_path: &Path) -> TestResult<RuntimeProcess> {
        self.spawn_with_config(wasm_path, None)
    }

    #[cfg(feature = "aps-runner-proxy")]
    fn spawn_aps_runner_proxy(
        &self,
        wasm_path: &Path,
        fixture_url: &str,
    ) -> TestResult<RuntimeProcess> {
        let config = self.aps_runner_proxy_config(fixture_url)?;
        self.spawn_with_config(wasm_path, Some(config))
    }
}

impl FastlyViceroy {
    #[cfg(feature = "aps-runner-proxy")]
    fn aps_runner_proxy_backend_definition(authority: &str) -> toml::Value {
        let first_byte_timeout_ms = i64::try_from(APS_RUNNER_FIRST_BYTE_TIMEOUT.as_millis())
            .expect("should fit the APS first-byte timeout in Viceroy configuration");
        let between_bytes_timeout_ms = i64::try_from(APS_RUNNER_BLOCKING_READ_TIMEOUT.as_millis())
            .expect("should fit the APS between-bytes timeout in Viceroy configuration");
        toml::Value::Table(toml::Table::from_iter([
            (
                "url".to_string(),
                toml::Value::String(format!("http://{authority}/")),
            ),
            (
                "override_host".to_string(),
                toml::Value::String("client.aps.amazon-adsystem.com".to_string()),
            ),
            (
                "first_byte_timeout_ms".to_string(),
                toml::Value::Integer(first_byte_timeout_ms),
            ),
            (
                "between_bytes_timeout_ms".to_string(),
                toml::Value::Integer(between_bytes_timeout_ms),
            ),
        ]))
    }

    /// Select the Viceroy executable for this test process.
    ///
    /// `VICEROY_BIN` allows a task to validate a different simulator build
    /// without changing the repository's pinned installation or mutating
    /// `PATH`.
    fn viceroy_binary() -> OsString {
        Self::viceroy_binary_from_override(std::env::var_os("VICEROY_BIN"))
    }

    fn viceroy_binary_from_override(override_binary: Option<OsString>) -> OsString {
        override_binary
            .filter(|binary| !binary.as_os_str().is_empty())
            .unwrap_or_else(|| OsString::from("viceroy"))
    }

    fn spawn_with_config(
        &self,
        wasm_path: &Path,
        generated_config: Option<NamedTempFile>,
    ) -> TestResult<RuntimeProcess> {
        let port = super::find_available_port()?;

        let viceroy_config = generated_config.as_ref().map_or_else(
            || self.viceroy_config_path(),
            |file| file.path().to_path_buf(),
        );
        if !viceroy_config.exists() {
            return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
                "Viceroy config `{}` does not exist; run `scripts/generate-integration-viceroy-configs.sh` or `scripts/integration-tests.sh`, or set VICEROY_CONFIG_PATH to a generated config",
                viceroy_config.display()
            )));
        }

        let mut command = Command::new(Self::viceroy_binary());
        command
            .arg(wasm_path)
            .arg("-C")
            .arg(&viceroy_config)
            .arg("--addr")
            .arg(format!("127.0.0.1:{port}"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach("Failed to spawn viceroy process")?;
        super::register_process_group(&mut child)?;

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.is_empty() {
                        log::debug!("viceroy: {line}");
                    }
                }
            });
        }

        // Wrap immediately so Drop::drop kills the process if readiness check fails
        let handle = ViceroyHandle {
            child,
            _generated_config: generated_config,
        };
        let base_url = format!("http://127.0.0.1:{port}");

        // Fastly exposes a dedicated `/health` route, so root fallback only
        // adds redundant requests while the runtime is still starting up.
        super::wait_for_ready(&base_url, self.health_check_path(), false)?;

        Ok(RuntimeProcess {
            inner: Box::new(handle),
            base_url,
        })
    }

    #[cfg(feature = "aps-runner-proxy")]
    fn aps_runner_proxy_config(&self, fixture_url: &str) -> TestResult<NamedTempFile> {
        let fixture = reqwest::Url::parse(fixture_url)
            .change_context(TestError::RuntimeSpawn)
            .attach("invalid fictional APS runner fixture URL")?;
        if fixture.scheme() != "http"
            || !matches!(fixture.host_str(), Some("127.0.0.1" | "::1"))
            || fixture.port().is_none()
            || fixture.path() != "/prebid-creative.js"
            || fixture.query().is_some()
            || fixture.fragment().is_some()
        {
            return Err(Report::new(TestError::RuntimeSpawn)
                .attach("fictional APS runner fixture must be the exact loopback path"));
        }
        let base_path = self.viceroy_config_path();
        let source = std::fs::read_to_string(&base_path)
            .change_context(TestError::RuntimeSpawn)
            .attach(format!(
                "failed to read generated Viceroy config at {}",
                base_path.display()
            ))?;
        let mut config: toml::Value = toml::from_str(&source)
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to parse generated Viceroy config")?;
        let backends = config
            .get_mut("local_server")
            .and_then(toml::Value::as_table_mut)
            .and_then(|local| local.get_mut("backends"))
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| {
                Report::new(TestError::RuntimeSpawn)
                    .attach("generated Viceroy config is missing local_server.backends")
            })?;
        let host = fixture.host_str().ok_or_else(|| {
            Report::new(TestError::RuntimeSpawn).attach("fixture has no authority")
        })?;
        let port = fixture.port().ok_or_else(|| {
            Report::new(TestError::RuntimeSpawn).attach("fixture has no explicit port")
        })?;
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        backends.insert(
            "aps_runner_proxy_fixture".to_string(),
            Self::aps_runner_proxy_backend_definition(&authority),
        );
        let serialized = toml::to_string(&config)
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to serialize APS Viceroy config")?;
        let mut output = NamedTempFile::new()
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to create temporary APS Viceroy config")?;
        output
            .write_all(serialized.as_bytes())
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to write temporary APS Viceroy config")?;
        Ok(output)
    }

    /// Path to the generated Viceroy configuration.
    ///
    /// This contains `[local_server]` configuration (backends, KV stores,
    /// secret stores) plus generated test application config stores.
    ///
    /// Honors the `VICEROY_CONFIG_PATH` environment variable so CI jobs can
    /// select a generated config. This mirrors the browser harness's
    /// `global-setup.ts`, which reads the same variable. Falls back to the local
    /// generated config path when unset.
    fn viceroy_config_path(&self) -> std::path::PathBuf {
        if let Ok(path) = std::env::var("VICEROY_CONFIG_PATH")
            && !path.is_empty()
        {
            return std::path::PathBuf::from(path);
        }
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/integration-test-artifacts/configs/viceroy.toml")
    }
}

/// Process handle for a running Viceroy instance.
///
/// Implements [`Drop`] to ensure the process is killed on test cleanup,
/// preventing orphaned Viceroy processes.
struct ViceroyHandle {
    child: Child,
    _generated_config: Option<NamedTempFile>,
}

impl RuntimeProcessHandle for ViceroyHandle {}

impl Drop for ViceroyHandle {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::killpg(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::FastlyViceroy;
    use std::ffi::OsString;

    #[test]
    fn viceroy_binary_uses_task_specific_override_or_default() {
        assert_eq!(
            FastlyViceroy::viceroy_binary_from_override(None),
            OsString::from("viceroy")
        );
        assert_eq!(
            FastlyViceroy::viceroy_binary_from_override(Some(OsString::new())),
            OsString::from("viceroy")
        );
        assert_eq!(
            FastlyViceroy::viceroy_binary_from_override(Some(OsString::from(
                "/tmp/viceroy 0.19/bin/viceroy"
            ))),
            OsString::from("/tmp/viceroy 0.19/bin/viceroy")
        );
    }

    #[test]
    #[cfg(feature = "aps-runner-proxy")]
    fn aps_runner_proxy_static_backend_has_bounded_transport_timeouts() {
        let definition = FastlyViceroy::aps_runner_proxy_backend_definition("127.0.0.1:43210");

        assert_eq!(
            definition["first_byte_timeout_ms"].as_integer(),
            Some(4_000),
            "fixture should enforce the APS first-byte timeout"
        );
        assert_eq!(
            definition["between_bytes_timeout_ms"].as_integer(),
            Some(250),
            "fixture should enforce the APS between-bytes timeout"
        );
    }
}
