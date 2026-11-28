//! Executes supplied shipping artifacts; this worker never builds a guest or CLI.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use error_stack::{Report, ResultExt as _};
use reqwest::blocking::Client;
use serde_json::{Value, json};

use super::process::{self, Cancellation, ManagedChild};
use super::{HarnessError, HarnessResult, config};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const FIXTURE: &str = "crates/trusted-server-integration-tests/fixtures/auctioneer/tls-origin.mjs";
const BROWSER: &str = "crates/trusted-server-integration-tests/browser";
const GUEST: &str = "/inputs/guest.wasm";
const CLI: &str = "/inputs/ts";

struct Worker {
    root: PathBuf,
    cancellation: Cancellation,
    environment: BTreeMap<OsString, OsString>,
}

impl Worker {
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir("/work")
            .env_clear()
            .envs(&self.environment);
        command
    }

    fn run(&self, command: &mut Command, name: &str) -> HarnessResult<()> {
        process::logged_command(
            command,
            &Path::new("/results").join(format!("{name}.log")),
            &self.cancellation,
        )
    }

    fn serve(&self, command: &mut Command, name: &str) -> HarnessResult<ManagedChild> {
        let log = File::create(Path::new("/results").join(format!("{name}.log")))
            .change_context(HarnessError::Io)?;
        command
            .stdout(Stdio::from(
                log.try_clone().change_context(HarnessError::Io)?,
            ))
            .stderr(Stdio::from(log));
        ManagedChild::spawn(command, &self.cancellation)
    }

    fn ready(&self, child: &mut ManagedChild, url: &str) -> HarnessResult<()> {
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .change_context(HarnessError::Http)?;
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            self.cancellation.check()?;
            if client
                .get(url)
                .send()
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            if child.status()?.is_some() || Instant::now() >= until {
                return Err(Report::new(HarnessError::Runtime)
                    .attach(format!("listener does not start: {url}")));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn configure(&self, bidder_port: u16) -> HarnessResult<()> {
        let template =
            fs::read_to_string("/work/template.toml").change_context(HarnessError::Io)?;
        let bundle: Value = serde_json::from_slice(
            &fs::read("/inputs/prebid/manifest.json").change_context(HarnessError::Io)?,
        )
        .change_context(HarnessError::Configuration)?;
        let value = production_config(&template, &bundle, bidder_port)?;
        fs::write("/work/trusted-server.toml", config::serialize(&value)?)
            .change_context(HarnessError::Io)?;
        fs::write("/work/fastly.toml", "manifest_version = 3\nname = 'example-production-auction'\nlanguage = 'rust'\n[local_server]\n").change_context(HarnessError::Io)?;
        self.run(
            self.command(CLI).args([
                "config",
                "push",
                "--adapter",
                "fastly",
                "--local",
                "--manifest",
                "/work/edgezero.toml",
                "--app-config",
                "/work/trusted-server.toml",
                "--yes",
                "--no-diff",
                "--no-env",
            ]),
            &format!("push-{bidder_port}"),
        )?;
        let mut manifest = config::parse(
            &fs::read_to_string("/work/fastly.toml").change_context(HarnessError::Io)?,
        )?;
        let secrets = config::SECRETS
            .iter()
            .map(|(key, value)| {
                let mut entry = toml::map::Map::new();
                entry.insert("key".to_owned(), toml::Value::String((*key).to_owned()));
                entry.insert("data".to_owned(), toml::Value::String((*value).to_owned()));
                toml::Value::Table(entry)
            })
            .collect();
        manifest["local_server"]
            .as_table_mut()
            .ok_or_else(|| Report::new(HarnessError::Configuration))?
            .insert(
                "secret_stores".to_owned(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "trusted_server_secrets".to_owned(),
                    toml::Value::Array(secrets),
                )])),
            );
        fs::write("/work/fastly.toml", config::serialize(&manifest)?)
            .change_context(HarnessError::Io)
    }

    fn viceroy(&self, name: &str) -> HarnessResult<ManagedChild> {
        let mut command = self.command("viceroy");
        // Required trust is owned by this worker and injected after filtering.
        command
            .args([
                "--config",
                "/work/fastly.toml",
                "--addr",
                "127.0.0.1:18081",
                GUEST,
            ])
            .env("SSL_CERT_FILE", "/work/ca.pem");
        let mut child = self.serve(&mut command, name)?;
        self.ready(&mut child, "http://localhost:18081/health")?;
        Ok(child)
    }
}

