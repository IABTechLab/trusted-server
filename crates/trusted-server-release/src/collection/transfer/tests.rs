use super::*;

#[test]
fn signed_download_location_rejects_unapproved_authority_and_normalization() {
    let endpoints = Endpoints::fixture(
        "https://api.example.com/repos/example/project/",
        ".blob.example.com",
        None,
    );
    for text in [
        "http://account.blob.example.com/archive.zip?sig=fictional",
        "https://account.blob.example.com:444/archive.zip",
        "https://user:secret@account.blob.example.com/archive.zip",
        "https://account.blob.example.com/archive.zip#fragment",
        "https://account.blob.example.com.evil.example.com/archive.zip",
        "https://nested.account.blob.example.com/archive.zip",
        "https://127.0.0.1/archive.zip",
        r"https://account.blob.example.com\archive.zip",
        " https://account.blob.example.com/archive.zip",
        "https://account.blob.example.com/archive.zip ",
    ] {
        assert!(
            endpoints.signed_location(text).is_err(),
            "should reject unapproved signed URL"
        );
    }
    assert!(
        endpoints
            .signed_location("https://account.blob.example.com/archive.zip?sig=fictional")
            .is_ok(),
        "should accept one authenticated storage hop within reviewed host scope"
    );
}

#[test]
fn streamed_append_rejects_a_byte_over_limit_before_growing_buffer() {
    let mut body = Vec::new();
    let mut hash = Sha256::new();
    append_chunk(&mut body, &mut hash, b"1234", 4).expect("should allow exact bound");
    assert!(
        append_chunk(&mut body, &mut hash, b"5", 4).is_err(),
        "should reject one extra byte"
    );
    assert_eq!(body, b"1234", "should not append excess bytes");
}

fn selection(archive: &[u8]) -> Selection {
    Selection {
        repository: "example/project".to_owned(),
        repository_id: 17,
        run_id: 29,
        source_commit: "a".repeat(40),
        artifacts: BTreeMap::from([(
            41,
            SelectedArtifact {
                name: "upload".to_owned(),
                sha256: hex::encode(Sha256::digest(archive)),
            },
        )]),
    }
}

fn metadata(archive: &[u8]) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({"id":41,"name":"upload","expired":false,"digest":format!("sha256:{}",hex::encode(Sha256::digest(archive))),"workflow_run":{"id":29,"repository_id":17,"head_repository_id":17,"head_sha":"a".repeat(40)}})).expect("should encode raw API fixture")
}

#[test]
fn exact_raw_metadata_correlates_repository_run_source_id_and_digest() {
    let archive = b"fictional archive";
    let expected = selection(archive);
    let raw = metadata(archive);
    expected
        .metadata(41, &raw)
        .expect("should accept exact selected metadata");
    for (pointer, replacement) in [
        ("/id", serde_json::json!(42)),
        ("/name", serde_json::json!("other")),
        ("/expired", serde_json::json!(true)),
        ("/digest", serde_json::json!("sha256:changed")),
        ("/workflow_run/id", serde_json::json!(30)),
        ("/workflow_run/head_sha", serde_json::json!("c".repeat(40))),
        ("/workflow_run/repository_id", serde_json::json!(18)),
        ("/workflow_run/head_repository_id", serde_json::json!(18)),
    ] {
        let mut value: serde_json::Value =
            serde_json::from_slice(&raw).expect("should parse fixture");
        *value.pointer_mut(pointer).expect("should select field") = replacement;
        let changed = serde_json::to_vec(&value).expect("should encode changed fixture");
        assert!(
            expected.metadata(41, &changed).is_err(),
            "should reject changed metadata identity"
        );
    }
    assert!(
        expected.metadata(41, b"{\"id\":41,\"id\":41}").is_err(),
        "should reject duplicate JSON keys"
    );
}

struct Plan {
    wire: Vec<u8>,
    stall: Duration,
}

