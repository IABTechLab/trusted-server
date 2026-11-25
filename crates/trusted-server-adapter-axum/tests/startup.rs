//! Native startup contracts exercised through the real production binary.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use edgezero_core::app::{Hooks as _, StoreMetadata};
use edgezero_core::blob_envelope::BlobEnvelope;
use serde_json::json;
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

#[cfg(feature = "celld-ec")]
#[path = "support/celld.rs"]
mod celld_fixture;

#[cfg(feature = "celld-ec")]
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires official CELLD_EC_TEST_BINARY and matching EDGEZERO_CELLD_STORE_SERVICE"]
async fn real_celld_provider_prepares_from_published_envelope_and_fails_closed_on_outage() {
    let mut celld = celld_fixture::CelldFixture::new();
    for _ in 0..2 {
        let mut server = ServerFixture::new(Some(&storage_envelope(
            json!({"provider":"records","store":"trusted_server_kv"}),
        )));
        let mut binding = serde_json::to_value(celld.settings()).expect("should serialize binding");
        binding["provider"] = json!("celld-records");
        server.record_binding(binding);
        server.spawn(false, false);
        server.wait_ready();
        let store = celld.store().await;
        assert!(
            !trusted_server_core::ec::kv_backend::EcKvStore::key_exists(
                &store,
                &format!("{}.neverissued", "a".repeat(64))
            )
            .await
            .expect("should strongly read the stock celld store service")
        );
        drop(server);
        celld.restart();
    }
    let mut binding =
        serde_json::to_value(celld.settings()).expect("should serialize final binding");
    binding["provider"] = json!("celld-records");
    celld.stop();
    let mut server = ServerFixture::new(Some(&storage_envelope(
        json!({"provider":"records","store":"trusted_server_kv"}),
    )));
    server.record_binding(binding);
    server.spawn(false, false);
    server.assert_failed_before_listen("startup kv: trusted_server_kv: provider unavailable");
}

/// Local route that runs the response middleware without reaching an origin:
/// the TSJS handler answers an unknown bundle hash with `404`.
const GEO_ROUTE: &str = "/static/tsjs=0000000000000000";

