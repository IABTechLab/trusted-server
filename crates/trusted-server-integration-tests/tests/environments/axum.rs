use crate::common::config::{envelope_with_public_origin, integration_app_config_envelope};
use crate::common::runtime::{
    RuntimeEnvironment, RuntimeProcess, RuntimeProcessHandle, TestError, TestResult, origin_port,
};
use error_stack::{Report, ResultExt as _};
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

/// Local prepared-secret entries referenced by the integration app-config fixture.
const INTEGRATION_SECRETS: &[(&str, &str)] = &[
    (
        "EDGEZERO__SECRETS__INTEGRATION_ADMIN_PASSWORD",
        "integration-admin-password-32-bytes-ok",
    ),
    (
        "EDGEZERO__SECRETS__INTEGRATION_PROXY_SECRET",
        "integration-test-proxy-secret-32-bytes-ok",
    ),
    (
        "EDGEZERO__SECRETS__INTEGRATION_EC_PASSPHRASE",
        "integration-test-ec-secret-padded-32",
    ),
    (
        "EDGEZERO__SECRETS__INTEGRATION_PARTNER_TOKEN_ALPHA",
        "integration-test-token-alpha-32-bytes-ok",
    ),
    (
        "EDGEZERO__SECRETS__INTEGRATION_PARTNER_TOKEN_BRAVO",
        "integration-test-token-bravo-32-bytes-ok",
    ),
];

/// Native runtime with finite prepared config and secret inputs.
pub struct AxumDevServer;

impl RuntimeEnvironment for AxumDevServer {
    fn id(&self) -> &'static str {
        "axum"
    }

    fn spawn(&self, _wasm_path: &Path) -> TestResult<RuntimeProcess> {
        let app_config = integration_app_config_envelope(origin_port())?;
        self.spawn_with_app_config(&app_config, &[])
    }

    fn health_check_path(&self) -> &str {
        "/health"
    }
}

impl AxumDevServer {
    /// Spawn with an isolated app-config envelope and explicit environment overrides.
    ///
    /// # Errors
    ///
    /// Returns an error if preparing inputs, spawning, or readiness fails.
    pub(crate) fn spawn_with_app_config(
        &self,
        app_config: &str,
        env: &[(&str, &str)],
    ) -> TestResult<RuntimeProcess> {
        let port = super::find_available_port()?;
        let base_url = format!("http://127.0.0.1:{port}");
        let app_config = envelope_with_public_origin(app_config, &base_url)?;
        let (mut command, workspace) = self.command(&app_config, port)?;
        let mut child = command
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .change_context(TestError::RuntimeSpawn)
            .attach("Failed to spawn the prepared native runtime")?;

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if !line.is_empty() {
                        log::debug!("axum: {line}");
                    }
                }
            });
        }

        // Own both resources before readiness so errors tear them down together.
        let mut handle = AxumHandle {
            child,
            _workspace: workspace,
        };
        wait_for_health(&base_url, &mut handle.child)?;
        Ok(RuntimeProcess {
            inner: Box::new(handle),
            base_url,
        })
    }

    /// Prepare a command and its config directory. The caller must retain the
    /// directory until the child exits.
    ///
    /// # Errors
    ///
    /// Returns an error if the isolated local config cannot be written.
    pub(crate) fn command(
        &self,
        app_config: &str,
        port: u16,
    ) -> TestResult<(Command, tempfile::TempDir)> {
        let workspace = tempfile::tempdir().change_context(TestError::ConfigGeneration)?;
        let root = workspace
            .path()
            .canonicalize()
            .change_context(TestError::ConfigGeneration)?;
        let config = serde_json::to_vec(&serde_json::json!({ CONFIG_BLOB_KEY: app_config }))
            .change_context(TestError::ConfigGeneration)?;
        std::fs::write(
            root.join(format!("local-config-{DEFAULT_CONFIG_STORE_ID}.json")),
            config,
        )
        .change_context(TestError::ConfigGeneration)?;
        let mut command = Command::new(self.binary_path());
        command
            .env_clear()
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", port.to_string())
            .env("EDGEZERO__ADAPTER__CONFIG_DIR", &root)
            .env("EDGEZERO__ADAPTER__DATA_DIR", &root)
            .env("EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS", "1")
            .envs(INTEGRATION_SECRETS.iter().copied());
        Ok((command, workspace))
    }

    fn binary_path(&self) -> std::path::PathBuf {
        if let Ok(path) = std::env::var("AXUM_BINARY_PATH") {
            return std::path::PathBuf::from(path);
        }
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/trusted-server-axum")
    }
}

fn wait_for_health(base_url: &str, child: &mut Child) -> TestResult<()> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(300))
        .build()
        .change_context(TestError::RuntimeNotReady)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(status) = child
            .try_wait()
            .change_context(TestError::RuntimeNotReady)?
        {
            return Err(Report::new(TestError::RuntimeNotReady)
                .attach(format!("Axum exited before readiness: {status}")));
        }
        if let Ok(response) = client.get(format!("{base_url}/health")).send()
            && response.status().as_u16() == 200
            && response.text().is_ok_and(|body| body == "ok")
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(Report::new(TestError::RuntimeNotReady).attach(format!(
        "Axum at {base_url} did not return HTTP 200 /health body ok within 15s"
    )))
}

struct AxumHandle {
    child: Child,
    _workspace: tempfile::TempDir,
}

impl RuntimeProcessHandle for AxumHandle {}

impl Drop for AxumHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        log::error!("Axum child did not exit within bounded kill deadline");
    }
}