fn response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Plan {
    let mut wire = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    )
    .into_bytes();
    for (name, value) in headers {
        wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(body);
    Plan {
        wire,
        stall: Duration::ZERO,
    }
}

struct Server {
    address: std::net::SocketAddr,
    requests: std::sync::mpsc::Receiver<String>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(plans: Vec<Plan>) -> Self {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("should bind transport fixture");
        let address = listener.local_addr().expect("should read fixture address");
        listener
            .set_nonblocking(true)
            .expect("should bound fixture accepts");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown = std::sync::Arc::clone(&stop);
        let (sender, requests) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut plans = plans.into_iter();
            while !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => return,
                };
                // Accepted sockets inherit the listener's nonblocking mode on macOS.
                stream
                    .set_nonblocking(false)
                    .expect("should read complete fixture requests in blocking mode");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("should bound fixture reads");
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("should bound fixture writes");
                let mut raw = Vec::new();
                let mut byte = [0; 1];
                while raw.len() < 16 * 1024 && !raw.ends_with(b"\r\n\r\n") {
                    match std::io::Read::read(&mut stream, &mut byte) {
                        Ok(1) => raw.push(byte[0]),
                        _ => break,
                    }
                }
                if !raw.ends_with(b"\r\n\r\n") {
                    continue;
                }
                sender
                    .send(String::from_utf8_lossy(&raw).into_owned())
                    .expect("should capture request");
                if let Some(plan) = plans.next() {
                    let _ = std::io::Write::write_all(&mut stream, &plan.wire);
                    let until = Instant::now() + plan.stall;
                    while Instant::now() < until
                        && !shutdown.load(std::sync::atomic::Ordering::SeqCst)
                    {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn request(&self) -> String {
        self.requests
            .recv_timeout(Duration::from_secs(2))
            .expect("should receive fixture request")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("should join fixture server");
        }
    }
}

#[test]
fn fixture_waits_for_fragmented_request_headers() {
    let server = Server::start(vec![response("200 OK", &[], b"complete")]);
    let mut client = std::net::TcpStream::connect(server.address)
        .expect("should connect fragmented request fixture");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("should bound fragmented response read");
    std::io::Write::write_all(&mut client, b"GET / HTTP/1.1\r\n")
        .expect("should send first header fragment");
    assert!(
        matches!(
            server.requests.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "should wait for complete headers before capturing request or consuming response"
    );
    std::io::Write::write_all(
        &mut client,
        b"Host: api.example.com\r\nAuthorization: Bearer fictional\r\n\r\n",
    )
    .expect("should send final header fragment");
    let request = server.request();
    assert!(
        request.ends_with("Authorization: Bearer fictional\r\n\r\n"),
        "should capture final header fragment: {request:?}"
    );
    let mut reply = String::new();
    std::io::Read::read_to_string(&mut client, &mut reply)
        .expect("should receive response after complete headers");
    assert!(reply.ends_with("complete"), "should preserve response plan");
}

#[test]
fn fixture_does_not_consume_response_for_aborted_headers() {
    let server = Server::start(vec![response("200 OK", &[], b"complete")]);
    let mut aborted = std::net::TcpStream::connect(server.address)
        .expect("should connect aborted request fixture");
    std::io::Write::write_all(&mut aborted, b"GET /incomplete HTTP/1.1\r\n")
        .expect("should send incomplete headers");
    aborted
        .shutdown(std::net::Shutdown::Both)
        .expect("should abort incomplete request");
    let mut client = std::net::TcpStream::connect(server.address)
        .expect("should connect complete request fixture");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("should bound complete response read");
    std::io::Write::write_all(
        &mut client,
        b"GET /complete HTTP/1.1\r\nHost: api.example.com\r\n\r\n",
    )
    .expect("should send complete request");
    assert!(
        server.request().starts_with("GET /complete "),
        "should capture complete requests only"
    );
    let mut reply = String::new();
    std::io::Read::read_to_string(&mut client, &mut reply)
        .expect("should receive unconsumed response");
    assert!(
        reply.ends_with("complete"),
        "should preserve response after aborted headers"
    );
}

fn endpoints(api: &Server, storage: Option<&Server>) -> Endpoints {
    let mut endpoints = Endpoints::fixture(
        &format!(
            "http://api.example.com:{}/repos/example/project/",
            api.address.port()
        ),
        ".blob.example.com",
        storage.map(|server| server.address.port()),
    );
    endpoints.addresses.push(("api.example.com", api.address));
    if let Some(storage) = storage {
        endpoints
            .addresses
            .push(("account.blob.example.com", storage.address));
    }
    endpoints
}

fn source(archive: &[u8], endpoints: Endpoints) -> HttpArtifactSource {
    HttpArtifactSource::new(
        selection(archive),
        "fictional-api-token",
        Instant::now() + Duration::from_secs(5),
        Cancellation::default(),
        endpoints,
    )
    .expect("should construct diagnostic test source")
}

#[test]
fn actual_transport_preserves_raw_metadata_and_separates_storage_credentials() {
    let archive = b"fictional archive";
    let raw = metadata(archive);
    let storage = Server::start(vec![response("200 OK", &[], archive)]);
    let location = format!(
        "http://account.blob.example.com:{}/archive.zip?sig=fictional-signed-query",
        storage.address.port()
    );
    let api = Server::start(vec![
        response("200 OK", &[], &raw),
        response("302 Found", &[("Location", location)], b""),
    ]);
    let mut source = source(archive, endpoints(&api, Some(&storage)));
    assert_eq!(
        source
            .metadata("example/project", 41)
            .expect("should fetch exact metadata"),
        raw,
        "should return exact original response bytes"
    );
    assert_eq!(
        source
            .download("example/project", 41)
            .expect("should download selected archive")
            .archive,
        archive,
        "should bind exact archive bytes"
    );
    for request in [api.request(), api.request()] {
        let request = request.to_ascii_lowercase();
        assert!(
            request.contains("authorization: bearer fictional-api-token"),
            "should authenticate fixed API requests only: {request:?}"
        );
        assert!(
            request.contains("accept-encoding: identity"),
            "should request original transfer bytes"
        );
    }
    let request = storage.request().to_ascii_lowercase();
    assert!(
        !request.contains("authorization:") && !request.contains("fictional-api-token"),
        "should omit GitHub credential on signed storage hop"
    );
    assert!(
        !format!("{source:?}").contains("fictional-api-token"),
        "should redact credential in source Debug"
    );
    assert!(
        source.download("example/project", 41).is_err(),
        "should require fresh checked metadata before another download"
    );
}

#[test]
fn unexpected_repository_or_artifact_id_and_missing_metadata_make_no_network_request() {
    let api = Server::start(vec![]);
    let mut source = source(b"archive", endpoints(&api, None));
    assert!(
        source.metadata("other/project", 41).is_err(),
        "should reject another repository"
    );
    assert!(
        source.metadata("example/project", 42).is_err(),
        "should reject an unselected artifact ID"
    );
    assert!(
        source.download("example/project", 41).is_err(),
        "should require exact checked metadata before download"
    );
    assert!(
        api.requests.try_recv().is_err(),
        "should not contact server for invalid selection"
    );
}

#[test]
fn transport_rejects_redirects_wrong_status_truncation_and_oversized_body_declarations() {
    let oversized = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        Role::Metadata.limit() + 1
    )
    .into_bytes();
    for plan in [
        response(
            "302 Found",
            &[("Location", "http://other.example.com/metadata".to_owned())],
            b"",
        ),
        response("410 Gone", &[], b"gone"),
        Plan {
            wire: b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\nConnection: close\r\n\r\nshort"
                .to_vec(),
            stall: Duration::ZERO,
        },
        Plan {
            wire: oversized,
            stall: Duration::ZERO,
        },
        response(
            "200 OK",
            &[("Content-Encoding", "gzip".to_owned())],
            b"encoded",
        ),
        response(
            "200 OK",
            &[
                ("Content-Encoding", "identity".to_owned()),
                ("Content-Encoding", "gzip".to_owned()),
            ],
            &metadata(b"archive"),
        ),
    ] {
        let api = Server::start(vec![plan]);
        let mut source = source(b"archive", endpoints(&api, None));
        assert!(
            source.metadata("example/project", 41).is_err(),
            "should reject unsafe or incomplete API response"
        );
        assert!(
            source.download("example/project", 41).is_err(),
            "should not authorize download after rejected metadata"
        );
    }
}

#[test]
fn transport_bounds_chunked_body_without_trusting_content_length() {
    let mut wire =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    let chunk = vec![b'x'; Role::Metadata.limit() as usize + 1];
    wire.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
    wire.extend_from_slice(&chunk);
    wire.extend_from_slice(b"\r\n0\r\n\r\n");
    let api = Server::start(vec![Plan {
        wire,
        stall: Duration::ZERO,
    }]);
    let mut source = source(b"archive", endpoints(&api, None));
    assert!(
        source.metadata("example/project", 41).is_err(),
        "should reject one byte over role bound without Content-Length"
    );
}

#[test]
fn cancellation_and_attempt_deadline_bound_header_and_body_stalls() {
    for (cancel, wire) in [
        (false, Vec::new()),
        (true, Vec::new()),
        (
            false,
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nx".to_vec(),
        ),
        (
            true,
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nx".to_vec(),
        ),
    ] {
        let api = Server::start(vec![Plan {
            wire,
            stall: Duration::from_secs(2),
        }]);
        let mut source = source(b"archive", endpoints(&api, None));
        source.deadline = Instant::now() + Duration::from_millis(if cancel { 1000 } else { 80 });
        let flag = source.cancellation.clone();
        let trigger = cancel.then(|| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                flag.cancel();
            })
        });
        let started = Instant::now();
        assert!(
            source.metadata("example/project", 41).is_err(),
            "should reject stalled response"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "should cancel or time out while headers/body are pending"
        );
        if let Some(trigger) = trigger {
            trigger.join().expect("should join cancellation trigger");
        }
    }
}

