use crate::common::config::integration_app_config_envelope;
use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult, origin_port,
};
use error_stack::{Report, ResultExt as _};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

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
const CI_CONFIG: &str = "wrangler.ci.toml";

/// The arguments that store `envelope_path`'s contents in the Worker's local
/// config store, the namespace bound as the store's default platform name in
/// `config`.
fn seed_config_store_args<'a>(config: &'a str, envelope_path: &'a str) -> Vec<&'a str> {
    vec![
        "kv",
        "key",
        "put",
        CONFIG_BLOB_KEY,
        "--path",
        envelope_path,
        "--binding",
        DEFAULT_CONFIG_STORE_ID,
        "--local",
        "--config",
        config,
    ]
}

/// Stores `envelope` as the settings blob in the local config store that
/// `wrangler dev --config <config>` reads.
fn seed_config_store(wrangler_dir: &Path, config: &str, envelope: &str) -> TestResult<()> {
    let envelope_path =
        std::env::temp_dir().join(format!("ts-integration-config-{}.json", std::process::id()));
    std::fs::write(&envelope_path, envelope)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to write the settings blob to {}",
            envelope_path.display()
        ))?;
    let envelope_arg = envelope_path.to_string_lossy().into_owned();
    let output = Command::new("wrangler")
        .args(seed_config_store_args(config, &envelope_arg))
        .current_dir(wrangler_dir)
        .output();
    let _ = std::fs::remove_file(&envelope_path);
    let output = output
        .change_context(TestError::RuntimeSpawn)
        .attach("failed to run `wrangler kv key put` for the config store")?;
    if !output.status.success() {
        return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
            "`wrangler kv key put` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

impl RuntimeEnvironment for CloudflareWorkers {
    fn id(&self) -> &'static str {
        "cloudflare"
    }

    fn spawn(&self, _wasm_path: &Path) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        let config = if std::env::var("CI").is_ok() {
            seed_config_store(
                &wrangler_dir,
                CI_CONFIG,
                &integration_app_config_envelope(origin_port())?,
            )?;
            CI_CONFIG
        } else {
            "wrangler.toml"
        };
        self.spawn_wrangler(&wrangler_dir, config)
    }

    fn health_check_path(&self) -> &str {
        "/.well-known/trusted-server.json"
    }
}

impl CloudflareWorkers {
    /// Start `wrangler dev` with a test-specific settings blob in its config
    /// store.
    ///
    /// Unlike [`RuntimeEnvironment::spawn`], this always runs the CI
    /// wrangler config, so tests can enable features the shared fixture
    /// leaves off without changing the other runtimes' configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::RuntimeSpawn`] if the config store cannot be
    /// seeded or `wrangler dev` cannot be started, and
    /// [`TestError::RuntimeNotReady`] if the health check times out.
    pub fn spawn_with_config_envelope(&self, envelope: &str) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        seed_config_store(&wrangler_dir, CI_CONFIG, envelope)?;
        self.spawn_wrangler(&wrangler_dir, CI_CONFIG)
    }

    fn spawn_wrangler(&self, wrangler_dir: &Path, config: &str) -> TestResult<RuntimeProcess> {
        let port = super::find_available_port().unwrap_or(CLOUDFLARE_DEFAULT_PORT);

        #[cfg(unix)]
        let child = {
            use std::os::unix::process::CommandExt as _;
            Command::new("wrangler")
                .args([
                    "dev",
                    "--config",
                    config,
                    "--port",
                    &port.to_string(),
                    "--ip",
                    "127.0.0.1",
                ])
                .current_dir(wrangler_dir)
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
                config,
                "--port",
                &port.to_string(),
                "--ip",
                "127.0.0.1",
            ])
            .current_dir(wrangler_dir)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_writes_the_blob_key_through_the_config_store_binding() {
        let args = seed_config_store_args(CI_CONFIG, "/tmp/blob.json");

        assert_eq!(
            args,
            [
                "kv",
                "key",
                "put",
                "trusted_server_config",
                "--path",
                "/tmp/blob.json",
                "--binding",
                "trusted_server_config",
                "--local",
                "--config",
                "wrangler.ci.toml",
            ],
            "should store the blob where the Worker reads its settings"
        );
    }

    #[test]
    fn the_ci_config_binds_the_config_store_the_seed_writes() {
        let config: toml::Value = toml::from_str(include_str!(
            "../../../trusted-server-adapter-cloudflare/wrangler.ci.toml"
        ))
        .expect("should parse the CI wrangler config");

        let bound = config["kv_namespaces"]
            .as_array()
            .expect("should declare KV namespaces")
            .iter()
            .any(|namespace| namespace["binding"].as_str() == Some(DEFAULT_CONFIG_STORE_ID));

        assert!(
            bound,
            "the CI config should bind the config store's namespace"
        );
    }
}
