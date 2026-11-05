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
use edgezero_core::blob_envelope::BlobEnvelope;
use serde_json::json;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_core::config_payload::{DEFAULT_CONFIG_STORE_ID, DEFAULT_SECRET_STORE_ID};

// The hash covers the sorted, compact data object, using the existing TS
// BlobEnvelope v1 format. All values are synthetic local fixture inputs.
const ENVELOPE: &str = r#"{"data":{"ec":{"passphrase":"ec_key"},"handlers":[{"password":"admin_key","path":"^/_ts/admin","username":"admin"}],"publisher":{"cookie_domain":".publisher.example.com","domain":"publisher.example.com","origin_url":"https://origin.publisher.example.com","proxy_secret":"proxy_key"},"request_signing":null},"sha256":"f4dc03017deb24bfd589efde7dce0f9f8a20d952c28ebcbdb6141b08661cd137","version":1,"generated_at":"2026-10-20T00:00:00Z"}"#;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "celld-ec")]
#[path = "support/celld.rs"]
mod celld_fixture;

#[cfg(feature = "celld-ec")]
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires CELLD_EC_TEST_BINARY pointing to the built real conditional-record server"]
async fn real_celld_provider_prepares_from_published_envelope_and_fails_closed_on_outage() {
    let mut celld = celld_fixture::CelldFixture::new();
    for _ in 0..2 {
        let storage =
            serde_json::to_value(celld.settings()).expect("should serialize pure celld settings");
        let mut storage = storage;
        storage["provider"] = json!("celld");
        let mut server = ServerFixture::new(Some(&storage_envelope(storage)));
        server.spawn(false, false);
        server.wait_ready();
        let store = celld.store().await;
        assert!(
            !trusted_server_core::ec::kv_backend::EcKvStore::key_exists(&store, "never-issued")
                .await
                .expect("should read strongly through prepared typed backend")
        );
        drop(server);
        celld.restart();
    }
    let mut storage =
        serde_json::to_value(celld.settings()).expect("should serialize final binding");
    storage["provider"] = json!("celld");
    celld.stop();
    let mut server = ServerFixture::new(Some(&storage_envelope(storage)));
    server.spawn(false, false);
    server.assert_failed_before_listen("native EC celld preparation failed");
}

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
        self.spawn_configured_with_env(missing_secret, short_passphrase, profile, &[]);
    }

    fn spawn_configured_with_env(
        &mut self,
        missing_secret: bool,
        short_passphrase: bool,
        profile: Option<&str>,
        environment: &[(&str, &str)],
    ) {
        self.spawn_inputs_with_env(missing_secret, short_passphrase, profile, None, environment);
    }

    fn spawn_inputs(
        &mut self,
        missing_secret: bool,
        short_passphrase: bool,
        profile: Option<&str>,
        legacy: Option<&str>,
    ) {
        self.spawn_inputs_with_env(missing_secret, short_passphrase, profile, legacy, &[]);
    }

    fn spawn_inputs_with_env(
        &mut self,
        missing_secret: bool,
        short_passphrase: bool,
        profile: Option<&str>,
        legacy: Option<&str>,
        environment: &[(&str, &str)],
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
        command.envs(environment.iter().copied());
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
        if let Some(value) = legacy {
            command.env("TS_EC_DYNAMODB_ENABLED", value);
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
                for sensitive in ["private-database-name", "invalid-private-database-content"] {
                    assert!(
                        !diagnostic.contains(sensitive),
                        "should not expose database inputs"
                    );
                }
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
        self.wait_ready_with_timeout(Duration::from_secs(10))
    }

    fn wait_ready_with_timeout(&mut self, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
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

fn storage_envelope(storage: serde_json::Value) -> String {
    let mut envelope: serde_json::Value =
        serde_json::from_str(ENVELOPE).expect("should decode fixture envelope");
    let mut data = envelope["data"].take();
    data["ec"]["storage"] = storage;
    serde_json::to_string(&BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned()))
        .expect("should encode existing envelope format")
}

#[test]
fn active_legacy_ec_environment_cannot_disable_or_override_published_storage() {
    let configured = storage_envelope(
        json!({"provider":"celld","endpoint":"http://127.0.0.1:7780","store":"identity","access":{"mode":"trusted-network"}}),
    );
    for envelope in [ENVELOPE, configured.as_str()] {
        let mut server = ServerFixture::new(Some(envelope));
        server.spawn_inputs(false, false, None, Some("true"));
        server.assert_failed_before_listen(
            "publish ec.storage and remove legacy EC environment selection",
        );
    }
}

#[test]
fn explicit_unsupported_and_uncompiled_ec_providers_fail_before_listen() {
    let cases=[
        Some((json!({"provider":"fastly","store":"exact-binding"}),"unsupported by the selected serving runtime")),
        (!cfg!(feature="aws-dynamodb-ec")).then(||(json!({"provider":"dynamodb","region":"us-east-1","table":"example-identity","namespace":"fixture"}),"aws-dynamodb-ec build feature is unavailable")),
        (!cfg!(feature="celld-ec")).then(||(json!({"provider":"celld","endpoint":"http://127.0.0.1:7780","store":"identity","access":{"mode":"trusted-network"}}),"celld-ec build feature is unavailable")),
    ];
    for (storage, expected) in cases.into_iter().flatten() {
        let mut server = ServerFixture::new(Some(&storage_envelope(storage)));
        server.spawn(false, false);
        server.assert_failed_before_listen(expected);
    }
}

#[cfg(feature = "celld-ec")]
#[test]
fn selected_celld_captures_only_its_explicit_bootstrap_reference_before_network() {
    let storage = json!({"provider":"celld","endpoint":"https://127.0.0.1:7780","store":"identity","access":{"mode":"authenticated","token_env":"FIXTURE_MISSING_CELLD_TOKEN"}});
    let mut server = ServerFixture::new(Some(&storage_envelope(storage)));
    server.spawn(false, false);
    server.assert_failed_before_listen("selected EC celld bootstrap token is unavailable");
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
#[cfg(not(all(feature = "geo-maxmind", unix)))]
fn compiled_out_maxmind_fails_before_store_preparation_and_listen() {
    let mut server = ServerFixture::new(None);
    server.spawn_configured(false, false, Some(
        "version = 1\n[geo]\nprovider = 'maxmind'\ndatabase_path = '/missing.mmdb'\nreader_ip_expectation = 'forwarded_required'"
    ));
    server.assert_failed_before_listen("maxmind implementation unavailable");
}

#[cfg(all(feature = "geo-maxmind", unix))]
mod maxmind {
    use std::path::Path;

    use super::*;

    const CITY: &[u8] = include_bytes!("fixtures/geoip/city.mmdb");
    const COUNTRY: &[u8] = include_bytes!("fixtures/geoip/country.mmdb");
    const IPV6: &[u8] = include_bytes!("fixtures/geoip/ipv6.mmdb");
    const TRUSTED_XFF: &[(&str, &str)] = &[
        (
            "EDGEZERO__ADAPTER__TRUSTED_PROXY_CIDRS",
            "127.0.0.1/32,198.51.100.8/32",
        ),
        ("EDGEZERO__ADAPTER__FORWARDING_HEADER_FAMILY", "x-forwarded"),
    ];

    fn profile(database: &Path, expectation: &str) -> String {
        let mut geo = toml::Table::new();
        geo.insert("provider".to_owned(), "maxmind".into());
        geo.insert(
            "database_path".to_owned(),
            database
                .to_str()
                .expect("should use UTF-8 test path")
                .into(),
        );
        geo.insert("reader_ip_expectation".to_owned(), expectation.into());
        let mut profile = toml::Table::new();
        profile.insert("version".to_owned(), 1.into());
        profile.insert("geo".to_owned(), geo.into());
        toml::to_string(&profile).expect("should serialize native Geo profile")
    }

    fn spawn(bytes: &[u8], expectation: &str, environment: &[(&str, &str)]) -> ServerFixture {
        let mut server = ServerFixture::new(Some(ENVELOPE));
        let path = server.root.join("test.mmdb");
        fs::write(&path, bytes).expect("should write synthetic database");
        let profile = profile(&path, expectation);
        server.spawn_configured_with_env(false, false, Some(&profile), environment);
        server.wait_ready();
        server
    }

    fn assert_location(response: &str, country: &str, region: Option<&str>) {
        let lower = response.to_ascii_lowercase();
        assert!(lower.contains("x-geo-info-available: true"), "{response}");
        assert!(
            lower.contains(&format!(
                "x-geo-country: {}\r\n",
                country.to_ascii_lowercase()
            )),
            "{response}"
        );
        if let Some(region) = region {
            assert!(
                lower.contains(&format!(
                    "x-geo-region: {}\r\n",
                    region.to_ascii_lowercase()
                )),
                "{response}"
            );
        } else {
            assert!(!lower.contains("x-geo-region:"), "{response}");
        }
        for absent in [
            "x-geo-city:",
            "x-geo-coordinates:",
            "x-geo-continent:",
            "x-geo-metro-code:",
        ] {
            assert!(
                !lower.contains(absent),
                "should not invent unavailable detail: {response}"
            );
        }
    }

    fn assert_unavailable(response: &str) {
        let lower = response.to_ascii_lowercase();
        assert!(lower.contains("x-geo-info-available: false"), "{response}");
        assert!(!lower.contains("x-geo-country:"), "{response}");
        assert!(!lower.contains("x-geo-region:"), "{response}");
    }

    #[test]
    fn direct_reader_uses_snapshot_and_ignores_geographic_and_untrusted_forwarding_headers() {
        let server = spawn(CITY, "peer_allowed", &[]);
        let headers = "CloudFront-Viewer-Country: GB\r\nCloudFront-Viewer-Country-Region: ENG\r\nX-Forwarded-For: 192.0.2.2\r\nFly-Client-IP: 192.0.2.2\r\n";
        assert_location(
            &server
                .get_with_headers("/health", headers)
                .expect("should reach health"),
            "US",
            Some("CA"),
        );
        fs::remove_file(server.root.join("test.mmdb"))
            .expect("should remove source after readiness");
        assert_location(
            &server
                .get("/health")
                .expect("should retain prepared snapshot"),
            "US",
            Some("CA"),
        );
    }

    #[test]
    fn forwarded_reader_uses_checked_chain_and_finalizes_error_routes() {
        let server = spawn(CITY, "forwarded_required", TRUSTED_XFF);
        // A deployment-controlled rightmost hop resembles proxy chains that append an app address.
        // The forged leftmost country-only reader must not override the first untrusted reader.
        let headers = "X-Forwarded-For: 192.0.2.2, 192.0.2.1, 198.51.100.8\r\nCloudFront-Viewer-Country: GB\r\n";
        for (path, status) in [
            ("/health", "200"),
            ("/_ts/admin/ec", "401"),
            ("/static/tsjs=0000000000000000", "404"),
        ] {
            let response = server
                .get_with_headers(path, headers)
                .expect("should reach managed route");
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
            assert_location(&response, "US", Some("CA"));
        }
        let country_only = server
            .get_with_headers("/health", "X-Forwarded-For: 192.0.2.2\r\n")
            .expect("should resolve a different reader");
        assert_location(&country_only, "GB", None);
        for headers in [
            "",
            "X-Forwarded-For: malformed\r\n",
            "X-Forwarded-For: 127.0.0.1\r\n",
            "CloudFront-Viewer-Country: US\r\nFly-Client-IP: 192.0.2.1\r\n",
            "X-Forwarded-For: 203.0.113.1\r\n",
        ] {
            assert_unavailable(
                &server
                    .get_with_headers("/health", headers)
                    .expect("should preserve health"),
            );
        }
    }

    #[test]
    fn forwarded_reader_requires_actual_proxy_peer_trust() {
        let server = spawn(
            CITY,
            "forwarded_required",
            &[
                ("EDGEZERO__ADAPTER__TRUSTED_PROXY_CIDRS", "198.51.100.8/32"),
                ("EDGEZERO__ADAPTER__FORWARDING_HEADER_FAMILY", "x-forwarded"),
            ],
        );
        assert_unavailable(
            &server
                .get_with_headers("/health", "X-Forwarded-For: 192.0.2.1, 198.51.100.8\r\n")
                .expect("should ignore untrusted peer"),
        );
    }

    #[test]
    fn standard_forwarded_supports_ipv6_readers() {
        let server = spawn(
            IPV6,
            "forwarded_required",
            &[
                ("EDGEZERO__ADAPTER__TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
                ("EDGEZERO__ADAPTER__FORWARDING_HEADER_FAMILY", "forwarded"),
            ],
        );
        assert_location(
            &server
                .get_with_headers(
                    "/health",
                    "Forwarded: for=\"[2001:db8::1234]\";proto=https\r\n",
                )
                .expect("should resolve IPv6 reader"),
            "GB",
            Some("ENG"),
        );
    }

    #[test]
    fn country_database_never_supplies_a_subdivision() {
        let server = spawn(COUNTRY, "peer_allowed", &[]);
        assert_location(
            &server.get("/health").expect("should use country snapshot"),
            "US",
            None,
        );
    }

    #[test]
    fn missing_or_corrupt_database_fails_before_listen() {
        for corrupt in [false, true] {
            let mut server = ServerFixture::new(Some(ENVELOPE));
            let path = server.root.join("private-database-name.mmdb");
            if corrupt {
                fs::write(&path, b"invalid-private-database-content")
                    .expect("should write invalid database");
            }
            server.spawn_configured(false, false, Some(&profile(&path, "peer_allowed")));
            server.assert_failed_before_listen(if corrupt {
                "native_geo: invalid database"
            } else {
                "native_geo: database unavailable"
            });
        }
    }

    #[test]
    #[ignore = "requires an operator GeoLite snapshot and explicit expected location"]
    fn operator_geolite_snapshot_resolves_checked_reader() {
        let database = std::env::var("TS_TEST_GEOLITE_DATABASE")
            .expect("should supply TS_TEST_GEOLITE_DATABASE");
        let reader = std::env::var("TS_TEST_GEO_IP").expect("should supply TS_TEST_GEO_IP");
        let country =
            std::env::var("TS_TEST_GEO_COUNTRY").expect("should supply TS_TEST_GEO_COUNTRY");
        let region = std::env::var("TS_TEST_GEO_REGION").ok();
        let mut server = ServerFixture::new(Some(ENVELOPE));
        server.spawn_configured_with_env(
            false,
            false,
            Some(&profile(Path::new(&database), "forwarded_required")),
            TRUSTED_XFF,
        );
        // Production-size snapshots take longer to validate than synthetic fixtures.
        server.wait_ready_with_timeout(Duration::from_secs(120));
        assert_location(
            &server
                .get_with_headers(
                    "/health",
                    &format!("X-Forwarded-For: {reader}, 198.51.100.8\r\n"),
                )
                .expect("should resolve operator snapshot"),
            &country,
            region.as_deref(),
        );
        assert_unavailable(
            &server
                .get("/health")
                .expect("should not geolocate the proxy"),
        );
    }
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
