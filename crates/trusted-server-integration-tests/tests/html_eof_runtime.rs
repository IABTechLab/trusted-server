//! Native verification of ordered HTML EOF work on actual local runtimes.

#![cfg(unix)]

#[path = "html_eof_runtime/config.rs"]
mod config;
#[path = "html_eof_runtime/origin.rs"]
mod origin;
#[path = "html_eof_runtime/process.rs"]
mod process;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use error_stack::{Report, ResultExt as _};
use reqwest::blocking::Client;
use serde_json::{Value, json};

use process::{Cancellation, ManagedChild};

#[derive(Debug, derive_more::Display)]
enum HarnessError {
    #[display("Invalid isolated runtime configuration")]
    Configuration,
    #[display("Runtime command fails")]
    Runtime,
    #[display("Runtime HTTP request fails")]
    Http,
    #[display("Runtime fixture I/O fails")]
    Io,
    #[display("Runtime verification is cancelled")]
    Cancelled,
}

impl core::error::Error for HarnessError {}

type HarnessResult<T> = Result<T, Report<HarnessError>>;

struct Harness<'a> {
    origin: &'a origin::Origin,
    root: PathBuf,
    workspace: PathBuf,
    target: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    cancellation: Cancellation,
}

impl<'a> Harness<'a> {
    fn new(adapter: &str, origin: &'a origin::Origin) -> HarnessResult<Self> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .change_context(HarnessError::Io)?;
        let workspace = tempfile::Builder::new()
            .prefix(&format!("trusted-server-finalize-{adapter}-"))
            .tempdir()
            .change_context(HarnessError::Io)?
            .keep();
        log::info!("Artifacts: {}", workspace.display());
        let mut environment = process::build_environment(std::env::vars_os());
        let target = environment
            .get(&OsString::from("CARGO_TARGET_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        let target = if target.is_absolute() {
            target
        } else {
            root.join(target)
        };
        for (name, value) in [
            ("CARGO_TARGET_DIR", target.as_os_str().to_owned()),
            (
                "XDG_CONFIG_HOME",
                workspace.join("cli-config").into_os_string(),
            ),
            ("WRANGLER_SEND_METRICS", OsString::from("false")),
            (
                "WRANGLER_LOG_PATH",
                workspace.join("wrangler.log").into_os_string(),
            ),
            (
                "TRUSTED_SERVER__PUBLISHER__DOMAIN",
                OsString::from("127.0.0.1"),
            ),
            (
                "TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN",
                OsString::from("127.0.0.1"),
            ),
            (
                "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL",
                OsString::from(format!("http://127.0.0.1:{}", origin.port)),
            ),
        ] {
            environment.insert(OsString::from(name), value);
        }
        let cancellation = Cancellation::default();
        cancellation.watch(origin.runtime.handle())?;
        Ok(Self {
            origin,
            root,
            workspace,
            target,
            environment,
            cancellation,
        })
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.root)
            .env_clear()
            .envs(&self.environment);
        command
    }

    fn run(&self, command: &mut Command, name: &str) -> HarnessResult<()> {
        process::logged_command(
            command,
            &self.workspace.join(format!("{name}.log")),
            &self.cancellation,
        )
    }

    fn preflight(&self, adapter: &str) -> HarnessResult<()> {
        if adapter == "fastly" {
            self.run(self.command("viceroy").arg("--version"), "viceroy-version")?;
            let version = fs::read_to_string(self.workspace.join("viceroy-version.log"))
                .change_context(HarnessError::Io)?;
            validate_viceroy(&version)?;
        }
        if adapter == "cloudflare" {
            self.run(
                self.command("worker-build").arg("--version"),
                "worker-version",
            )?;
            let actual = fs::read_to_string(self.workspace.join("worker-version.log"))
                .change_context(HarnessError::Io)?;
            let lock = config::parse(
                &fs::read_to_string(self.root.join("Cargo.lock"))
                    .change_context(HarnessError::Io)?,
            )?;
            let expected = lock["package"]
                .as_array()
                .and_then(|packages| {
                    packages
                        .iter()
                        .find(|package| package["name"].as_str() == Some("worker"))
                })
                .and_then(|package| package["version"].as_str())
                .ok_or_else(|| Report::new(HarnessError::Configuration))?;
            if actual.trim() != expected {
                return Err(Report::new(HarnessError::Runtime).attach(format!(
                    "worker-build {expected} required; found {}",
                    actual.trim()
                )));
            }
        }
        Ok(())
    }