fn production_config(
    template: &str,
    bundle: &Value,
    bidder_port: u16,
) -> HarnessResult<toml::Value> {
    let mut value = config::parse(template)?;
    for (key, replacement) in [
        ("domain", "localhost"),
        ("cookie_domain", "localhost"),
        ("origin_url", "http://127.0.0.1:18080"),
        ("public_origin", "http://localhost:18081"),
    ] {
        config::replace(
            &mut value,
            &["publisher", key],
            toml::Value::String(replacement.to_owned()),
        )?;
    }
    value["publisher"]
        .as_table_mut()
        .ok_or_else(|| Report::new(HarnessError::Configuration))?
        .insert(
            "allow_async_html_finish".to_owned(),
            toml::Value::Boolean(true),
        );
    value["auction"]["enabled"] = toml::Value::Boolean(true);
    value["auction"]["providers"]["pbs-main"]["endpoint"] = toml::Value::String(format!(
        "https://pbs.example.com:{bidder_port}/openrtb2/auction"
    ));
    let slot = config::parse(
        "id = 'example-slot'\ndiv_id = 'example-slot'\ngam_unit_path = '/123456789/example-slot'\npage_patterns = ['/browser-publisher']\nformats = [{width=300,height=250}]\n[providers.prebid.bidders.example-bidder]\nplacement = 'fixture'\n",
    )?;
    value["creative_opportunities"]
        .as_table_mut()
        .ok_or_else(|| Report::new(HarnessError::Configuration))?
        .insert("slot".to_owned(), toml::Value::Array(vec![slot]));
    value["creative_opportunities"]["auction_timeout_ms"] = toml::Value::Integer(2000);
    value["proxy"]
        .as_table_mut()
        .ok_or_else(|| Report::new(HarnessError::Configuration))?
        .insert(
            "allowed_domains".to_owned(),
            toml::Value::Array(
                ["assets.example.com", "creative.example.com"]
                    .map(|host| toml::Value::String(host.to_owned()))
                    .to_vec(),
            ),
        );
    value["integrations"]["prebid"]["enabled"] = toml::Value::Boolean(true);
    value["integrations"]["gpt"]["enabled"] = toml::Value::Boolean(true);
    let prebid = value["integrations"]["prebid"]
        .as_table_mut()
        .ok_or_else(|| Report::new(HarnessError::Configuration))?;
    prebid.insert(
        "external_bundle_url".to_owned(),
        toml::Value::String("https://assets.example.com:18443/external-prebid.js".to_owned()),
    );
    for (field, key) in [
        ("external_bundle_sha256", "sha256"),
        ("external_bundle_sri", "sri"),
    ] {
        let metadata = bundle[key]
            .as_str()
            .ok_or_else(|| Report::new(HarnessError::Configuration))?;
        prebid.insert(field.to_owned(), toml::Value::String(metadata.to_owned()));
    }
    Ok(value)
}

