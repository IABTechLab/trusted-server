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

/// The CI wrangler config without the key-value store's namespace, which
/// [`CloudflareWorkers::spawn_without_kv_binding`] writes.
const GENERATED_UNBOUND_CONFIG: &str = "wrangler.integration.generated.toml";

/// The header of a table of the CI wrangler config that binds a key-value
/// namespace.
const KV_NAMESPACES_TABLE: &str = "[[kv_namespaces]]";

/// The binding of the key-value namespace the adapter declares as its store.
const KV_STORE_BINDING: &str = "TRUSTED_SERVER_KV";

/// What [`CloudflareWorkers::spawn_wrangler`] waits for before it hands the
/// Worker to a test.
#[derive(Clone, Copy)]
enum Readiness {
    /// The health check answers with success.
    Healthy,
    /// The Worker answers at all, for a Worker whose every answer is an error.
    AnyAnswer,
}

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

/// Writes the CI wrangler config without the key-value store's namespace,
/// keeping the config store's, and returns the file's name.
fn write_ci_config_without_kv_binding(wrangler_dir: &Path) -> TestResult<String> {
    let template_path = wrangler_dir.join(CI_CONFIG);
    let template = std::fs::read_to_string(&template_path)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to read Cloudflare CI wrangler config at {}",
            template_path.display()
        ))?;
    let generated = without_kv_store_binding(&template)?;
    let output_path = wrangler_dir.join(GENERATED_UNBOUND_CONFIG);
    std::fs::write(&output_path, generated)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to write generated Cloudflare CI wrangler config at {}",
            output_path.display()
        ))?;
    Ok(GENERATED_UNBOUND_CONFIG.to_string())
}

/// `template` without the `[[kv_namespaces]]` table that binds
/// [`KV_STORE_BINDING`], being the table's header and every line after it up
/// to the next blank line or table. Every other table is kept.
fn without_kv_store_binding(template: &str) -> TestResult<String> {
    let binding = format!("binding = \"{KV_STORE_BINDING}\"");
    let mut kept = Vec::new();
    let mut table = Vec::new();
    let mut dropped = 0;
    for line in template.lines() {
        let trimmed = line.trim();
        if !table.is_empty() && (trimmed.is_empty() || trimmed.starts_with('[')) {
            dropped += keep_unless_bound(&mut table, &mut kept, &binding);
        }
        if trimmed == KV_NAMESPACES_TABLE || !table.is_empty() {
            table.push(line);
        } else {
            kept.push(line);
        }
    }
    dropped += keep_unless_bound(&mut table, &mut kept, &binding);
    if dropped != 1 {
        return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
            "Cloudflare CI wrangler config must bind `{KV_STORE_BINDING}` in exactly one `{KV_NAMESPACES_TABLE}` table, found {dropped}"
        )));
    }
    let mut generated = kept.join("\n");
    generated.push('\n');
    Ok(generated)
}

/// Moves the lines of `table` onto `kept`, unless the table holds the line
/// `binding`, and returns the number of tables dropped.
fn keep_unless_bound<'a>(
    table: &mut Vec<&'a str>,
    kept: &mut Vec<&'a str>,
    binding: &str,
) -> usize {
    if table.iter().any(|line| line.trim() == binding) {
        table.clear();
        1
    } else {
        kept.append(table);
        0
    }
}

/// Polls `base_url` until the Worker answers with any status.
fn wait_for_any_answer(base_url: &str) -> TestResult<()> {
    for _ in 0..30 {
        if reqwest::blocking::get(base_url).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(Report::new(TestError::RuntimeNotReady)
        .attach(format!("Runtime at {base_url} did not answer after 15s")))
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
        self.spawn_wrangler(&wrangler_dir, config, Readiness::Healthy)
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
        self.spawn_wrangler(&wrangler_dir, CI_CONFIG, Readiness::Healthy)
    }

    /// Start `wrangler dev` with a test-specific settings blob in its config
    /// store and no namespace bound for the key-value store, returning once
    /// the Worker answers at all, because every answer it gives is then an
    /// error.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::RuntimeSpawn`] if the wrangler config cannot be
    /// written, the config store cannot be seeded or `wrangler dev` cannot be
    /// started, and [`TestError::RuntimeNotReady`] if the Worker never
    /// answers.
    pub fn spawn_without_kv_binding(&self, envelope: &str) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        let config = write_ci_config_without_kv_binding(&wrangler_dir)?;
        seed_config_store(&wrangler_dir, &config, envelope)?;
        self.spawn_wrangler(&wrangler_dir, &config, Readiness::AnyAnswer)
    }

    fn spawn_wrangler(
        &self,
        wrangler_dir: &Path,
        config: &str,
        readiness: Readiness,
    ) -> TestResult<RuntimeProcess> {
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

        match readiness {
            Readiness::Healthy => {
                super::wait_for_ready(&base_url, self.health_check_path(), true)?;
            }
            Readiness::AnyAnswer => wait_for_any_answer(&base_url)?,
        }

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

    #[test]
    fn without_kv_store_binding_drops_only_the_key_value_stores_table() {
        let template = include_str!("../../../trusted-server-adapter-cloudflare/wrangler.ci.toml");

        let generated =
            without_kv_store_binding(template).expect("should drop the key-value store's table");

        let manifest: toml::Value = toml::from_str(&generated).expect("should stay valid TOML");
        let original: toml::Value = toml::from_str(template).expect("should parse the template");
        let bound: Vec<&str> = manifest["kv_namespaces"]
            .as_array()
            .expect("should keep the config store's namespace")
            .iter()
            .filter_map(|namespace| namespace["binding"].as_str())
            .collect();
        assert_eq!(
            bound,
            [DEFAULT_CONFIG_STORE_ID],
            "should bind the config store's namespace alone"
        );
        assert_eq!(
            manifest.get("vars"),
            original.get("vars"),
            "should keep the variables"
        );
        assert_eq!(
            manifest.get("main"),
            original.get("main"),
            "should keep the bundle"
        );
    }

    #[test]
    fn without_kv_store_binding_rejects_a_template_that_does_not_bind_the_store() {
        let result = without_kv_store_binding("[vars]\n");

        assert!(
            result.is_err(),
            "should reject a template that does not bind the store"
        );
    }
}
