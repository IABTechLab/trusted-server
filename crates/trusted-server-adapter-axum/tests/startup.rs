//! Native startup contracts exercised through the real production binary.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
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

struct ServerFixture {
    root: PathBuf,
    addr: SocketAddr,
    child: Option<Child>,
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
        let listener = TcpListener::bind("127.0.0.1:0").expect("should reserve a fresh test port");
        let addr = listener.local_addr().expect("should read the test address");
        drop(listener);
        Self {
            root,
            addr,
            child: None,
        }
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"));
        command
            .env_clear()
            .env("RUST_LOG", "info")
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", self.addr.port().to_string())
            .env("EDGEZERO__ADAPTER__CONFIG_DIR", &self.root)
            .env(
                "EDGEZERO__SECRETS__PROXY_KEY",
                "native-fixture-proxy-secret-32-bytes",
            )
            .env(
                "EDGEZERO__SECRETS__ADMIN_KEY",
                "native-fixture-admin-password",
            )
            // These poisoned legacy values must never become native inputs.
            .env(
                "TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG",
                "poisoned-legacy-config",
            )
            .env(
                "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_KEY",
                "poisoned-legacy-secret",
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(profile) = profile {
            let path = self.root.join("native-geo.toml");
            fs::write(&path, profile).expect("should write synthetic Geo profile");
            command.env("TS_NATIVE_GEO_PROFILE_FILE", path);
        }
        if !missing_secret {
            command.env(
                "EDGEZERO__SECRETS__EC_KEY",
                if short_passphrase {
                    "short"
                } else {
                    "native-fixture-passphrase-32-bytes-minimum"
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
            assert!(
                TcpStream::connect_timeout(&self.addr, Duration::from_millis(20)).is_err(),
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
                assert!(
                    !diagnostic.contains("poisoned-legacy"),
                    "should not expose legacy inputs"
                );
                assert!(
                    !diagnostic.contains("native-fixture-passphrase"),
                    "should not expose resolved secrets"
                );
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
    // Local preparation itself rejects an absent required file.
    server.assert_failed_before_listen("startup");
}

#[test]
fn missing_secret_fails_before_listen_without_legacy_fallback() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(true, false);
    server.assert_failed_before_listen("required app-config secret is missing");
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
fn valid_prepared_inputs_serve_real_routes_and_ignore_changed_config_until_restart() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn(false, false);
    let health = server.wait_ready();
    assert!(health.to_ascii_lowercase().contains("x-ts-version:"));
    let tsjs = server
        .get("/static/tsjs=0000000000000000")
        .expect("should reach TSJS route");
    assert!(
        tsjs.starts_with("HTTP/1.1 404"),
        "should execute unknown-hash TSJS handler: {tsjs}"
    );
    assert!(
        tsjs.to_ascii_lowercase()
            .contains("x-geo-info-available: false")
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
            .starts_with("HTTP/1.1 200")
    );
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
    let health = server.wait_ready();
    assert!(
        health
            .to_ascii_lowercase()
            .contains("x-geo-info-available: false")
    );
    let headers = "CloudFront-Viewer-Country: us\r\nCloudFront-Viewer-Country-Region: ca\r\nx-origin-test-removal: fixture-origin-credential\r\n";
    for (path, status) in [
        ("/health", "200"),
        ("/_ts/admin/ec", "401"),
        ("/static/tsjs=0000000000000000", "404"),
    ] {
        let response = server
            .get_with_headers(path, headers)
            .expect("should reach managed native route");
        let lower = response.to_ascii_lowercase();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "should preserve route status: {response}"
        );
        assert!(lower.contains("x-geo-info-available: true"));
        assert!(lower.contains("x-geo-country: us"));
        assert!(lower.contains("x-geo-region: ca"));
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
        .get_with_headers("/health", "CloudFront-Viewer-Country: GB\r\n")
        .expect("should process another country");
    assert!(gb.to_ascii_lowercase().contains("x-geo-country: gb"));
    assert!(!gb.to_ascii_lowercase().contains("x-geo-region:"));
    for headers in [
        "",
        "CloudFront-Viewer-Country: UK\r\n",
        "CloudFront-Viewer-Country: US\r\nCloudFront-Viewer-Country: US\r\n",
        "CloudFront-Viewer-Country-Region: CA\r\n",
    ] {
        let response = server
            .get_with_headers("/health", headers)
            .expect("should preserve health with unavailable geography");
        assert!(
            response
                .to_ascii_lowercase()
                .contains("x-geo-info-available: false")
        );
        assert!(!response.to_ascii_lowercase().contains("x-geo-country:"));
    }
}

#[test]
#[cfg(feature = "geo-cloudfront")]
fn admitted_body_error_keeps_managed_geo_without_losing_framework_status() {
    let mut server = ServerFixture::new(Some(ENVELOPE));
    server.spawn_configured(false, false, Some(GEO_PROFILE));
    server.wait_ready();
    let client = reqwest::blocking::Client::builder()
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
        .body(vec![
            b' ';
            trusted_server_core::auction::endpoints::MAX_AUCTION_BODY_SIZE
                + 1
        ])
        .send()
        .expect("should receive managed oversized-body response");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "should retain framework payload limit status"
    );
    assert_eq!(response.headers()["x-geo-info-available"], "true");
    assert_eq!(response.headers()["x-geo-country"], "US");
    assert_eq!(response.headers()["x-geo-region"], "CA");
    assert!(!response.headers().contains_key("x-origin-test-removal"));
    let body = response.text().expect("should read framework error");
    assert!(!body.contains("fixture-error-credential"));
    assert!(!body.contains("x-origin-test-removal"));
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
    let response = server.get_with_headers("/health", "CloudFront-Viewer-Country: US\r\nCloudFront-Viewer-Country-Region: CA\r\nX-Forwarded-For: 192.0.2.1\r\n")
        .expect("should reach health without trusting forged peer");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("x-geo-info-available: false")
    );
    assert!(!response.to_ascii_lowercase().contains("x-geo-country:"));
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
        assert_eq!(quoted_values("default"), vec![default]);
        assert_eq!(quoted_values("ids"), ids);
    }
    assert_eq!(
        stores.config.expect("should declare config").default,
        DEFAULT_CONFIG_STORE_ID
    );
    assert_eq!(
        stores.secrets.expect("should declare secrets").default,
        DEFAULT_SECRET_STORE_ID
    );
    assert!(stores.kv.is_none(), "should not prepare unused generic KV");
    assert!(
        !manifest.contains("\n[stores.kv]"),
        "should not declare unused generic KV and accidentally require a local or remote provider"
    );
}
