use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult,
};
use error_stack::ResultExt as _;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Fastly Compute runtime using Viceroy local simulator.
///
/// Spawns a `viceroy` child process with the WASM binary and the
/// Viceroy-specific `fastly.toml` config (KV stores, secrets).
/// The application config (origin URL, integrations) is baked into
/// the WASM binary at build time.
pub struct FastlyViceroy;

impl RuntimeEnvironment for FastlyViceroy {
    fn id(&self) -> &'static str {
        "fastly"
    }

    fn spawn(&self, wasm_path: &Path) -> TestResult<RuntimeProcess> {
        let port = super::find_available_port()?;

        let viceroy_config = self.viceroy_config_path();

        let mut child = Command::new("viceroy")
            .arg(wasm_path)
            .arg("-C")
            .arg(&viceroy_config)
            .arg("--addr")
            .arg(format!("127.0.0.1:{port}"))
            .stdout(Stdio::null())
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

        // Wrap immediately so Drop::drop kills the process if readiness check fails
        let handle = ViceroyHandle { child };
        let base_url = format!("http://127.0.0.1:{port}");

        super::wait_for_ready(&base_url, self.health_check_path())?;

        Ok(RuntimeProcess {
            inner: Box::new(handle),
            base_url,
        })
    }

    fn health_check_path(&self) -> &str {
        "/"
    }
}

impl FastlyViceroy {
    /// Path to the Viceroy-specific `fastly.toml` template.
    ///
    /// This contains `[local_server]` configuration (backends, KV stores,
    /// secret stores) that Viceroy needs, separate from the application config.
    fn viceroy_config_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/configs/viceroy-template.toml")
    }
}

/// Process handle for a running Viceroy instance.
///
/// Implements [`Drop`] to ensure the process is killed on test cleanup,
/// preventing orphaned Viceroy processes.
struct ViceroyHandle {
    child: Child,
}

impl RuntimeProcessHandle for ViceroyHandle {
    fn kill(&mut self) -> TestResult<()> {
        self.child
            .kill()
            .change_context(TestError::RuntimeKill)
            .attach("Failed to kill viceroy process")?;
        Ok(())
    }

    fn wait(&mut self) -> TestResult<()> {
        self.child
            .wait()
            .change_context(TestError::RuntimeWait)
            .attach("Failed to wait on viceroy process")?;
        Ok(())
    }
}

impl Drop for ViceroyHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