fn certificates() -> HarnessResult<()> {
    let key = rcgen::KeyPair::generate().change_context(HarnessError::Configuration)?;
    let mut parameters = rcgen::CertificateParams::new(Vec::<String>::new())
        .change_context(HarnessError::Configuration)?;
    parameters.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    parameters.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    parameters
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Example production fixture CA");
    let ca = parameters
        .self_signed(&key)
        .change_context(HarnessError::Configuration)?;
    fs::write("/work/ca.pem", ca.pem()).change_context(HarnessError::Io)?;
    for (name, hosts, trusted) in [
        (
            "matching",
            vec![
                "pbs.example.com",
                "assets.example.com",
                "creative.example.com",
            ],
            true,
        ),
        ("wrong-san", vec!["other.example.com"], true),
        (
            "untrusted",
            vec!["pbs.example.com", "creative.example.com"],
            false,
        ),
    ] {
        let leaf_key = rcgen::KeyPair::generate().change_context(HarnessError::Configuration)?;
        let parameters = rcgen::CertificateParams::new(
            hosts
                .iter()
                .map(|host| (*host).to_owned())
                .collect::<Vec<_>>(),
        )
        .change_context(HarnessError::Configuration)?;
        let certificate = if trusted {
            parameters.signed_by(&leaf_key, &ca, &key)
        } else {
            parameters.self_signed(&leaf_key)
        }
        .change_context(HarnessError::Configuration)?;
        fs::write(format!("/work/{name}.key"), leaf_key.serialize_pem())
            .change_context(HarnessError::Io)?;
        fs::write(format!("/work/{name}.pem"), certificate.pem())
            .change_context(HarnessError::Io)?;
    }
    Ok(())
}

