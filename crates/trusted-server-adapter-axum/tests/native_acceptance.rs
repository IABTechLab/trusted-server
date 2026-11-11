//! Subprocess acceptance for strict native startup and managed TS requests.

#[path = "fixtures/native/origin.rs"]
mod origin;
#[path = "fixtures/native/proxy.rs"]
mod proxy;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use edgezero_core::blob_envelope::BlobEnvelope;
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};
use tempfile::TempDir;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::{
    CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID, VerifiedConfigBlob,
};
use trusted_server_core::test_support::nextjs_auction;

const ADMIN_PASSWORD: &str = "native-admin-password-32-bytes-example";
const PROXY_SECRET: &str = "native-proxy-secret-32-bytes-example";
const EC_PASSPHRASE: &str = "native-ec-passphrase-32-bytes-example";

struct Prepared {
    workspace: TempDir,
    data: Value,
    secrets: BTreeMap<String, String>,
}

impl Prepared {
    fn new(origin: &str) -> Self {
        // Reuse core's typed Next.js fixture and the production BlobEnvelope.
        // Only the existing TS shape is customized, never another config format.
        let mut settings = nextjs_auction::settings();
        settings.publisher.domain = "publisher.example.com".to_owned();
        settings.publisher.cookie_domain = ".publisher.example.com".to_owned();
        settings.publisher.origin_url = origin.to_owned();
        settings.auction.enabled = false;
        settings.auction.providers.clear();
        settings.creative_opportunities = None;
        let config =
            TrustedServerAppConfig::new(settings).expect("should validate fixture settings");
        let mut data = serde_json::to_value(config).expect("should serialize typed TS config");
        data["handlers"][0]["password"] = json!("native_admin");
        let mut protected_handler = data["handlers"][0].clone();
        protected_handler["path"] = json!("^/protected");
        data["handlers"]
            .as_array_mut()
            .expect("should have fixture handlers")
            .push(protected_handler);
        data["publisher"]["proxy_secret"] = json!("native_proxy");
        data["ec"]["passphrase"] = json!("native_ec");
        let fixture = Self {
            workspace: tempfile::tempdir().expect("should create isolated prepared-store root"),
            data,
            secrets: BTreeMap::from([
                ("native_admin".to_owned(), ADMIN_PASSWORD.to_owned()),
                ("native_proxy".to_owned(), PROXY_SECRET.to_owned()),
                ("native_ec".to_owned(), EC_PASSPHRASE.to_owned()),
            ]),
        };
        let verified = VerifiedConfigBlob::parse(&fixture.envelope())
            .expect("should verify production envelope");
        assert_eq!(
            verified
                .required_secret_keys()
                .expect("should read active references"),
            fixture.secrets.keys().cloned().collect::<BTreeSet<_>>(),
            "should prepare exactly active fixture secrets"
        );
        fixture.publish();
        fixture
    }

    fn envelope(&self) -> String {
        serde_json::to_string(&BlobEnvelope::new(
            self.data.clone(),
            "2026-10-20T00:00:00Z".to_owned(),
        ))
        .expect("should serialize production envelope")
    }

    fn config_path(&self) -> PathBuf {
        self.workspace
            .path()
            .join(format!("local-config-{DEFAULT_CONFIG_STORE_ID}.json"))
    }

    fn publish(&self) {
        fs::write(
            self.config_path(),
            serde_json::to_vec(&json!({ CONFIG_BLOB_KEY: self.envelope() }))
                .expect("should serialize local store map"),
        )
        .expect("should publish local prepared config");
    }