struct ServerFixture {
    root: PathBuf,
    addr: SocketAddr,
    child: Option<Child>,
    command: Option<Command>,
    logs: [Arc<Mutex<Vec<u8>>>; 2],
    log_readers: Vec<thread::JoinHandle<std::io::Result<()>>>,
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

#[test]
fn geo_consent_fixture_retries_an_occupied_port() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("should occupy fixture port");
    let occupied_address = occupied.local_addr().expect("should read occupied address");
    occupied
        .set_nonblocking(true)
        .expect("should poll competing listener");
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(35);
            while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                if let Ok((mut stream, _)) = occupied.accept() {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    let mut request = [0; 1024];
                    let _ = stream.read(&mut request);
                    // A foreign healthy response must never establish this child's readiness.
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let mut server = ServerFixture::new(Some(ENVELOPE));
        server.addr = occupied_address;
        server.spawn(false, false);
        server.wait_ready();
        stop.store(true, Ordering::Relaxed);
        assert_ne!(
            server.addr, occupied_address,
            "should retry instead of accepting a foreign healthy listener"
        );
        server.assert_logs_hold_no_secrets();
    });
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
            command: None,
            logs: [
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(Mutex::new(Vec::new())),
            ],
            log_readers: Vec::new(),
        }
    }

    fn record_binding(&self, binding: serde_json::Value) {
        let mut document = json!({"version":1,"kv":{}});
        document["kv"]["trusted_server_kv"] = binding;
        fs::write(
            self.root.join("record-bindings.toml"),
            toml::to_string(&document).expect("should encode EdgeZero binding"),
        )
        .expect("should write isolated native record binding");
    }

    fn spawn(&mut self, missing_secret: bool, short_passphrase: bool) {
        self.spawn_configured(missing_secret, short_passphrase, None);
    }

    fn spawn_configured(
        &mut self,
        missing_secret: bool,
        short_passphrase: bool,
        profile: Option<&str>,
    ) {
        self.spawn_inputs(missing_secret, short_passphrase, profile, None);
    }

    fn spawn_inputs(
        &mut self,
        missing_secret: bool,
        short_passphrase: bool,
        profile: Option<&str>,
        legacy: Option<&str>,
    ) {
        for output in &self.logs {
            output
                .lock()
                .expect("should reset previous child diagnostics")
                .clear();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"));
        command
            .env_clear()
            .env("TMPDIR", std::env::temp_dir())
            .env("EDGEZERO__LOGGING__LEVEL", "info")
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", self.addr.port().to_string())
            .env("EDGEZERO__ADAPTER__CONFIG_DIR", &self.root)
            .env("EDGEZERO__ADAPTER__DATA_DIR", &self.root)
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
        let bindings = self.root.join("record-bindings.toml");
        if bindings.is_file() {
            command.env("EDGEZERO__STORE_BINDINGS_FILE", bindings);
        }
        if let Some(profile) = profile {
            let path = self.root.join("native-geo.toml");
            fs::write(&path, profile).expect("should write synthetic Geo profile");
            command.env("TRUSTED_SERVER__NATIVE_GEO_PROFILE_FILE", path);
        }
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
        if let Some(value) = legacy {
            command.env("TRUSTED_SERVER__EC_DYNAMODB_ENABLED", value);
        }
        self.child = Some(
            command
                .spawn()
                .expect("should spawn production native binary"),
        );
        self.command = Some(command);
        self.capture_child_logs();
    }

    fn capture_child_logs(&mut self) {
        let child = self.child.as_mut().expect("should own child log pipes");
        let stdout = child.stdout.take().expect("should own child stdout");
        let stderr = child.stderr.take().expect("should own child stderr");
        self.log_readers
            .push(capture_log(stdout, Arc::clone(&self.logs[0])));
        self.log_readers
            .push(capture_log(stderr, Arc::clone(&self.logs[1])));
    }

    fn diagnostics(&self) -> String {
        self.logs
            .iter()
            .map(|output| {
                String::from_utf8_lossy(&output.lock().expect("should read captured child logs"))
                    .into_owned()
            })
            .collect::<String>()
    }

    fn finish_logs(&mut self) {
        for reader in self.log_readers.drain(..) {
            let result = reader.join();
            if !thread::panicking() {
                assert!(
                    matches!(result, Ok(Ok(()))),
                    "should finish draining owned child logs: {result:?}"
                );
            }
        }
    }

    fn assert_failed_before_listen(&mut self, expected: &str) -> String {
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
                self.child
                    .take()
                    .expect("should own child output")
                    .wait()
                    .expect("should reap failed child");
                self.finish_logs();
                let diagnostic = self.diagnostics();
                assert!(
                    diagnostic.contains(expected),
                    "should explain startup failure: {diagnostic}"
                );
                assert_no_secrets(&diagnostic);
                return diagnostic;
            }
            assert!(
                Instant::now() < deadline,
                "should fail startup within the test bound"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Asserts that a caught initialization failure keeps the process
    /// listening with the failure router: liveness stays `200`, while
    /// readiness and an ordinary route answer the fixed `503`. Returns the
    /// process output for further checks.
    fn assert_serves_failure_router(&mut self, expected: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        let health = loop {
            if let Ok(response) = self.get("/health") {
                break response;
            }
            let status = self
                .child
                .as_mut()
                .expect("should own child")
                .try_wait()
                .expect("should poll production child");
            assert!(
                status.is_none(),
                "should keep running after an initialization failure, exited with {status:?}"
            );
            assert!(
                Instant::now() < deadline,
                "should listen within the test bound"
            );
            thread::sleep(Duration::from_millis(20));
        };
        assert!(
            health.starts_with("HTTP/1.1 200") && health.ends_with("\r\n\r\nok"),
            "should keep liveness up after an initialization failure: {health}"
        );
        for path in ["/ready", "/"] {
            let response = self
                .get(path)
                .expect("should answer from the failure router");
            assert!(
                response.starts_with("HTTP/1.1 503")
                    && response.ends_with("\r\n\r\nService Unavailable\n"),
                "should answer {path} with the fixed unavailable response: {response}"
            );
        }
        let mut child = self.child.take().expect("should own child");
        assert!(
            child
                .try_wait()
                .expect("should poll production child")
                .is_none(),
            "should keep serving the failure router"
        );
        child.kill().expect("should stop production child");
        child.wait().expect("should reap production child");
        self.finish_logs();
        let diagnostic = self.diagnostics();
        assert!(
            diagnostic.contains(expected),
            "should explain startup failure: {diagnostic}"
        );
        assert_no_secrets(&diagnostic);
        diagnostic
    }

    /// Waits for readiness, then returns the liveness response.
    ///
    /// Liveness stays `200` after a caught initialization failure, so only
    /// readiness shows that initialization succeeded.
    fn wait_ready(&mut self) -> String {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(30);
        let mut retries = 0;
        loop {
            let status = self
                .child
                .as_mut()
                .expect("should own child")
                .try_wait()
                .expect("should poll child");
            let diagnostic = if status.is_some() {
                self.child
                    .take()
                    .expect("should own failed child")
                    .wait()
                    .expect("should reap failed child");
                self.finish_logs();
                self.diagnostics()
            } else {
                String::new()
            };
            if status.is_some()
                && diagnostic.contains("startup bind: listener: address unavailable")
                && retries < 4
                && Instant::now() < deadline
            {
                retries += 1;
                assert_no_secrets(&diagnostic);
                // The failed child's readers have been joined above. Clear
                // its output before any reader for the next attempt can run.
                for output in &self.logs {
                    output
                        .lock()
                        .expect("should reset failed-attempt diagnostics")
                        .clear();
                }
                let reservation = reserve_port();
                self.addr = reservation.local_addr().expect("should read retry address");
                drop(reservation);
                self.child = Some(
                    self.command
                        .as_mut()
                        .expect("should retain startup command")
                        .env("EDGEZERO__ADAPTER__PORT", self.addr.port().to_string())
                        .spawn()
                        .expect("should retry port collision"),
                );
                self.capture_child_logs();
                continue;
            }
            assert!(
                status.is_none(),
                "should keep running after valid prepared-store initialization: {diagnostic}"
            );
            assert!(
                Instant::now() < deadline,
                "should become ready within the test bound: address={} attempt={} elapsed={:?}; {}",
                self.addr,
                retries + 1,
                started.elapsed(),
                self.diagnostics()
            );
            // Readiness belongs to this child, not whichever process won the port.
            if self
                .diagnostics()
                .contains("event=lifecycle phase=ready reason=ready")
                && let Ok(response) = self.get("/ready")
            {
                assert!(
                    response.starts_with("HTTP/1.1 200"),
                    "should become ready only after successful initialization: {response}"
                );
                assert!(
                    Instant::now() < deadline,
                    "should become ready within the test bound: address={} attempt={} elapsed={:?}; {}",
                    self.addr,
                    retries + 1,
                    started.elapsed(),
                    self.diagnostics()
                );
                return self
                    .get("/health")
                    .expect("should answer liveness once ready");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stops a running server and checks its whole output for secret values.
    fn assert_logs_hold_no_secrets(&mut self) {
        let mut child = self.child.take().expect("should own running child");
        child.kill().expect("should stop the running server");
        child.wait().expect("should reap running server");
        self.finish_logs();
        assert_no_secrets(&self.diagnostics());
    }

    fn get(&self, path: &str) -> std::io::Result<String> {
        self.get_with_headers(path, "")
    }

    fn get_with_headers(&self, path: &str, headers: &str) -> std::io::Result<String> {
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: publisher.example.com\r\nConnection: close\r\n{headers}\r\n"
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
        self.finish_logs();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn capture_log(
    mut reader: impl std::io::Read + Send + 'static,
    output: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        let mut buffer = [0; 4096];
        loop {
            let length = reader.read(&mut buffer)?;
            if length == 0 {
                return Ok(());
            }
            output
                .lock()
                .expect("should append captured child log")
                .extend_from_slice(&buffer[..length]);
        }
    })
}

fn storage_envelope(storage: serde_json::Value) -> String {
    let mut envelope: serde_json::Value =
        serde_json::from_str(ENVELOPE).expect("should decode fixture envelope");
    let mut data = envelope["data"].take();
    data["ec"]["storage"] = storage;
    serde_json::to_string(&BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned()))
        .expect("should encode existing envelope format")
}

#[test]
fn obsolete_ec_environment_does_not_select_or_override_declared_storage() {
    let configured = storage_envelope(json!({"provider":"records","store":"trusted_server_kv"}));
    for envelope in [ENVELOPE, configured.as_str()] {
        let mut server = ServerFixture::new(Some(envelope));
        server.spawn_inputs(false, false, None, Some("true"));
        server.wait_ready();
        server.assert_logs_hold_no_secrets();
    }
}

#[test]
fn unsupported_ec_storage_serves_the_failure_router_and_uncompiled_providers_fail_before_listen() {
    // EC storage the application rejects follows the shared startup-failure
    // contract; only provider preparation, which EdgeZero runs before the
    // initializer, stops the process before it listens.
    let mut server = ServerFixture::new(Some(&storage_envelope(
        json!({"provider":"fastly","store":"exact-binding"}),
    )));
    server.spawn(false, false);
    server.assert_serves_failure_router("unsupported by the selected serving runtime");

    let configured = storage_envelope(json!({"provider":"records","store":"trusted_server_kv"}));
    let mut server = ServerFixture::new(Some(&configured));
    server.spawn(false, false);
    server.wait_ready();
    server.assert_logs_hold_no_secrets();

    let mut server = ServerFixture::new(Some(&storage_envelope(
        json!({"provider":"records","store":"undeclared-identity"}),
    )));
    server.spawn(false, false);
    server.assert_serves_failure_router("configured EC record store is not prepared");

    for binding in [
        (!cfg!(feature="aws-dynamodb-ec")).then(|| json!({"provider":"aws-dynamodb-records","region":"us-east-1","table":"example-identity","namespace":"fixture"})),
        (!cfg!(feature="celld-ec")).then(|| json!({"provider":"celld-records","endpoint":"http://127.0.0.1:7780","store":"identity","access":{"mode":"trusted-network"}})),
    ].into_iter().flatten() {
        let mut server = ServerFixture::new(Some(&configured));
        server.record_binding(binding);
        server.spawn(false, false);
        server.assert_failed_before_listen("provider not compiled");
    }
}

#[cfg(feature = "celld-ec")]
#[test]
fn selected_celld_captures_only_its_explicit_bootstrap_reference_before_network() {
    let mut server = ServerFixture::new(Some(&storage_envelope(
        json!({"provider":"records","store":"trusted_server_kv"}),
    )));
    server.record_binding(json!({"provider":"celld-records","endpoint":"https://127.0.0.1:7780","store":"identity","access":{"mode":"authenticated","token_env":"FIXTURE_MISSING_CELLD_TOKEN"}}));
    server.spawn(false, false);
    server.assert_failed_before_listen("celld token: configured value absent");
}

#[test]
fn local_records_require_data_dir_even_without_identity_storage() {
    // ENVELOPE deliberately omits ec.storage: declarations prepare before app config loads.
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.child = Some(
        Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"))
            .env_clear()
            .env("EDGEZERO__LOGGING__LEVEL", "info")
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", server.addr.port().to_string())
            .env("EDGEZERO__ADAPTER__CONFIG_DIR", &server.root)
            .env("EDGEZERO__SECRETS__PROXY_KEY", PROXY_SECRET)
            .env("EDGEZERO__SECRETS__ADMIN_KEY", ADMIN_PASSWORD)
            .env("EDGEZERO__SECRETS__EC_KEY", EC_PASSPHRASE)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("should spawn native startup without a data directory"),
    );
    server.capture_child_logs();
    server.assert_failed_before_listen("DATA_DIR: required for declared stores");

    // The same no-identity config starts once its local provider has a writable root.
    server.spawn(false, false);
    server.wait_ready();
    server.assert_logs_hold_no_secrets();
}

#[test]
fn missing_config_fails_before_listen_without_legacy_fallback() {
    let mut server = ServerFixture::new(None);
    server.spawn(false, false);
    // Local preparation rejects the absent file before the initializer runs.
    server.assert_failed_before_listen("required snapshot missing");
}

#[test]
fn missing_secret_serves_the_failure_router_without_legacy_fallback() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(true, false);
    let diagnostic = server
        .assert_serves_failure_router("required app-config secret at `ec.passphrase` is missing");
    assert!(
        !diagnostic.contains("ec_key"),
        "should name the config path, never the reference key: {diagnostic}"
    );
}

#[test]
fn runtime_secret_validation_serves_the_failure_router() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(false, true);
    server.assert_serves_failure_router("passphrase");
}

#[test]
fn tampered_envelope_serves_the_failure_router() {
    let tampered = ENVELOPE.replace("f4dc0301", "00000000");
    let mut server = ServerFixture::new(Some(&tampered));
    server.spawn(false, false);
    server.assert_serves_failure_router("integrity verification");
}

#[test]
fn valid_prepared_inputs_serve_real_routes_and_ignore_changed_config_while_running() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(false, false);
    let health = server.wait_ready();
    assert!(
        health.to_ascii_lowercase().contains("x-ts-version:"),
        "should report the compiled version on the liveness probe: {health}"
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
    // Liveness stays up even after a failed initialization, so readiness
    // shows that the initialized state is retained.
    assert!(
        server
            .get("/ready")
            .expect("should retain initialized state")
            .starts_with("HTTP/1.1 200"),
        "should keep serving the startup snapshot after the input changes"
    );
    server.assert_logs_hold_no_secrets();
}

const GEO_PROFILE: &str = r#"version = 1
[geo]
provider = "cloudfront"
alb_peer_cidrs = ["127.0.0.1/32"]
origin_credential_header_name = "x-origin-test-removal"
"#;

#[test]
fn invalid_geo_profile_fails_before_store_preparation_and_listen() {
    let mut server = ServerFixture::new(None);
    server.spawn_configured(false, false, Some("version = 999\n[geo]\nprovider = 'off'"));
    server.assert_failed_before_listen("native Geo profile is invalid");
}

#[test]
#[cfg(not(feature = "geo-cloudfront"))]
fn compiled_out_geo_selection_fails_before_store_preparation_and_listen() {
    let mut server = ServerFixture::new(None);
    server.spawn_configured(false, false, Some(GEO_PROFILE));
    server.assert_failed_before_listen("cloudfront implementation unavailable");
}

#[test]
#[cfg(feature = "geo-cloudfront")]
fn managed_geo_is_partial_immutable_per_request_and_finalized_on_auth_errors() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn_configured(false, false, Some(GEO_PROFILE));
    server.wait_ready();
    // Probes bypass the response middleware, so Geo headers are read from
    // ordinary routes.
    let initial = server
        .get(GEO_ROUTE)
        .expect("should reach the initial managed native route");
    assert!(
        initial
            .to_ascii_lowercase()
            .contains("x-geo-info-available: false"),
        "should report unavailable Geo on the initial request without country headers"
    );
    let headers = "CloudFront-Viewer-Country: us\r\nCloudFront-Viewer-Country-Region: ca\r\nx-origin-test-removal: fixture-origin-credential\r\n";
    let probe = server
        .get_with_headers("/health", headers)
        .expect("should reach the liveness probe");
    assert!(
        probe.starts_with("HTTP/1.1 200") && !probe.to_ascii_lowercase().contains("x-geo-"),
        "should answer the liveness probe without Geo headers: {probe}"
    );
    for (path, status) in [("/_ts/admin/ec", "401"), (GEO_ROUTE, "404")] {
        let response = server
            .get_with_headers(path, headers)
            .expect("should reach managed native route");
        let lower = response.to_ascii_lowercase();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "should preserve route status: {response}"
        );
        assert!(
            lower.contains("x-geo-info-available: true"),
            "should mark managed US/CA Geo available on {path}"
        );
        assert!(
            lower.contains("x-geo-country: us"),
            "should retain the managed US country on {path}"
        );
        assert!(
            lower.contains("x-geo-region: ca"),
            "should retain the managed CA region on {path}"
        );
        for absent in [
            "x-geo-coordinates:",
            "x-geo-city:",
            "x-geo-continent:",
            "x-geo-metro-code:",
            "x-origin-test-removal",
            "fixture-origin-credential",
        ] {
            assert!(
                !lower.contains(absent),
                "should not export unknown detail or credential"
            );
        }
    }
    let gb = server
        .get_with_headers(GEO_ROUTE, "CloudFront-Viewer-Country: GB\r\n")
        .expect("should process another country");
    assert!(
        gb.to_ascii_lowercase().contains("x-geo-country: gb"),
        "should use the independent GB request's country"
    );
    assert!(
        !gb.to_ascii_lowercase().contains("x-geo-region:"),
        "should not reuse the earlier CA region for a country-only GB request"
    );
    for headers in [
        "",
        "CloudFront-Viewer-Country: UK\r\n",
        "CloudFront-Viewer-Country: US\r\nCloudFront-Viewer-Country: US\r\n",
        "CloudFront-Viewer-Country-Region: CA\r\n",
    ] {
        let response = server
            .get_with_headers(GEO_ROUTE, headers)
            .expect("should preserve the route with unavailable geography");
        assert!(
            response
                .to_ascii_lowercase()
                .contains("x-geo-info-available: false"),
            "should report unavailable Geo for missing or invalid country headers {headers:?}"
        );
        assert!(
            !response.to_ascii_lowercase().contains("x-geo-country:"),
            "should not emit a country for missing or invalid country headers {headers:?}"
        );
    }
}

