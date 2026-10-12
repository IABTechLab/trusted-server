//! Exercises the proxy against the production Axum listener and trace router.
//!
//! A child test process owns the real HTTP server. A TLS relay additionally
//! exercises the proxy's encrypted upstream transport without inventing ingress
//! metadata or replacing the server's trace authorization with an echo handler.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::convert::Infallible;
use std::io::Cursor;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use clap::Parser as _;
use edgezero_adapter_axum::dev_server::{AxumDevServer, AxumDevServerConfig};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt as _, Full};
use hyper::client::conn::http1::SendRequest;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_cli::commands::dev::proxy::{ProxyArgs, ca, config};
use trusted_server_core::settings::Settings;

mod support;

const PUBLIC_HOST: &str = "www.publisher.example.com";
const PUBLIC_AUTHORITY: &str = "www.publisher.example.com:8443";
const PUBLIC_ORIGIN: &str = "https://www.publisher.example.com:8443";
const FIXTURE_ADDR: &str = "TS_TEST_AXUM_ADDR";
const FIXTURE_ORIGIN: &str = "TS_TEST_AXUM_ORIGIN";
const FIXTURE_TRUST: &str = "TS_TEST_AXUM_TRUST";

/// Run only as a fixture child; the parent always kills and reaps this process.
#[test]
#[ignore = "server fixture invoked only by proxy integration tests"]
fn trusted_server_fixture() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let addr: SocketAddr = std::env::var(FIXTURE_ADDR)
        .expect("should supply the fixture listener address")
        .parse()
        .expect("should parse the fixture listener address");
    let origin = std::env::var(FIXTURE_ORIGIN).expect("should supply the publisher origin");
    let trust = std::env::var(FIXTURE_TRUST).expect("should supply the forwarder policy");
    let mut value = json!({
        "handlers": [{
            "path": "^/_ts/admin",
            "username": "example-admin",
            "password": "fictional-admin-password-0123456789"
        }],
        "publisher": {
            "domain": "publisher.example.com",
            "cookie_domain": ".publisher.example.com",
            "origin_url": origin,
            "origin_host_header_override": "origin.example.com",
            "proxy_secret": "fictional-proxy-signing-secret-0123456789"
        },
        "ec": {"passphrase": "fictional-ec-passphrase-0123456789"},
        "integrations": {
            "gpt_diagnostics": {"enabled": true, "trace_page_enabled": true}
        }
    });
    if trust == "enabled" {
        value["trusted_forwarder"] = json!({
            "auth_header": "x-ts-forwarder-auth",
            "shared_secret": support::FORWARDER_TOKEN
        });
    }
    let settings = Settings::from_json_value(value).expect("should load fixture settings");
    let router = TrustedServerApp::routes_with_settings(settings)
        .expect("should build the actual Trusted Server router");
    AxumDevServer::with_config(
        router,
        AxumDevServerConfig {
            addr,
            enable_ctrl_c: false,
        },
    )
    .run()
    .expect("should run the actual Axum HTTP listener");
}

