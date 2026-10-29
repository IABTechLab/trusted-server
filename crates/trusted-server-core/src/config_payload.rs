//! Verified app-config loading and atomic native credential snapshots.

use std::ops::Deref;

use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;

use crate::config::TrustedServerAppConfig;
use crate::error::TrustedServerError;
use crate::platform::{PlatformSecretStore, StoreName};
use crate::redacted::Redacted;
use crate::secret_bindings::{ResolvedBinding, resolve_bindings};
use crate::settings::{AssetOriginAuth, Settings};

/// Logical native store for app credentials. Physical mapping is deployment-owned.
pub const DEFAULT_SECRET_STORE_ID: &str = "trusted_server_secrets";
/// Default logical config-store ID, derived from the portable manifest.
pub const DEFAULT_CONFIG_STORE_ID: &str = env!("TRUSTED_SERVER_DEFAULT_CONFIG_STORE_ID");
/// Config blob entry shared by production, staging, and local logical mappings.
pub const CONFIG_BLOB_KEY: &str = DEFAULT_CONFIG_STORE_ID;

/// An atomically loaded settings and credential/target snapshot.
///
/// Only successful production loading constructs this type. It deliberately has
/// no serialization or mutable access. Cloned settings retain the same private
/// capabilities; exporters recheck their compiled identity before using them.
#[derive(Clone, Debug)]
pub struct LoadedConfig {
    settings: Settings,
}

impl LoadedConfig {
    /// Read the settings belonging to this successful snapshot.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}

impl Deref for LoadedConfig {
    type Target = Settings;

    fn deref(&self) -> &Self::Target {
        &self.settings
    }
}

fn invalid(path: &str, category: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("{path}: {category}"),
    })
}

/// Verify feature configuration, resolve its implicit native bindings, and
/// publish settings and compiled credential uses together.
///
/// # Errors
/// Rejects invalid envelopes, removed reference properties, malformed feature
/// configuration, missing/invalid native bindings, or invalid resolved settings.
/// Diagnostics never attach provider errors, values, or untrusted input snippets.
pub fn settings_from_config_blob(
    envelope_json: &str,
    secret_store: &dyn PlatformSecretStore,
    default_secret_store_name: &StoreName,
) -> Result<LoadedConfig, Report<TrustedServerError>> {
    let envelope: BlobEnvelope = serde_json::from_str(envelope_json)
        .map_err(|_| invalid("trusted_server", "invalid config envelope"))?;
    envelope
        .verify()
        .map_err(|_| invalid("trusted_server", "config integrity verification failed"))?;
    let config: TrustedServerAppConfig = serde_json::from_value(envelope.into_data())
        .map_err(|error| invalid("trusted_server", &error.to_string()))?;
    edgezero_core::app_config::validate_excluding_secrets(&config)
        .map_err(|_| invalid("trusted_server", "invalid unresolved configuration"))?;

    let mut settings = config.into_settings();
    let bindings = resolve_bindings(&settings, secret_store, default_secret_store_name)?;
    for binding in bindings.entries() {
        install_shadow(&mut settings, binding)?;
    }
    settings.resolved_bindings = bindings;
    let settings = Settings::finalize_deserialized(settings, "Configuration")
        .map_err(|_| invalid("trusted_server", "invalid resolved configuration"))?;
    crate::config::validate_settings_for_runtime(&settings)
        .map_err(|_| invalid("trusted_server", "invalid resolved configuration"))?;
    Ok(LoadedConfig { settings })
}

fn index(path: &str, prefix: &str) -> Result<usize, Report<TrustedServerError>> {
    path.strip_prefix(prefix)
        .and_then(|suffix| suffix.split_once(']'))
        .and_then(|(index, _)| index.parse().ok())
        .ok_or_else(|| invalid("secret_bindings", "invalid compiled schema path"))
}

