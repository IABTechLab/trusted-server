use crate::common::config::cloudflare_config_json;
use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult, origin_port,
};
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

/// Fallback port when dynamic allocation fails.
const CLOUDFLARE_DEFAULT_PORT: u16 = 8787;
const CI_CONFIG_TEMPLATE: &str = "wrangler.ci.toml";
const GENERATED_CI_CONFIG: &str = "wrangler.integration.generated.toml";

fn write_generated_ci_config(wrangler_dir: &Path) -> TestResult<String> {
    let template_path = wrangler_dir.join(CI_CONFIG_TEMPLATE);
    let template = std::fs::read_to_string(&template_path)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to read Cloudflare CI wrangler config at {}",
            template_path.display()
        ))?;
    let config_json = cloudflare_config_json(origin_port())?;
    let generated = template.replace(
        "TRUSTED_SERVER_CONFIG = \"{}\"",
        &format!("TRUSTED_SERVER_CONFIG = '''{config_json}'''"),
    );
    let output_path = wrangler_dir.join(GENERATED_CI_CONFIG);
    std::fs::write(&output_path, generated)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to write generated Cloudflare CI wrangler config at {}",
            output_path.display()
        ))?;
    Ok(GENERATED_CI_CONFIG.to_string())
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
        let base_url = format!("http://127.0.0.1:{port}");

        super::wait_for_ready(&base_url, self.health_check_path(), true)?;

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