struct Fixture {
    addr: SocketAddr,
    child: Child,
    origin_headers: Arc<Mutex<Vec<HeaderMap>>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start_fixture(trust: bool) -> Fixture {
    let origin = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind the fictional publisher origin");
    let origin_url = format!(
        "http://{}",
        origin.local_addr().expect("should read the origin address")
    );
    let headers = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&headers);
    let html =
        format!("<html><head></head><body><a href=\"{origin_url}/next\">next</a></body></html>");
    let origin_task = tokio::spawn(async move {
        while let Ok((stream, _)) = origin.accept().await {
            let recorded = Arc::clone(&recorded);
            let html = html.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                    recorded
                        .lock()
                        .expect("should record publisher-bound headers")
                        .push(request.headers().clone());
                    let html = html.clone();
                    async move {
                        Ok::<_, Infallible>(
                            Response::builder()
                                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                                .body(Full::new(Bytes::from(html)))
                                .expect("should return publisher HTML"),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    let reserved = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("should reserve the Axum fixture address");
    let addr = reserved.local_addr().expect("should read the Axum address");
    drop(reserved);
    let child = Command::new(std::env::current_exe().expect("should locate the test executable"))
        .args([
            "--ignored",
            "--exact",
            "trusted_server_fixture",
            "--nocapture",
        ])
        .env(FIXTURE_ADDR, addr.to_string())
        .env(FIXTURE_ORIGIN, origin_url)
        .env(FIXTURE_TRUST, if trust { "enabled" } else { "disabled" })
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("should spawn the real Axum fixture");
    let mut fixture = Fixture {
        addr,
        child,
        origin_headers: headers,
        tasks: vec![origin_task],
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if TcpStream::connect(addr).await.is_ok() {
                return;
            }
            assert!(
                fixture
                    .child
                    .try_wait()
                    .expect("should inspect the child")
                    .is_none(),
                "should keep the Axum fixture running until ready"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("should start the actual Axum listener promptly");
    fixture
}

async fn add_tls_relay(fixture: &mut Fixture) -> SocketAddr {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind the upstream TLS relay");
    let addr = listener.local_addr().expect("should read the TLS address");
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .expect("should generate the fictional relay certificate");
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certificate.cert.der().to_vec())],
            PrivateKeyDer::try_from(certificate.key_pair.serialize_der())
                .expect("should encode the relay key"),
        )
        .expect("should configure upstream TLS");
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let target = fixture.addr;
    fixture.tasks.push(tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let mut encrypted = acceptor
                    .accept(stream)
                    .await
                    .expect("should accept proxy TLS");
                let mut plain = TcpStream::connect(target)
                    .await
                    .expect("should connect TLS relay to the actual listener");
                let _ = tokio::io::copy_bidirectional(&mut encrypted, &mut plain).await;
            });
        }
    }));
    addr
}

async fn browser(
    fixture: &mut Fixture,
    plaintext: bool,
    rewrite_host: bool,
    credential: bool,
) -> SendRequest<Full<Bytes>> {
    let addr = if plaintext {
        fixture.addr
    } else {
        add_tls_relay(fixture).await
    };
    let token_directory = tempfile::tempdir().expect("should create the proxy token directory");
    let token_path = token_directory.path().join("forwarder-token");
    std::fs::write(&token_path, support::FORWARDER_TOKEN)
        .expect("should write the fictional token");
    let mapping = format!("{PUBLIC_HOST}={addr}");
    let mut arguments = vec![
        "ts",
        "--map",
        &mapping,
        "--listen",
        "127.0.0.1:0",
        "--insecure",
        "--forwarder-secret-file",
        token_path.to_str().expect("should encode the token path"),
    ];
    if plaintext {
        arguments.push("--upstream-plaintext");
    }
    if rewrite_host {
        arguments.push("--rewrite-host");
    }
    let parsed = ProxyArguments::try_parse_from(arguments).expect("should parse proxy arguments");
    let mut configuration =
        config::resolve(&parsed.args).expect("should resolve the real proxy configuration");
    if !credential {
        configuration.forwarder_secret = None;
    }
    connect_browser(configuration).await
}

#[derive(clap::Parser)]
struct ProxyArguments {
    #[command(flatten)]
    args: ProxyArgs,
}