// These shadows support existing local validators and non-exporting consumers.
// They are never an outbound authority and cannot appear in serialized config.
fn install_shadow(
    settings: &mut Settings,
    binding: &ResolvedBinding,
) -> Result<(), Report<TrustedServerError>> {
    let compiled = binding.compiled();
    let value = || Redacted::new(binding.credential().to_owned());
    match compiled.descriptor_id() {
        "publisher.proxy" => settings.publisher.proxy_secret = value(),
        "ec.passphrase" => settings.ec.passphrase = value(),
        "ec.inbound" | "ec.pull" => {
            let partner = settings
                .ec
                .partners
                .get_mut(index(compiled.field_path(), "ec.partners[")?)
                .ok_or_else(|| invalid("ec.partners", "invalid compiled identity"))?;
            if compiled.descriptor_id() == "ec.inbound" {
                partner.api_token = Some(value());
            } else {
                partner.ts_pull_token = Some(value());
            }
        }
        "handler.password" => {
            settings
                .handlers
                .get_mut(index(compiled.field_path(), "handlers[")?)
                .ok_or_else(|| invalid("handlers", "invalid compiled identity"))?
                .password = value();
        }
        "clientip.shared" => {
            settings
                .trusted_client_ip
                .as_mut()
                .ok_or_else(|| invalid("trusted_client_ip", "invalid compiled identity"))?
                .shared_secret = value();
        }
        "tinybird.auction" => settings.tinybird.auction_token_secret = Some(value()),
        // DataDome consumes private uses directly. Never copy native values into
        // its serializable, dynamically typed integration configuration.
        "datadome.server" | "datadome.bypass" => {}
        "s3.access-id" | "s3.secret" | "s3.session" => {
            let route = settings
                .proxy
                .asset_routes
                .get_mut(index(compiled.field_path(), "proxy.asset_routes[")?)
                .ok_or_else(|| invalid("proxy.asset_routes", "invalid compiled identity"))?;
            let Some(AssetOriginAuth::S3SigV4(auth)) = route.auth.as_mut() else {
                return Err(invalid("proxy.asset_routes", "invalid compiled identity"));
            };
            match compiled.descriptor_id() {
                "s3.access-id" => auth.access_key_id = value(),
                "s3.secret" => auth.secret_access_key = value(),
                _ => auth.session_token = Some(value()),
            }
        }
        _ => return Err(invalid("secret_bindings", "unknown compiled purpose")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentContext;
    use crate::ec::EcContext;
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_types::KvEntry;
    use crate::ec::pull_sync::{build_pull_sync_context, dispatch_pull_sync};
    use crate::ec::rate_limiter::RateLimiter;
    use crate::ec::registry::PartnerRegistry;
    use crate::platform::test_support::{
        NoopConfigStore, NoopGeo, NoopSecretStore, RecordingBackend, StubHttpClient,
    };
    use crate::platform::{ClientInfo, PlatformError, RuntimeServices, StoreId};
    use crate::secret_bindings::compile_bindings;
    use crate::test_support::tests::crate_test_settings_str;

    #[derive(Default)]
    struct Store {
        values: BTreeMap<String, Vec<u8>>,
        reads: Mutex<Vec<String>>,
    }

    impl PlatformSecretStore for Store {
        fn get_bytes(&self, _: &StoreName, key: &str) -> Result<Vec<u8>, Report<PlatformError>> {
            self.reads
                .lock()
                .expect("should lock read spy")
                .push(key.to_owned());
            self.values.get(key).cloned().ok_or_else(|| {
                Report::new(PlatformError::SecretStore)
                    .attach("fictional-provider-diagnostic-must-not-leak")
            })
        }
        fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            unreachable!("loader never writes native credentials")
        }
        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            unreachable!("loader never deletes native credentials")
        }
    }

    fn fixture() -> (serde_json::Value, Store) {
        let mut settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse resolved unit settings");
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
        let data = crate::config::unresolved_settings_value(&settings)
            .expect("should serialize feature config");
        let mut store = Store::default();
        for entry in compile_bindings(&settings)
            .expect("should compile roles")
            .entries()
        {
            let value = match entry.scope() {
                "proxy" => "fictional-proxy-credential-0123456789",
                "ecpass" => "fictional-passphrase-credential-0123456789",
                "handler" => "fictional-handler-credential-0123456789",
                _ => panic!("unexpected active fixture scope"),
            };
            store
                .values
                .insert(entry.key().to_owned(), value.as_bytes().to_vec());
        }
        (data, store)
    }

    fn envelope(data: serde_json::Value) -> String {
        serde_json::to_string(&BlobEnvelope::new(data, "2026-10-26T00:00:00Z".to_owned()))
            .expect("should encode a valid-checksum envelope")
    }

    fn all_roles_fixture() -> (serde_json::Value, Store) {
        let mut settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse resolved unit settings");
        settings.proxy.allowed_domains = vec!["*.example".to_owned(), "*.example.com".to_owned()];
        settings.ec.partners = vec![
            serde_json::from_value(serde_json::json!({
                "name": "Fictional partner", "source_domain": "partner.example.com",
                "api_token_enabled": true, "pull_sync_enabled": true,
                "pull_sync_url": "https://sync.example.com/pull",
                "pull_sync_allowed_domains": ["sync.example.com"]
            }))
            .expect("should parse partner features"),
        ];
        settings.trusted_client_ip = Some(
            serde_json::from_value(serde_json::json!({
                "ip_header": "x-fictional-client-ip", "auth_header": "x-fictional-auth"
            }))
            .expect("should parse trusted client IP features"),
        );
        settings.tinybird.enabled = true;
        settings.tinybird.api_host = "api.tinybird.example.com".to_owned();
        settings.integrations.insert(
            "datadome".to_owned(),
            serde_json::json!({
                "enabled": true, "enable_protection": true,
                "sdk_origin": "https://sdk.example.com", "api_origin": "https://api.example.com",
                "protection_api_origin": "https://protection.example.com",
                "protection_test_bypass": {"enabled": true}
            }),
        );
        let mut route =
            crate::settings::ProxyAssetRoute::new("/assets/", "https://bucket.example.com");
        route.auth = Some(AssetOriginAuth::S3SigV4(
            serde_json::from_value(serde_json::json!({
                "region": "us-east-1", "session_token_enabled": true
            }))
            .expect("should parse S3 features"),
        ));
        settings.proxy.asset_routes = vec![route];
        let mut store = Store::default();
        for entry in compile_bindings(&settings)
            .expect("should compile all roles")
            .entries()
        {
            store.values.insert(
                entry.key().to_owned(),
                format!("fictional-credential-{}-{}", entry.scope(), entry.key()).into_bytes(),
            );
        }
        (
            crate::config::unresolved_settings_value(&settings)
                .expect("should serialize feature config"),
            store,
        )
    }

    fn feature_pointer(field_path: &str) -> String {
        format!(
            "/{}",
            field_path
                .replace('[', "/")
                .replace(']', "")
                .replace('.', "/")
        )
    }

    #[test]
    fn secret_scoping_attacker_all_twelve_roles_load_through_verified_envelope() {
        let (data, store) = all_roles_fixture();
        let loaded =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect("should load every implicit role through production reconstruction");
        let scopes: std::collections::BTreeSet<_> = loaded
            .resolved_bindings
            .entries()
            .iter()
            .map(|binding| binding.compiled().scope())
            .collect();
        assert_eq!(scopes.len(), 12);
        let output = serde_json::to_string(loaded.settings())
            .expect("should serialize feature settings without native values");
        assert!(!output.contains("fictional-credential-"));
        assert!(
            store
                .reads
                .lock()
                .expect("should lock spy")
                .iter()
                .all(|key| key.starts_with("ts1_"))
        );
    }

    #[test]
    fn secret_scoping_attacker_reference_injection_rejects_all_twelve_roles_before_reads() {
        let (data, _) = all_roles_fixture();
        let config: TrustedServerAppConfig =
            serde_json::from_value(data.clone()).expect("should parse feature fixture");
        let plan = compile_bindings(config.settings()).expect("should compile fixture");
        for entry in plan.entries() {
            let (_, store) = all_roles_fixture();
            let mut attack = data.clone();
            let pointer = feature_pointer(entry.field_path());
            let (parent, field) = pointer.rsplit_once('/').expect("should split field path");
            attack
                .pointer_mut(parent)
                .and_then(serde_json::Value::as_object_mut)
                .expect("should find role owner")
                .insert(
                    field.to_owned(),
                    serde_json::json!("SENTINEL-unrelated-publisher-secret-key"),
                );
            let error = settings_from_config_blob(
                &envelope(attack),
                &store,
                &DEFAULT_SECRET_STORE_ID.into(),
            )
            .expect_err("should reject config-only reference injection");
            assert!(store.reads.lock().expect("should lock spy").is_empty());
            assert!(!format!("{error:?}").contains("SENTINEL-unrelated-publisher"));
        }
    }

    #[test]
    fn secret_scoping_attacker_retargeting_never_looks_up_the_old_credential() {
        for (pointer, value, descriptor, role_path) in [
            (
                "/ec/partners/0/pull_sync_url",
                "https://attacker.example.com/pull",
                "ec.pull",
                "ec.partners[0].ts_pull_token",
            ),
            (
                "/ec/partners/0/source_domain",
                "other.example.com",
                "ec.inbound",
                "ec.partners[0].api_token",
            ),
            (
                "/handlers/0/username",
                "other-user",
                "handler.password",
                "handlers[0].password",
            ),
            (
                "/tinybird/api_host",
                "attacker.example.com",
                "tinybird.auction",
                "tinybird.auction_token_secret",
            ),
            (
                "/tinybird/auction_dataset",
                "other_dataset",
                "tinybird.auction",
                "tinybird.auction_token_secret",
            ),
            (
                "/integrations/datadome/protection_api_origin",
                "https://attacker.example.com",
                "datadome.server",
                "integrations.datadome.server_side_key_secret_name",
            ),
            (
                "/proxy/asset_routes/0/origin_url",
                "https://attacker.example.com",
                "s3.access-id",
                "proxy.asset_routes[0].auth.access_key_id",
            ),
            (
                "/proxy/asset_routes/0/auth/region",
                "eu-west-1",
                "s3.access-id",
                "proxy.asset_routes[0].auth.access_key_id",
            ),
        ] {
            let (mut data, store) = all_roles_fixture();
            let config: TrustedServerAppConfig =
                serde_json::from_value(data.clone()).expect("should parse feature fixture");
            let plan = compile_bindings(config.settings()).expect("should compile fixture");
            let original_key = plan
                .entries()
                .iter()
                .find(|entry| {
                    entry.descriptor_id() == descriptor && entry.field_path() == role_path
                })
                .expect("should find original role")
                .key()
                .to_owned();
            *data
                .pointer_mut(pointer)
                .expect("should find mutable feature") = serde_json::json!(value);
            if descriptor == "ec.pull" {
                data["ec"]["partners"][0]["pull_sync_allowed_domains"] =
                    serde_json::json!(["attacker.example.com"]);
            }
            let _ =
                settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                    .expect_err(
                        "should reject new unprovisioned authorization despite valid checksum",
                    );
            assert!(
                !store
                    .reads
                    .lock()
                    .expect("should lock spy")
                    .contains(&original_key)
            );
        }
    }

    struct AllowPull;

    impl RateLimiter for AllowPull {
        fn exceeded(&self, _: &str, _: u32) -> Result<bool, Report<TrustedServerError>> {
            Ok(false)
        }
    }

    fn dispatch_loaded_pull(
        loaded: &LoadedConfig,
        backend: Arc<RecordingBackend>,
        http: Arc<StubHttpClient>,
    ) -> Result<(), Report<TrustedServerError>> {
        let registry = PartnerRegistry::from_settings(loaded)?;
        let graph = KvIdentityGraph::in_memory("binding-attack-fixture");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut entry = KvEntry::tombstone(1_000);
        entry.consent.ok = true;
        graph.create(&ec_id, &entry)?;
        let mut context = EcContext::new_for_test(
            Some(ec_id.clone()),
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..ConsentContext::default()
            },
        );
        context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        let pull = build_pull_sync_context(&context, &registry)
            .expect("should make the positive fixture eligible for pull sync");
        let services = RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(backend)
            .http_client(http)
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo::default())
            .build();
        dispatch_pull_sync(loaded, &graph, &registry, &AllowPull, &pull, &services);
        Ok(())
    }

    fn load_and_dispatch_pull(
        data: serde_json::Value,
        store: &Store,
        backend: Arc<RecordingBackend>,
        http: Arc<StubHttpClient>,
    ) -> Result<(), Report<TrustedServerError>> {
        let loaded =
            settings_from_config_blob(&envelope(data), store, &DEFAULT_SECRET_STORE_ID.into())?;
        dispatch_loaded_pull(&loaded, backend, http)
    }

    #[test]
    fn secret_scoping_attacker_pull_reconstruction_rejects_before_backend_and_http() {
        let (data, store) = all_roles_fixture();
        let backend = Arc::new(RecordingBackend::default());
        let http = Arc::new(StubHttpClient::new());
        http.push_response(200, br#"{"uid":"fictional-partner-uid"}"#.to_vec());
        load_and_dispatch_pull(data, &store, backend.clone(), http.clone())
            .expect("should dispatch the unchanged production-loaded config");
        assert_eq!(backend.specs().len(), 1, "should exercise backend creation");
        assert_eq!(
            http.recorded_request_uris().len(),
            1,
            "should exercise dispatch"
        );
        assert!(http.recorded_request_uris()[0].starts_with("https://sync.example.com/pull?"));

        for mutation in 0..8 {
            let (mut data, store) = all_roles_fixture();
            let config: TrustedServerAppConfig =
                serde_json::from_value(data.clone()).expect("should parse the authorized config");
            let plan =
                compile_bindings(config.settings()).expect("should compile authorized roles");
            let key_for = |role| {
                plan.entries()
                    .iter()
                    .find(|binding| binding.descriptor_id() == role)
                    .expect("should find the selected authorized role")
                    .key()
            };
            let pull_key = key_for("ec.pull");
            let partner = &mut data["ec"]["partners"][0];
            match mutation {
                0 => {
                    partner["pull_sync_url"] =
                        serde_json::json!("https://attacker.example.com/pull");
                    partner["pull_sync_allowed_domains"] =
                        serde_json::json!(["attacker.example.com"]);
                }
                1 => {
                    partner["pull_sync_url"] =
                        serde_json::json!("https://sync.example.com:8443/pull")
                }
                2 => partner["pull_sync_url"] = serde_json::json!("https://sync.example.com/other"),
                3 => partner["ts_pull_token"] = serde_json::json!(key_for("publisher.proxy")),
                4 => partner["ts_pull_token"] = serde_json::json!(key_for("ec.inbound")),
                5 => partner["ts_pull_token"] = serde_json::json!(key_for("handler.password")),
                6 => partner["pull_sync_url"] = serde_json::json!("http://sync.example.com/pull"),
                _ => {
                    partner["pull_sync_url"] =
                        serde_json::json!("https://sync.example.com/pull?export=1")
                }
            }
            data["proxy"]["certificate_check"] = serde_json::json!(false);
            let backend = Arc::new(RecordingBackend::default());
            let http = Arc::new(StubHttpClient::new());
            let _ = load_and_dispatch_pull(data, &store, backend.clone(), http.clone())
                .expect_err("should reject a valid-checksum config-only attack before dispatch");
            assert!(
                !store
                    .reads
                    .lock()
                    .expect("should lock reads")
                    .iter()
                    .any(|key| key == pull_key),
                "should never retrieve the original pull credential for the modified context"
            );
            assert!(
                backend.specs().is_empty(),
                "should create no backend for the attack"
            );
            assert!(
                http.recorded_request_uris().is_empty(),
                "should send no request for the attack"
            );
        }
    }

    #[test]
    fn secret_scoping_rotation_and_retirement_require_snapshot_reconstruction() {
        let (data, mut store) = all_roles_fixture();
        let original = settings_from_config_blob(
            &envelope(data.clone()),
            &store,
            &DEFAULT_SECRET_STORE_ID.into(),
        )
        .expect("should load the original authorized snapshot");
        let pull_key = original
            .resolved_bindings
            .entries()
            .iter()
            .find(|binding| binding.compiled().descriptor_id() == "ec.pull")
            .expect("should find the loaded pull capability")
            .compiled()
            .key()
            .to_owned();
        let old_value = String::from_utf8(store.values[&pull_key].clone())
            .expect("should decode the fictional original credential");
        let rotated_value = "fictional-rotated-pull-credential-32-bytes";
        store
            .values
            .insert(pull_key.clone(), rotated_value.as_bytes().to_vec());
        let refreshed = settings_from_config_blob(
            &envelope(data.clone()),
            &store,
            &DEFAULT_SECRET_STORE_ID.into(),
        )
        .expect("should reconstruct a snapshot after same-context replacement");

        for (snapshot, value) in [(&original, old_value.as_str()), (&refreshed, rotated_value)] {
            let http = Arc::new(StubHttpClient::new());
            http.push_response(200, br#"{"uid":"fictional-partner-uid"}"#.to_vec());
            dispatch_loaded_pull(
                snapshot,
                Arc::new(RecordingBackend::default()),
                http.clone(),
            )
            .expect("should dispatch the snapshot's own authorized credential");
            assert_eq!(http.recorded_request_uris().len(), 1);
            assert!(
                http.recorded_request_headers()[0]
                    .iter()
                    .any(|(name, header)| name == "authorization"
                        && header == &format!("Bearer {value}")),
                "should preserve the credential captured by this snapshot, not reread native storage"
            );
        }

        store.values.remove(&pull_key);
        let backend = Arc::new(RecordingBackend::default());
        let http = Arc::new(StubHttpClient::new());
        let _ = load_and_dispatch_pull(data, &store, backend.clone(), http.clone())
            .expect_err("should reject valid-checksum replay after the binding is retired");
        assert!(
            backend.specs().is_empty(),
            "should not create a replay backend"
        );
        assert!(
            http.recorded_request_uris().is_empty(),
            "should not export a retired credential on fresh state"
        );
    }

    #[test]
    fn legacy_blob_without_rewrite_creatives_preserves_rewriting() {
        let (data, store) = fixture();
        let auction = data
            .get("auction")
            .and_then(serde_json::Value::as_object)
            .expect("should serialize auction settings as an object");
        assert!(
            !auction.contains_key("rewrite_creatives"),
            "should omit the default rewrite setting from the payload"
        );
        let reconstructed =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect("should reconstruct legacy settings");

        assert!(
            reconstructed.auction.rewrite_creatives,
            "should enable creative rewriting for legacy blobs"
        );
    }

    #[test]
    fn legacy_blob_without_rewrite_include_domains_loads_with_empty_list() {
        let (data, store) = fixture();
        let rewrite = data
            .get("rewrite")
            .and_then(serde_json::Value::as_object)
            .expect("should serialize rewrite settings as an object");
        assert!(
            !rewrite.contains_key("include_domains"),
            "should omit the default include list from the payload"
        );

        let reconstructed =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect("should reconstruct settings without include_domains");

        assert!(
            reconstructed.rewrite.include_domains.is_empty(),
            "should load an empty include list from a blob without the key"
        );
    }

    #[test]
    fn rewrite_include_domains_survive_blob_round_trip() {
        let (mut data, store) = fixture();
        data["rewrite"]["include_domains"] = serde_json::json!(["*.cdn.example.com"]);

        let reconstructed =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect("should reconstruct settings with include_domains");

        assert_eq!(
            reconstructed.rewrite.include_domains,
            vec!["*.cdn.example.com".to_owned()],
            "should preserve the configured include list"
        );
    }

    #[test]
    fn disabled_rewrite_creatives_survives_blob_round_trip() {
        let (mut data, store) = fixture();
        data["auction"]["rewrite_creatives"] = serde_json::json!(false);

        let reconstructed =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect("should reconstruct disabled rewriting");

        assert!(
            !reconstructed.auction.rewrite_creatives,
            "should preserve the explicit rewrite opt-out"
        );
    }

    #[test]
    fn legacy_blob_without_rewrite_clicks_follows_rewrite_creatives() {
        for rewrite_creatives in [true, false] {
            let (mut data, store) = fixture();
            data["auction"]["rewrite_creatives"] = serde_json::json!(rewrite_creatives);
            assert!(
                data["auction"].get("rewrite_clicks").is_none(),
                "should omit unset rewrite_clicks from the payload"
            );

            let reconstructed =
                settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                    .expect("should reconstruct settings without rewrite_clicks");

            assert_eq!(
                reconstructed.auction.rewrite_clicks, None,
                "should load a blob without rewrite_clicks as unset"
            );
            assert_eq!(
                reconstructed.auction.rewrites_auction_clicks(),
                rewrite_creatives,
                "should follow rewrite_creatives = {rewrite_creatives} when unset"
            );
        }
    }

    #[test]
    fn explicit_rewrite_clicks_survives_blob_round_trip() {
        for (rewrite_creatives, rewrite_clicks) in [(true, false), (false, true)] {
            let (mut data, store) = fixture();
            data["auction"]["rewrite_creatives"] = serde_json::json!(rewrite_creatives);
            data["auction"]["rewrite_clicks"] = serde_json::json!(rewrite_clicks);

            let reconstructed =
                settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                    .expect("should reconstruct explicit rewrite_clicks");

            assert_eq!(
                reconstructed.auction.rewrite_clicks,
                Some(rewrite_clicks),
                "should preserve rewrite_clicks = {rewrite_clicks} with rewrite_creatives = {rewrite_creatives}"
            );
        }
    }

    #[test]
    fn strings_that_look_like_json_scalars_load_as_native_strings() {
        let (data, mut store) = fixture();
        let config: TrustedServerAppConfig =
            serde_json::from_value(data.clone()).expect("should parse feature fixture");
        for entry in compile_bindings(config.settings())
            .expect("should compile fixture")
            .entries()
        {
            let value = if entry.scope() == "handler" {
                "true"
            } else {
                "12345678901234567890123456789012"
            };
            store
                .values
                .insert(entry.key().to_owned(), value.as_bytes().to_vec());
        }
        let loaded =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect("should load scalar-looking native credentials");
        assert_eq!(
            loaded.publisher.proxy_secret.expose(),
            "12345678901234567890123456789012"
        );
        assert_eq!(
            loaded.ec.passphrase.expose(),
            "12345678901234567890123456789012"
        );
        assert_eq!(loaded.handlers[0].password.expose(), "true");
    }

    #[test]
    fn implicit_bindings_publish_one_nonserializable_snapshot() {
        let (data, store) = fixture();
        let before = data.clone();
        let loaded =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect("should load implicit native bindings");

        assert_eq!(
            loaded.publisher.proxy_secret.expose(),
            "fictional-proxy-credential-0123456789"
        );
        assert!(
            store
                .reads
                .lock()
                .expect("should lock read spy")
                .iter()
                .all(|key| key.starts_with("ts1_"))
        );
        assert_eq!(
            crate::config::unresolved_settings_value(&loaded).expect("should serialize features"),
            before
        );
        let output = serde_json::to_string(loaded.settings())
            .expect("should serialize only feature settings");
        assert!(!output.contains("fictional-proxy-credential"));
        assert!(!output.contains("fictional-handler-credential"));
    }

    #[test]
    fn removed_references_reject_before_even_the_first_lookup() {
        for path in ["/publisher/proxy_secret", "/ec/passphrase"] {
            let (mut data, store) = fixture();
            let (parent, field) = path.rsplit_once('/').expect("should split fixture path");
            data.pointer_mut(parent)
                .expect("should find object")
                .as_object_mut()
                .expect("should be object")
                .insert(
                    field.to_owned(),
                    serde_json::json!("fictional-sensitive-reference"),
                );
            let error =
                settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                    .expect_err("should reject removed reference");
            assert!(store.reads.lock().expect("should lock read spy").is_empty());
            assert!(!format!("{error:?}").contains("fictional-sensitive-reference"));
        }
    }

    #[test]
    fn failed_load_does_not_fall_back_or_publish_partial_values() {
        let (data, mut store) = fixture();
        store.values.clear();
        store.values.insert(
            "publisher_proxy_secret".to_owned(),
            b"unrelated-legacy-value".to_vec(),
        );
        let error =
            settings_from_config_blob(&envelope(data), &store, &DEFAULT_SECRET_STORE_ID.into())
                .expect_err("should reject missing generated binding");
        assert!(
            store
                .reads
                .lock()
                .expect("should lock read spy")
                .iter()
                .all(|key| key.starts_with("ts1_"))
        );
        let diagnostic = format!("{error:?}");
        assert!(!diagnostic.contains("fictional-provider-diagnostic"));
        assert!(!diagnostic.contains("unrelated-legacy-value"));
    }
}
