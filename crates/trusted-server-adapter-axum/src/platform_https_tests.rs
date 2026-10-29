//! Real HTTPS regression tests for the adapter boundary after core approves a binding.
//! Only the test CA and loopback DNS are added to the production client policy.

use super::*;
use edgezero_core::body::Body;
use rcgen::{BasicConstraints, Certificate, CertificateParams, IsCa, KeyPair};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;

const CREDENTIAL: &str = "fictional-1037-approved-api-key";
const CREDENTIAL_HEADER: &str = "x-api-key";
const APPROVED_HOST: &str = "approved.example.test";
const WRONG_HOST: &str = "wrong.example.test";

#[derive(Clone, Copy, Debug)]
enum SendMode {
    Send,
    SendAsync,
}

impl SendMode {
    async fn request(
        self,
        client: &AxumPlatformHttpClient,
        url: &str,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let request = edgezero_core::http::request_builder()
            .uri(url)
            .header(CREDENTIAL_HEADER, CREDENTIAL)
            .body(Body::empty())
            .expect("should build approved credential request");
        let request = PlatformHttpRequest::new(request, "approved_backend");
        match self {
            Self::Send => client.send(request).await,
            Self::SendAsync => {
                let pending = client.send_async(request).await?;
                let selected = client.select(vec![pending]).await?;
                assert!(
                    selected.remaining.is_empty(),
                    "should finish the only request"
                );
                selected.ready
            }
        }
    }
}

const SEND_MODES: [SendMode; 2] = [SendMode::Send, SendMode::SendAsync];

struct TestCa {
    certificate: Certificate,
    key: KeyPair,
}

impl TestCa {
    fn new() -> Self {
        let key = KeyPair::generate().expect("should generate test CA key");
        let mut params =
            CertificateParams::new(Vec::<String>::new()).expect("should build test CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let certificate = params.self_signed(&key).expect("should sign test CA");
        Self { certificate, key }
    }

    fn acceptor(&self) -> TlsAcceptor {
        let key = KeyPair::generate().expect("should generate fixture key");
        let certificate = CertificateParams::new(vec![APPROVED_HOST.to_owned()])
            .expect("should build fixture certificate params")
            .signed_by(&key, &self.certificate, &self.key)
            .expect("should sign fixture certificate");
        // reqwest enables ring while the workspace rustls enables aws-lc.
        // Choose the server provider explicitly rather than changing a global default.
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("should select TLS versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .expect("should configure HTTPS fixture");
        TlsAcceptor::from(Arc::new(config))
    }
}

struct CapturedRequest {
    path: String,
    credential: Option<String>,
}

impl std::fmt::Debug for CapturedRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapturedRequest")
            .field("path", &self.path)
            .field("credential_present", &self.credential.is_some())
            .finish()
    }
}

#[derive(Debug, Default)]
struct Observations {
    connections: usize,
    tls_rejections: usize,
    requests: Vec<CapturedRequest>,
}

impl Observations {
    fn assert_approved_requests(&self, count: usize) {
        assert_eq!(self.requests.len(), count, "{self:?}");
        assert!(
            self.requests
                .iter()
                .all(|request| request.credential.as_deref() == Some(CREDENTIAL)),
            "every HTTP request should carry the exact fictional credential: {self:?}"
        );
    }
}

struct HttpsFixture {
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Observations>,
}

impl HttpsFixture {
    async fn start(ca: &TestCa, cross_origin_sink: Option<String>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind HTTPS fixture");
        let addr = listener.local_addr().expect("should read fixture address");
        let acceptor = ca.acceptor();
        let (shutdown, mut stop) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut observed = Observations::default();
            loop {
                let accepted = tokio::select! {
                    _ = &mut stop => break,
                    accepted = listener.accept() => accepted,
                };
                let (stream, _) = accepted.expect("should accept fixture connection");
                observed.connections += 1;
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    observed.tls_rejections += 1;
                    continue;
                };
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 2048];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream
                        .read(&mut chunk)
                        .await
                        .expect("should read HTTP headers");
                    assert_ne!(
                        count, 0,
                        "TLS completed but no complete HTTP request arrived"
                    );
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let headers = String::from_utf8(bytes).expect("should receive ASCII request");
                let path = headers
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .expect("should receive request path")
                    .to_owned();
                let credential = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case(CREDENTIAL_HEADER))
                    .map(|(_, value)| value.trim().to_owned());
                let mut status = 200;
                let mut location = String::new();
                if let Some(redirect) = path.strip_prefix("/redirect/") {
                    let (code, destination) = redirect
                        .split_once('/')
                        .expect("should specify redirect destination");
                    status = code.parse::<u16>().expect("should specify redirect status");
                    let target = match destination {
                        "same" => "/sink",
                        "cross" => cross_origin_sink
                            .as_deref()
                            .expect("should configure cross-origin sink"),
                        _ => unreachable!("test should use a known redirect destination"),
                    };
                    location = format!("Location: {target}\r\n");
                }
                observed.requests.push(CapturedRequest { path, credential });
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\n{location}Content-Length: 2\r\nConnection: close\r\n\r\nok"
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("should write fixture response");
                stream
                    .shutdown()
                    .await
                    .expect("should close fixture TLS stream");
            }
            observed
        });
        Self {
            addr,
            shutdown,
            task,
        }
    }

    fn url(&self, host: &str, path: &str) -> String {
        format!("https://{host}:{}{path}", self.addr.port())
    }

    async fn finish(self) -> Observations {
        self.shutdown
            .send(())
            .expect("fixture should still be running");
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("fixture should finish processing all connections")
            .expect("fixture should not panic")
    }
}