    fn build(&self, adapter: &str, release: bool) -> HarnessResult<PathBuf> {
        self.run(
            self.command("cargo").args([
                "build",
                "-p",
                "trusted-server-cli",
                "--bin",
                "ts",
                "--locked",
            ]),
            "build-cli",
        )?;
        let crate_root = self
            .root
            .join(format!("crates/trusted-server-adapter-{adapter}"));
        let profile = if release { "release" } else { "debug" };
        let artifact = if adapter == "cloudflare" {
            let mut command = self.command("worker-build");
            command
                .current_dir(crate_root)
                .arg(if release { "--release" } else { "--dev" })
                .arg("--out-dir")
                .arg(self.workspace.join("build"));
            if !release {
                command.arg("--no-opt");
            }
            command.args(["--features", "cloudflare,test-utils", "--locked"]);
            self.run(&mut command, "build-runtime")?;
            self.workspace.join("build/index.js")
        } else {
            let mut command = self.command("cargo");
            command.args([
                "build",
                "-p",
                &format!("trusted-server-adapter-{adapter}"),
                "--locked",
                "--features",
                if adapter == "spin" {
                    "trusted-server-core/test-utils,spin,test-utils"
                } else {
                    "trusted-server-core/test-utils"
                },
            ]);
            if adapter != "axum" {
                command.args(["--target", "wasm32-wasip1"]);
            }
            if release {
                command.arg("--release");
            }
            self.run(&mut command, "build-runtime")?;
            match adapter {
                "axum" => self.target.join(format!("{profile}/trusted-server-axum")),
                "spin" => self.target.join(format!(
                    "wasm32-wasip1/{profile}/trusted_server_adapter_spin.wasm"
                )),
                _ => self.target.join(format!(
                    "wasm32-wasip1/{profile}/trusted-server-adapter-fastly.wasm"
                )),
            }
        };
        if !artifact.is_file() {
            return Err(Report::new(HarnessError::Runtime)
                .attach(format!("fresh artifact missing: {}", artifact.display())));
        }
        Ok(artifact)
    }

    fn push(&self, adapter: &str) -> HarnessResult<()> {
        let mut command = self.command(self.target.join("debug/ts"));
        command
            .current_dir(&self.workspace)
            .args([
                "config",
                "push",
                "--adapter",
                adapter,
                "--local",
                "--manifest",
            ])
            .arg(self.workspace.join("edgezero.toml"))
            .arg("--app-config")
            .arg(self.workspace.join("trusted-server.toml"))
            .args(["--yes", "--no-diff"]);
        if adapter == "spin" {
            command.env(
                "EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME",
                "default",
            );
        }
        self.run(&mut command, &format!("push-{adapter}"))
    }