#[test]
#[cfg(feature = "geo-cloudfront")]
fn admitted_body_error_keeps_managed_geo_without_losing_framework_status() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn_configured(false, false, Some(GEO_PROFILE));
    server.wait_ready();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("should create local fixture client");
    let response = client
        .post(format!("http://{}/auction", server.addr))
        .header("host", "publisher.example.com")
        .header("content-type", "application/json")
        .header("cloudfront-viewer-country", "US")
        .header("cloudfront-viewer-country-region", "CA")
        .header("x-origin-test-removal", "fixture-error-credential")
        // The existing auction API rejects bodies above 256 KiB.
        .body(vec![b' '; 256 * 1024 + 1])
        .send()
        .expect("should receive managed oversized-body response");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "should retain framework payload limit status"
    );
    assert_eq!(
        response.headers()["x-geo-info-available"],
        "true",
        "should retain available managed Geo on the oversized-body error"
    );
    assert_eq!(
        response.headers()["x-geo-country"],
        "US",
        "should retain the managed US country on the oversized-body error"
    );
    assert_eq!(
        response.headers()["x-geo-region"],
        "CA",
        "should retain the managed CA region on the oversized-body error"
    );
    assert!(
        !response.headers().contains_key("x-origin-test-removal"),
        "should omit the origin credential header from the oversized-body error response"
    );
    let body = response.text().expect("should read framework error");
    assert!(
        !body.contains("fixture-error-credential"),
        "should not expose the fixture credential value in the oversized-body error"
    );
    assert!(
        !body.contains("x-origin-test-removal"),
        "should not expose the credential header name in the oversized-body error"
    );
}