async fn connect_browser(configuration: config::ResolvedConfig) -> SendRequest<Full<Bytes>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let directory = tempfile::tempdir().expect("should create the browser CA directory");
    let ca = Arc::new(
        ca::CertAuthority::load_or_generate(directory.path())
            .expect("should generate the browser CA"),
    );
    let pem = std::fs::read(ca::CertAuthority::cert_path(directory.path()))
        .expect("should read the browser CA");
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut Cursor::new(pem)) {
        roots
            .add(certificate.expect("should decode the browser CA"))
            .expect("should trust the browser CA");
    }
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let proxy = support::spawn_proxy(configuration, ca).await;
    let mut tcp = TcpStream::connect(proxy)
        .await
        .expect("should connect the browser to the proxy");
    tcp.write_all(
        format!("CONNECT {PUBLIC_AUTHORITY} HTTP/1.1\r\nHost: {PUBLIC_AUTHORITY}\r\n\r\n")
            .as_bytes(),
    )
    .await
    .expect("should request the actual browser tunnel");
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(
            head.len() < 8192,
            "should bound the CONNECT response headers"
        );
        head.push(
            tcp.read_u8()
                .await
                .expect("should read the CONNECT response"),
        );
    }
    assert!(
        head.starts_with(b"HTTP/1.1 200 "),
        "should establish the mapped browser tunnel"
    );
    let tls = TlsConnector::from(Arc::new(tls_config))
        .connect(
            ServerName::try_from(PUBLIC_HOST.to_owned())
                .expect("should parse the browser TLS name"),
            tcp,
        )
        .await
        .expect("should authenticate the proxy leaf using the generated CA");
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .expect("should establish a real browser HTTP connection through the proxy");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
}

async fn send(
    browser: &mut SendRequest<Full<Bytes>>,
    request: Request<Full<Bytes>>,
) -> (StatusCode, HeaderMap, Bytes) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let response = browser
            .send_request(request)
            .await
            .expect("should receive the actual Trusted Server response");
        let (parts, body) = response.into_parts();
        let body = body
            .collect()
            .await
            .expect("should collect the response")
            .to_bytes();
        (parts.status, parts.headers, body)
    })
    .await
    .expect("should complete the proxied response promptly")
}