#[test]
fn storage_redirect_loops_duplicate_locations_and_digest_changes_fail_closed() {
    for mode in ["second-hop", "duplicate", "digest"] {
        let archive = b"selected archive";
        let storage = Server::start(vec![if mode == "second-hop" {
            response(
                "302 Found",
                &[(
                    "Location",
                    "http://other.example.com/archive.zip?sig=fictional-hidden-query".to_owned(),
                )],
                b"",
            )
        } else {
            response("200 OK", &[], b"changed archive")
        }]);
        let location = format!(
            "http://account.blob.example.com:{}/archive.zip?sig=fictional-hidden-query",
            storage.address.port()
        );
        let mut locations = vec![("Location", location.clone())];
        if mode == "duplicate" {
            locations.push(("Location", location));
        }
        let api = Server::start(vec![
            response("200 OK", &[], &metadata(archive)),
            response("302 Found", &locations, b""),
        ]);
        let mut source = source(archive, endpoints(&api, Some(&storage)));
        source
            .metadata("example/project", 41)
            .expect("should fetch exact metadata");
        let error = source
            .download("example/project", 41)
            .err()
            .expect("should reject unsafe or changed archive");
        let text = format!("{error:?} {source:?}");
        assert!(
            !text.contains("fictional-hidden-query") && !text.contains("fictional-api-token"),
            "should redact signed query and API credential from errors and Debug"
        );
        if mode == "duplicate" {
            assert!(
                storage.requests.try_recv().is_err(),
                "should reject ambiguous Location before storage contact"
            );
        }
    }
}