#[test]
#[cfg(feature = "geo-cloudfront")]
fn nonmatching_real_socket_peer_cannot_supply_geo() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn_configured(
        false,
        false,
        Some(&GEO_PROFILE.replace("127.0.0.1/32", "192.0.2.1/32")),
    );
    server.wait_ready();
    let response = server.get_with_headers(GEO_ROUTE, "CloudFront-Viewer-Country: US\r\nCloudFront-Viewer-Country-Region: CA\r\nX-Forwarded-For: 192.0.2.1\r\n")
        .expect("should reach the route without trusting forged peer");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("x-geo-info-available: false"),
        "should report unavailable Geo when the real socket peer does not match the protected CIDR"
    );
    assert!(
        !response.to_ascii_lowercase().contains("x-geo-country:"),
        "should not accept US country headers or a forged forwarded peer from an untrusted socket"
    );
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
    server.wait_ready();
    // Probes bypass configured response headers, so an ordinary route carries them.
    let response = server
        .get(GEO_ROUTE)
        .expect("should reach an ordinary route");
    let headers = response.to_ascii_lowercase();
    assert!(
        headers.contains("x-example: preserved"),
        "should preserve safe existing runtime headers"
    );
    assert!(
        !headers.contains("content-length: 999"),
        "should not install an operator framing value on the real response"
    );
}

