//! Isolated actual-runtime probes at the SDK-visible trace boundary.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use http::{HeaderMap, HeaderValue};
use testcontainers::runners::SyncRunner as _;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

use crate::common::config::integration_app_config_envelope_with_trace;
use crate::common::runtime::TestError;
use crate::common::runtime::{
    RuntimeEnvironment as _, RuntimeProcess, RuntimeProcessHandle, origin_port, wasm_binary_path,
};
use crate::environments::{
    axum::AxumDevServer, cloudflare::CloudflareWorkers, fastly::FastlyViceroy,
};
use crate::frameworks::{FrontendFramework as _, nextjs::NextJs};

/// Actual runtime selected by an ignored boundary test.
#[derive(Clone, Copy)]
pub(crate) enum TraceRuntime {
    /// Local Cloudflare Workers via Wrangler/workerd.
    Cloudflare,
    /// Local Fastly Compute via Viceroy.
    Fastly,
    /// Local Spin HTTP component.
    Spin,
}

/// Real runtime selected by the normal browser workflow.
#[derive(Clone, Copy)]
pub(crate) enum TraceBrowserRuntime {
    /// Native Axum dev-server binary.
    Axum,
    /// Local Cloudflare Workers via Wrangler/workerd.
    Cloudflare,
    /// Local Fastly Compute via Viceroy.
    Fastly,
    /// Local Spin HTTP component.
    Spin,
}

impl TraceBrowserRuntime {
    fn id(self) -> &'static str {
        match self {
            Self::Axum => "axum",
            Self::Cloudflare => "cloudflare",
            Self::Fastly => "fastly",
            Self::Spin => "spin",
        }
    }
}

struct IsolatedFixture(PathBuf);

impl IsolatedFixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("should read fixture clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("ts-trace-boundary-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&path).expect("should create isolated runtime fixture");
        Self(path)
    }
}