    fn spawn(&self, trusted_proxy: bool) -> Running {
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("should reserve isolated port")
            .local_addr()
            .expect("should read reserved port")
            .port();
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let root = self
            .workspace
            .path()
            .canonicalize()
            .expect("should use absolute store roots");
        let log_path = root.join(format!("native-{port}.log"));
        let log = File::create(&log_path).expect("should create child log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"));
        command
            .env_clear()
            .current_dir(&root)
            .env("RUST_LOG", "info")
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", port.to_string())
            .env("EDGEZERO__ADAPTER__CONFIG_DIR", &root)
            .env("EDGEZERO__ADAPTER__DATA_DIR", &root)
            .env("EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS", "1")
            // A valid old bridge cannot rescue missing prepared inputs. An
            // invalid settings override cannot poison a valid prepared input.
            .env(
                "TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG",
                self.envelope(),
            )
            .env(
                "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_NATIVE_ADMIN",
                ADMIN_PASSWORD,
            )
            .env(
                "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_NATIVE_PROXY",
                PROXY_SECRET,
            )
            .env(
                "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_NATIVE_EC",
                EC_PASSPHRASE,
            )
            .env(
                "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL",
                "http://poison.example.com",
            )
            .env("TRUSTED_SERVER__EC__PASSPHRASE", "poison")
            .stdout(Stdio::from(
                log.try_clone().expect("should clone child log"),
            ))
            .stderr(Stdio::from(log));
        for (key, value) in &self.secrets {
            command.env(
                format!("EDGEZERO__SECRETS__{}", key.to_ascii_uppercase()),
                value,
            );
        }
        if trusted_proxy {
            command
                .env("EDGEZERO__ADAPTER__TRUSTED_PROXY_CIDRS", "127.0.0.1/32")
                .env("EDGEZERO__ADAPTER__FORWARDING_HEADER_FAMILY", "forwarded");
        }
        Running {
            child: command
                .spawn()
                .expect("should launch real native TS binary"),
            address,
            log_path,
        }
    }
}

struct Running {
    child: Child,
    address: SocketAddr,
    log_path: PathBuf,
}

impl Running {
    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    fn ready(&mut self) {
        let client = client(None);
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            assert!(
                self.child
                    .try_wait()
                    .expect("should poll child exit")
                    .is_none(),
                "should not exit before readiness: {}",
                self.log()
            );
            if let Ok(response) = client.get(format!("{}/health", self.base_url())).send()
                && response.status().as_u16() == 200
                && response.text().is_ok_and(|body| body == "ok")
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            Instant::now() < deadline,
            "should reach exact HTTP 200 health body ok: {}",
            self.log()
        );
    }

    fn rejected(&mut self, diagnostic: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            assert!(
                TcpStream::connect_timeout(&self.address, Duration::from_millis(30)).is_err(),
                "should fail before opening listener: {}",
                self.log()
            );
            if let Some(status) = self.child.try_wait().expect("should poll rejected child") {
                assert!(!status.success(), "should exit nonzero for invalid startup");
                let log = self.log();
                assert!(
                    log.contains(diagnostic),
                    "should report {diagnostic}: {log}"
                );
                for secret in [ADMIN_PASSWORD, PROXY_SECRET, EC_PASSPHRASE] {
                    assert!(!log.contains(secret), "should not log secret values");
                }
                return;
            }
            assert!(
                Instant::now() < deadline,
                "should reject startup within deadline: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        log::error!(
            "native child did not exit within bounded teardown: {}",
            self.log()
        );
    }
}

fn client(source: Option<IpAddr>) -> Client {
    Client::builder()
        .no_proxy()
        .local_address(source)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .expect("should build bounded local client")
}

fn successful_body(response: Response) -> String {
    assert_eq!(
        response.status().as_u16(),
        200,
        "should serve real TS request"
    );
    response.text().expect("should collect TS response")
}

fn page(client: &Client, base_url: &str) -> String {
    successful_body(
        client
            .get(format!("{base_url}/"))
            .send()
            .expect("should fetch TS publisher page"),
    )
}

fn assert_rewrite(body: &str, public_origin: &str, origin: &str) {
    assert!(
        body.contains(&format!("href=\"{public_origin}/asset\"")),
        "should rewrite using request-local checked public URL: {body}"
    );
    assert!(
        !body.contains(&format!("href=\"{origin}/asset\"")),
        "should not leak origin href"
    );
}

#[test]
fn prepared_envelope_serves_real_health_tsjs_and_rewritten_page() {
    let origin = origin::Origin::start("native-page");
    let fixture = Prepared::new(&origin.base_url);
    let mut app = fixture.spawn(false);
    app.ready();
    let client = client(None);
    let body = page(&client, &app.base_url());
    assert!(
        body.contains("native-page"),
        "should reach actual configured origin"
    );
    assert!(
        body.contains("self.__next_f.push"),
        "should retain core Next.js fixture Flight data"
    );
    assert_rewrite(&body, &app.base_url(), &origin.base_url);
    let start = body
        .find("/static/tsjs=")
        .expect("should inject real TSJS URL");
    let end = body[start..]
        .find('"')
        .expect("should terminate TSJS source attribute");
    let script = body[start..start + end].replace("&amp;", "&");
    let response = client
        .get(format!("{}{script}", app.base_url()))
        .send()
        .expect("should fetch injected TSJS URL");
    assert!(
        response
            .headers()
            .get("content-type")
            .expect("should serve script content type")
            .to_str()
            .expect("should read content type")
            .contains("javascript")
    );
    assert!(
        successful_body(response).len() > 100,
        "should serve embedded bundle, not an empty router"
    );
}

