//! Command-path coverage for safe generation config diagnostics.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use trusted_server_core::config::TrustedServerAppConfig;

#[test]
fn generate_rejects_invalid_config_without_echoing_source() {
    for (source, guidance) in [
        (
            "passphrase = FICTIONAL_SECRET_SENTINEL\n",
            "fix the TOML syntax and re-run",
        ),
        (
            "[creative_opportunities]\ngam_network_id = \"123\"\nslot = \"FICTIONAL_SECRET_SENTINEL\"\n",
            "Fix the section (or delete it) and re-run",
        ),
    ] {
        for dry_run in [false, true] {
            let directory = tempfile::tempdir().expect("should create temporary config directory");
            let path = directory.path().join("trusted-server.toml");
            fs::write(&path, source).expect("should write malformed config");
            let mut command = Command::new(env!("CARGO_BIN_EXE_ts"));
            command
                .args([
                    "audit",
                    "ad-templates",
                    "generate",
                    "https://publisher.example.com",
                    "--app-config",
                ])
                .arg(&path)
                .current_dir(directory.path());
            if dry_run {
                command.arg("--dry-run");
            }

            let output = command.output().expect("should run generation command");
            let stderr = String::from_utf8(output.stderr).expect("should decode stderr");

            assert_eq!(
                output.status.code(),
                Some(2),
                "should reject config before browser launch"
            );
            assert!(
                output.stdout.is_empty(),
                "should not print config to stdout"
            );
            assert!(
                !stderr.contains("FICTIONAL_SECRET_SENTINEL"),
                "should not echo source in errors"
            );
            assert!(
                stderr.contains(guidance),
                "should explain how to repair the config: {stderr}"
            );
            assert_eq!(
                fs::read_to_string(&path).expect("should read config"),
                source,
                "should preserve invalid config"
            );
        }
    }
}

const EXAMPLE_CONFIG: &str = include_str!("../../../trusted-server.example.toml");
fn baseline() -> String {
    EXAMPLE_CONFIG
        .replace("\"example.com\"", "\"publisher.example.com\"")
        .replace("\".example.com\"", "\".publisher.example.com\"")
        .replace(
            "https://origin.example.com",
            "https://origin.publisher.example.com",
        )
}

const GPT_PAGE: &str = r#"<!doctype html><html><head><script>
window.googletag = { pubads: function () { return { getSlots: function () {
  return [{
    getAdUnitPath: function () { return '/123456789/header' },
    getSlotElementId: function () {
      return location.hash === '#invalid-div' ? ' ' : 'ad-header'
    },
    getSizes: function () {
      return [{ getWidth: function () { return 300 }, getHeight: function () { return 250 } }]
    }
  }]
} } } }
</script></head><body><div id="ad-header"></div></body></html>"#;

/// Concurrent loopback fixture; speculative or silent Chrome sockets must not
/// prevent the document request from being served.
struct PageFixture {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
}

impl PageFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("should bind fixture server");
        let address = listener.local_addr().expect("should read fixture address");
        let shutdown = Arc::new(AtomicBool::new(false));
        let acceptor_shutdown = Arc::clone(&shutdown);
        let acceptor = thread::spawn(move || {
            while !acceptor_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if acceptor_shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::spawn(move || serve_connection(stream));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            shutdown,
            acceptor: Some(acceptor),
        }
    }
}

impl Drop for PageFixture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        if let Some(acceptor) = self.acceptor.take() {
            acceptor.join().expect("should join fixture server");
        }
    }
}

fn serve_connection(mut stream: TcpStream) {
    if stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .is_err()
    {
        return;
    }
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut chunk = [0; 1024];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => request.extend_from_slice(&chunk[..count]),
        }
        if request.len() > 16 * 1024 {
            return;
        }
    }
    if request.starts_with(b"GET / HTTP") {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            GPT_PAGE.len(),
            GPT_PAGE
        );
    } else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
}