fn action(path: &str, origin: &str) -> Request<Full<Bytes>> {
    let action = if path.starts_with("/_ts/trace/end") {
        "end"
    } else {
        "enable"
    };
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::HOST, PUBLIC_AUTHORITY)
        .header(header::ORIGIN, origin)
        .header("sec-fetch-site", "same-origin")
        .header("x-ts-trace-action", action)
        .body(Full::new(Bytes::new()))
        .expect("should build a deliberate trace action")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_proxy_reaches_real_trace_actions_for_every_upstream_mode() {
    for plaintext in [false, true] {
        for rewrite_host in [false, true] {
            let mut fixture = start_fixture(true).await;
            let mut browser = browser(&mut fixture, plaintext, rewrite_host, true).await;
            for (action_name, lifetime) in [("enable", "Max-Age=1800"), ("end", "Max-Age=0")] {
                let request = action(&format!("/_ts/trace/{action_name}"), PUBLIC_ORIGIN);
                let (status, headers, body) = send(&mut browser, request).await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "should accept {action_name} with plaintext={plaintext}, rewrite_host={rewrite_host}"
                );
                let cookie = headers
                    .get(header::SET_COOKIE)
                    .expect("should mutate the diagnostics cookie")
                    .to_str()
                    .expect("should encode the diagnostics cookie");
                assert!(
                    cookie.contains(lifetime),
                    "should apply {action_name} lifetime: {cookie}"
                );
                assert!(
                    cookie.contains("Secure") && cookie.contains("SameSite=Lax"),
                    "should preserve the browser cookie policy: {cookie}"
                );
                assert!(
                    !cookie.contains("Domain="),
                    "should keep diagnostics cookies host-only"
                );
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&body)
                        .expect("should decode the real action response"),
                    json!({"mutation_requested": true}),
                    "should execute the actual action handler"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_trace_authorization_rejects_foreign_origin_queries_and_missing_trust() {
    let mut fixture = start_fixture(true).await;
    let mut authenticated = browser(&mut fixture, true, true, true).await;
    for (path, origin) in [
        ("/_ts/trace/enable", "https://foreign.example.com"),
        ("/_ts/trace/enable?source=test", PUBLIC_ORIGIN),
        ("/_ts/trace/end?", PUBLIC_ORIGIN),
    ] {
        let (status, headers, _) = send(&mut authenticated, action(path, origin)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "should reject browser controls for {path} from {origin}"
        );
        assert!(
            !headers.contains_key(header::SET_COOKIE),
            "should never mutate rejected cookies"
        );
    }
    let mut duplicated = action("/_ts/trace/enable", PUBLIC_ORIGIN);
    duplicated
        .headers_mut()
        .append(header::ORIGIN, HeaderValue::from_static(PUBLIC_ORIGIN));
    let (status, headers, _) = send(&mut authenticated, duplicated).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "should preserve duplicate browser Origin fields so the server rejects ambiguity"
    );
    assert!(
        !headers.contains_key(header::SET_COOKIE),
        "should never mutate cookies for ambiguous Origin"
    );
    let mut unauthenticated = browser(&mut fixture, true, true, false).await;
    let (status, headers, _) = send(
        &mut unauthenticated,
        action("/_ts/trace/enable", PUBLIC_ORIGIN),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "should require proxy credentials for the browser origin"
    );
    assert!(
        !headers.contains_key(header::SET_COOKIE),
        "should reject unauthenticated mutations"
    );
    let mut default_fixture = start_fixture(false).await;
    let mut untrusted = browser(&mut default_fixture, true, true, true).await;
    let (status, headers, _) =
        send(&mut untrusted, action("/_ts/trace/enable", PUBLIC_ORIGIN)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "should require explicit server opt-in even with a valid proxy token"
    );
    assert!(
        !headers.contains_key(header::SET_COOKIE),
        "should keep default policy fail-closed"
    );
    let transport_origin = format!("http://{}", default_fixture.addr);
    let (status, _, _) = send(
        &mut untrusted,
        action("/_ts/trace/enable", &transport_origin),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "should retain the default transport-origin authorization when forwarding is disabled"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_publisher_rewrites_public_port_and_strips_forwarding_credentials() {
    let mut fixture = start_fixture(true).await;
    let mut browser = browser(&mut fixture, true, true, true).await;
    let request = Request::builder()
        .uri("/article")
        .header(header::HOST, PUBLIC_AUTHORITY)
        .header(header::ORIGIN, PUBLIC_ORIGIN)
        .header(header::ACCEPT, "text/html")
        .header("sec-fetch-dest", "document")
        .body(Full::new(Bytes::new()))
        .expect("should build a publisher navigation");
    let (status, _, body) = send(&mut browser, request).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "should fetch the configured publisher origin"
    );
    let html = std::str::from_utf8(&body).expect("should decode publisher HTML");
    assert!(
        html.contains(&format!("{PUBLIC_ORIGIN}/next")),
        "should preserve the real browser authority and port: {html}"
    );
    {
        let requests = fixture
            .origin_headers
            .lock()
            .expect("should inspect publisher requests");
        assert_eq!(
            requests.len(),
            1,
            "should send exactly one publisher request"
        );
        assert_eq!(
            requests[0].get(header::HOST).and_then(|v| v.to_str().ok()),
            Some("origin.example.com"),
            "should retain the configured publisher backend Host"
        );
        assert_eq!(
            requests[0]
                .get(header::ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some(PUBLIC_ORIGIN),
            "should preserve the browser Origin on publisher-bound requests"
        );
        for name in [
            "x-ts-forwarder-auth",
            "x-forwarded-host",
            "x-forwarded-proto",
        ] {
            assert!(
                !requests[0].contains_key(name),
                "should remove {name} before ordinary outbound forwarding"
            );
        }
    }
    let body = json!({"url": "//cdn.example.com/asset.js"}).to_string();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/first-party/sign")
        .header(header::HOST, PUBLIC_AUTHORITY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("should build a protocol-relative signing request");
    let (status, _, body) = send(&mut browser, request).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "should sign a first-party proxy URL"
    );
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("should decode signed URL metadata");
    assert_eq!(
        value["base"], "https://cdn.example.com/asset.js",
        "should use public HTTPS for protocol-relative signing over plaintext ingress"
    );
}