#[test]
fn missing_prepared_inputs_and_invalid_runtime_config_exit_before_listen() {
    let origin = origin::Origin::start("unused");
    let fixture = Prepared::new(&origin.base_url);
    fs::remove_file(fixture.config_path()).expect("should remove required config snapshot");
    fixture.spawn(false).rejected("required snapshot missing");
    fs::write(fixture.config_path(), "{}").expect("should publish empty config map");
    fixture.spawn(false).rejected("config envelope is missing");
    fixture.publish();
    for key in ["native_admin", "native_proxy", "native_ec"] {
        let mut incomplete = Prepared::new(&origin.base_url);
        incomplete.secrets.remove(key);
        incomplete
            .spawn(false)
            .rejected("a required app-config secret is missing");
    }
    let mut invalid = Prepared::new(&origin.base_url);
    invalid.data["publisher"]["origin_url"] = json!("not-a-valid-url");
    invalid.publish();
    invalid
        .spawn(false)
        .rejected("Trusted Server initialization failed");
    let mut invalid = Prepared::new(&origin.base_url);
    invalid
        .secrets
        .insert("native_ec".to_owned(), "short".to_owned());
    invalid
        .spawn(false)
        .rejected("Trusted Server initialization failed");
}

#[test]
fn absolute_request_target_cannot_override_checked_public_scheme() {
    let origin = origin::Origin::start("checked-target");
    let fixture = Prepared::new(&origin.base_url);
    let mut app = fixture.spawn(false);
    app.ready();
    let mut socket =
        TcpStream::connect(app.address).expect("should connect directly to native ingress");
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("should bound the response read");
    socket.write_all(b"GET https://publisher.example.com/first-party/sign?url=%2F%2Fads.example.com%2Fcreative HTTP/1.1\r\nHost: publisher.example.com\r\nX-Forwarded-Proto: https\r\nConnection: close\r\n\r\n")
        .expect("should submit an absolute request target with a spoofed scheme");
    let mut response = String::new();
    socket
        .take(64 * 1024)
        .read_to_string(&mut response)
        .expect("should read a bounded native response");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "should execute the local signing route: {response}"
    );
    assert!(
        response.contains("http://ads.example.com"),
        "should use checked direct HTTP, including URI-based consumers: {response}"
    );
    assert!(
        !response.contains("https://ads.example.com"),
        "should not adopt raw target or forwarding scheme"
    );
}

#[test]
fn managed_post_bodies_reach_buffered_api_handlers_and_keep_their_limits() {
    let origin = origin::Origin::start("buffered-api");
    let fixture = Prepared::new(&origin.base_url);
    let mut app = fixture.spawn(false);
    app.ready();
    let client = client(None);
    let auction = client
        .post(format!("{}/auction", app.base_url()))
        .json(&json!({"adUnits": []}))
        .send()
        .expect("should submit a lazy managed auction body");
    assert_eq!(auction.status().as_u16(), 200);
    let auction: Value = auction
        .json()
        .expect("should receive a real OpenRTB no-bid response");
    assert!(
        auction
            .get("seatbid")
            .is_none_or(|bids| bids.as_array().is_some_and(Vec::is_empty))
    );
    let signed = client
        .post(format!("{}/first-party/sign", app.base_url()))
        .json(&json!({"url": "https://ads.example.com/creative"}))
        .send()
        .expect("should submit first-party signing JSON");
    assert_eq!(
        signed.status().as_u16(),
        200,
        "should buffer the lazy request before the existing synchronous parser"
    );
    let verified = client
        .post(format!("{}/verify-signature", app.base_url()))
        .json(&json!({"payload": "fixture", "signature": "invalid", "kid": "fixture"}))
        .send()
        .expect("should submit verification JSON");
    assert_eq!(
        verified.status().as_u16(),
        200,
        "missing key is a verification result, not a discarded-body parse failure"
    );
    let oversized = client
        .post(format!("{}/verify-signature", app.base_url()))
        .json(&json!({"payload": "x".repeat(4096), "signature": "invalid", "kid": "fixture"}))
        .send()
        .expect("should reject the bounded API upload");
    assert_eq!(oversized.status().as_u16(), 413);
}

