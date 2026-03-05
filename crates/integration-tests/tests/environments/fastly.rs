use crate::common::runtime::{
    RuntimeConfig, RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError,
};
use error_stack::ResultExt as _;
use std::collections::HashMap;
use std::process::{Child, Command};

/// Fastly Compute runtime using Viceroy local simulator.
///
/// Spawns a `viceroy` child process with the WASM binary and a generated
/// `fastly.toml` config. Dynamic port allocation allows parallel test execution.
pub struct FastlyViceroy;

impl RuntimeEnvironment for FastlyViceroy {
    fn id(&self) -> &'static str {
        "fastly"
    }

    fn spawn(&self, config: &RuntimeConfig) -> error_stack::Result<RuntimeProcess, TestError> {
        let port = super::find_available_port()?;

        // Viceroy requires a fastly.toml for local_server config (KV stores, secrets).
        // The app config (trusted-server.toml) is read by the WASM binary itself via
        // environment variable or compiled-in path.
        let viceroy_config = self.viceroy_config_path();

        let child = Command::new("viceroy")
            .arg(config.wasm_path())
            .arg("-C")
            .arg(&viceroy_config)
            .arg("--addr")
            .arg(format!("127.0.0.1:{port}"))
            .env("TRUSTED_SERVER_CONFIG", config.config_path())
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach_printable("Failed to spawn viceroy process")?;

        let base_url = format!("http://127.0.0.1:{port}");

        super::wait_for_ready(&base_url, self.health_check_path())?;

        Ok(RuntimeProcess {
            inner: Box::new(ViceroyHandle { child }),
            base_url,
        })
    }

    fn config_template(&self) -> &str {
        include_str!("../../fixtures/configs/fastly-template.toml")
    }

    fn health_check_path(&self) -> &str {
        "/health"
    }

    fn env_vars(&self) -> HashMap<String, String> {
        HashMap::new()
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
    fn kill(&mut self) -> error_stack::Result<(), TestError> {
        self.child
            .kill()
            .change_context(TestError::RuntimeKill)
            .attach_printable("Failed to kill viceroy process")?;
        Ok(())
    }

    fn wait(&mut self) -> error_stack::Result<(), TestError> {
        self.child
            .wait()
            .change_context(TestError::RuntimeWait)
            .attach_printable("Failed to wait on viceroy process")?;
        Ok(())
    }
}

impl Drop for ViceroyHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