struct PendingDns {
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct DnsGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for DnsGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl reqwest::dns::Resolve for PendingDns {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let guard = DnsGuard(std::sync::Arc::clone(&self.dropped));
        Box::pin(async move {
            let _guard = guard;
            std::future::pending().await
        })
    }
}

#[test]
fn cancellation_during_dns_drops_resolution_with_the_operation_runtime() {
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut endpoints = Endpoints::fixture(
        "https://api.example.com/repos/example/project/",
        ".blob.example.com",
        None,
    );
    endpoints.resolver = Some(std::sync::Arc::new(PendingDns {
        dropped: std::sync::Arc::clone(&dropped),
    }));
    let mut source = source(b"archive", endpoints);
    let cancellation = source.cancellation.clone();
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        cancellation.cancel();
    });
    let started = Instant::now();
    assert!(
        source.metadata("example/project", 41).is_err(),
        "should cancel a pending async DNS operation"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "should bound DNS cancellation and runtime shutdown"
    );
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "should drop resolver work before returning from operation"
    );
    trigger.join().expect("should join cancellation trigger");
}

#[test]
fn source_rejects_bad_credentials_expired_attempts_and_invalid_identity_sets() {
    for token in ["", "fictional token", "fictional\ncredential"] {
        assert!(
            Credential::new(token).is_err(),
            "should reject invalid explicit credential"
        );
    }
    let api = Server::start(vec![]);
    assert!(
        HttpArtifactSource::new(
            selection(b"archive"),
            "fictional-token",
            Instant::now() - Duration::from_secs(1),
            Cancellation::default(),
            endpoints(&api, None)
        )
        .is_err(),
        "should reject expired attempt before transport"
    );
    let mut invalid = selection(b"archive");
    invalid.run_id = 0;
    assert!(
        invalid.check().is_err(),
        "should reject missing current run identity"
    );
    let cancellation = Cancellation::default();
    cancellation.cancel();
    assert!(
        HttpArtifactSource::new(
            selection(b"archive"),
            "fictional-token",
            Instant::now() + Duration::from_secs(1),
            cancellation,
            endpoints(&api, None)
        )
        .is_err(),
        "should reject cancelled attempt before transport"
    );
    assert!(
        api.requests.try_recv().is_err(),
        "should not contact API for invalid attempt"
    );
}