#[test]
fn running_snapshot_is_immutable_and_restart_adopts_config_and_secret() {
    let first = origin::Origin::start("snapshot-first");
    let second = origin::Origin::start("snapshot-second");
    let mut fixture = Prepared::new(&first.base_url);
    let mut app = fixture.spawn(false);
    app.ready();
    let client = client(None);
    assert!(page(&client, &app.base_url()).contains("snapshot-first"));
    fixture.data["publisher"]["origin_url"] = json!(second.base_url);
    fixture.secrets.insert(
        "native_admin".to_owned(),
        "replacement-admin-password-32-bytes-example".to_owned(),
    );
    fixture.publish();
    assert!(
        page(&client, &app.base_url()).contains("snapshot-first"),
        "should not reload edited config under a running app"
    );
    let protected = format!("{}/protected", app.base_url());
    assert_eq!(
        client
            .get(&protected)
            .basic_auth("admin", Some(ADMIN_PASSWORD))
            .send()
            .expect("should authorize old snapshot password")
            .status()
            .as_u16(),
        200
    );
    assert_eq!(
        client
            .get(&protected)
            .basic_auth("admin", fixture.secrets.get("native_admin"))
            .send()
            .expect("should reject unpublished child secret change")
            .status()
            .as_u16(),
        401
    );
    drop(app);
    let mut app = fixture.spawn(false);
    app.ready();
    assert!(
        page(&client, &app.base_url()).contains("snapshot-second"),
        "should adopt config on restart"
    );
    let protected = format!("{}/protected", app.base_url());
    assert_eq!(
        client
            .get(&protected)
            .basic_auth("admin", Some(ADMIN_PASSWORD))
            .send()
            .expect("should reject superseded password")
            .status()
            .as_u16(),
        401
    );
    assert_eq!(
        client
            .get(&protected)
            .basic_auth("admin", fixture.secrets.get("native_admin"))
            .send()
            .expect("should authorize restarted secret")
            .status()
            .as_u16(),
        200
    );
}

#[test]
fn managed_ingress_ignores_direct_spoof_and_isolates_two_gateway_clients() {
    let origin = origin::Origin::start("ingress-page");
    let fixture = Prepared::new(&origin.base_url);
    let mut app = fixture.spawn(false);
    app.ready();
    let visitor = client(None);
    let response = visitor
        .get(app.base_url())
        .header(
            "forwarded",
            "for=192.0.2.99;host=spoof.example.com;proto=https",
        )
        .header("x-forwarded-host", "spoof.example.com")
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "192.0.2.99")
        .send()
        .expect("should send untrusted forwarding assertions");
    let body = successful_body(response);
    assert_rewrite(&body, &app.base_url(), &origin.base_url);
    assert!(!body.contains("https://spoof.example.com/asset"));
    drop(app);

    let mut app = fixture.spawn(true);
    app.ready();
    // Only the gateways connect from the declared trusted 127.0.0.1 address.
    // Visitors use distinct source addresses and independent client pools.
    let alpha_client = client(Some(
        "127.0.0.2".parse().expect("should parse alpha source"),
    ));
    let beta_client = client(Some("127.0.0.3".parse().expect("should parse beta source")));
    let direct = alpha_client
        .get(app.base_url())
        .header(
            "forwarded",
            "for=192.0.2.99;host=spoof.example.com;proto=https",
        )
        .send()
        .expect("should bypass gateway from an untrusted peer");
    assert_rewrite(&successful_body(direct), &app.base_url(), &origin.base_url);
    let alpha = proxy::Proxy::start(&app.base_url(), "alpha.example.com", "192.0.2.10");
    let beta = proxy::Proxy::start(&app.base_url(), "beta.example.com", "192.0.2.20");
    std::thread::scope(|scope| {
        let alpha_request = scope.spawn(|| page(&alpha_client, &alpha.base_url));
        let beta_request = scope.spawn(|| page(&beta_client, &beta.base_url));
        let alpha_body = alpha_request.join().expect("should complete alpha request");
        let beta_body = beta_request.join().expect("should complete beta request");
        assert_rewrite(&alpha_body, "https://alpha.example.com", &origin.base_url);
        assert_rewrite(&beta_body, "https://beta.example.com", &origin.base_url);
        assert!(!alpha_body.contains("https://beta.example.com/asset"));
        assert!(!beta_body.contains("https://alpha.example.com/asset"));
    });
    assert_rewrite(
        &page(&alpha_client, &alpha.base_url),
        "https://alpha.example.com",
        &origin.base_url,
    );
}

