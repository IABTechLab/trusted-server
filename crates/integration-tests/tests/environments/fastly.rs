use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult,
};
use error_stack::ResultExt as _;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fastly Compute runtime using Viceroy local simulator.
///
/// Spawns a `viceroy` child process with the WASM binary and a rendered
/// Viceroy-specific config (KV stores, secrets, and runtime app config store
/// contents).
pub struct FastlyViceroy;

impl RuntimeEnvironment for FastlyViceroy {
    fn id(&self) -> &'static str {
        "fastly"
    }

    fn spawn(&self, wasm_path: &Path) -> TestResult<RuntimeProcess> {
        let port = super::find_available_port()?;

        let viceroy_config = self.render_viceroy_config()?;

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
        let handle = ViceroyHandle {
            child,
            rendered_config_path: viceroy_config,
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
}

impl FastlyViceroy {
    /// Render a Viceroy config with the application config projected into the
    /// runtime config store.
    fn render_viceroy_config(&self) -> TestResult<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let template = manifest_dir.join("fixtures/configs/viceroy-template.toml");
        let app_config = manifest_dir.join("fixtures/configs/trusted-server.integration.toml");
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("should compute monotonic temp suffix")
            .as_nanos();
        let output = std::env::temp_dir().join(format!(
            "trusted-server-viceroy-{unique_suffix}.toml"
        ));
        let render_script = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("should find repository root")
            .join("scripts/render-fastly-local-config.py");

        let status = Command::new("python3")
            .arg(render_script)
            .arg("--app-config")
            .arg(app_config)
            .arg("--template")
            .arg(template)
            .arg("--output")
            .arg(&output)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .change_context(TestError::RuntimeSpawn)
            .attach("Failed to render Viceroy config")?;

        if !status.success() {
            return Err(error_stack::Report::new(TestError::RuntimeSpawn)
                .attach("render-fastly-local-config.py exited unsuccessfully"));
        }

        Ok(output)
    }
}

/// Process handle for a running Viceroy instance.
///
/// Implements [`Drop`] to ensure the process is killed on test cleanup,
/// preventing orphaned Viceroy processes.
struct ViceroyHandle {
    child: Child,
    rendered_config_path: PathBuf,
}

impl RuntimeProcessHandle for ViceroyHandle {}

impl Drop for ViceroyHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.rendered_config_path);
    }
}