#[cfg(feature = "geo-cloudfront")]
mod managed_geo_consumers {
    use super::*;

    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

    use axum::Router;
    use axum::body::to_bytes;
    use axum::extract::{Request, State};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse as _, Response};
    use edgezero_core::blob_envelope::BlobEnvelope;
    use reqwest::blocking::Client;
    use serde_json::{Value, json};
    use tokio::sync::oneshot;
    use trusted_server_core::test_support::geo_consent;

    struct CapturedRequest {
        path: String,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    struct LocalUpstream {
        url: String,
        received: Receiver<CapturedRequest>,
        stop: Option<oneshot::Sender<()>>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl LocalUpstream {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .expect("should bind local bidder and publisher capture");
            let url = format!(
                "http://{}",
                listener.local_addr().expect("should read capture address")
            );
            listener
                .set_nonblocking(true)
                .expect("should set nonblocking capture listener");
            let (sent, received) = mpsc::channel();
            let (stop, stopped) = oneshot::channel();
            let worker = thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("should build local capture runtime");
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("should adopt capture listener");
                    let router = Router::new().fallback(capture).with_state(sent);
                    tokio::select! {
                        result = axum::serve(listener, router) => {
                            result.expect("should serve local capture");
                        }
                        _ = stopped => {}
                    }
                });
            });
            Self {
                url,
                received,
                stop: Some(stop),
                worker: Some(worker),
            }
        }

        fn receive(&self) -> CapturedRequest {
            self.received
                .recv_timeout(Duration::from_secs(2))
                .expect("should capture the real outbound request within the test bound")
        }

        fn assert_no_dispatch(&self) {
            assert!(
                matches!(
                    self.received.recv_timeout(Duration::from_millis(200)),
                    Err(RecvTimeoutError::Timeout)
                ),
                "should not dispatch any bidder request after consent denial"
            );
        }
    }

    impl Drop for LocalUpstream {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(worker) = self.worker.take() {
                let result = worker.join();
                if !thread::panicking() {
                    assert!(result.is_ok(), "should stop local capture runtime");
                }
            }
        }
    }

    async fn capture(State(sent): State<Sender<CapturedRequest>>, request: Request) -> Response {
        let (parts, body) = request.into_parts();
        let body = tokio::time::timeout(Duration::from_secs(2), to_bytes(body, usize::MAX))
            .await
            .expect("should receive outbound body within the test bound")
            .expect("should read outbound body");
        let response = if parts.uri.path() == "/bid" {
            let auction: Value =
                serde_json::from_slice(&body).expect("should receive OpenRTB JSON");
            axum::Json(json!({
                "id": auction["id"],
                "cur": "USD",
                "seatbid": [{"seat": "fixture-bidder", "bid": [{
                    "id": "fixture-bid",
                    "impid": auction["imp"][0]["id"],
                    "price": 1.25,
                    "adm": "<div>local fixture bid</div>",
                    "crid": "fixture-creative",
                    "adomain": ["advertiser.example"],
                    "w": 300,
                    "h": 250
                }]}]
            }))
            .into_response()
        } else {
            "local publisher response".into_response()
        };
        sent.send(CapturedRequest {
            path: parts.uri.path().to_owned(),
            headers: parts.headers,
            body: body.to_vec(),
        })
        .expect("should record outbound request");
        response
    }

    fn envelope(upstream: &LocalUpstream) -> String {
        let original: BlobEnvelope =
            serde_json::from_str(ENVELOPE).expect("should parse existing startup envelope");
        let mut data = original.into_data();
        data["publisher"]["origin_url"] = json!(upstream.url);
        data["auction"] = json!({
            "enabled": true,
            "providers": {"local-bidder": {
                "protocol": "openrtb-2.6",
                "profile": "standard",
                "endpoint": format!("{}/bid", upstream.url),
                "routing": "all_eligible"
            }}
        });
        serde_json::to_string(&BlobEnvelope::new(data, "2026-11-12T00:00:00Z".to_owned()))
            .expect("should serialize a correctly hashed local auction envelope")
    }

    fn client() -> Client {
        Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("should create bounded local HTTP client")
    }

    fn auction(
        client: &Client,
        server: &ServerFixture,
        country: Option<&str>,
        region: Option<&str>,
    ) -> Value {
        let mut request = client
            .post(format!("http://{}/auction", server.addr))
            .header("host", "publisher.example.com")
            .header("x-origin-test-removal", "fixture-origin-credential")
            .json(&json!({"adUnits": [{
                "code": "fixture-slot",
                "mediaTypes": {"banner": {"sizes": [[300, 250]]}}
            }]}));
        if let Some(country) = country {
            request = request.header("cloudfront-viewer-country", country);
        }
        if let Some(region) = region {
            request = request.header("cloudfront-viewer-country-region", region);
        }
        let response = request.send().expect("should execute real auction route");
        let status = response.status();
        let body = response.text().expect("should read real auction response");
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "should execute auction: {body}"
        );
        serde_json::from_str(&body).expect("should return OpenRTB JSON")
    }

    fn assert_bid(response: &Value) {
        assert_eq!(
            response["seatbid"][0]["bid"][0]["id"], "fixture-bid",
            "should return the local bidder's bid after Core consent allows dispatch: {response}"
        );
    }

    fn assert_denied(response: &Value, upstream: &LocalUpstream) {
        // BidResponse omits an empty seatbid array on the wire.
        assert!(
            response.get("seatbid").is_none(),
            "should omit bids when Core consent denies dispatch: {response}"
        );
        assert_eq!(
            response["ext"]["orchestrator"]["providers"], 0,
            "should not execute any provider when consent denies dispatch: {response}"
        );
        assert_eq!(
            response["ext"]["orchestrator"]["total_bids"], 0,
            "should report no provider bids when consent denies dispatch: {response}"
        );
        upstream.assert_no_dispatch();
    }

    #[test]
    fn geo_consent_runtime_axum() {
        let upstream = LocalUpstream::start();
        let mut trusted = ServerFixture::new(Some(&envelope(&upstream)));
        trusted.spawn_configured(false, false, Some(GEO_PROFILE));
        trusted.wait_ready();
        let mut untrusted = ServerFixture::new(Some(&envelope(&upstream)));
        untrusted.spawn_configured(
            false,
            false,
            Some(&GEO_PROFILE.replace("127.0.0.1/32", "192.0.2.1/32")),
        );
        untrusted.wait_ready();
        let mut unselected = ServerFixture::new(Some(&envelope(&upstream)));
        unselected.spawn_configured(false, false, Some("version = 1\n[geo]\nprovider = 'off'"));
        unselected.wait_ready();
        let client = client();
        for &location in geo_consent::LOCATIONS.iter() {
            // A synchronous provider cannot time out. ProviderFailure is an
            // injected managed-outcome case in geo_conformance, not a runtime
            // failure a header-only CloudFront provider can produce.
            if location.source == geo_consent::Source::Error {
                continue;
            }
            let server = match location.source {
                geo_consent::Source::Untrusted => &untrusted,
                geo_consent::Source::Unselected => &unselected,
                _ => &trusted,
            };
            for signal in [
                geo_consent::Signal::None,
                geo_consent::Signal::Purpose1,
                geo_consent::Signal::Denied,
            ] {
                let mut request = client.post(format!("http://{}/auction", server.addr))
                    .header("host", "publisher.example.com")
                    .header("cookie", signal.cookie())
                    .json(&json!({"adUnits":[{"code":"example-slot","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}));
                if let Some(country) = location.country {
                    request = request.header("cloudfront-viewer-country", country);
                }
                if let Some(region) = location.region {
                    request = request.header("cloudfront-viewer-country-region", region);
                }
                let response = request.send().unwrap_or_else(|error| {
                    panic!(
                        "should send real process auction request for {} {signal:?}: {error}",
                        location.name
                    )
                });
                assert_eq!(
                    response.status(),
                    reqwest::StatusCode::OK,
                    "should accept {} {signal:?}",
                    location.name
                );
                let headers = response.headers().clone();
                let body: Value = response
                    .json()
                    .expect("should consume real process auction response");
                let located = location.policy != geo_consent::Policy::Unknown;
                assert_eq!(
                    headers["x-geo-info-available"],
                    if located { "true" } else { "false" },
                    "should expose real managed outcome for {}",
                    location.name
                );
                let allowed = signal != geo_consent::Signal::Denied
                    && (signal == geo_consent::Signal::Purpose1
                        || !matches!(
                            location.policy,
                            geo_consent::Policy::Gdpr | geo_consent::Policy::Unknown
                        ));
                if allowed {
                    assert_bid(&body);
                    let captured = upstream.receive();
                    assert_eq!(
                        captured.path, "/bid",
                        "should capture actual bidder dispatch"
                    );
                    let outgoing: Value = serde_json::from_slice(&captured.body)
                        .expect("should capture process OpenRTB");
                    let country = located.then(|| {
                        location
                            .country
                            .expect("should have located country")
                            .to_ascii_uppercase()
                    });
                    let region = located
                        .then(|| location.region.map(str::to_ascii_uppercase))
                        .flatten();
                    assert_eq!(
                        outgoing["device"]["geo"]["country"].as_str(),
                        country.as_deref(),
                        "should export real process country for {}",
                        location.name
                    );
                    assert_eq!(
                        outgoing["device"]["geo"]["region"].as_str(),
                        region.as_deref(),
                        "should export real process region for {}",
                        location.name
                    );
                    assert_eq!(
                        headers
                            .get("x-geo-country")
                            .and_then(|value| value.to_str().ok()),
                        country.as_deref(),
                        "should expose same response country"
                    );
                    assert_eq!(
                        headers
                            .get("x-geo-region")
                            .and_then(|value| value.to_str().ok()),
                        region.as_deref(),
                        "should expose same response region"
                    );
                } else {
                    assert_denied(&body, &upstream);
                }
            }
        }
        for server in [&mut trusted, &mut untrusted, &mut unselected] {
            server.assert_logs_hold_no_secrets();
        }
    }

    #[test]
    fn protected_origin_credential_is_stripped_before_publisher_forwarding() {
        let upstream = LocalUpstream::start();
        let mut server = ServerFixture::new(Some(&envelope(&upstream)));
        server.spawn_configured(false, false, Some(GEO_PROFILE));
        server.wait_ready();

        let response = client()
            .get(format!("http://{}/credential-capture.txt", server.addr))
            .header("host", "publisher.example.com")
            .header("cloudfront-viewer-country", "US")
            .header("cloudfront-viewer-country-region", "CA")
            .header(
                "X-Origin-Test-Removal",
                "fixture-publisher-origin-credential",
            )
            .header("x-publisher-capture-control", "retained")
            .send()
            .expect("should forward the request to the local publisher");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "should preserve the local publisher's response status"
        );
        assert_eq!(
            response.headers()["x-geo-country"],
            "US",
            "should process the publisher request through managed Geo ingress"
        );
        assert_eq!(
            response.text().expect("should read publisher response"),
            "local publisher response",
            "should return the captured publisher's response"
        );

        let captured = upstream.receive();
        assert_eq!(
            captured.path, "/credential-capture.txt",
            "should observe the actual outbound publisher request"
        );
        assert_eq!(
            captured.headers["x-publisher-capture-control"], "retained",
            "should preserve an ordinary request header while stripping the credential"
        );
        assert!(
            !captured.headers.contains_key("x-origin-test-removal"),
            "should strip the protected credential before publisher forwarding, not merely from the response"
        );
        assert!(
            !captured.headers.values().any(|value| value
                .as_bytes()
                .windows(b"fixture-publisher-origin-credential".len())
                .any(|window| window == b"fixture-publisher-origin-credential")),
            "should not leak the protected credential under another outbound header"
        );
        upstream.assert_no_dispatch();
    }

    #[test]
    fn protected_geo_reaches_core_consent_and_outgoing_auction_without_reuse() {
        let upstream = LocalUpstream::start();
        let mut server = ServerFixture::new(Some(&envelope(&upstream)));
        server.spawn_configured(false, false, Some(GEO_PROFILE));
        server.wait_ready();
        let client = client();

        // No consent signals are supplied. US/CA allows the auction, whereas
        // missing geography and GDPR geography must fail closed in real Core.
        assert_bid(&auction(&client, &server, Some("us"), Some("ca")));
        let captured = upstream.receive();
        assert_eq!(
            captured.path, "/bid",
            "should reach the configured local bidder"
        );
        let outgoing: Value = serde_json::from_slice(&captured.body)
            .expect("should capture the real outgoing auction JSON");
        assert_eq!(
            outgoing["device"]["geo"],
            json!({"country": "US", "region": "CA", "type": 2}),
            "should export the same normalized country/subdivision and no invented optional detail"
        );
        assert!(
            !captured.headers.contains_key("x-origin-test-removal"),
            "should not forward protected ingress credential to the bidder"
        );

        assert_denied(&auction(&client, &server, None, None), &upstream);
        assert_denied(&auction(&client, &server, Some("GB"), None), &upstream);

        // A later country-only request must neither retain CA nor require
        // consent because an earlier request was in a GDPR jurisdiction.
        assert_bid(&auction(&client, &server, Some("JP"), None));
        let captured = upstream.receive();
        let outgoing: Value = serde_json::from_slice(&captured.body)
            .expect("should capture independent country-only auction JSON");
        assert_eq!(
            outgoing["device"]["geo"],
            json!({"country": "JP", "type": 2}),
            "should not reuse the previous country's subdivision or invent missing detail"
        );
        upstream.assert_no_dispatch();

        let mut untrusted = ServerFixture::new(Some(&envelope(&upstream)));
        untrusted.spawn_configured(
            false,
            false,
            Some(&GEO_PROFILE.replace("127.0.0.1/32", "192.0.2.1/32")),
        );
        untrusted.wait_ready();
        assert_denied(
            &auction(&client, &untrusted, Some("US"), Some("CA")),
            &upstream,
        );
    }
}

#[test]
fn handwritten_store_metadata_matches_the_manifest() {
    let manifest = include_str!("../../../edgezero.toml");
    let stores = TrustedServerApp::stores();
    for (kind, metadata) in [
        ("config", stores.config),
        ("secrets", stores.secrets),
        ("kv", stores.kv),
    ] {
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
        if kind == "kv" {
            // The manifest declares every adapter's KV stores: Fastly and Spin's
            // generic runtime store and the EC record store. Axum declares only
            // the record store, so each of its KV IDs must be one the manifest
            // declares.
            let manifest_ids = quoted_values("ids");
            assert!(
                manifest_ids.contains(&default),
                "should default to a KV store the manifest declares"
            );
            assert!(
                ids.iter().all(|id| manifest_ids.contains(id)),
                "should declare only KV stores the manifest declares: {ids:?}"
            );
            continue;
        }
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
}