fn truncate_inbound_post(app: &Running) {
    let mut socket = TcpStream::connect_timeout(&app.address, Duration::from_secs(1))
        .expect("should connect partial POST");
    socket
        .set_write_timeout(Some(Duration::from_secs(1)))
        .expect("should bound partial write");
    write!(socket,
        "POST /action HTTP/1.1\r\nHost: {}\r\nNext-Action: native-action\r\nContent-Type: text/plain\r\nContent-Length: 128\r\n\r\n[",
        app.address,
    ).expect("should write deliberately incomplete POST");
    // Closing without the declared bytes reproduces an aborted upload.
    drop(socket);
}

#[test]
fn truncated_post_then_action_and_api_keep_using_the_same_live_origin() {
    let origin = origin::Origin::start("transport-sequence");
    let fixture = Prepared::new(&origin.base_url);
    let mut app = fixture.spawn(false);
    app.ready();
    let client = client(None);
    truncate_inbound_post(&app);
    let broken = client
        .post(format!("{}/truncate-response", app.base_url()))
        .body("[]")
        .send();
    match broken {
        Err(_) => {}
        Ok(response) if response.status().is_server_error() => {}
        Ok(response) => assert!(
            response.bytes().is_err(),
            "should expose truncated origin response"
        ),
    }
    for _ in 0..3 {
        let response = client
            .post(format!("{}/action", app.base_url()))
            .header("next-action", "native-action")
            .header("content-type", "text/plain;charset=UTF-8")
            .body("[\"synthetic-action\"]")
            .send()
            .expect("should forward action after truncated POST");
        assert_eq!(successful_body(response), "0:{\"accepted\":true}\n");
        let body = successful_body(
            client
                .get(format!("{}/api/data", app.base_url()))
                .send()
                .expect("should forward API after action"),
        );
        assert_eq!(
            serde_json::from_str::<Value>(&body).expect("should retain API JSON"),
            json!({"native": "transport-sequence"})
        );
    }
    let received = origin
        .received
        .lock()
        .expect("should read origin observations");
    assert_eq!(
        received
            .iter()
            .filter(|request| request.path == "/action"
                && request.body == b"[\"synthetic-action\"]"
                && request.next_action.as_deref() == Some("native-action"))
            .count(),
        3,
        "should deliver three complete action bodies despite preceding truncated POST"
    );
}

/// The ordinary origin above checks wire contracts, not a real Next.js runtime.
/// Run this separately against the real server-action fixture described in
/// tests/fixtures/native/README.md. Missing external setup is never a pass.
#[test]
#[ignore = "requires the local real Next.js fixture and NATIVE_NEXTJS_ORIGIN"]
fn real_nextjs_action_and_api_after_truncated_post() {
    let origin = std::env::var("NATIVE_NEXTJS_ORIGIN")
        .expect("should explicitly supply local real Next.js origin");
    let url = reqwest::Url::parse(&origin).expect("should parse local Next.js URL");
    assert_eq!(
        url.scheme(),
        "http",
        "should use account-free local HTTP fixture"
    );
    assert!(
        matches!(url.host_str(), Some("127.0.0.1" | "localhost")),
        "should never contact external origin"
    );
    let fixture = Prepared::new(&origin);
    let mut app = fixture.spawn(false);
    app.ready();
    let client = client(None);
    // The fixture instructions prewarm Next's compiler before this test.
    let body = page(&client, &app.base_url());
    let marker = "$ACTION_ID_";
    let start = body
        .find(marker)
        .expect("should find actual compiled server-action form")
        + marker.len();
    let end = body[start..]
        .find('"')
        .expect("should terminate compiled action identifier");
    let action = &body[start..start + end];
    assert!(
        !action.is_empty(),
        "should extract nonempty real server-action ID"
    );
    truncate_inbound_post(&app);
    let boundary = "native-acceptance-boundary";
    let form = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"$ACTION_ID_{action}\"\r\n\r\n\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"message\"\r\n\r\nnative-acceptance\r\n--{boundary}--\r\n"
    );
    let response = client
        .post(app.base_url())
        .header("origin", "http://publisher.example.com")
        .header("host", "publisher.example.com")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(form)
        .send()
        .expect("should invoke real Next.js server action after truncated upload");
    assert_eq!(
        response.status().as_u16(),
        303,
        "should execute real action redirect, not accept unknown-ID 404"
    );
    assert!(
        response
            .headers()
            .get("location")
            .expect("should redirect from action")
            .to_str()
            .expect("should read action redirect")
            .contains("action=accepted")
    );
    let body = successful_body(
        client
            .get(format!("{}/api/data", app.base_url()))
            .send()
            .expect("should reach real Next.js API after real action"),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&body).expect("should parse real API response"),
        json!({"native": "real-nextjs-api"})
    );
}
