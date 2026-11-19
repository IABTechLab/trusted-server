//! Native startup contracts exercised through the real production binary.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use edgezero_core::app::{Hooks as _, StoreMetadata};
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_core::config_payload::{DEFAULT_CONFIG_STORE_ID, DEFAULT_SECRET_STORE_ID};

// The hash covers the sorted, compact data object, using the existing TS
// BlobEnvelope v1 format. All values are synthetic local fixture inputs.
const ENVELOPE: &str = r#"{"data":{"ec":{"passphrase":"ec_key"},"handlers":[{"password":"admin_key","path":"^/_ts/admin","username":"admin"}],"publisher":{"cookie_domain":".publisher.example.com","domain":"publisher.example.com","origin_url":"https://origin.publisher.example.com","proxy_secret":"proxy_key"},"request_signing":null},"sha256":"f4dc03017deb24bfd589efde7dce0f9f8a20d952c28ebcbdb6141b08661cd137","version":1,"generated_at":"2026-10-20T00:00:00Z"}"#;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
const ADMIN_PASSWORD: &str = "native-fixture-admin-password";
const PROXY_SECRET: &str = "native-fixture-proxy-secret-32-bytes";
const EC_PASSPHRASE: &str = "native-fixture-passphrase-32-bytes-minimum";
// Too short for runtime validation, and distinctive enough to find in logs.
const SHORT_EC_PASSPHRASE: &str = "short-ec-fixture";
const LEGACY_PREFIX: &str = "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_";

struct ServerFixture {
    root: PathBuf,
    addr: SocketAddr,
    child: Option<Child>,
}

fn reserve_port() -> tokio::net::TcpSocket {
    // Bind without listening to let the OS pick a free port. Callers drop the
    // socket before spawning, so the port is released, not held: another
    // process could take it, but no probe ever sees a fixture listener.
    let socket = tokio::net::TcpSocket::new_v4().expect("should create a port reservation");
    socket
        .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("should bind without creating a listener");
    socket
}

#[test]
fn fixture_port_reservation_does_not_create_a_listener() {
    let reservation = reserve_port();
    let address = reservation
        .local_addr()
        .expect("should read the reserved address");
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_millis(20)).is_err(),
        "port allocation itself must not create a listener observed by concurrent startup probes"
    );
}