struct TlsEndpoint {
    address: std::net::SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TlsEndpoint {
    fn start(
        cert: rustls::pki_types::CertificateDer<'static>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> Self {
        let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("should select test TLS protocols")
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("should configure fictional TLS endpoint");
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("should bind fictional TLS endpoint");
        let address = listener.local_addr().expect("should read TLS address");
        listener
            .set_nonblocking(true)
            .expect("should bound TLS accept");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => return,
                };
                stream
                    .set_nonblocking(false)
                    .expect("should perform fixture TLS handshake in blocking mode");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("should bound TLS reads");
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("should bound TLS writes");
                let connection = rustls::ServerConnection::new(std::sync::Arc::new(config.clone()))
                    .expect("should create test TLS session");
                let mut stream = rustls::StreamOwned::new(connection, stream);
                let mut raw = Vec::new();
                let mut byte = [0; 1];
                while raw.len() < 16 * 1024 && !raw.ends_with(b"\r\n\r\n") {
                    match std::io::Read::read(&mut stream, &mut byte) {
                        Ok(1) => raw.push(byte[0]),
                        _ => break,
                    }
                }
                if raw.ends_with(b"\r\n\r\n") {
                    let plan = response("200 OK", &[], &metadata(b"archive"));
                    let _ = std::io::Write::write_all(&mut stream, &plan.wire);
                    let _ = std::io::Write::flush(&mut stream);
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for TlsEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("should join TLS endpoint");
        }
    }
}