fn fixture_client(fixture: &HttpsFixture, trusted_ca: Option<&TestCa>) -> AxumPlatformHttpClient {
    let mut builder = AxumPlatformHttpClient::client_builder()
        .no_proxy()
        .resolve(APPROVED_HOST, fixture.addr)
        .resolve(WRONG_HOST, fixture.addr);
    if let Some(ca) = trusted_ca {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_der(ca.certificate.der())
                .expect("should parse test CA for reqwest"),
        );
    }
    AxumPlatformHttpClient {
        client: builder.build().expect("should build fixture client"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_approved_credential_reaches_trusted_origin() {
    let ca = TestCa::new();
    let origin = HttpsFixture::start(&ca, None).await;
    let client = fixture_client(&origin, Some(&ca));
    for mode in SEND_MODES {
        let response = mode
            .request(&client, &origin.url(APPROVED_HOST, "/approved"))
            .await
            .expect("approved HTTPS request should succeed");
        assert_eq!(response.response.status().as_u16(), 200, "{mode:?}");
    }
    let observed = origin.finish().await;
    assert_eq!(observed.connections, 2, "{observed:?}");
    assert_eq!(observed.tls_rejections, 0, "{observed:?}");
    observed.assert_approved_requests(2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_redirects_never_forward_credentials_to_same_or_cross_origin_sink() {
    let ca = TestCa::new();
    let sink = HttpsFixture::start(&ca, None).await;
    let sink_url = sink.url(APPROVED_HOST, "/sink");
    let origin = HttpsFixture::start(&ca, Some(sink_url.clone())).await;
    let origin_client = fixture_client(&origin, Some(&ca));
    let sink_client = fixture_client(&sink, Some(&ca));
    let mut responses = Vec::new();
    for mode in SEND_MODES {
        // Exercise both listeners before relying on their zero-leak counters.
        for (fixture, client) in [(&origin, &origin_client), (&sink, &sink_client)] {
            let response = mode
                .request(client, &fixture.url(APPROVED_HOST, "/approved"))
                .await
                .expect("HTTPS fixture positive control should succeed");
            assert_eq!(response.response.status().as_u16(), 200);
        }
        for status in [301_u16, 302, 303, 307, 308] {
            for target in ["same", "cross"] {
                let response = mode
                    .request(
                        &origin_client,
                        &origin.url(APPROVED_HOST, &format!("/redirect/{status}/{target}")),
                    )
                    .await
                    .expect("should surface HTTPS redirect response");
                responses.push((mode, status, target, response.response));
            }
        }
    }
    let origin_observed = origin.finish().await;
    let sink_observed = sink.finish().await;
    origin_observed.assert_approved_requests(22);
    sink_observed.assert_approved_requests(2);
    assert_eq!(origin_observed.tls_rejections, 0, "{origin_observed:?}");
    assert_eq!(sink_observed.tls_rejections, 0, "{sink_observed:?}");
    assert!(
        origin_observed
            .requests
            .iter()
            .chain(&sink_observed.requests)
            .all(|request| request.path != "/sink"),
        "redirect sinks must receive no HTTP requests, even without credentials: origin={origin_observed:?}, sink={sink_observed:?}"
    );
    for (mode, status, target, response) in responses {
        assert_eq!(response.status().as_u16(), status, "{mode:?} {target}");
        let expected = if target == "same" {
            "/sink".to_owned()
        } else {
            sink_url.clone()
        };
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some(expected.as_str()),
            "{mode:?} {status} {target}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_untrusted_certificate_rejects_before_http_credential_arrives() {
    let ca = TestCa::new();
    let origin = HttpsFixture::start(&ca, None).await;
    let trusted = fixture_client(&origin, Some(&ca));
    let untrusted = fixture_client(&origin, None);
    for mode in SEND_MODES {
        let response = mode
            .request(&trusted, &origin.url(APPROVED_HOST, "/approved"))
            .await
            .expect("same certificate should work with the test CA");
        assert_eq!(response.response.status().as_u16(), 200);
        assert!(
            mode.request(&untrusted, &origin.url(APPROVED_HOST, "/rejected"))
                .await
                .is_err(),
            "{mode:?} must reject the untrusted test CA"
        );
    }
    let observed = origin.finish().await;
    assert_eq!(observed.connections, 4, "{observed:?}");
    assert_eq!(observed.tls_rejections, 2, "{observed:?}");
    observed.assert_approved_requests(2);
    assert!(
        observed
            .requests
            .iter()
            .all(|request| request.path == "/approved"),
        "{observed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_hostname_mismatch_rejects_before_http_credential_arrives() {
    let ca = TestCa::new();
    let origin = HttpsFixture::start(&ca, None).await;
    let client = fixture_client(&origin, Some(&ca));
    for mode in SEND_MODES {
        let response = mode
            .request(&client, &origin.url(APPROVED_HOST, "/approved"))
            .await
            .expect("matching hostname should accept the trusted certificate");
        assert_eq!(response.response.status().as_u16(), 200);
        assert!(
            mode.request(&client, &origin.url(WRONG_HOST, "/rejected"))
                .await
                .is_err(),
            "{mode:?} must reject the wrong hostname even with a trusted CA"
        );
    }
    let observed = origin.finish().await;
    assert_eq!(observed.connections, 4, "{observed:?}");
    assert_eq!(observed.tls_rejections, 2, "{observed:?}");
    observed.assert_approved_requests(2);
    assert!(
        observed
            .requests
            .iter()
            .all(|request| request.path == "/approved"),
        "{observed:?}"
    );
}