impl ServerFixture {
    fn new(envelope: Option<&str>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ts-native-startup-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).expect("should create isolated native config directory");
        if let Some(envelope) = envelope {
            // The fixed fixture is ASCII; Debug escaping yields the JSON string
            // value required by the local provider's outer key-to-string map.
            fs::write(
                root.join(format!("local-config-{DEFAULT_CONFIG_STORE_ID}.json")),
                format!("{{{DEFAULT_CONFIG_STORE_ID:?}:{envelope:?}}}"),
            )
            .expect("should write a local config envelope map");
        }
        let reservation = reserve_port();
        let addr = reservation
            .local_addr()
            .expect("should read the test address");
        drop(reservation);
        Self {
            root,
            addr,
            child: None,
        }
    }

    fn spawn(&mut self, missing_secret: bool, short_passphrase: bool) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"));
        command
            .env_clear()
            .env("EDGEZERO__LOGGING__LEVEL", "info")
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", self.addr.port().to_string())
            .env("EDGEZERO__ADAPTER__CONFIG_DIR", &self.root)
            .env("EDGEZERO__SECRETS__PROXY_KEY", PROXY_SECRET)
            .env("EDGEZERO__SECRETS__ADMIN_KEY", ADMIN_PASSWORD)
            // Valid legacy inputs: a fallback to them would let a missing
            // prepared input start the server, so the failure tests would fail.
            .env(
                "TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG",
                ENVELOPE,
            )
            .env(format!("{LEGACY_PREFIX}EC_KEY"), EC_PASSPHRASE)
            .env(format!("{LEGACY_PREFIX}PROXY_KEY"), PROXY_SECRET)
            .env(format!("{LEGACY_PREFIX}ADMIN_KEY"), ADMIN_PASSWORD)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !missing_secret {
            command.env(
                "EDGEZERO__SECRETS__EC_KEY",
                if short_passphrase {
                    SHORT_EC_PASSPHRASE
                } else {
                    EC_PASSPHRASE
                },
            );
        }
        self.child = Some(
            command
                .spawn()
                .expect("should spawn production native binary"),
        );
    }

    fn assert_failed_before_listen(&mut self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // Linux can connect a socket to itself when its ephemeral source
            // port equals the target port. That is not a listener.
            let listener_reachable =
                TcpStream::connect_timeout(&self.addr, Duration::from_millis(20)).is_ok_and(
                    |stream| {
                        stream.local_addr().expect("should read probe source")
                            != stream.peer_addr().expect("should read probe peer")
                    },
                );
            assert!(
                !listener_reachable,
                "should not listen while required startup inputs are invalid"
            );
            if let Some(status) = self
                .child
                .as_mut()
                .expect("should own child")
                .try_wait()
                .expect("should poll production child")
            {
                assert!(
                    !status.success(),
                    "should exit nonzero on initialization failure"
                );
                let output = self
                    .child
                    .take()
                    .expect("should own child output")
                    .wait_with_output()
                    .expect("should collect startup diagnostics");
                let diagnostic = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    diagnostic.contains(expected),
                    "should explain startup failure: {diagnostic}"
                );
                assert_no_secrets(&diagnostic);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "should fail startup within the test bound"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_ready(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(response) = self.get("/health") {
                assert!(
                    response.starts_with("HTTP/1.1 200"),
                    "should become healthy only after successful initialization: {response}"
                );
                return response;
            }
            let status = self
                .child
                .as_mut()
                .expect("should own child")
                .try_wait()
                .expect("should poll child");
            let diagnostic = if status.is_some() {
                let output = self
                    .child
                    .take()
                    .expect("should own failed child")
                    .wait_with_output()
                    .expect("should collect startup diagnostics");
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            } else {
                String::new()
            };
            assert!(
                status.is_none(),
                "should keep running after valid prepared-store initialization: {diagnostic}"
            );
            assert!(
                Instant::now() < deadline,
                "should become ready within the test bound"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stops a running server and checks its whole output for secret values.
    fn assert_logs_hold_no_secrets(&mut self) {
        let mut child = self.child.take().expect("should own running child");
        child.kill().expect("should stop the running server");
        let output = child
            .wait_with_output()
            .expect("should collect running server output");
        assert_no_secrets(&format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    fn get(&self, path: &str) -> std::io::Result<String> {
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: publisher.example.com\r\nConnection: close\r\n\r\n"
        )?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response)
    }
}

fn assert_no_secrets(output: &str) {
    for secret in [
        ADMIN_PASSWORD,
        PROXY_SECRET,
        EC_PASSPHRASE,
        SHORT_EC_PASSPHRASE,
    ] {
        assert!(
            !output.contains(secret),
            "should not expose a secret value in process output"
        );
    }
}

impl Drop for ServerFixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn missing_config_fails_before_listen_without_legacy_fallback() {
    let mut server = ServerFixture::new(None);
    server.spawn(false, false);
    // Local preparation rejects the absent file before the initializer runs.
    server.assert_failed_before_listen("required snapshot missing");
}

#[test]
fn missing_secret_fails_before_listen_without_legacy_fallback() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(true, false);
    server.assert_failed_before_listen("required app-config secret `ec_key` is missing");
}

#[test]
fn runtime_secret_validation_fails_before_listen() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(false, true);
    server.assert_failed_before_listen("passphrase");
}

#[test]
fn tampered_envelope_fails_before_listen() {
    let tampered = ENVELOPE.replace("f4dc0301", "00000000");
    let mut server = ServerFixture::new(Some(&tampered));
    server.spawn(false, false);
    server.assert_failed_before_listen("integrity verification");
}

