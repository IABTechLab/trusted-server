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

struct ServerFixture {
    root: PathBuf,
    addr: SocketAddr,
    child: Option<Child>,
}

fn reserve_port() -> tokio::net::TcpSocket {
    // A listening reservation can survive another test's concurrent spawn and
    // satisfy a negative startup probe before that child reaches exec.
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
            .contains("x-geo-info-available: false"),
        "should report unavailable Geo on the initial health request without country headers"
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
        .get_with_headers("/health", "CloudFront-Viewer-Country: GB\r\n")
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
            .get_with_headers("/health", headers)
            .expect("should preserve health with unavailable geography");
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
    let response = server.get_with_headers("/health", "CloudFront-Viewer-Country: US\r\nCloudFront-Viewer-Country-Region: CA\r\nX-Forwarded-For: 192.0.2.1\r\n")
        .expect("should reach health without trusting forged peer");
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
                worker.join().expect("should stop local capture runtime");
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
    // The portable manifest may declare KV for other adapters. Native startup
    // selects only its handwritten config/secret metadata above.
}