    fn runtime_command(&self, adapter: &str, artifact: &Path, port: u16) -> HarnessResult<Command> {
        let mut command;
        let raw = fs::read_to_string(
            self.workspace
                .join(".edgezero/local-config-trusted_server_config.json"),
        )
        .change_context(HarnessError::Io)?;
        let value: Value =
            serde_json::from_str(&raw).change_context(HarnessError::Configuration)?;
        let envelope = value["trusted_server_config"]
            .as_str()
            .ok_or_else(|| Report::new(HarnessError::Configuration))?;
        let source = fs::read_to_string(self.workspace.join("trusted-server.toml"))
            .change_context(HarnessError::Io)?;
        let fixtures = config::native_fixtures(&source, adapter)?;
        match adapter {
            "axum" => {
                command = self.command(artifact);
                command.env("PORT", port.to_string()).env(
                    "TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG",
                    envelope,
                );
                for fixture in &fixtures {
                    command.env(&fixture.native.native_name, &fixture.value);
                }
            }
            "cloudflare" => {
                let mut manifest = config::parse(
                    &fs::read_to_string(
                        self.root
                            .join("crates/trusted-server-adapter-cloudflare/wrangler.ci.toml"),
                    )
                    .change_context(HarnessError::Io)?,
                )?;
                config::replace(
                    &mut manifest,
                    &["main"],
                    toml::Value::String(artifact.to_string_lossy().into_owned()),
                )?;
                config::replace(
                    &mut manifest,
                    &["vars", "TRUSTED_SERVER_CONFIG"],
                    toml::Value::String(json!({"app_config": envelope}).to_string()),
                )?;
                let vars = manifest["vars"]
                    .as_table_mut()
                    .ok_or_else(|| Report::new(HarnessError::Configuration))?;
                for fixture in &fixtures {
                    vars.insert(
                        fixture.native.native_name.clone(),
                        toml::Value::String(fixture.value.clone()),
                    );
                }
                fs::write(
                    self.workspace.join("wrangler.toml"),
                    config::serialize(&manifest)?,
                )
                .change_context(HarnessError::Io)?;
                command = self.command("wrangler");
                command
                    .args(["dev", "--local", "--config"])
                    .arg(self.workspace.join("wrangler.toml"))
                    .args([
                        "--ip",
                        "127.0.0.1",
                        "--port",
                        &port.to_string(),
                        "--persist-to",
                    ])
                    .arg(self.workspace.join("worker-state"));
            }
            "fastly" => {
                let mut manifest = config::parse(
                    &fs::read_to_string(self.workspace.join("fastly.toml"))
                        .change_context(HarnessError::Io)?,
                )?;
                let secrets: Vec<_> = fixtures
                    .iter()
                    .map(|fixture| {
                        let mut entry = toml::map::Map::new();
                        entry.insert(
                            "key".to_owned(),
                            toml::Value::String(fixture.native.native_name.clone()),
                        );
                        entry.insert(
                            "data".to_owned(),
                            toml::Value::String(fixture.value.clone()),
                        );
                        toml::Value::Table(entry)
                    })
                    .collect();
                let local = manifest["local_server"]
                    .as_table_mut()
                    .ok_or_else(|| Report::new(HarnessError::Configuration))?;
                let mut stores = toml::map::Map::new();
                stores.insert(
                    "trusted_server_secrets".to_owned(),
                    toml::Value::Array(secrets),
                );
                local.insert("secret_stores".to_owned(), toml::Value::Table(stores));
                fs::write(
                    self.workspace.join("fastly.toml"),
                    config::serialize(&manifest)?,
                )
                .change_context(HarnessError::Io)?;
                command = self.command("viceroy");
                command
                    .arg("--config")
                    .arg(self.workspace.join("fastly.toml"))
                    .args(["--addr", &format!("127.0.0.1:{port}")])
                    .arg(artifact);
            }
            "spin" => {
                let manifest_path = self.workspace.join("spin.toml");
                let template =
                    fs::read_to_string(&manifest_path).change_context(HarnessError::Io)?;
                fs::write(
                    &manifest_path,
                    config::spin_fixture_manifest(&template, &fixtures)?,
                )
                .change_context(HarnessError::Io)?;
                command = self.command("spin");
                command
                    .args(["up", "--from"])
                    .arg(self.workspace.join("spin.toml"))
                    .args(["--listen", &format!("127.0.0.1:{port}"), "--state-dir"])
                    .arg(self.workspace.join(".spin"))
                    .arg("--cache-dir")
                    .arg(self.workspace.join("spin-cache"))
                    .arg("--log-dir")
                    .arg(self.workspace.join("spin-logs"));
                for fixture in &fixtures {
                    command
                        .arg("--variable")
                        .arg(format!("{}={}", fixture.native.native_name, fixture.value));
                }
            }
            _ => {
                return Err(
                    Report::new(HarnessError::Configuration).attach("unknown EOF runtime adapter")
                );
            }
        }
        command.current_dir(&self.workspace);
        Ok(command)
    }
}