#[test]
fn valid_prepared_inputs_serve_real_routes_and_ignore_changed_config_while_running() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(false, false);
    let health = server.wait_ready();
    assert!(
        health.to_ascii_lowercase().contains("x-ts-version:"),
        "should serve health through the real middleware: {health}"
    );
    let tsjs = server
        .get("/static/tsjs=0000000000000000")
        .expect("should reach TSJS route");
    assert!(
        tsjs.starts_with("HTTP/1.1 404"),
        "should execute unknown-hash TSJS handler: {tsjs}"
    );
    assert!(
        tsjs.to_ascii_lowercase()
            .contains("x-geo-info-available: false"),
        "should report unavailable geo on the native server: {tsjs}"
    );
    fs::write(
        server
            .root
            .join(format!("local-config-{DEFAULT_CONFIG_STORE_ID}.json")),
        "invalid-changed-config",
    )
    .expect("should replace fixture publication input");
    assert!(
        server
            .get("/health")
            .expect("should retain initialized state")
            .starts_with("HTTP/1.1 200"),
        "should keep serving the startup snapshot after the input changes"
    );
    server.assert_logs_hold_no_secrets();
}

#[test]
fn legacy_live_framing_headers_load_without_reapplying_operator_push_rules() {
    let original: edgezero_core::blob_envelope::BlobEnvelope =
        serde_json::from_str(ENVELOPE).expect("should parse the valid existing live fixture");
    let mut data = original.into_data();
    data["response_headers"] = serde_json::json!({
        "Content-Length": "999",
        "Transfer-Encoding": "chunked",
        "Connection": "close",
        "X-Example": "preserved"
    });
    let envelope =
        edgezero_core::blob_envelope::BlobEnvelope::new(data, "2026-11-12T00:00:00Z".to_owned());
    let envelope = serde_json::to_string(&envelope)
        .expect("should serialize a correctly hashed existing live fixture");
    let mut server = ServerFixture::new(Some(&envelope));
    server.spawn(false, false);
    let health = server.wait_ready();
    let headers = health.to_ascii_lowercase();
    assert!(
        headers.contains("x-example: preserved"),
        "should preserve safe existing runtime headers"
    );
    assert!(
        !headers.contains("content-length: 999"),
        "should not install an operator framing value on the real response"
    );
}

#[test]
fn handwritten_store_metadata_matches_the_manifest() {
    let manifest = include_str!("../../../edgezero.toml");
    let stores = TrustedServerApp::stores();
    for (kind, metadata) in [("config", stores.config), ("secrets", stores.secrets)] {
        let StoreMetadata { default, ids } =
            metadata.expect("should declare required native store");
        let section = manifest
            .split(&format!("[stores.{kind}]\n"))
            .nth(1)
            .expect("should declare the same store kind in the manifest")
            .split("\n[")
            .next()
            .expect("should read store section");
        let quoted_values = |name: &str| -> Vec<&str> {
            section
                .lines()
                .find_map(|line| line.trim().strip_prefix(&format!("{name} = ")))
                .expect("should contain the portable store field")
                .split('"')
                .enumerate()
                .filter_map(|(index, part)| (index % 2 == 1).then_some(part))
                .collect()
        };
        assert_eq!(
            quoted_values("default"),
            vec![default],
            "should match the manifest's default {kind} store"
        );
        assert_eq!(
            quoted_values("ids"),
            ids,
            "should match the manifest's {kind} store ids"
        );
    }
    assert_eq!(
        stores.config.expect("should declare config").default,
        DEFAULT_CONFIG_STORE_ID,
        "should default to the core config store id"
    );
    assert_eq!(
        stores.secrets.expect("should declare secrets").default,
        DEFAULT_SECRET_STORE_ID,
        "should default to the core secret store id"
    );
    assert!(stores.kv.is_none(), "should not prepare unused generic KV");
    // The portable manifest may declare KV for other adapters. Native startup
    // selects only its handwritten config/secret metadata above.
}
