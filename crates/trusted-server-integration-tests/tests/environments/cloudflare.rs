use crate::common::config::cloudflare_config_json;
use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult, origin_port,
};
use error_stack::{Report, ResultExt as _};
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
const TRUSTED_SERVER_CONFIG_PLACEHOLDER: &str = "TRUSTED_SERVER_CONFIG = \"{}\"";

/// The table of the CI wrangler template that binds the key-value namespace
/// the adapter declares.
const KV_NAMESPACES_TABLE: &str = "[[kv_namespaces]]";

/// Whether a generated wrangler config binds the key-value namespace.
#[derive(Clone, Copy)]
enum KvBinding {
    Bound,
    Unbound,
}

/// What [`CloudflareWorkers::spawn_wrangler`] waits for before it hands the
/// Worker to a test.
#[derive(Clone, Copy)]
enum Readiness {
    /// The health check answers with success.
    Healthy,
    /// The Worker answers at all, for a Worker whose every answer is an error.
    AnyAnswer,
}

fn write_generated_ci_config(
    wrangler_dir: &Path,
    config_json: &str,
    kv: KvBinding,
) -> TestResult<String> {
    let template_path = wrangler_dir.join(CI_CONFIG_TEMPLATE);
    let template = std::fs::read_to_string(&template_path)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to read Cloudflare CI wrangler config at {}",
            template_path.display()
        ))?;
    let mut generated = inject_cloudflare_config(&template, config_json)?;
    if matches!(kv, KvBinding::Unbound) {
        generated = without_kv_namespaces(&generated)?;
    }
    let output_path = wrangler_dir.join(GENERATED_CI_CONFIG);
    std::fs::write(&output_path, generated)
        .change_context(TestError::RuntimeSpawn)
        .attach(format!(
            "failed to write generated Cloudflare CI wrangler config at {}",
            output_path.display()
        ))?;
    Ok(GENERATED_CI_CONFIG.to_string())
}

fn inject_cloudflare_config(template: &str, config_json: &str) -> TestResult<String> {
    let placeholder_count = template.matches(TRUSTED_SERVER_CONFIG_PLACEHOLDER).count();
    if placeholder_count != 1 {
        return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
            "Cloudflare CI wrangler config must contain exactly one `{TRUSTED_SERVER_CONFIG_PLACEHOLDER}` placeholder, found {placeholder_count}"
        )));
    }

    Ok(template.replace(
        TRUSTED_SERVER_CONFIG_PLACEHOLDER,
        &format!("TRUSTED_SERVER_CONFIG = '''{config_json}'''"),
    ))
}

/// `template` without its one `[[kv_namespaces]]` table, being the table's
/// header and every line after it up to the next blank line or table.
fn without_kv_namespaces(template: &str) -> TestResult<String> {
    let tables = template
        .lines()
        .filter(|line| line.trim() == KV_NAMESPACES_TABLE)
        .count();
    if tables != 1 {
        return Err(Report::new(TestError::RuntimeSpawn).attach(format!(
            "Cloudflare CI wrangler config must contain exactly one `{KV_NAMESPACES_TABLE}` table, found {tables}"
        )));
    }

    let mut kept = Vec::new();
    let mut in_table = false;
    for line in template.lines() {
        let trimmed = line.trim();
        if trimmed == KV_NAMESPACES_TABLE {
            in_table = true;
            continue;
        }
        if in_table && (trimmed.is_empty() || trimmed.starts_with('[')) {
            in_table = false;
        }
        if !in_table {
            kept.push(line);
        }
    }
    let mut generated = kept.join("\n");
    generated.push('\n');
    Ok(generated)
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
            write_generated_ci_config(
                &wrangler_dir,
                &cloudflare_config_json(origin_port())?,
                KvBinding::Bound,
            )?
        } else {
            "wrangler.toml".to_string()
        };
        self.spawn_wrangler(&wrangler_dir, &config, Readiness::Healthy)
    }

    fn health_check_path(&self) -> &str {
        "/.well-known/trusted-server.json"
    }
}

impl CloudflareWorkers {
    /// Start `wrangler dev` with a test-specific `TRUSTED_SERVER_CONFIG` binding.
    ///
    /// Unlike [`RuntimeEnvironment::spawn`], this always renders the CI
    /// wrangler template, so tests can enable features the shared fixture
    /// leaves off without changing the other runtimes' configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::RuntimeSpawn`] if the wrangler config cannot be
    /// written or `wrangler dev` cannot be started, and
    /// [`TestError::RuntimeNotReady`] if the health check times out.
    pub fn spawn_with_config_json(&self, config_json: &str) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        let config = write_generated_ci_config(&wrangler_dir, config_json, KvBinding::Bound)?;
        self.spawn_wrangler(&wrangler_dir, &config, Readiness::Healthy)
    }

    /// Start `wrangler dev` with a test-specific `TRUSTED_SERVER_CONFIG`
    /// binding and no key-value namespace bound, returning once the Worker
    /// answers at all, because every answer it gives is then an error.
    ///
    /// # Errors
    ///
    /// Returns [`TestError::RuntimeSpawn`] if the wrangler config cannot be
    /// written or `wrangler dev` cannot be started, and
    /// [`TestError::RuntimeNotReady`] if the Worker never answers.
    pub fn spawn_without_kv_binding(&self, config_json: &str) -> TestResult<RuntimeProcess> {
        let wrangler_dir = self.wrangler_dir();
        let config = write_generated_ci_config(&wrangler_dir, config_json, KvBinding::Unbound)?;
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
    fn inject_cloudflare_config_replaces_single_placeholder() {
        let template = format!("[vars]\n{TRUSTED_SERVER_CONFIG_PLACEHOLDER}\n");

        let generated = inject_cloudflare_config(&template, r#"{"app_config":"blob"}"#)
            .expect("should inject Cloudflare config");

        assert!(
            generated.contains("TRUSTED_SERVER_CONFIG = '''{\"app_config\":\"blob\"}'''"),
            "should inject generated config JSON"
        );
        assert!(
            !generated.contains(TRUSTED_SERVER_CONFIG_PLACEHOLDER),
            "should remove placeholder"
        );
    }

    #[test]
    fn inject_cloudflare_config_rejects_missing_placeholder() {
        let result = inject_cloudflare_config("[vars]\n", r#"{"app_config":"blob"}"#);

        assert!(result.is_err(), "should reject missing placeholder");
    }

    #[test]
    fn inject_cloudflare_config_rejects_duplicate_placeholders() {
        let template = format!(
            "[vars]\n{TRUSTED_SERVER_CONFIG_PLACEHOLDER}\n{TRUSTED_SERVER_CONFIG_PLACEHOLDER}\n"
        );

        let result = inject_cloudflare_config(&template, r#"{"app_config":"blob"}"#);

        assert!(result.is_err(), "should reject duplicate placeholders");
    }

    #[test]
    fn without_kv_namespaces_drops_only_the_namespace_table() {
        let template = include_str!("../../../trusted-server-adapter-cloudflare/wrangler.ci.toml");

        let generated = without_kv_namespaces(template).expect("should drop the namespace table");

        let manifest: toml::Value = toml::from_str(&generated).expect("should stay valid TOML");
        let original: toml::Value = toml::from_str(template).expect("should parse the template");
        assert!(
            manifest.get("kv_namespaces").is_none(),
            "should bind no namespace"
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
    fn without_kv_namespaces_rejects_a_template_without_the_table() {
        let result = without_kv_namespaces("[vars]\n");

        assert!(result.is_err(), "should reject a template with no table");
    }
}