impl Drop for Harness<'_> {
    fn drop(&mut self) {
        if let Err(error) = self
            .origin
            .write_events(&self.workspace.join("events.json"))
        {
            log::warn!("EOF harness cannot retain event diagnostics: {error:?}");
        }
    }
}

fn available_port() -> HarnessResult<u16> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .change_context(HarnessError::Io)
}

fn validate_viceroy(output: &str) -> HarnessResult<()> {
    let version = output
        .trim()
        .strip_prefix("viceroy ")
        .and_then(|version| version.split_whitespace().next())
        .and_then(|version| {
            version
                .split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()
        });
    if !version
        .is_some_and(|version| version.len() == 3 && version.as_slice() >= [0, 19, 0].as_slice())
    {
        return Err(Report::new(HarnessError::Runtime)
            .attach(format!("Viceroy >= 0.19.0 required; found {output:?}")));
    }
    Ok(())
}

fn measured_terminal_ms(
    adapter: &str,
    diagnostic: &str,
    lookup_elapsed: Duration,
) -> HarnessResult<Option<u128>> {
    let measured = diagnostic
        .split("times out after ")
        .nth(1)
        .and_then(|tail| tail.split_once(" ms"))
        .and_then(|(millis, _)| millis.parse::<u128>().ok());
    if let Some(millis) = measured {
        if !(700..2500).contains(&millis) {
            return Err(
                Report::new(HarnessError::Runtime).attach("terminal budget escaped its tolerance")
            );
        }
        return Ok(Some(millis));
    }
    if adapter != "fastly"
        || !diagnostic.contains("retains input: unavailable")
        || !(Duration::from_millis(700)..Duration::from_millis(2500)).contains(&lookup_elapsed)
    {
        return Err(
            Report::new(HarnessError::Runtime).attach("missing measured bounded timeout evidence")
        );
    }
    Ok(None)
}

fn consent_cookie() -> String {
    let mut bits = [0_u8; 29];
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("should have a current fixture clock")
        .as_millis()
        / 100;
    for (offset, width, value) in [
        (0, 6, 2),
        (6, 36, now),
        (42, 36, now),
        (108, 6, 4),
        (114, 6, 13),
        (152, 1, 1),
    ] {
        for index in 0..width {
            if value & (1 << (width - index - 1)) != 0 {
                bits[(offset + index) / 8] |= 1 << (7 - (offset + index) % 8);
            }
        }
    }
    format!("euconsent-v2={}", URL_SAFE_NO_PAD.encode(bits))
}

struct CapturedBody {
    bytes: Vec<u8>,
    first_byte_at: Instant,
    result: std::io::Result<()>,
}

fn capture_body(reader: &mut impl std::io::Read) -> CapturedBody {
    let mut first = [0_u8; 1];
    let result = reader.read_exact(&mut first);
    let first_byte_at = Instant::now();
    let mut bytes = if result.is_ok() {
        Vec::from(first)
    } else {
        Vec::new()
    };
    let result = result.and_then(|()| reader.read_to_end(&mut bytes).map(|_| ()));
    CapturedBody {
        bytes,
        first_byte_at,
        result,
    }
}