#[test]
fn isolated_child_ignores_proxy_config_cwd_and_ambient_ca_roots() {
    let root = tempfile::tempdir().expect("should create poisoned configuration fixture");
    let certificate = rcgen::generate_simple_self_signed(vec!["api.example.com".to_owned()])
        .expect("should generate fictional CA fixture");
    let ca = root.path().join("fictional-ca.pem");
    std::fs::write(&ca, certificate.cert.pem()).expect("should write fictional CA");
    std::fs::create_dir(root.path().join("gh")).expect("should create poisoned gh config");
    std::fs::write(
        root.path().join("gh/config.yml"),
        "http_unix_socket: /fictional/socket\napi_host: other.example.com\n",
    )
    .expect("should write ignored config");
    let tls = TlsEndpoint::start(
        certificate.cert.der().clone(),
        rustls::pki_types::PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
    );
    let program = std::env::current_exe().expect("should locate test binary");
    let mut command = crate::process::BoundedCommand::new(
        &program,
        root.path(),
        crate::process::ProcessLimits {
            wall: Duration::from_secs(10),
            stdout_max: 16 * 1024,
            stderr_max: 16 * 1024,
        },
    )
    .expect("should create bounded child")
    .arguments(
        [
            "--ignored",
            "--exact",
            "collection::transfer::tests::environment_configuration_child",
            "--nocapture",
        ]
        .map(std::ffi::OsString::from),
    );
    for (name, value) in [
        ("TS_RELEASE_TRANSFER_ENV_CHILD", "1".to_owned()),
        (
            "TS_RELEASE_TRANSFER_TLS_PORT",
            tls.address.port().to_string(),
        ),
        ("HTTP_PROXY", "http://proxy.example.com:9".to_owned()),
        ("HTTPS_PROXY", "http://proxy.example.com:9".to_owned()),
        ("ALL_PROXY", "http://proxy.example.com:9".to_owned()),
        ("NO_PROXY", String::new()),
        ("SSL_CERT_FILE", ca.display().to_string()),
        ("SSL_CERT_DIR", root.path().display().to_string()),
        ("CURL_CA_BUNDLE", ca.display().to_string()),
        ("REQUESTS_CA_BUNDLE", ca.display().to_string()),
        ("GH_HOST", "other.example.com".to_owned()),
        ("GH_REPO", "other/project".to_owned()),
        (
            "GH_CONFIG_DIR",
            root.path().join("gh").display().to_string(),
        ),
        ("HOME", root.path().display().to_string()),
    ] {
        command = command
            .environment(name, value)
            .expect("should set child-only poison environment");
    }
    command
        .run()
        .expect("should supervise isolated policy test")
        .require_success()
        .expect("should reject ambient CA and ignore proxy/config channels");
}

#[test]
#[ignore = "invoked only by isolated_child_ignores_proxy_config_cwd_and_ambient_ca_roots"]
fn environment_configuration_child() {
    assert_eq!(
        std::env::var("TS_RELEASE_TRANSFER_ENV_CHILD").expect("should receive child marker"),
        "1",
        "should run only isolated environment fixture"
    );
    let api = Server::start(vec![response("200 OK", &[], &metadata(b"archive"))]);
    let mut plain = source(b"archive", endpoints(&api, None));
    plain
        .metadata("example/project", 41)
        .expect("should ignore ambient proxies and gh/config/cwd");
    let port = std::env::var("TS_RELEASE_TRANSFER_TLS_PORT")
        .expect("should receive TLS fixture port")
        .parse::<u16>()
        .expect("should parse TLS fixture port");
    let mut endpoints = Endpoints::fixture(
        &format!("https://api.example.com:{port}/repos/example/project/"),
        ".blob.example.com",
        None,
    );
    endpoints.addresses.push((
        "api.example.com",
        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
    ));
    let mut tls = source(b"archive", endpoints);
    assert!(
        tls.metadata("example/project", 41).is_err(),
        "should reject fictional TLS certificate even when ambient CA variables name it"
    );
}

#[test]
fn streaming_allocation_capacity_stays_inside_selected_byte_bound() {
    let mut bytes = Vec::new();
    let mut hash = Sha256::new();
    append_chunk(&mut bytes, &mut hash, b"1234", 4).expect("should append bounded body");
    assert!(
        bytes.capacity() <= 4,
        "should not grow allocation beyond selected body bound"
    );
}