fn generate_from_fixture(
    fixture: &PageFixture,
    path: &Path,
    dry_run: bool,
    invalid_div: bool,
) -> Output {
    let url = format!(
        "http://{}/{}",
        fixture.address,
        if invalid_div { "#invalid-div" } else { "" }
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_ts"));
    command
        .args(["audit", "ad-templates", "generate", &url, "--app-config"])
        .arg(path)
        .args([
            "--replace",
            "--max-pages",
            "1",
            "--page-delay-ms",
            "0",
            "--settle-quiet-ms",
            "10",
            "--settle-max-ms",
            "1000",
        ]);
    if dry_run {
        command.arg("--dry-run");
    }
    command
        .output()
        .expect("should run generation against local GPT fixture")
}

#[test]
#[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
fn generate_deploy_validation_warning_does_not_echo_baseline_values() {
    let fixture = PageFixture::start();
    let source = baseline().replace(
        "domain = \"publisher.example.com\"",
        "domain = \"FICTIONAL_SECRET_SENTINEL/invalid.example.com\"",
    );
    let parsed: TrustedServerAppConfig =
        toml::from_str(&source).expect("should pass syntax and schema");
    assert!(
        TrustedServerAppConfig::new(parsed.into_settings()).is_err(),
        "should fail deploy validation"
    );
    for dry_run in [false, true] {
        let directory = tempfile::tempdir().expect("should create temporary config directory");
        let path = directory.path().join("trusted-server.toml");
        fs::write(&path, &source).expect("should write invalid baseline");

        let output = generate_from_fixture(&fixture, &path, dry_run, false);
        let stderr = String::from_utf8(output.stderr).expect("should decode stderr");
        let stdout = String::from_utf8(output.stdout).expect("should decode stdout");
        assert_eq!(
            output.status.code(),
            Some(0),
            "should warn about pre-existing validation failure: {stderr}"
        );
        assert!(
            !stderr.contains("FICTIONAL_SECRET_SENTINEL"),
            "should not leak baseline through warnings"
        );
        assert!(
            !stdout.contains("FICTIONAL_SECRET_SENTINEL"),
            "should not leak baseline through preview output"
        );
        assert!(
            stderr.contains("already invalid"),
            "should identify baseline failure"
        );
        assert!(
            stderr.contains("config failed deploy validation"),
            "should identify deploy validation"
        );
        let written = fs::read_to_string(&path).expect("should read resulting config");
        if dry_run {
            assert_eq!(written, source, "should preserve baseline during preview");
        } else {
            assert!(
                written.contains("ad-header"),
                "should write generated slot despite baseline failure"
            );
            assert!(
                written.contains("FICTIONAL_SECRET_SENTINEL"),
                "should preserve unrelated config values"
            );
        }
    }
}

#[test]
#[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
fn generate_deploy_validation_refusal_preserves_config_and_safe_output() {
    let fixture = PageFixture::start();
    let source = format!("{}\n# FICTIONAL_SECRET_SENTINEL\n", baseline());
    let parsed: TrustedServerAppConfig =
        toml::from_str(&source).expect("should parse valid baseline");
    TrustedServerAppConfig::new(parsed.into_settings()).expect("should pass deploy validation");
    for dry_run in [false, true] {
        let directory = tempfile::tempdir().expect("should create temporary config directory");
        let path = directory.path().join("trusted-server.toml");
        fs::write(&path, &source).expect("should write valid baseline");

        // A whitespace div ID survives collection but fails deploy validation.
        let output = generate_from_fixture(&fixture, &path, dry_run, true);
        let stderr = String::from_utf8(output.stderr).expect("should decode stderr");
        assert_eq!(
            output.status.code(),
            Some(2),
            "should refuse the invalid generated candidate: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "should not print an invalid preview"
        );
        assert!(
            stderr.contains("refusing to write"),
            "should report write refusal"
        );
        assert!(
            stderr.contains("config failed deploy validation"),
            "should identify deploy validation"
        );
        assert!(
            !stderr.contains("FICTIONAL_SECRET_SENTINEL"),
            "should not leak config values"
        );
        assert!(
            !stderr.contains("div_id override"),
            "should not forward raw validation detail"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("should read original config"),
            source,
            "should preserve config on refusal"
        );
    }
}

#[test]
#[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
fn generate_unavailable_proxy_reports_navigation_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should reserve a proxy port");
    let address = listener.local_addr().expect("should read proxy address");
    drop(listener);
    let directory = tempfile::tempdir().expect("should create config directory");
    let path = directory.path().join("app.toml");
    let source = baseline();
    fs::write(&path, &source).expect("should write config");

    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "audit",
            "ad-templates",
            "generate",
            "https://example.com/",
            "--app-config",
        ])
        .arg(&path)
        .args([
            "--no-env",
            "--browser-proxy",
            &address.to_string(),
            "--max-pages",
            "1",
            "--settle-quiet-ms",
            "10",
            "--settle-max-ms",
            "100",
            "--dry-run",
        ])
        .output()
        .expect("should run audit with unavailable proxy");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "should refuse failed navigation"
    );
    assert!(
        stderr.contains("browser navigation did not produce an HTTP(S) page"),
        "should identify failed navigation: {stderr}"
    );
    assert!(
        !stderr.contains("cross-origin root redirect"),
        "should not invent a redirect: {stderr}"
    );
    assert!(output.stdout.is_empty(), "should not generate config");
    assert_eq!(
        fs::read_to_string(path).expect("should read config"),
        source,
        "should preserve config after navigation failure"
    );
}
