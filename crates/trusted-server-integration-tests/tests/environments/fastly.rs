use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult,
};
use error_stack::{Report, ResultExt as _};
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

/// Fastly Compute runtime using Viceroy local simulator.
///
/// Spawns a `viceroy` child process with the WASM binary and the
/// generated Viceroy config (runtime resources plus Trusted Server app-config
/// blob).
pub struct FastlyViceroy;

/// The lines a Viceroy guest printed to stdout, in order.
///
/// Viceroy prints each line written to a log endpoint as `<endpoint> :: <line>`.
#[derive(Clone, Default)]
pub(crate) struct GuestStdout(Arc<Mutex<Vec<String>>>);

impl GuestStdout {
    /// Returns every line printed so far.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl RuntimeEnvironment for FastlyViceroy {
    fn id(&self) -> &'static str {
        "fastly"
    }

    fn spawn(&self, wasm_path: &Path) -> TestResult<RuntimeProcess> {
        self.spawn_capturing_stdout(wasm_path)
            .map(|(process, _stdout)| process)
    }
}

impl FastlyViceroy {
    /// Spawns Viceroy as [`RuntimeEnvironment::spawn`] does, and also returns
    /// what its guest prints to stdout.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::RuntimeSpawn`] if the process cannot be started, and
    /// [`TestError::RuntimeNotReady`] if the health check times out.
    pub(crate) fn spawn_capturing_stdout(
        &self,
        wasm_path: &Path,
    ) -> TestResult<(RuntimeProcess, GuestStdout)> {
        let port = super::find_available_port()?;

        let viceroy_config = self.viceroy_config_path();
        if !viceroy_config.exists() {
            return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
                "Viceroy config `{}` does not exist; run `scripts/generate-integration-viceroy-configs.sh` or `scripts/integration-tests.sh`, or set VICEROY_CONFIG_PATH to a generated config",
                viceroy_config.display()
            )));
        }

        let mut child = Command::new("viceroy")
            .arg(wasm_path)
            .arg("-C")
            .arg(&viceroy_config)
            .arg("--addr")
            .arg(format!("127.0.0.1:{port}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach("Failed to spawn viceroy process")?;

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

        // Drained for the whole run, so the guest never blocks on a full pipe.
        let stdout = GuestStdout::default();
        if let Some(pipe) = child.stdout.take() {
            let recorded = stdout.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    recorded
                        .0
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(line);
                }
            });
        }

        // Wrap immediately so Drop::drop kills the process if readiness check fails
        let handle = ViceroyHandle { child };
        let base_url = format!("http://127.0.0.1:{port}");

        // Fastly exposes a dedicated `/health` route, so root fallback only
        // adds redundant requests while the runtime is still starting up.
        super::wait_for_ready(&base_url, self.health_check_path(), false)?;

        Ok((
            RuntimeProcess {
                inner: Box::new(handle),
                base_url,
            },
            stdout,
        ))
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
}

impl RuntimeProcessHandle for ViceroyHandle {}

impl Drop for ViceroyHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
