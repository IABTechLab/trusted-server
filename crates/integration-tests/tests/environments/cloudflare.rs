use crate::common::runtime::{RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult};
use error_stack::ResultExt as _;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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

/// Port wrangler dev binds to. Matches the Axum port; both run sequentially
/// under `--test-threads=1` so the port is never double-allocated.
const CLOUDFLARE_PORT: u16 = 8787;

impl RuntimeEnvironment for CloudflareWorkers {
    fn id(&self) -> &'static str {
        "cloudflare"
    }

    fn spawn(&self, _wasm_path: &Path) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        let config = if std::env::var("CI").is_ok() {
            "wrangler.ci.toml"
        } else {
            "wrangler.toml"
        };

        let mut child = Command::new("wrangler")
            .args([
                "dev",
                "--config",
                config,
                "--port",
                &CLOUDFLARE_PORT.to_string(),
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

        let handle = CloudflareHandle { child };
        let base_url = format!("http://127.0.0.1:{CLOUDFLARE_PORT}");

        super::wait_for_ready(&base_url, self.health_check_path(), true)?;

        Ok(RuntimeProcess { inner: Box::new(handle), base_url })
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
}

impl RuntimeProcessHandle for CloudflareHandle {}

impl Drop for CloudflareHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