impl Drop for IsolatedFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ChildHandle {
    child: Child,
    #[cfg(unix)]
    process_group: bool,
}
impl RuntimeProcessHandle for ChildHandle {}
impl Drop for ChildHandle {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.process_group {
            // Give Playwright's signal handlers time to close its browsers
            // before escalating the owned Node and worker process group.
            unsafe {
                libc::killpg(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                let group_exists = unsafe { libc::killpg(self.child.id() as libc::pid_t, 0) == 0 };
                if !group_exists {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            unsafe {
                libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run raw and normal requests against a separately configured runtime instance.
///
/// These probes distinguish successful SDK conversion from runtime rejection.
///
/// # Examples
///
/// ```ignore
/// exercise(TraceRuntime::Fastly);
/// ```
pub(crate) fn exercise(runtime: TraceRuntime) {
    exercise_case(runtime, true, None, false);
    for enabled in [false, true] {
        for pattern in [None, Some("^/"), Some("^/_ts")] {
            exercise_case(runtime, enabled, pattern, true);
        }
    }
}

fn exercise_case(runtime: TraceRuntime, enabled: bool, auth_pattern: Option<&str>, matrix: bool) {
    let envelope = integration_app_config_envelope_with_trace(8888, enabled)
        .expect("should generate isolated trace config");
    let envelope = if let Some(pattern) = auth_pattern {
        let mut envelope: BlobEnvelope =
            serde_json::from_str(&envelope).expect("should parse fixture envelope");
        envelope.data["handlers"].as_array_mut().expect("should retain handler array").insert(0,serde_json::json!({"path":pattern,"username":"example-user","password":"integration_admin_password"}));
        let envelope = BlobEnvelope::new(envelope.data, envelope.generated_at);
        envelope
            .verify()
            .expect("should rebuild valid fixture envelope integrity");
        serde_json::to_string(&envelope).expect("should serialize trace auth fixture")
    } else {
        envelope
    };
    let check = |base_url: &str| {
        if matrix {
            check_policy_matrix(base_url, &runtime, enabled, auth_pattern);
        } else {
            check_boundary(base_url, &runtime);
        }
    };
    with_trace_runtime(runtime, &envelope, None, check);
}

fn with_trace_runtime(
    runtime: TraceRuntime,
    envelope: &str,
    publisher_origin: Option<&str>,
    check: impl FnOnce(&str),
) {
    let fixture = IsolatedFixture::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match runtime {
        TraceRuntime::Cloudflare => {
            let original = root.join("crates/trusted-server-adapter-cloudflare");
            copy_directory(&original.join("build"), &fixture.0.join("build"));
            let template = fs::read_to_string(original.join("wrangler.ci.toml"))
                .expect("should read baseline Wrangler template");
            let binding = serde_json::to_string(&serde_json::json!({CONFIG_BLOB_KEY:envelope}))
                .expect("should serialize trace binding");
            let config = template.replace(
                "TRUSTED_SERVER_CONFIG = \"{}\"",
                &format!("TRUSTED_SERVER_CONFIG = '''{binding}'''"),
            );
            fs::write(fixture.0.join("wrangler.toml"), config)
                .expect("should write isolated Wrangler config");
            temp_env::with_vars(
                [
                    ("CLOUDFLARE_WRANGLER_DIR", Some(fixture.0.as_os_str())),
                    ("CI", None),
                ],
                || {
                    let process = CloudflareWorkers
                        .spawn_with_readiness(wait_for_trace_fixture_ready)
                        .expect("should spawn isolated Cloudflare trace runtime");
                    check(&process.base_url);
                },
            );
        }
        TraceRuntime::Fastly => {
            let template = include_str!("../../fixtures/configs/viceroy-template.toml");
            let config=template.replace("        # GENERATED_TRUSTED_SERVER_CONFIG_STORES",&format!("        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}]\n            format = \"inline-toml\"\n        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}.contents]\n            {CONFIG_BLOB_KEY} = '''{envelope}'''"));
            let path = fixture.0.join("viceroy.toml");
            fs::write(&path, config).expect("should write isolated Viceroy config");
            temp_env::with_var("VICEROY_CONFIG_PATH", Some(path.as_os_str()), || {
                let process = FastlyViceroy
                    .spawn(&wasm_binary_path())
                    .expect("should spawn isolated Fastly trace runtime");
                check(&process.base_url);
            });
        }
        TraceRuntime::Spin => {
            let artifact =
                root.join("target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm");
            assert!(
                artifact.is_file(),
                "should build production Spin component before boundary probe"
            );
            let mut config = format!(
                "spin_manifest_version = 2\n[application]\nname = \"trace-boundary-example\"\nversion = \"0.1.0\"\n[[trigger.http]]\nroute = \"/...\"\ncomponent = \"trace\"\n[component.trace]\nsource = {:?}\nkey_value_stores = [\"default\"]\n",
                artifact.to_str().expect("should represent component path")
            );
            if let Some(origin) = publisher_origin {
                config.push_str(&format!("allowed_outbound_hosts = [{origin:?}]\n"));
            }
            let secrets = [
                (
                    "integration_admin_password",
                    "integration-admin-password-32-bytes-ok",
                ),
                (
                    "integration_proxy_secret",
                    "integration-test-proxy-secret-32-bytes-ok",
                ),
                (
                    "integration_ec_passphrase",
                    "integration-test-ec-secret-padded-32",
                ),
                (
                    "integration_partner_token_alpha",
                    "integration-test-token-alpha-32-bytes-ok",
                ),
                (
                    "integration_partner_token_bravo",
                    "integration-test-token-bravo-32-bytes-ok",
                ),
            ];
            config.push_str("[variables]\n");
            for (name, value) in secrets {
                config.push_str(&format!(
                    "{} = {{ default = {value:?} }}\n",
                    spin_secret_variable(name)
                ));
            }
            config.push_str("[component.trace.variables]\n");
            for (name, _) in secrets {
                let variable = spin_secret_variable(name);
                config.push_str(&format!("{variable} = \"{{{{ {variable} }}}}\"\n"));
            }
            let manifest = fixture.0.join("spin.toml");
            fs::write(&manifest, config).expect("should write isolated Spin manifest");
            let port = crate::environments::find_available_port()
                .expect("should reserve Spin runtime port");
            let child = Command::new("spin")
                .arg("up")
                .arg("--from")
                .arg(&manifest)
                .arg("--listen")
                .arg(format!("127.0.0.1:{port}"))
                .arg("--state-dir")
                .arg(fixture.0.join("state"))
                .arg("--key-value")
                .arg(format!("{CONFIG_BLOB_KEY}={envelope}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("should spawn isolated Spin runtime");
            let process = RuntimeProcess {
                inner: Box::new(ChildHandle {
                    child,
                    #[cfg(unix)]
                    process_group: false,
                }),
                base_url: format!("http://127.0.0.1:{port}"),
            };
            wait_for_trace_fixture_ready(&process.base_url)
                .expect("should load Spin trace configuration");
            check(&process.base_url);
        }
    }
}

/// Exercise a real host-only cookie and publisher-to-viewer browser workflow.
///
/// Auction and shared assembly stay disabled. Only GPT callbacks are mocked;
/// activation, context, built bundles, handoff, exports and cleanup are real.
/// Run ignored browser tests sequentially because they share the origin port.
///
/// # Panics
///
/// Panics when a required artifact, runtime, container or browser check fails.
pub(crate) fn exercise_browser(runtime: TraceBrowserRuntime) {
    let port = origin_port();
    let _container = NextJs
        .build_container(port)
        .expect("should configure the existing Next.js origin fixture")
        .start()
        .expect("should start the existing Next.js origin fixture");
    crate::environments::wait_for_ready(&format!("http://127.0.0.1:{port}"), "/", false)
        .expect("should make the ordinary publisher origin available");
    let envelope = integration_app_config_envelope_with_trace(port, true)
        .expect("should generate isolated normal-browser trace configuration");
    let publisher_origin = format!("http://127.0.0.1:{port}");
    let check = |base_url: &str| run_browser_workflow(runtime, base_url);
    match runtime {
        TraceBrowserRuntime::Axum => {
            let process = AxumDevServer
                .spawn_with_app_config(&envelope)
                .expect("should spawn the real native trace runtime");
            wait_for_trace_fixture_ready(&process.base_url)
                .expect("should load native trace configuration");
            check(&process.base_url);
        }
        TraceBrowserRuntime::Cloudflare => {
            with_trace_runtime(TraceRuntime::Cloudflare, &envelope, None, check);
        }
        TraceBrowserRuntime::Fastly => {
            with_trace_runtime(TraceRuntime::Fastly, &envelope, None, check);
        }
        TraceBrowserRuntime::Spin => {
            with_trace_runtime(
                TraceRuntime::Spin,
                &envelope,
                Some(&publisher_origin),
                check,
            );
        }
    }
}

fn run_browser_workflow(runtime: TraceBrowserRuntime, base_url: &str) {
    let browser_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("browser");
    let mut origin = url::Url::parse(base_url).expect("should parse the owned runtime origin");
    origin
        .set_host(Some("localhost"))
        .expect("should use browser-suitable localhost without changing the runtime port");
    let mut command = Command::new("node");
    command
        .arg(browser_root.join("node_modules/@playwright/test/cli.js"))
        .args(["test", "--config", "playwright.trace-runtime.config.ts"])
        .env(
            "TRACE_BROWSER_ORIGIN",
            origin.origin().ascii_serialization(),
        )
        .env("TRACE_BROWSER_RUNTIME", runtime.id())
        .current_dir(&browser_root)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    command.process_group(0);
    let mut browser = ChildHandle {
        child: command.spawn().expect(
            "should launch installed Playwright without installing or building dependencies",
        ),
        #[cfg(unix)]
        process_group: true,
    };
    let started = Instant::now();
    loop {
        if let Some(status) = browser
            .child
            .try_wait()
            .expect("should poll the owned browser workflow")
        {
            assert!(
                status.success(),
                "should pass the real {} browser workflow: {status}",
                runtime.id()
            );
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "should finish the {} browser workflow within its bounded deadline",
            runtime.id()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_trace_fixture_ready(base_url: &str) -> crate::common::runtime::TestResult<()> {
    let client = reqwest::blocking::Client::new();
    for _ in 0..60 {
        if client
            .get(format!("{base_url}/_ts/trace/state"))
            .timeout(Duration::from_millis(500))
            .send()
            .is_ok_and(|response| matches!(response.status().as_u16(), 200 | 401 | 404))
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(Report::new(TestError::RuntimeNotReady))
}

fn check_policy_matrix(
    base_url: &str,
    runtime: &TraceRuntime,
    enabled: bool,
    pattern: Option<&str>,
) {
    let url = reqwest::Url::parse(base_url).expect("should parse matrix runtime origin");
    let authority = format!(
        "{}:{}",
        url.host_str().expect("should expose runtime host"),
        url.port().expect("should expose runtime port")
    );
    let authenticated = format!(
        "Authorization: Basic {}\r\n",
        STANDARD.encode("example-user:integration-admin-password-32-bytes-ok")
    );
    for (method, path, route_status) in [
        (b"GET".as_slice(), "/_ts/trace/state", 200),
        (b"HEAD".as_slice(), "/_ts/trace", 200),
        (b"GET".as_slice(), "/_ts/trace/assets/v1.js", 200),
        (b"GET".as_slice(), "/_ts/trace/assets/v1.css", 200),
        (b"POST".as_slice(), "/_ts/trace/enable", 403),
        (b"POST".as_slice(), "/_ts/trace/end", 403),
        (b"PATCH".as_slice(), "/_ts/trace/state", 405),
        (b"GET".as_slice(), "/_ts/trace/extra", 404),
        (
            b"GET".as_slice(),
            "/_ts/trace/../trace/state",
            if matches!(runtime, TraceRuntime::Spin) {
                400
            } else {
                200
            },
        ),
        (b"GET".as_slice(), "/%5Fts/trace", 400),
        (b"GET".as_slice(), "/_ts//trace/state", 404),
    ] {
        for (credentials, headers) in [
            (false, b"".as_slice()),
            (
                false,
                b"Authorization: Basic ZXhhbXBsZS11c2VyOndyb25n\r\n".as_slice(),
            ),
            (true, authenticated.as_bytes()),
        ] {
            let protected =
                pattern.is_some_and(|pattern| pattern == "^/" || path.starts_with("/_ts"));
            let expected = if protected && !credentials {
                401
            } else if !enabled {
                404
            } else {
                route_status
            };
            let response = raw_request(&authority, method, path, headers);
            assert_eq!(
                response.status, expected,
                "should apply actual-runtime auth before flag, reserved-path and method policy for {path}"
            );
            if !path.contains("/assets/") || expected != 200 {
                assert_private(&response);
            } else {
                assert_eq!(
                    response.headers["cache-control"],
                    if protected {
                        "no-store, private"
                    } else {
                        "public, max-age=31536000, immutable"
                    },
                    "should preserve auth-dependent asset privacy"
                );
                assert!(
                    response.headers.contains_key("etag"),
                    "should preserve protected strong ETag through actual runtime"
                );
            }
            if expected == 401 {
                assert!(
                    response.headers.contains_key("www-authenticate"),
                    "should retain local auth challenge"
                );
            }
            if expected == 405 {
                assert_eq!(
                    response.headers["allow"], "GET, HEAD",
                    "should retain local method Allow"
                );
            }
            if method == b"HEAD" {
                assert!(
                    response.body.is_empty(),
                    "should strip every trace HEAD success/challenge/error body"
                );
            }
        }
    }
    if matches!(runtime, TraceRuntime::Fastly) {
        let response = raw_request(&authority, b"GET", "/_ts/trace/../../_ts/debug/ja4", b"");
        assert_eq!(
            response.status, 404,
            "should preserve normalized-out disabled native JA4 behavior before ordinary auth"
        );
        assert!(
            !response.headers.contains_key("content-security-policy"),
            "should not relabel normalized-out native JA4 as a trace response"
        );
        assert!(
            !response.headers.contains_key("set-cookie"),
            "should not create diagnostics cookies for ordinary native shortcut"
        );
    }
}

fn spin_secret_variable(key: &str) -> String {
    let mut output = String::from("v_trusted_x5fserver_x5fsecrets_v_");
    for byte in key.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            output.push(char::from(byte));
        } else {
            output.push_str(&format!("_x{byte:02x}"));
        }
    }
    output
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("should create copied runtime build directory");
    for entry in fs::read_dir(source).expect("should locate prebuilt Cloudflare bundle") {
        let entry = entry.expect("should read build entry");
        if entry
            .file_type()
            .expect("should inspect build entry")
            .is_dir()
        {
            copy_directory(&entry.path(), &destination.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), destination.join(entry.file_name()))
                .expect("should copy isolated runtime build asset");
        }
    }
}

fn check_boundary(base_url: &str, runtime: &TraceRuntime) {
    let url = reqwest::Url::parse(base_url).expect("should parse actual runtime origin");
    let authority = format!(
        "{}:{}",
        url.host_str().expect("should expose local runtime host"),
        url.port().expect("should expose ephemeral runtime port")
    );
    let client = reqwest::blocking::Client::new();
    let state = client
        .get(format!("{base_url}/_ts/trace/state"))
        .header("cookie", "__Host-ts-console=1")
        .send()
        .expect("should request ordinary visible session");
    assert_eq!(
        state.status(),
        200,
        "should activate marker-free Unknown-fidelity runtime cookies"
    );
    assert_eq!(
        state.text().expect("should read state"),
        r#"{"observed_active":true}"#,
        "should observe valid incoming session"
    );
    for action in ["enable", "end"] {
        let response = client
            .post(format!("{base_url}/_ts/trace/{action}"))
            .header("origin", base_url)
            .header("sec-fetch-site", "same-origin")
            .header("x-ts-trace-action", action)
            .header("cookie", "__Host-ts-console=1, unrelated=value")
            .send()
            .expect("should perform actual-origin empty action");
        assert_eq!(
            response.status(),
            200,
            "should accept actual runtime origin"
        );
        let expected = if action == "enable" {
            "__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=1800"
        } else {
            "__Host-ts-console=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"
        };
        let cookies: Vec<_> = response.headers().get_all("set-cookie").iter().collect();
        assert_eq!(
            cookies.len(),
            1,
            "should retain exactly one explicit action cookie through native finalization"
        );
        assert_eq!(
            cookies[0], expected,
            "should preserve shared cookie attributes exactly"
        );
        assert_eq!(
            response.text().expect("should read requested mutation"),
            r#"{"mutation_requested":true}"#,
            "should report mutation without claiming browser acceptance"
        );
        let state = client
            .get(format!("{base_url}/_ts/trace/state"))
            .header("cookie", "__Host-ts-console=1, unrelated=value")
            .send()
            .expect("should observe a separate ambiguous-cookie state");
        assert_eq!(
            state.text().expect("should read independent state"),
            r#"{"observed_active":false}"#,
            "should not infer browser cookie acceptance from a requested mutation"
        );
    }
    let head = raw_request(&authority, b"HEAD", "/_ts/trace", b"");
    assert_eq!(head.status, 200, "should serve bodyless setup HEAD");
    assert!(head.body.is_empty(), "should strip setup HEAD body");
    assert_private(&head);
    let literal = raw_request(
        &authority,
        b"GET",
        "/_ts/trace",
        b"Cookie: __Host-ts-console=1; unrelated=\xef\xbf\xbd\r\n",
    );
    assert_eq!(
        literal.status, 200,
        "should project runtime-visible literal U+FFFD safely"
    );
    assert!(
        String::from_utf8_lossy(&literal.body).contains("runtime_header_ambiguous"),
        "should conservatively reject U+FFFD with unknown original octets"
    );
    assert_private(&literal);
    let repeated = raw_request(
        &authority,
        b"GET",
        "/_ts/trace",
        b"Cookie: __Host-ts-console=1\r\nCookie: __Host-ts-console=1\r\n",
    );
    assert_eq!(
        repeated.status, 200,
        "should retain local cookie-health observation for repeated fields"
    );
    let expected = if matches!(runtime, TraceRuntime::Cloudflare) {
        "runtime_header_ambiguous"
    } else {
        "duplicate"
    };
    assert!(
        String::from_utf8_lossy(&repeated.body).contains(expected),
        "should pin the runtime's visible repeated-cookie boundary"
    );
    assert_private(&repeated);
    let invalid = raw_request(
        &authority,
        b"GET",
        "/_ts/trace",
        b"Cookie: __Host-ts-console=1; unrelated=\xff\r\n",
    );
    match runtime {
        TraceRuntime::Cloudflare | TraceRuntime::Fastly => {
            assert_eq!(
                invalid.status, 200,
                "should inspect bytes accepted at the application boundary"
            );
            let reason = if matches!(runtime, TraceRuntime::Cloudflare) {
                "runtime_header_ambiguous"
            } else {
                "header_not_utf8"
            };
            assert!(
                String::from_utf8_lossy(&invalid.body).contains(reason),
                "should distinguish replacement ambiguity from actual visible non-UTF8"
            );
            assert_private(&invalid);
        }
        TraceRuntime::Spin => {
            assert_eq!(
                invalid.status, 500,
                "should pin the pre-component invalid-byte conversion failure separately"
            );
            assert!(
                !invalid.headers.contains_key("content-security-policy"),
                "should not claim local trace hardening before successful SDK conversion"
            );
            assert!(
                !invalid.headers.contains_key("set-cookie"),
                "should not mutate cookies on pre-component conversion failure"
            );
            assert!(
                !String::from_utf8_lossy(&invalid.body).contains("trace-request-context"),
                "should not capture trace context on conversion failure"
            );
        }
    }
    let extension = raw_request(&authority, b"EXAMPLE-METHOD", "/_ts/trace/state", b"");
    if matches!(runtime, TraceRuntime::Cloudflare) {
        assert_eq!(
            extension.status, 501,
            "should pin workerd's pre-application method rejection"
        );
        assert!(
            !extension.headers.contains_key("content-security-policy"),
            "should keep wire rejection separate from application hardened 405"
        );
    } else {
        assert_eq!(
            extension.status, 405,
            "should reject converted extension method locally"
        );
        assert_eq!(
            extension.headers["allow"], "GET, HEAD",
            "should retain application Allow policy"
        );
        assert_private(&extension);
    }
    let asset = client
        .get(format!("{base_url}/_ts/trace/assets/v1.js"))
        .send()
        .expect("should request fixed asset");
    assert_eq!(asset.status(), 200, "should serve verified fixed bytes");
    assert_eq!(
        asset.headers()["cache-control"],
        "public, max-age=31536000, immutable",
        "should retain immutable asset cache policy"
    );
    assert!(
        asset.headers().contains_key("etag"),
        "should retain strong asset validator through native conversion"
    );
    assert!(
        !asset.headers().contains_key("set-cookie"),
        "should never add identity cookies to fixed assets"
    );
    let normalized_in = raw_request(&authority, b"GET", "/_ts/trace/../trace/state", b"");
    let expected = if matches!(runtime, TraceRuntime::Spin) {
        400
    } else {
        200
    };
    assert_eq!(
        normalized_in.status, expected,
        "should classify the actual runtime-visible path, not recover a discarded wire target"
    );
    assert_private(&normalized_in);
    let normalized_out = raw_request(&authority, b"GET", "/_ts/trace/../../health", b"");
    if matches!(runtime, TraceRuntime::Spin) {
        assert_eq!(
            normalized_out.status, 400,
            "should reject dot ambiguity still visible to Spin"
        );
        assert_private(&normalized_out);
    } else if matches!(runtime, TraceRuntime::Fastly) {
        assert_eq!(
            normalized_out.status, 200,
            "should preserve ordinary health when runtime normalization removes the namespace"
        );
        assert_eq!(
            normalized_out.body, b"ok",
            "should keep normalized-out requests on the ordinary health surface"
        );
    } else {
        let ordinary = raw_request(&authority, b"GET", "/health", b"");
        assert_eq!(
            normalized_out.status, ordinary.status,
            "should preserve Cloudflare's ordinary publisher fallback for normalized-out health"
        );
        assert!(
            !String::from_utf8_lossy(&normalized_out.body).contains("trace-request-context"),
            "should not capture context after the runtime removes the namespace"
        );
    }
}

struct RawResponse {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

fn assert_private(response: &RawResponse) {
    assert_eq!(
        response.headers["cache-control"], "no-store, private",
        "should preserve dynamic/error cache policy through actual runtime"
    );
    assert!(
        response.headers.contains_key("content-security-policy"),
        "should retain fixed trace CSP"
    );
    assert!(
        !response.headers.contains_key("set-cookie"),
        "should not mutate read cookies"
    );
}

fn raw_request(authority: &str, method: &[u8], path: &str, headers: &[u8]) -> RawResponse {
    let mut connection = TcpStream::connect(authority).expect("should connect raw runtime client");
    connection
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("should bound raw response wait");
    connection
        .write_all(method)
        .expect("should write exact method");
    connection
        .write_all(
            format!(" {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n").as_bytes(),
        )
        .expect("should write runtime authority");
    connection
        .write_all(headers)
        .expect("should preserve original header bytes and repeated fields");
    connection
        .write_all(b"\r\n")
        .expect("should finish raw request");
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    while bytes.len() < 2 * 1024 * 1024 {
        let available = chunk.len().min(2 * 1024 * 1024 - bytes.len());
        let read = connection
            .read(&mut chunk[..available])
            .expect("should read bounded raw response");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if response_complete(&bytes, method == b"HEAD") {
            break;
        }
    }
    let end = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .expect("should receive HTTP response headers");
    let text = std::str::from_utf8(&bytes[..end]).expect("should receive UTF8 response headers");
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .expect("should receive status line")
        .split_ascii_whitespace()
        .nth(1)
        .expect("should receive numeric status")
        .parse()
        .expect("should parse status");
    let mut response_headers = HeaderMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').expect("should parse header field");
        response_headers.append(
            http::header::HeaderName::from_bytes(name.as_bytes())
                .expect("should parse header name"),
            HeaderValue::from_str(value.trim()).expect("should parse response header"),
        );
    }
    let body = if method != b"HEAD"
        && response_headers
            .get("transfer-encoding")
            .is_some_and(|value| value == "chunked")
    {
        decode_chunks(&bytes[end + 4..])
    } else {
        bytes[end + 4..].to_vec()
    };
    RawResponse {
        status,
        headers: response_headers,
        body,
    }
}

fn decode_chunks(mut bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let end = bytes
            .windows(2)
            .position(|part| part == b"\r\n")
            .expect("should read chunk length");
        let size = usize::from_str_radix(
            std::str::from_utf8(&bytes[..end])
                .expect("should read ASCII chunk length")
                .split(';')
                .next()
                .expect("should read chunk size"),
            16,
        )
        .expect("should parse chunk length");
        if size == 0 {
            return body;
        }
        bytes = &bytes[end + 2..];
        body.extend_from_slice(
            bytes
                .get(..size)
                .expect("should receive complete bounded chunk"),
        );
        bytes = bytes
            .get(size + 2..)
            .expect("should receive chunk terminator");
    }
}

fn response_complete(bytes: &[u8], head: bool) -> bool {
    let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
        return false;
    };
    if head {
        return true;
    }
    let Ok(headers) = std::str::from_utf8(&bytes[..end]) else {
        return false;
    };
    let body = &bytes[end + 4..];
    let mut length = None;
    let mut chunked = false;
    for line in headers.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        if name.eq_ignore_ascii_case("content-length") {
            let Ok(parsed) = value.trim().parse::<usize>() else {
                return false;
            };
            if length.replace(parsed).is_some() {
                return false;
            }
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.trim().eq_ignore_ascii_case("chunked")
        {
            chunked = true;
        }
    }
    if chunked {
        return chunks_complete(body);
    }
    length.is_some_and(|length| body.len() >= length)
}

fn chunks_complete(mut bytes: &[u8]) -> bool {
    loop {
        let Some(end) = bytes.windows(2).position(|part| part == b"\r\n") else {
            return false;
        };
        let Ok(line) = std::str::from_utf8(&bytes[..end]) else {
            return false;
        };
        let Some(size) = line
            .split(';')
            .next()
            .and_then(|size| usize::from_str_radix(size, 16).ok())
        else {
            return false;
        };
        bytes = &bytes[end + 2..];
        if size == 0 {
            loop {
                let Some(end) = bytes.windows(2).position(|part| part == b"\r\n") else {
                    return false;
                };
                if end == 0 {
                    return true;
                }
                bytes = &bytes[end + 2..];
            }
        }
        let Some(next) = size.checked_add(2) else {
            return false;
        };
        if bytes.get(size..next) != Some(b"\r\n".as_slice()) {
            return false;
        }
        bytes = &bytes[next..];
    }
}

#[test]
fn trace_raw_response_completion_respects_framing() {
    assert!(
        response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", false),
        "should finish an empty framed response without waiting for connection closure"
    );
    assert!(
        !response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nab", false),
        "should wait for the full declared body"
    );
    assert!(
        response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc", false),
        "should finish at the full declared body"
    );
    assert!(
        response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n", true),
        "should finish HEAD at the header boundary"
    );
    assert!(
        !response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n", true),
        "should require the complete header terminator"
    );
    assert!(
        !response_complete(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nabcde\r\n0\r\n",
            false
        ),
        "should wait for the complete final chunk terminator"
    );
    assert!(
        response_complete(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nabcde\r\n0\r\n\r\n",
            false
        ),
        "should finish a complete chunked response"
    );
    assert!(
        !response_complete(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n\r\n0\r\n\r\n",
            false
        ),
        "should not mistake a final-chunk marker inside chunk data for completion"
    );
    assert!(response_complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1;example=yes\r\na\r\n0\r\nExample: value\r\n\r\n", false), "should finish chunk extensions and bounded trailers");
    assert!(
        !response_complete(b"HTTP/1.1 200 OK\r\n\r\nabc", false),
        "should retain EOF framing when no length or transfer framing is present"
    );
}
