//! Local runtime regression inputs. Only the original configuration provisions credentials.
use core::error::Error;
use std::{env, io::Write as _, time::Duration};

use edgezero_core::blob_envelope::BlobEnvelope;
use serde_json::{Value, json};
use trusted_server_core::{
    config::TrustedServerAppConfig,
    platform::{BackendNamingPolicy, PlatformBackendSpec},
};
use trusted_server_integration_tests::native_fixtures;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn envelope(data: Value) -> Result<String> {
    let envelope = BlobEnvelope::new(data, "2026-06-23T00:00:00Z".to_owned());
    envelope
        .verify()
        .map_err(|error| format!("invalid generated checksum: {error:?}"))?;
    Ok(serde_json::to_string(&envelope)?)
}

fn fixtures(adapter: &str, trusted: u16, bad: u16, sink: u16) -> Result<Value> {
    let source = format!(
        r#"
[[handlers]]
path = "^/_ts/admin"
username = "fictional-admin"

[publisher]
domain = "127.0.0.1"
cookie_domain = "127.0.0.1"
origin_url = "https://localhost:{trusted}"

[ec]
[[ec.partners]]
name = "Fictional local pull partner"
source_domain = "partner.example.test"
pull_sync_enabled = true
pull_sync_url = "https://localhost:{trusted}/pull"
pull_sync_allowed_domains = ["localhost"]

[proxy]
certificate_check = false
[[proxy.asset_routes]]
prefix = "/assets/"
origin_url = "https://localhost:{trusted}"
[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
session_token_enabled = true

[[proxy.asset_routes]]
prefix = "/sink-control/"
origin_url = "https://localhost:{sink}"
[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
session_token_enabled = true

[[proxy.asset_routes]]
prefix = "/bad-cert/"
origin_url = "https://localhost:{bad}"
[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
session_token_enabled = true
"#
    );
    let source = if adapter == "fastly" {
        source
    } else {
        format!("{source}\n[integrations.secret_binding_runtime_fixture]\nenabled = true\n")
    };
    let config: TrustedServerAppConfig = toml::from_str(&source)?;
    let config = TrustedServerAppConfig::new(config.into_settings())
        .map_err(|error| format!("invalid local fixture: {error:?}"))?;
    let native = native_fixtures(config.settings(), adapter)?;
    let bindings: Vec<_> = native
        .iter()
        .map(|fixture| {
            json!({
                "descriptor_id": fixture.descriptor_id,
                "native": fixture.native,
                "value": fixture.value,
            })
        })
        .collect();
    let data = serde_json::to_value(config)?;
    let round_trip: TrustedServerAppConfig = serde_json::from_value(data.clone())?;
    TrustedServerAppConfig::new(round_trip.into_settings())
        .map_err(|error| format!("invalid serialized fixture: {error:?}"))?;
    let mut cases = serde_json::Map::new();
    cases.insert("original".to_owned(), json!(envelope(data.clone())?));

    // Recompute every checksum, but never compile/provision the changed context.
    let mut changed = data.clone();
    changed["proxy"]["asset_routes"][0]["origin_url"] = json!(format!("https://localhost:{sink}"));
    cases.insert("s3-destination".to_owned(), json!(envelope(changed)?));
    let mut changed = data.clone();
    changed["ec"]["partners"][0]["pull_sync_url"] =
        json!(format!("https://localhost:{sink}/changed-pull"));
    cases.insert("ec-url".to_owned(), json!(envelope(changed)?));
    let mut changed = data.clone();
    changed["ec"]["partners"][0]["pull_sync_allowed_domains"] =
        json!(["localhost", "extra.example.test"]);
    cases.insert("ec-allowlist".to_owned(), json!(envelope(changed)?));
    let mut changed = data;
    let ec = native
        .iter()
        .find(|fixture| fixture.descriptor_id == "ec.passphrase")
        .ok_or("missing EC control fixture")?;
    changed["proxy"]["asset_routes"][0]["auth"]["secret_access_key"] = json!({
        "store": "trusted_server_secrets", "key": ec.native.key,
    });
    cases.insert("cross-field".to_owned(), json!(envelope(changed)?));

    // Asset proxy uses 15-second transport timers. Predict names through production
    // policy rather than copying its digest/encoding algorithm into the driver.
    let backends: Vec<_> = [trusted, bad, sink].into_iter().map(|port| {
        let spec = PlatformBackendSpec {
            scheme: "https".to_owned(), host: "localhost".to_owned(), port: Some(port),
            host_header_override: None, certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15), discriminator: None,
        };
        BackendNamingPolicy::Fastly.predict(&spec)
            .map(|backend| json!({"name": backend.name, "url": format!("https://127.0.0.1:{port}")}))
            .map_err(|error| format!("backend prediction failed: {error:?}"))
    }).collect::<std::result::Result<_, _>>()?;
    Ok(json!({"bindings": bindings, "cases": cases, "backends": backends}))
}

fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() != 4 || !["fastly", "cloudflare", "spin"].contains(&args[0].as_str()) {
        return Err("usage: secret-bindings-fixture {fastly|cloudflare|spin} trusted-port bad-cert-port sink-port".into());
    }
    serde_json::to_writer(
        std::io::stdout().lock(),
        &fixtures(
            &args[0],
            args[1].parse()?,
            args[2].parse()?,
            args[3].parse()?,
        )?,
    )?;
    std::io::stdout().lock().write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use error_stack::Report;
    use trusted_server_core::{
        config_payload::settings_from_config_blob,
        platform::{PlatformError, PlatformSecretStore, StoreId, StoreName},
    };

    struct Store(Value);
    impl PlatformSecretStore for Store {
        fn get_bytes(
            &self,
            _: &StoreName,
            key: &str,
        ) -> std::result::Result<Vec<u8>, Report<PlatformError>> {
            self.0
                .as_array()
                .expect("should contain fixture array")
                .iter()
                .find(|fixture| fixture["native"]["key"].as_str() == Some(key))
                .map(|fixture| {
                    fixture["value"]
                        .as_str()
                        .expect("should contain fictional value")
                        .as_bytes()
                        .to_vec()
                })
                .ok_or_else(|| Report::new(PlatformError::SecretStore))
        }
        fn create(
            &self,
            _: &StoreId,
            _: &str,
            _: &str,
        ) -> std::result::Result<(), Report<PlatformError>> {
            Ok(())
        }
        fn delete(&self, _: &StoreId, _: &str) -> std::result::Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    #[test]
    fn original_loads_but_valid_checksum_mutations_cannot_reuse_native_credentials() {
        for adapter in ["fastly", "cloudflare", "spin"] {
            let fixture = fixtures(adapter, 12340, 12341, 12342).expect("should generate fixtures");
            let store = Store(fixture["bindings"].clone());
            for (case, raw) in fixture["cases"].as_object().expect("should contain cases") {
                let raw = raw.as_str().expect("should contain serialized envelope");
                serde_json::from_str::<BlobEnvelope>(raw)
                    .expect("should parse envelope")
                    .verify()
                    .expect("should verify checksum");
                if case == "ec-allowlist" {
                    let envelope: BlobEnvelope =
                        serde_json::from_str(raw).expect("should parse envelope");
                    let config: TrustedServerAppConfig =
                        serde_json::from_value(envelope.into_data())
                            .expect("should parse mutated feature config");
                    let error = TrustedServerAppConfig::new(config.into_settings())
                        .expect_err("should reject contradictory pull hostname assertion");
                    assert!(format!("{error:?}").contains("pull endpoint assertion mismatch"));
                }
                let loaded =
                    settings_from_config_blob(raw, &store, &"trusted_server_secrets".into());
                if case == "original" {
                    assert!(loaded.is_ok(), "original {adapter}: {loaded:?}");
                } else {
                    let error = loaded.expect_err("should reject mutation");
                    let diagnostic = format!("{error:?}");
                    assert!(
                        diagnostic.contains(if case == "cross-field" {
                            "removed credential reference"
                        } else if case == "ec-allowlist" {
                            "invalid unresolved configuration"
                        } else {
                            "native credential unavailable"
                        }),
                        "{case}: {diagnostic}"
                    );
                }
            }
        }
    }
}
