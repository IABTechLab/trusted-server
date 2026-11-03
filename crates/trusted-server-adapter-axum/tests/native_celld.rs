//! Real native application proof against an externally prepared disposable celld.
//!
//! Run the ignored test with `TS_TEST_CELLD_ENDPOINT`, `TS_TEST_CELLD_CA`,
//! `TS_TEST_CELLD_RUNTIME_TOKEN` and `TS_TEST_CELLD_OPERATOR_TOKEN`. The service
//! policy must grant operator config writes and runtime config reads on
//! ts-config, plus runtime read/write/delete/list on ts-state.

#![cfg(feature = "lab-store-check")]

use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use edgezero_core::app::Hooks as _;
use edgezero_core::blob_envelope::BlobEnvelope;
use reqwest::blocking::Client;
use reqwest::{Certificate, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::kv::STATE_STORE_ID;
use trusted_server_adapter_axum::lab_store_check::{PATH, PROOF_KEY};

const ADMIN_PASSWORD: &str = "disposable-native-admin-password";

struct NativeApp {
    child: Child,
    address: SocketAddr,
    log: PathBuf,
}

impl NativeApp {
    fn start(root: &Path, binding: &str, ca: Option<&Path>, token: &str, secret: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("should reserve test port");
        let address = listener.local_addr().expect("should obtain test address");
        drop(listener);
        let binding_path = root.join("bindings.toml");
        fs::write(&binding_path, binding).expect("should write native bindings");
        let log = root.join("child.log");
        let output = fs::File::create(&log).expect("should create child diagnostic log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"));
        command
            .env_clear()
            .current_dir(root)
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", address.port().to_string())
            .env("EDGEZERO__STORE_BINDINGS_FILE", binding_path)
            .env("TS_LAB_CELLD_TOKEN", token)
            .env(
                "EDGEZERO__SECRETS__PUBLISHER_PROXY_SECRET",
                "disposable-proxy-secret-32-bytes-minimum",
            )
            .env("EDGEZERO__SECRETS__HANDLER_PASSWORD", ADMIN_PASSWORD)
            .stdout(Stdio::from(
                output.try_clone().expect("should clone log handle"),
            ))
            .stderr(Stdio::from(output));
        if secret {
            command.env(
                "EDGEZERO__SECRETS__EC_PASSPHRASE",
                "disposable-ec-passphrase-32-bytes-minimum",
            );
        }
        if let Some(ca) = ca {
            command.env("SSL_CERT_FILE", ca);
            // Isolate the proof from host trust directories.
            command.env("SSL_CERT_DIR", root.join("empty-trust"));
        }
        Self {
            child: command
                .spawn()
                .expect("should start actual native application"),
            address,
            log,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn ready(&mut self, client: &Client) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(response) = client.get(self.url("/health")).send() {
                assert_eq!(response.status(), StatusCode::OK);
                return;
            }
            assert!(
                self.child.try_wait().expect("should poll child").is_none(),
                "should remain running until ready: {}",
                fs::read_to_string(&self.log).expect("should read child diagnostics")
            );
            assert!(Instant::now() < deadline, "should become ready");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn fails(&mut self, client: &Client) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                client.get(self.url("/health")).send().is_err(),
                "should not become ready after failed selection"
            );
            if let Some(status) = self.child.try_wait().expect("should poll failed startup") {
                assert!(!status.success());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "should reject invalid selected input"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn proof(
        &self,
        client: &Client,
        method: reqwest::Method,
        body: Option<Value>,
    ) -> reqwest::blocking::Response {
        let mut request = client
            .request(method, self.url(PATH))
            .header("host", "lab.example.com")
            .basic_auth("admin", Some(ADMIN_PASSWORD));
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
            .send()
            .expect("should call actual protected diagnostic")
    }
}

impl Drop for NativeApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn binding(endpoint: &str, document: &str, state: &str) -> String {
    format!(
        r#"
version = 1
[config.trusted_server_config]
provider = "celld-config"
endpoint = "{endpoint}"
default_key = "trusted_server_config"
[config.trusted_server_config.access]
mode = "authenticated"
token_env = "TS_LAB_CELLD_TOKEN"
[config.trusted_server_config.documents.trusted_server_config]
store = "ts-config"
key = "{document}"
[kv.trusted_server_state]
provider = "celld-kv"
endpoint = "{endpoint}"
store = "{state}"
[kv.trusted_server_state.access]
mode = "authenticated"
token_env = "TS_LAB_CELLD_TOKEN"
"#
    )
}

fn envelope(origin: &str) -> BlobEnvelope {
    BlobEnvelope::new(
        json!({
            "auction": { "enabled": false },
            "ec": { "passphrase": "ec_passphrase" },
            "handlers": [{ "path": "^/_ts/admin", "username": "admin", "password": "handler_password" }],
            "publisher": {
                "domain": "lab.example.com", "cookie_domain": ".lab.example.com",
                "origin_url": origin, "proxy_secret": "publisher_proxy_secret"
            },
            "request_signing": null
        }),
        "2026-11-03T00:00:00Z".to_owned(),
    )
}

#[test]
fn actual_metadata_declares_native_state() {
    let stores = TrustedServerApp::stores();
    let kv = stores.kv.expect("should declare lab KV");
    assert_eq!(kv.default, STATE_STORE_ID);
    assert_eq!(kv.ids, &[STATE_STORE_ID]);
    let manifest: toml::Value = toml::from_str(include_str!("../../../edgezero.lab.toml"))
        .expect("should parse the committed lab CLI manifest");
    assert_eq!(
        manifest["stores"]["kv"]["default"].as_str(),
        Some(kv.default)
    );
    assert_eq!(manifest["stores"]["kv"]["ids"][0].as_str(), Some(kv.ids[0]));
}

#[test]
fn selected_lab_cannot_omit_explicit_bindings() {
    let root = TempDir::new().expect("should create isolated root");
    let output = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"))
        .env_clear()
        .current_dir(root.path())
        .output()
        .expect("should run selected native app");
    assert!(!output.status.success());
    let diagnostics = String::from_utf8_lossy(&output.stdout);
    assert!(diagnostics.contains("local fallback is disabled"));
    assert!(!root.path().join(".edgezero").exists());
}

#[test]
#[ignore = "requires a disposable native TLS scoped celld service and dummy tokens"]
fn actual_tls_config_adoption_and_runtime_kv() {
    let endpoint =
        std::env::var("TS_TEST_CELLD_ENDPOINT").expect("should supply disposable TLS endpoint");
    let ca = std::env::var("TS_TEST_CELLD_CA").expect("should supply disposable CA");
    let runtime =
        std::env::var("TS_TEST_CELLD_RUNTIME_TOKEN").expect("should supply dummy runtime token");
    let operator =
        std::env::var("TS_TEST_CELLD_OPERATOR_TOKEN").expect("should supply dummy operator token");
    let root = TempDir::new().expect("should create isolated native app directory");
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .add_root_certificate(
            Certificate::from_pem(&fs::read(&ca).expect("should read public CA"))
                .expect("should parse CA"),
        )
        .build()
        .expect("should construct independent publisher client");
    let publish = |key: &str, bytes: Vec<u8>| {
        let response = client
            .put(format!("{endpoint}/v1/stores/ts-config/value"))
            .query(&[("key", key)])
            .bearer_auth(&operator)
            .body(bytes.clone())
            .send()
            .expect("should publish disposable full envelope");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let read = client
            .get(format!("{endpoint}/v1/stores/ts-config/value"))
            .query(&[("key", key)])
            .bearer_auth(&operator)
            .send()
            .expect("should read published envelope");
        assert_eq!(read.status(), StatusCode::OK);
        assert_eq!(
            read.bytes().expect("should read exact envelope").as_ref(),
            bytes.as_slice()
        );
    };
    let a = envelope("https://origin.publisher.example.com");
    publish(
        "config-a",
        serde_json::to_vec(&a).expect("should serialize full envelope"),
    );
    let valid_binding = binding(&endpoint, "config-a", "ts-state");

    // SSL_CERT_FILE is the only CA input to the production provider. No custom
    // reqwest client or test-only provider seam is injected into the child.
    let mut app = NativeApp::start(
        root.path(),
        &valid_binding,
        Some(Path::new(&ca)),
        &runtime,
        true,
    );
    app.ready(&client);
    assert_eq!(
        client
            .get(app.url(PATH))
            .send()
            .expect("should require admin")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let spoof = client
        .get(app.url(PATH))
        .header("host", "lab.example.com")
        .header("forwarded", "host=attacker.example;proto=https")
        .header("x-forwarded-host", "attacker.example")
        .header("x-forwarded-proto", "https")
        .basic_auth("admin", Some(ADMIN_PASSWORD))
        .send()
        .expect("should inspect checked native ingress");
    assert_eq!(spoof.status(), StatusCode::OK);
    let spoof: Value = spoof.json().expect("should decode checked ingress facts");
    assert_eq!(spoof["scheme"], "http");
    assert_eq!(spoof["host"], "lab.example.com");
    assert_eq!(
        app.proof(
            &client,
            reqwest::Method::POST,
            Some(json!({"value":"not-written"}))
        )
        .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    let result = app.proof(
        &client,
        reqwest::Method::PUT,
        Some(json!({ "value": "native-retained-proof" })),
    );
    assert_eq!(result.status(), StatusCode::OK);
    let result: Value = result.json().expect("should decode proof response");
    assert_eq!(result["config_sha256"], a.sha256);
    assert_eq!(result["value"], "native-retained-proof");
    assert_eq!(result["scheme"], "http");
    assert_eq!(result["host"], "lab.example.com");
    let independent = client
        .get(format!("{endpoint}/v1/stores/ts-state/value"))
        .query(&[("key", PROOF_KEY)])
        .bearer_auth(&runtime)
        .send()
        .expect("should independently observe the TS-written key");
    assert_eq!(independent.status(), StatusCode::OK);
    assert_eq!(
        independent.text().expect("should read TS-written proof"),
        "native-retained-proof"
    );

    let b = envelope("https://other.publisher.example.com");
    publish(
        "config-b",
        serde_json::to_vec(&b).expect("should serialize next config"),
    );
    let still_a: Value = app
        .proof(&client, reqwest::Method::GET, None)
        .json()
        .expect("should retain adopted config A");
    assert_eq!(still_a["config_sha256"], a.sha256);
    drop(app);

    let mut app = NativeApp::start(
        root.path(),
        &binding(&endpoint, "config-b", "ts-state"),
        Some(Path::new(&ca)),
        &runtime,
        true,
    );
    app.ready(&client);
    let retained: Value = app
        .proof(&client, reqwest::Method::GET, None)
        .json()
        .expect("should read without writing after restart");
    assert_eq!(retained["value"], "native-retained-proof");
    assert_eq!(retained["config_sha256"], b.sha256);
    let deleted = app.proof(&client, reqwest::Method::DELETE, None);
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(deleted.json::<Value>().expect("should verify delete")["value"].is_null());
    drop(app);

    // Config remains readable and health green while live writes to the
    // read-only config alias are forbidden. Never redirect to local redb.
    let mut denied = NativeApp::start(
        root.path(),
        &binding(&endpoint, "config-a", "ts-config"),
        Some(Path::new(&ca)),
        &runtime,
        true,
    );
    denied.ready(&client);
    assert_eq!(
        denied
            .proof(
                &client,
                reqwest::Method::PUT,
                Some(json!({"value":"denied"}))
            )
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(denied);

    let mut tampered = a.clone();
    tampered.sha256 = "ff".repeat(32);
    publish(
        "bad-config",
        serde_json::to_vec(&tampered).expect("should serialize tampered envelope"),
    );
    for (document, token, secret) in [
        ("missing-config", runtime.as_str(), true),
        ("bad-config", runtime.as_str(), true),
        (
            "config-a",
            "invalid-disposable-token-32-bytes-minimum",
            true,
        ),
        ("config-a", runtime.as_str(), false),
    ] {
        let mut invalid = NativeApp::start(
            root.path(),
            &binding(&endpoint, document, "ts-state"),
            Some(Path::new(&ca)),
            token,
            secret,
        );
        invalid.fails(&client);
    }
    // A valid certificate with no trusted root is still rejected.
    let mut untrusted = NativeApp::start(root.path(), &valid_binding, None, &runtime, true);
    untrusted.fails(&client);
    assert!(
        !root.path().join(".edgezero").exists(),
        "should not create native local stores"
    );
    assert!(
        !fs::read_dir(root.path())
            .expect("should inspect isolated root")
            .any(|entry| entry
                .expect("should inspect entry")
                .path()
                .extension()
                .is_some_and(|ext| ext == "redb"))
    );
}