#[test]
fn truncated_response_preserves_received_bytes() {
    struct TruncatedResponse(usize);
    impl std::io::Read for TruncatedResponse {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if bytes.is_empty() {
                return Ok(0);
            }
            if self.0 >= 2 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
            }
            bytes[0] = b"ab"[self.0];
            self.0 += 1;
            Ok(1)
        }
    }
    let captured = capture_body(&mut TruncatedResponse(0));
    assert!(
        captured.result.is_err(),
        "should report the truncated transfer"
    );
    assert_eq!(
        captured.bytes, b"ab",
        "should retain bytes for failure diagnostics"
    );
}

#[test]
#[ignore = "requires the selected local runtime and fresh adapter builds"]
fn actual_runtime() -> HarnessResult<()> {
    let _ = env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .try_init();
    let adapter = std::env::var("HTML_EOF_ADAPTER").change_context(HarnessError::Configuration)?;
    if !["axum", "fastly", "cloudflare", "spin"].contains(&adapter.as_str()) {
        return Err(Report::new(HarnessError::Configuration));
    }
    let release = std::env::var("HTML_EOF_RELEASE").as_deref() == Ok("true");
    let auction = std::env::var("HTML_EOF_AUCTION").as_deref() == Ok("true");
    let origin = origin::Origin::start()?;
    let harness = Harness::new(&adapter, &origin)?;
    harness.preflight(&adapter)?;
    let artifact = harness.build(&adapter, release)?;
    harness.run(
        harness
            .command(harness.target.join("debug/ts"))
            .args(["config", "init", "--app-config"])
            .arg(harness.workspace.join("trusted-server.toml")),
        "config-init",
    )?;
    let template = fs::read_to_string(harness.workspace.join("trusted-server.toml"))
        .change_context(HarnessError::Io)?;
    fs::write(
        harness.workspace.join("edgezero.toml"),
        config::fixture_manifest(
            include_str!("../../../edgezero.toml"),
            &harness.root,
            &harness.workspace,
        )?,
    )
    .change_context(HarnessError::Io)?;
    if adapter == "spin" {
        let mut manifest =
            config::parse(include_str!("../../trusted-server-adapter-spin/spin.toml"))?;
        config::replace(
            &mut manifest,
            &["component", "trusted-server", "source"],
            toml::Value::String(artifact.to_string_lossy().into_owned()),
        )?;
        fs::write(
            harness.workspace.join("spin.toml"),
            config::serialize(&manifest)?,
        )
        .change_context(HarnessError::Io)?;
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .change_context(HarnessError::Http)?;
    let mut cases = vec!["disabled", "success", "hung"];
    if auction {
        cases.push("auction");
    }
    if adapter == "fastly" {
        cases.push("streaming");
    }
    let mut baseline = Vec::new();
    for case in cases {
        harness.cancellation.check()?;
        // Auction controls deliberately use an unreachable bidder and verify empty-bid assembly.
        let config = config::fixture_config(&template, case, origin.port, available_port()?)?;
        fs::write(harness.workspace.join("trusted-server.toml"), config)
            .change_context(HarnessError::Io)?;
        fs::write(harness.workspace.join("fastly.toml"), "manifest_version = 3\nname = 'example-finalization'\nlanguage = 'rust'\n[local_server]\n").change_context(HarnessError::Io)?;
        harness.push("axum")?;
        if matches!(adapter.as_str(), "fastly" | "spin") {
            harness.push(&adapter)?;
        }
        let port = available_port()?;
        let log = harness.workspace.join(format!("{case}.log"));
        let output = File::create(&log).change_context(HarnessError::Io)?;
        let mut command = harness.runtime_command(&adapter, &artifact, port)?;
        command
            .stdout(Stdio::from(
                output.try_clone().change_context(HarnessError::Io)?,
            ))
            .stderr(Stdio::from(output));
        let mut process = ManagedChild::spawn(&mut command, &harness.cancellation)?;
        let base_url = format!("http://127.0.0.1:{port}");
        let ready_client = Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .change_context(HarnessError::Http)?;
        let mut ready = false;
        for _ in 0..80 {
            if let Some(status) = process.status()? {
                return Err(Report::new(HarnessError::Runtime)
                    .attach(format!("runtime exits {status}; see {}", log.display())));
            }
            if ready_client
                .head(format!("{base_url}/health"))
                .send()
                .is_ok_and(|response| response.status().is_success())
            {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if !ready {
            return Err(Report::new(HarnessError::Runtime)
                .attach(format!("runtime not ready; see {}", log.display())));
        }
        let started = Instant::now();
        let mut request = client
            .get(format!("{base_url}/article?case={case}"))
            .header("accept", "text/html")
            .header("accept-encoding", "identity")
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-mode", "navigate");
        if case == "auction" {
            request = request.header("cookie", consent_cookie());
        }
        let mut response = request.send().change_context(HarnessError::Http)?;
        let headers = response.headers().clone();
        let status = response.status();
        fs::write(
            harness.workspace.join(format!("{case}.headers")),
            format!("status={status}\n{headers:?}"),
        )
        .change_context(HarnessError::Io)?;
        let captured = capture_body(&mut response);
        let first_byte_at = captured.first_byte_at;
        let body = captured.bytes;
        let completed = Instant::now();
        fs::write(harness.workspace.join(format!("{case}.body")), &body)
            .change_context(HarnessError::Io)?;
        captured
            .result
            .change_context(HarnessError::Http)
            .attach(format!(
                "HTTP {status} during {case}; response artifacts in {}",
                harness.workspace.display()
            ))?;
        assert!(
            status.is_success(),
            "should serve {case} HTML; see {}",
            log.display()
        );
        match case {
            "disabled" => {
                assert!(
                    String::from_utf8_lossy(&body).contains("EXAMPLE_UNSIGNED")
                        && !String::from_utf8_lossy(&body).contains("EXAMPLE_FINALIZED"),
                    "should preserve disabled output"
                );
                baseline = body.clone();
            }
            "success" => {
                assert_eq!(
                    body,
                    String::from_utf8_lossy(&baseline)
                        .replace("EXAMPLE_UNSIGNED", "EXAMPLE_FINALIZED")
                        .as_bytes(),
                    "should emit the exact approved rewrite"
                );
                assert!(
                    completed.duration_since(started) >= Duration::from_millis(150),
                    "should perform delayed I/O"
                );
            }
            "streaming" => {
                let mut expected = baseline.clone();
                expected.extend_from_slice(b"<!--example-async-eof-complete-->");
                assert_eq!(body, expected, "should append only the EOF output");
            }
            "auction" => {
                let body = String::from_utf8_lossy(&body);
                let finalized = body
                    .find("EXAMPLE_FINALIZED")
                    .expect("should finalize the document");
                let assembled = body
                    .find("var b=JSON.parse(")
                    .expect("should assemble the empty-bid script");
                assert!(
                    finalized < assembled
                        && body.contains("adSlots")
                        && !body.contains("ts-inline-body-close-"),
                    "should emit finalized output before empty-bid assembly"
                );
            }
            _ => {
                assert_eq!(
                    body, baseline,
                    "should emit byte-identical timeout fallback"
                );
                assert!(
                    (Duration::from_millis(600)..Duration::from_secs(3))
                        .contains(&completed.duration_since(started)),
                    "should bound the hung control"
                );
            }
        }
        if case != "disabled" {
            assert_eq!(
                headers["cache-control"], "no-store, private",
                "should prohibit shared caching"
            );
        }
        let events = origin.events();
        let lookup = origin::lookup_for_request(&events, started, completed, case != "disabled")?;
        if let Some(lookup) = lookup {
            assert_eq!(
                lookup.data["complete"], true,
                "should observe complete HTML"
            );
            assert_eq!(
                lookup.data["sha256"].as_str().map(str::len),
                Some(64),
                "should hash complete output"
            );
            let timeout = lookup.data["transport_timeout_ms"]
                .as_u64()
                .expect("should report the transport budget");
            if adapter == "fastly" {
                assert!(
                    (1..=750).contains(&timeout),
                    "should bound blocking host calls"
                );
            } else {
                assert_eq!(
                    timeout, 30000,
                    "should time out before the SDK transport deadline"
                );
            }
        }
        if matches!(case, "success" | "auction" | "streaming") {
            let finish = events
                .iter()
                .find(|event| {
                    event.kind == "lookup_complete" && event.at >= started && event.at <= completed
                })
                .expect("should complete the current lookup");
            if case == "streaming" {
                assert!(
                    first_byte_at + Duration::from_millis(50) < finish.at,
                    "should deliver actual HTML while EOF I/O remains pending"
                );
            }
        }
        let lookup_elapsed = lookup.map(|event| completed.duration_since(event.at));
        let measured = if case == "hung" {
            let mut diagnostic = fs::read_to_string(&log).change_context(HarnessError::Io)?;
            if adapter == "spin" {
                for path in fs::read_dir(harness.workspace.join("spin-logs"))
                    .change_context(HarnessError::Io)?
                {
                    let path = path.change_context(HarnessError::Io)?.path();
                    if path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().contains("stderr"))
                    {
                        diagnostic
                            .push_str(&fs::read_to_string(path).change_context(HarnessError::Io)?);
                    }
                }
            }
            measured_terminal_ms(
                &adapter,
                &diagnostic,
                lookup_elapsed.expect("should measure the hung lookup"),
            )?
        } else {
            None
        };
        let timing = json!({"request_started_seconds": started.duration_since(origin.started).as_secs_f64(), "request_seconds": completed.duration_since(started).as_secs_f64(), "first_body_byte_seconds": first_byte_at.duration_since(started).as_secs_f64(), "terminal_ms": measured, "lookup_to_response_seconds": lookup_elapsed.map(|elapsed| elapsed.as_secs_f64()), "configured_budget_ms": 750, "cooperative_timer": adapter != "fastly"});
        fs::write(
            harness.workspace.join(format!("{case}.timing.json")),
            timing.to_string(),
        )
        .change_context(HarnessError::Io)?;
        origin.write_events(&harness.workspace.join("events.json"))?;
        log::info!("PASS {adapter} {case}: {timing}");
        drop(process);
    }
    Ok(())
}

#[test]
fn viceroy_preflight_rejects_old_or_invalid_versions() {
    for version in ["viceroy 0.17.0", "unknown"] {
        assert!(
            validate_viceroy(version).is_err(),
            "should reject {version}"
        );
    }
    for version in ["viceroy 0.19.0", "viceroy 0.20.1", "viceroy 1.0.0"] {
        assert!(validate_viceroy(version).is_ok(), "should accept {version}");
    }
}

#[test]
fn timeout_proof_requires_measured_evidence() {
    for adapter in ["axum", "cloudflare", "spin"] {
        assert!(
            measured_terminal_ms(adapter, "missing", Duration::from_millis(750)).is_err(),
            "should require a timer diagnostic"
        );
        assert_eq!(
            measured_terminal_ms(
                adapter,
                "times out after 751 ms",
                Duration::from_millis(750)
            )
            .expect("should parse measured expiry"),
            Some(751),
            "should report actual expiry"
        );
    }
    assert!(
        measured_terminal_ms(
            "fastly",
            "retains input: unavailable",
            Duration::from_millis(100)
        )
        .is_err(),
        "should reject early unmeasured failure"
    );
    assert!(
        measured_terminal_ms(
            "fastly",
            "retains input: unavailable",
            Duration::from_secs(3)
        )
        .is_err(),
        "should reject escaped budget"
    );
    assert_eq!(
        measured_terminal_ms(
            "fastly",
            "retains input: unavailable",
            Duration::from_millis(749)
        )
        .expect("should accept measured transport fallback"),
        None,
        "should not invent a cooperative timer measurement"
    );
}