pub(super) fn run() -> HarnessResult<()> {
    if std::env::consts::OS != "linux"
        || std::env::consts::ARCH != "x86_64"
        || std::env::var("PRODUCTION_AUCTION_WORKER").as_deref() != Ok("pbs")
    {
        return Err(Report::new(HarnessError::Configuration)
            .attach("use the isolated Linux production coordinator"));
    }
    let runtime = tokio::runtime::Runtime::new().change_context(HarnessError::Runtime)?;
    let cancellation = Cancellation::default();
    cancellation.watch(runtime.handle())?;
    let mut environment = process::build_environment(std::env::vars_os());
    for (name, value) in [
        ("HOME", "/work/home"),
        ("TMPDIR", "/tmp"),
        ("XDG_CONFIG_HOME", "/work/cli-config"),
    ] {
        environment.insert(OsString::from(name), OsString::from(value));
    }
    let worker = Worker {
        root: PathBuf::from(ROOT)
            .canonicalize()
            .change_context(HarnessError::Io)?,
        cancellation,
        environment,
    };
    certificates()?;
    fs::write("/work/bid-mode.json", json!({"empty":false}).to_string())
        .change_context(HarnessError::Io)?;
    fs::create_dir_all("/work/home/.pki/nssdb").change_context(HarnessError::Io)?;
    worker.run(
        worker.command("certutil").args([
            "-N",
            "--empty-password",
            "-d",
            "sql:/work/home/.pki/nssdb",
        ]),
        "nss-init",
    )?;
    worker.run(
        worker.command("certutil").args([
            "-A",
            "-t",
            "C,,",
            "-n",
            "Example fixture CA",
            "-i",
            "/work/ca.pem",
            "-d",
            "sql:/work/home/.pki/nssdb",
        ]),
        "nss-import",
    )?;
    worker.run(
        worker.command("openssl").args([
            "x509",
            "-in",
            "/work/ca.pem",
            "-noout",
            "-fingerprint",
            "-sha256",
        ]),
        "ca-fingerprint",
    )?;
    let listeners = json!({
        "publisherOrigin": "http://localhost:18081", "creativeOrigin": "https://creative.example.com:18443",
        "listeners": [
            {"port":18080,"tls":false},
            {"port":18443,"tls":true,"certificate":"matching"},
            {"port":18444,"tls":true,"certificate":"wrong-san"},
            {"port":18445,"tls":true,"certificate":"untrusted"}
        ],
        "consentCookie": super::consent_cookie(),
        "allowedOrigins": ["http://localhost:18081", "https://creative.example.com:18443", "https://creative.example.com:18444", "https://creative.example.com:18445"]
    });
    fs::write("/work/listeners.json", listeners.to_string()).change_context(HarnessError::Io)?;
    let mut origin = worker.serve(
        worker.command("node").arg(worker.root.join(FIXTURE)),
        "fixture-origin",
    )?;
    worker.ready(&mut origin, "http://127.0.0.1:18080/health")?;
    worker.run(worker.command(CLI).args(["--version"]), "cli-version")?;
    let identity: Value = serde_json::from_slice(
        &fs::read("/inputs/manifest.json").change_context(HarnessError::Io)?,
    )
    .change_context(HarnessError::Configuration)?;
    let source = identity["productCommit"]
        .as_str()
        .ok_or_else(|| Report::new(HarnessError::Configuration))?;
    let version =
        fs::read_to_string("/results/cli-version.log").change_context(HarnessError::Io)?;
    if !version.trim().ends_with(&format!(
        "source={source} target=x86_64-unknown-linux-gnu variant=default"
    )) {
        return Err(Report::new(HarnessError::Configuration)
            .attach("supplied Linux CLI source/variant differs from its producer identity"));
    }
    worker.run(
        worker.command("sha256sum").args([
            GUEST,
            CLI,
            "/opt/production-worker",
            "/usr/local/bin/viceroy",
            "/opt/node/bin/node",
        ]),
        "executables-before",
    )?;
    worker.run(
        worker
            .command(CLI)
            .args(["config", "init", "--app-config", "/work/template.toml"]),
        "config-init",
    )?;
    fs::write(
        "/work/edgezero.toml",
        config::fixture_manifest(
            include_str!("../../../../edgezero.toml"),
            &worker.root,
            Path::new("/work"),
        )?,
    )
    .change_context(HarnessError::Io)?;
    worker.configure(18443)?;
    let viceroy = worker.viceroy("viceroy-positive")?;
    let observed = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .change_context(HarnessError::Http)?
        .get("http://localhost:18081/browser-direct")
        .header("cookie", super::consent_cookie())
        .header("accept", "text/html")
        .header("sec-fetch-dest", "document")
        .header("sec-fetch-mode", "navigate")
        .send()
        .change_context(HarnessError::Http)?
        .error_for_status()
        .change_context(HarnessError::Http)?;
    let guest_source = observed
        .headers()
        .get("x-ts-version")
        .and_then(|header| header.to_str().ok());
    if guest_source != Some(source) {
        return Err(Report::new(HarnessError::Configuration).attach(format!(
            "executed guest source differs from producer identity: {guest_source:?}"
        )));
    }
    fs::write(
        "/results/observed-guest-source.json",
        json!({"x-ts-version":guest_source,"productCommit":source}).to_string(),
    )
    .change_context(HarnessError::Io)?;
    let browser = worker.root.join(BROWSER);
    let mut command = worker.command("node");
    command
        .current_dir(&browser)
        .args([
            "node_modules/@playwright/test/cli.js",
            "test",
            "--config",
            "playwright.auction-runtime.config.ts",
        ])
        .env("AUCTIONEER_BROWSER_ORIGIN", "http://localhost:18081")
        .env("AUCTIONEER_EVIDENCE_MANIFEST", "/work/listeners.json")
        .env("PLAYWRIGHT_BROWSERS_PATH", "/ms-playwright")
        .env("HOME", "/work/home");
    worker.run(&mut command, "browser")?;
    drop(viceroy);
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .change_context(HarnessError::Http)?;
    for (name, port) in [("wrong-san", 18444), ("untrusted-ca", 18445)] {
        worker.configure(port)?;
        let viceroy = worker.viceroy(&format!("viceroy-{name}"))?;
        let offset = fs::read_to_string("/results/receipts.jsonl")
            .change_context(HarnessError::Io)?
            .lines()
            .count();
        let response: Value = client.post("http://localhost:18081/auction")
            .header("cookie", super::consent_cookie())
            .json(&json!({"adUnits":[{"code":"example-slot","mediaTypes":{"banner":{"sizes":[[300,250]]}},"bids":[{"bidder":"example-bidder","params":{"placement":"fixture"}}]}]}))
            .send().change_context(HarnessError::Http)?.error_for_status().change_context(HarnessError::Http)?.json().change_context(HarnessError::Http)?;
        fs::write(
            format!("/results/{name}-response.json"),
            response.to_string(),
        )
        .change_context(HarnessError::Io)?;
        if !response["id"].is_string()
            || !response
                .get("seatbid")
                .is_none_or(|seats| seats.as_array().is_some_and(Vec::is_empty))
        {
            return Err(Report::new(HarnessError::Runtime).attach(format!(
                "TLS refusal should return no winners: {name}: {response}"
            )));
        }
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let log =
                fs::read_to_string("/results/receipts.jsonl").change_context(HarnessError::Io)?;
            let fresh = log
                .lines()
                .skip(offset)
                .map(serde_json::from_str::<Value>)
                .collect::<Result<Vec<_>, _>>()
                .change_context(HarnessError::Configuration)?;
            let certificate_error = fresh.iter().any(|receipt| {
                let message = receipt["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                receipt["port"] == port
                    && receipt["sni"] == "pbs.example.com"
                    && receipt["tlsError"].is_string()
                    && (message.contains("certificate") || message.contains("unknown ca"))
            });
            if certificate_error {
                break;
            }
            if Instant::now() >= until {
                return Err(Report::new(HarnessError::Runtime).attach(format!(
                    "should observe a fresh bidder TLS certificate rejection for {name}: {fresh:?}"
                )));
            }
            worker.cancellation.check()?;
            thread::sleep(Duration::from_millis(20));
        }
        drop(viceroy);
    }
    let receipts =
        fs::read_to_string("/results/receipts.jsonl").change_context(HarnessError::Io)?;
    let receipts = receipts
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .change_context(HarnessError::Configuration)?;
    if !receipts.iter().any(|receipt| {
        receipt["path"] == "/openrtb2/auction"
            && receipt["host"] == "pbs.example.com:18443"
            && receipt["sni"] == "pbs.example.com"
    }) || receipts
        .iter()
        .any(|receipt| receipt["path"] == "/openrtb2/auction" && receipt["port"] != 18443)
    {
        return Err(Report::new(HarnessError::Runtime).attach(
            "should observe verified original bidder Host/SNI and no rejected-TLS HTTP receipts",
        ));
    }
    fs::write("/results/worker.json", json!({"mode":"production-pbs-baseline","auctioneerAcceptance":false,"tlsNegativeCases":["wrong-san","untrusted-ca"],"listeners":listeners,"prebid":serde_json::from_slice::<Value>(&fs::read("/inputs/prebid/manifest.json").change_context(HarnessError::Io)?).change_context(HarnessError::Configuration)?}).to_string()).change_context(HarnessError::Io)?;
    worker.run(
        worker.command("sha256sum").args([
            GUEST,
            CLI,
            "/opt/production-worker",
            "/usr/local/bin/viceroy",
            "/opt/node/bin/node",
            "/work/home/.pki/nssdb/cert9.db",
        ]),
        "executables-and-trust-after",
    )?;
    drop(origin);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_config_uses_verified_remote_demand_and_publisher_only_slots() {
        let bundle = json!({"sha256":"a".repeat(64),"sri":format!("sha384-{}", "A".repeat(64))});
        let value = production_config(
            include_str!("../../../../trusted-server.example.toml"),
            &bundle,
            18443,
        )
        .expect("should configure the production baseline");
        assert_eq!(
            value["auction"]["providers"]["pbs-main"]["execution"].as_str(),
            Some("remote"),
            "should retain remote execution"
        );
        assert_eq!(
            value["creative_opportunities"]["slot"][0]["page_patterns"][0].as_str(),
            Some("/browser-publisher"),
            "should avoid SSAT on the direct path"
        );
        assert!(
            value["integrations"]
                .get("html_finalization_fixture")
                .is_none(),
            "should omit instrumented fixture hooks"
        );
        let settings: trusted_server_core::settings::Settings = value
            .clone()
            .try_into()
            .expect("should deserialize the production fixture");
        trusted_server_core::config::TrustedServerAppConfig::new(settings)
            .expect("should pass real push-safe configuration validation");
        assert_eq!(
            value["integrations"]["prebid"]["external_bundle_sha256"].as_str(),
            Some("a".repeat(64).as_str()),
            "should consume supplied asset integrity"
        );
    }
}
