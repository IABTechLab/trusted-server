use super::*;
use crate::platform::{PlatformError, StoreId};
use crate::settings::{ProxyAssetRoute, S3SigV4AuthConfig};
use serde_json::json;
use std::sync::Mutex;

fn partner(domain: &str) -> EcPartner {
    serde_json::from_value(json!({
        "name": "Fictional partner", "source_domain": domain,
        "api_token_enabled": true, "pull_sync_enabled": true,
        "pull_sync_url": "https://sync.example.com/pull"
    }))
    .expect("should deserialize partner feature settings")
}

fn s3_route() -> ProxyAssetRoute {
    let mut route = ProxyAssetRoute::new("/assets/", "https://bucket.example.com");
    route.auth = Some(AssetOriginAuth::S3SigV4(
        serde_json::from_value::<S3SigV4AuthConfig>(json!({
            "region": "us-east-1", "session_token_enabled": true
        }))
        .expect("should deserialize S3 authentication policy"),
    ));
    route
}

fn all_roles() -> Settings {
    let mut settings = Settings::default();
    settings.ec.partners.push(partner("partner.example.com"));
    settings.handlers.push(
        serde_json::from_value(json!({
            "path": "^/admin/.*$", "username": "Jöhn"
        }))
        .expect("should deserialize the exact handler identity"),
    );
    settings.trusted_client_ip = Some(
        serde_json::from_value(json!({
            "ip_header": "x-fictional-client-ip", "auth_header": "x-fictional-auth"
        }))
        .expect("should deserialize trusted-client-IP feature settings"),
    );
    settings.tinybird.enabled = true;
    settings.tinybird.api_host = "api.tinybird.example.com".into();
    settings.integrations.insert(
        "datadome".into(),
        json!({
            "enabled": true, "enable_protection": true,
            "protection_api_origin": "https://protection.example.com",
            "protection_test_bypass": {"enabled": true}
        }),
    );
    settings.proxy.asset_routes.push(s3_route());
    settings
}

struct SpyStore {
    values: BTreeMap<String, Vec<u8>>,
    reads: Mutex<Vec<(String, String)>>,
}
impl SpyStore {
    fn for_settings(settings: &Settings) -> Self {
        Self {
            values: compile_bindings(settings)
                .expect("should compile fixture credential bindings")
                .entries()
                .iter()
                .map(|entry| {
                    (
                        entry.key().to_owned(),
                        b"fictional-credential-0123456789-ABCDEFGHIJ".to_vec(),
                    )
                })
                .collect(),
            reads: Mutex::new(Vec::new()),
        }
    }
}
impl PlatformSecretStore for SpyStore {
    fn get_bytes(&self, store: &StoreName, key: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        self.reads
            .lock()
            .expect("should lock the native-read spy")
            .push((store.as_ref().to_owned(), key.to_owned()));
        self.values.get(key).cloned().ok_or_else(|| {
            Report::new(PlatformError::SecretStore)
                .attach("SENTINEL-native-diagnostic-must-not-escape")
        })
    }
    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        unreachable!("read-only fixture")
    }
    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        unreachable!("read-only fixture")
    }
}
fn resolve(settings: &Settings, store: &SpyStore) -> BindingResult<ResolvedBindings> {
    resolve_bindings(settings, store, &StoreName::from(SECRET_STORE_ID))
}

#[test]
fn all_twelve_golden_contexts_match_independent_sha256_base32_vectors() {
    // Independently generated using Bun's node:crypto SHA256 and an LP/VEC encoder.
    let golden = [
        (
            "proxy",
            "ts1_mu4d7mg5sfuleogirpka2zbh2b33lsocgyl6wcovawmjs4nfmi6a",
        ),
        (
            "ecpass",
            "ts1_juuqz3tbywbbzruyd2bvt56qmxuiokdflb4ii2dfmjvw7ypyjwbq",
        ),
        (
            "ecin",
            "ts1_k3wrjithgzt3ebzhyh75w7zte6hk5afym2inqwq76hkekmbdascq",
        ),
        (
            "ecpull",
            "ts1_2hsjuldwgmfhyotnokgn5mqrao6xnkyo3dwva33hez4vx43yvcjq",
        ),
        (
            "handler",
            "ts1_4xxbylw4mueqnarjo7xebdfkai2zsqqxg4uovsaonf2vkpsoggea",
        ),
        (
            "clientip",
            "ts1_c5xmtldm34soypp5vwjijlo5f2nhbh5whlhplffr4y2rldgpoida",
        ),
        (
            "tinybird",
            "ts1_mo6xycw4jkjdmzy36w7n4uyskkyjdrcyqxxdpof25mmklaofqvfq",
        ),
        (
            "ddserver",
            "ts1_rm3vdfl6uwxihluj7uyftndcygyevzacjjqkuwtigqbosoa2czna",
        ),
        (
            "ddbypass",
            "ts1_pu57gijeer2zxfusydlssdiz5lgns423kilp25zp3crzz3p2tzsq",
        ),
        (
            "s3id",
            "ts1_c7h6zs7vr5ru73ij46hztdctlvzcjvgkxoopcirtc3qqxuyzzzea",
        ),
        (
            "s3secret",
            "ts1_wvbtvxuoqctkntcdraq7gzigeq5m5hw2kkuwtjjzecan4iilkosq",
        ),
        (
            "s3sess",
            "ts1_x4xisdyxzwrk446nxsn62rpjyozb5i3ng3xqgr6xp2sdnnr54cpa",
        ),
    ];
    let plan = compile_bindings(&all_roles()).expect("should compile all twelve credential roles");
    assert_eq!(plan.entries().len(), 12);
    for (scope, key) in golden {
        let entry = plan
            .entries()
            .iter()
            .find(|entry| entry.scope() == scope)
            .expect("should find the golden-vector scope in the compiled plan");
        assert_eq!(entry.key(), key, "scope {scope}");
        assert_eq!(entry.key().len(), 56);
    }
    assert_eq!(secret_fields().len(), 12);
    assert_eq!(DESCRIPTORS.len(), 12);
}

#[test]
fn framing_uses_utf8_byte_lengths_and_unambiguous_vectors() {
    let mut encoded = Vec::new();
    lp(&mut encoded, "é").expect("should frame the UTF-8 string using its byte length");
    assert_eq!(encoded, [0, 0, 0, 2, 0xc3, 0xa9]);
    assert_ne!(
        derive_key("proxy", &strings(&["ab", "c"]), &strings(&["none"]))
            .expect("should derive a key for separate ab and c subject components"),
        derive_key("proxy", &strings(&["a", "bc"]), &strings(&["none"]))
            .expect("should derive a key for separate a and bc subject components")
    );
    assert_ne!(
        derive_key("proxy", &strings(&[""]), &[])
            .expect("should derive a key for one empty subject component"),
        derive_key("proxy", &[], &strings(&[""]))
            .expect("should derive a key for one empty export component")
    );
    assert_ne!(
        derive_key("proxy", &strings(&["app"]), &strings(&["none"]))
            .expect("should derive the publisher proxy key"),
        derive_key("ecpass", &strings(&["app"]), &strings(&["none"]))
            .expect("should derive the EC passphrase key")
    );
    assert_eq!(
        checked_u32(u32::MAX as usize, "fixture")
            .expect("should accept the largest encodable component length"),
        u32::MAX
    );
    #[cfg(target_pointer_width = "64")]
    assert!(checked_u32((u32::MAX as usize) + 1, "fixture").is_err());
}

#[test]
fn endpoint_normalization_preserves_only_specified_equivalences() {
    for (raw, expected) in [
        (
            "HTTPS://Sync.Example.COM:443/pull",
            "https://sync.example.com/pull",
        ),
        ("https://sync.example.com", "https://sync.example.com/"),
        (
            "https://sync.example.com:8443/A//b_/~x-",
            "https://sync.example.com:8443/A//b_/~x-",
        ),
    ] {
        assert_eq!(
            normalize_endpoint(raw, "fixture.endpoint")
                .expect("should normalize the accepted HTTPS endpoint"),
            expected
        );
    }
    let variants = [
        "https://sync.example.com/pull",
        "https://sync.example.com/pull/",
        "https://sync.example.com//pull",
        "https://sync.example.com/Pull",
        "https://sync.example.com:8443/pull",
    ];
    let keys: std::collections::BTreeSet<_> = variants
        .iter()
        .map(|raw| {
            derive_key(
                "ecpull",
                &strings(&["partner", "partner.example.com"]),
                &strings(&[
                    "pull",
                    &normalize_endpoint(raw, "fixture")
                        .expect("should normalize the endpoint before deriving its key"),
                ]),
            )
            .expect("should derive the endpoint-specific pull key")
        })
        .collect();
    assert_eq!(keys.len(), variants.len());
}

#[test]
fn original_endpoint_rejections_precede_parser_cleanup_and_are_redacted() {
    for endpoint in [
        "http://sync.example.com/pull",
        "https://@sync.example.com/pull",
        "https://a@sync.example.com/pull",
        "https://sync.example.com/pull?",
        "https://sync.example.com/pull?token=SENTINEL",
        "https://sync.example.com/pull#",
        "https://sync.example.com./pull",
        "https://127.0.0.1/pull",
        "https://127.1/pull",
        "https://2130706433/pull",
        "https://0x7f000001/pull",
        "https://[::1]/pull",
        "https://é.example.com/pull",
        "https://sync.example.com/a/../pull",
        "https://sync.example.com/./pull",
        "https://sync.example.com/%70ull",
        "https://sync.example.com/with space",
        " https://sync.example.com/pull",
        "https://sync.example.com/pull\n",
        "https://sync.example.com\\evil",
        "https://sync.example.com:0/pull",
        "https://sync.example.com:65536/pull", // allow-domain: sync.example.com:65536
        "https://sync.example.com:/pull",
        "https://-bad.example.com/pull",
        "https://sync..example.com/pull",
    ] {
        let error = normalize_endpoint(endpoint, "ec.partners[0].pull_sync_url")
            .expect_err("should reject forbidden original endpoint syntax");
        let diagnostic = format!("{error:?}");
        assert!(diagnostic.contains("ec.partners[0].pull_sync_url"));
        assert!(!diagnostic.contains(endpoint));
        assert!(!diagnostic.contains("SENTINEL"));
    }
}

#[test]
fn optional_activation_avoids_probes_and_does_not_treat_missing_as_disabled() {
    let mut settings = Settings::default();
    assert_eq!(
        compile_bindings(&settings)
            .expect("should compile only the always-required roles by default")
            .entries()
            .len(),
        2
    );
    let mut inactive = partner("partner.example.com");
    inactive.api_token_enabled = false;
    inactive.pull_sync_enabled = false;
    inactive.pull_sync_url = None;
    settings.ec.partners.push(inactive);
    assert_eq!(
        compile_bindings(&settings)
            .expect("should compile no credentials for the inactive partner")
            .entries()
            .len(),
        2
    );
    let store = SpyStore::for_settings(&settings);
    assert_eq!(
        resolve(&settings, &store)
            .expect("should resolve only the active required roles")
            .entries()
            .len(),
        2
    );
    assert_eq!(
        store
            .reads
            .lock()
            .expect("should lock the native-read spy")
            .len(),
        2
    );
    settings.ec.partners[0].api_token_enabled = true;
    assert!(resolve(&settings, &store).is_err());
    settings.ec.partners[0].pull_sync_enabled = true;
    assert!(compile_bindings(&settings).is_err());
}

#[test]
fn new_activation_flags_are_boolean_only_and_default_false() {
    let partner: EcPartner =
        serde_json::from_value(json!({"name":"Fictional", "source_domain":"partner.example.com"}))
            .expect("should deserialize a partner with default activation flags");
    assert!(!partner.api_token_enabled);
    assert!(serde_json::from_value::<EcPartner>(json!({"name":"Fictional", "source_domain":"partner.example.com", "api_token_enabled":"true"})).is_err());
    let auth: S3SigV4AuthConfig = serde_json::from_value(json!({"region":"us-east-1"}))
        .expect("should deserialize S3 auth with its default session-token flag");
    assert!(!auth.session_token_enabled);
    assert!(
        serde_json::from_value::<S3SigV4AuthConfig>(
            json!({"region":"us-east-1", "session_token_enabled":"true"})
        )
        .is_err()
    );
}

#[test]
fn pull_allowlist_is_only_an_optional_endpoint_assertion() {
    let mut partner = partner("PARTNER.EXAMPLE.COM.");
    assert_eq!(
        normalize_pull_endpoint(&partner, "fixture")
            .expect("should normalize the pull endpoint without an allowlist assertion"),
        "https://sync.example.com/pull"
    );
    partner.pull_sync_allowed_domains = vec!["SYNC.EXAMPLE.COM".into()];
    assert!(normalize_pull_endpoint(&partner, "fixture").is_ok());
    for domain in [
        "example.com",
        "evil.example.com",
        "sync.example.com.",
        " sync.example.com",
        "sync.example.com:443",
    ] {
        partner.pull_sync_allowed_domains = vec![domain.into()];
        assert!(normalize_pull_endpoint(&partner, "fixture").is_err());
    }
}

#[test]
fn collection_order_does_not_change_identity_keys() {
    let mut settings = Settings::default();
    settings.ec.partners = vec![
        partner("partner.example.com"),
        partner("second.example.com"),
    ];
    let keyset = |settings: &Settings| -> std::collections::BTreeSet<String> {
        compile_bindings(settings)
            .expect("should compile the partner identity key set")
            .entries()
            .iter()
            .map(|entry| entry.key().to_owned())
            .collect()
    };
    let original = keyset(&settings);
    settings.ec.partners.reverse();
    assert_eq!(keyset(&settings), original);
    settings.ec.partners[0].source_domain = "SECOND.EXAMPLE.COM.".into();
    assert_eq!(keyset(&settings), original);
    settings.ec.partners[1].source_domain = "second.example.com".into();
    assert!(compile_bindings(&settings).is_err());
}

#[test]
fn equal_contexts_are_read_once_and_not_arbitrary_references() {
    let mut settings = Settings::default();
    let handler: Handler =
        serde_json::from_value(json!({"path":"^/private", "username":"fictional"}))
            .expect("should deserialize the repeated handler identity");
    settings.handlers = vec![handler.clone(), handler];
    let store = SpyStore::for_settings(&settings);
    let bindings =
        resolve(&settings, &store).expect("should resolve repeated equal handler contexts");
    assert_eq!(bindings.entries().len(), 4);
    assert_eq!(
        store
            .reads
            .lock()
            .expect("should lock the native-read spy")
            .len(),
        3
    );
    assert!(
        store
            .reads
            .lock()
            .expect("should lock the native-read spy")
            .iter()
            .all(|(store, key)| store == SECRET_STORE_ID
                && key.starts_with("ts1_")
                && key.len() == 56)
    );
}

#[test]
fn authorization_equality_ignores_only_owner_location() {
    let mut settings = Settings::default();
    let handler: Handler =
        serde_json::from_value(json!({"path":"^/private", "username":"fictional"}))
            .expect("should deserialize a repeated-handler authorization fixture");
    settings.handlers = vec![handler.clone(), handler];
    let plan = compile_bindings(&settings).expect("should compile both repeated handler owners");
    let handlers: Vec<_> = plan
        .entries()
        .iter()
        .filter(|entry| entry.descriptor_id() == "handler.password")
        .collect();
    assert_eq!(handlers.len(), 2);
    assert_ne!(handlers[0], handlers[1]);
    assert!(handlers[0].same_authorization(handlers[1]));
    assert!(handlers[1].same_authorization(handlers[0]));
    settings.handlers[1].username = crate::redacted::Redacted::new("different-user".to_owned());
    let changed = compile_bindings(&settings).expect("should compile the distinct handler subject");
    let changed_owner = changed
        .entries()
        .iter()
        .find(|entry| entry.field_path() == "handlers[1].password")
        .expect("should retain the changed handler owning path");
    assert!(!handlers[0].same_authorization(changed_owner));
    let mut changed_target = (*handlers[0]).clone();
    changed_target.target = CompiledTarget::Pull {
        endpoint: "https://fictional.example.com/pull".to_owned(),
    };
    assert!(!handlers[0].same_authorization(&changed_target));
}

#[test]
fn trusted_mapped_default_store_is_used_exactly_without_fallback() {
    let settings = Settings::default();
    let store = SpyStore::for_settings(&settings);
    let selected = StoreName::from("fictional_mapped_secrets");
    assert_eq!(
        resolve_bindings(&settings, &store, &selected)
            .expect("should resolve credentials from the selected trusted store")
            .entries()
            .len(),
        2
    );
    assert!(
        store
            .reads
            .lock()
            .expect("should lock the native-read spy")
            .iter()
            .all(|(name, _)| name == "fictional_mapped_secrets")
    );
}

#[test]
fn native_read_failures_invalid_utf8_and_invalid_values_are_safe() {
    let settings = all_roles();
    let mut store = SpyStore::for_settings(&settings);
    let key = compile_bindings(&settings)
        .expect("should compile the fixture before replacing its native value")
        .entries()[0]
        .key()
        .to_owned();
    for invalid in [Vec::new(), vec![0xff], b"SENTINEL\ninvalid-token".to_vec()] {
        store.values.insert(key.clone(), invalid);
        let diagnostic = format!(
            "{:?}",
            resolve(&settings, &store).expect_err("should reject invalid native credential bytes")
        );
        assert!(!diagnostic.contains("SENTINEL"));
    }
    store.values.remove(&key);
    let diagnostic = format!(
        "{:?}",
        resolve(&settings, &store).expect_err("should reject a missing generated native binding")
    );
    assert!(!diagnostic.contains("SENTINEL"));
    assert!(!diagnostic.contains(&key));
    assert!(!diagnostic.contains("https://"));
}

#[test]
fn exact_bytes_survive_resolution_and_debug_is_redacted() {
    let settings = all_roles();
    let mut store = SpyStore::for_settings(&settings);
    let plan = compile_bindings(&settings)
        .expect("should compile all roles before seeding exact credential bytes");
    for entry in plan.entries() {
        let value: &[u8] = match entry.scope() {
            "proxy" | "ecpass" | "handler" | "ddserver" | "ddbypass" => {
                b"  SENTINEL-exact-credential-0123456789  "
            }
            "s3id" | "s3secret" | "s3sess" => b" SENTINEL-exact-signing-material ",
            _ => b"SENTINELexactBearertoken0123456789==",
        };
        store.values.insert(entry.key().into(), value.to_vec());
    }
    let resolved = resolve(&settings, &store)
        .expect("should resolve credentials without changing their bytes");
    for binding in resolved.entries() {
        assert_eq!(
            binding.credential().as_bytes(),
            store
                .values
                .get(binding.compiled().key())
                .expect("should find the exact native value for the resolved key")
        );
        let diagnostic = format!("{binding:?}");
        assert!(!diagnostic.contains("SENTINEL"));
        assert!(!diagnostic.contains("Jöhn"));
        assert!(!diagnostic.contains("https://"));
        if let Some(header) = binding.bearer_header() {
            assert!(header.is_sensitive());
        }
    }
    assert!(!format!("{resolved:?}").contains("SENTINEL"));
}

#[test]
fn bearer_grammar_is_rfc6750_and_never_trims() {
    for scope in ["ecpull", "tinybird"] {
        for value in [
            "abcdefghijklmnopqrstuvwxyz0123456789",
            "abcdefghijklmnopqrstuvwxyz0123456789==",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ-._~+/123456789",
        ] {
            assert!(validate_credential(scope, value.as_bytes()).is_ok());
        }
        for value in [
            "",
            "===",
            "short token 012345678901234567890123456789",
            "a=b012345678901234567890123456789",
            "abcdefghijklmnopqrstuvwxy0123456789\n",
            " abcdefghijklmnopqrstuvwxy0123456789",
            "abcdefghijklmnopqrstuvwxy0123456789:",
            "abcdefghijklmnopqrstuvwxy0123456789é",
        ] {
            assert!(validate_credential(scope, value.as_bytes()).is_err());
        }
    }
    assert!(validate_credential("ecpull", b"short").is_err());
    assert!(validate_credential("unknown", b"fictional").is_err());
    assert!(validate_credential("clientip", b"  fictional-credential-0123456789-ABCD  ").is_err());
    assert!(validate_credential("ecin", &[b' '; 32]).is_err());
    assert!(validate_credential("s3id", b"fictional\r\nheader").is_err());
    assert!(validate_credential("s3sess", "fictional-é".as_bytes()).is_err());
    assert!(validate_credential("s3secret", " fictional-é\r\nHMAC-key ".as_bytes()).is_ok());
    // DataDome form encoding must preserve characters rather than trim them.
    assert!(validate_credential("ddserver", " value&=+é\n".as_bytes()).is_ok());
}

#[test]
fn loaded_credential_target_cannot_be_reinterpreted_by_mutating_settings() {
    let mut settings = all_roles();
    let store = SpyStore::for_settings(&settings);
    let resolved = resolve(&settings, &store)
        .expect("should resolve the original credential and target snapshot");
    let pull = resolved
        .require_for_settings(&settings, "ec.pull", "ec.partners[0].ts_pull_token")
        .expect("should find the loaded pull capability for its original context")
        .clone();
    settings.ec.partners[0].pull_sync_url = Some("https://attacker.example.com/pull".into());
    settings.ec.partners[0].pull_sync_allowed_domains = vec!["attacker.example.com".into()];
    assert!(
        resolved
            .require_for_settings(&settings, "ec.pull", "ec.partners[0].ts_pull_token")
            .is_err()
    );
    assert_eq!(pull.endpoint(), Some("https://sync.example.com/pull"));
    assert_eq!(pull.subject(), strings(&["partner", "partner.example.com"]));
    assert!(
        ResolvedBindings::default()
            .require_for_settings(&settings, "ec.pull", "ec.partners[0].ts_pull_token")
            .is_err()
    );
}

#[test]
fn operation_plan_validation_matches_settings_checks_and_rejects_changed_contexts() {
    let mut settings = all_roles();
    let store = SpyStore::for_settings(&settings);
    let loaded =
        resolve(&settings, &store).expect("should resolve operation-plan fixture credentials");
    let current = compile_bindings(&settings).expect("should compile one current validation plan");
    for binding in loaded.entries() {
        let id = binding.compiled().descriptor_id();
        let path = binding.compiled().field_path();
        let from_plan = loaded
            .require_for_plan(&current, id, path)
            .expect("should validate each loaded role against the shared plan");
        let from_settings = loaded
            .require_for_settings(&settings, id, path)
            .expect("should preserve settings convenience validation semantics");
        assert!(std::ptr::eq(binding, from_plan));
        assert!(std::ptr::eq(from_plan, from_settings));
    }
    assert!(
        loaded
            .validated_for_plan(&current, "absent.role", "absent.path")
            .expect("should return no capability for an inactive absent role")
            .is_none()
    );
    assert!(
        loaded
            .require_for_plan(&current, "absent.role", "absent.path")
            .is_err()
    );
    assert!(
        ResolvedBindings::default()
            .require_for_plan(&current, "ec.pull", "ec.partners[0].ts_pull_token")
            .is_err()
    );
    settings.ec.partners[0].pull_sync_url = Some("https://changed.example.com/pull".into());
    let changed =
        compile_bindings(&settings).expect("should compile the changed destination context");
    assert!(
        loaded
            .require_for_plan(&changed, "ec.pull", "ec.partners[0].ts_pull_token")
            .is_err()
    );
    assert!(
        loaded
            .require_for_settings(&settings, "ec.pull", "ec.partners[0].ts_pull_token")
            .is_err()
    );
    assert!(
        loaded
            .require_for_plan(&current, "ec.pull", "ec.partners[0].ts_pull_token")
            .is_ok()
    );
}

#[test]
fn s3_named_region_is_bound_without_runtime_subject_decoding() {
    let mut settings = all_roles();
    let store = SpyStore::for_settings(&settings);
    let loaded =
        resolve(&settings, &store).expect("should resolve credentials with their typed targets");
    let current = compile_bindings(&settings).expect("should compile typed S3 targets");
    for binding in loaded.entries() {
        if binding.compiled().scope().starts_with("s3") {
            assert_eq!(binding.s3_region(), Some("us-east-1"));
            assert!(
                matches!(binding.compiled().target(), CompiledTarget::S3 { origin, region }
                if origin == "https://bucket.example.com" && region == "us-east-1")
            );
        } else {
            assert_eq!(binding.s3_region(), None);
        }
    }
    let path = "proxy.asset_routes[0].auth.access_key_id";
    let binding = loaded
        .require_for_plan(&current, "s3.access-id", path)
        .expect("should retrieve the original typed S3 access-ID binding");
    let mut altered_target = current.clone();
    let entry = altered_target
        .entries
        .iter_mut()
        .find(|entry| entry.descriptor_id() == "s3.access-id")
        .expect("should locate the S3 access-ID entry for equality coverage");
    let CompiledTarget::S3 { region, .. } = &mut entry.target else {
        unreachable!()
    };
    *region = "us-west-2".into();
    assert!(
        loaded
            .require_for_plan(&altered_target, "s3.access-id", path)
            .is_err()
    );
    let Some(AssetOriginAuth::S3SigV4(auth)) = &mut settings.proxy.asset_routes[0].auth else {
        unreachable!()
    };
    auth.region = "us-west-2".into();
    let changed = compile_bindings(&settings).expect("should compile a new signing-region context");
    assert!(
        loaded
            .require_for_plan(&changed, "s3.access-id", path)
            .is_err()
    );
    assert_eq!(binding.s3_region(), Some("us-east-1"));
}

#[test]
fn all_send_affecting_s3_route_policy_mutations_change_keys() {
    let mut settings = Settings::default();
    settings.proxy.asset_routes.push(s3_route());
    let key = |settings: &Settings| {
        compile_bindings(settings)
            .expect("should compile the authenticated S3 route policy")
            .entries()
            .iter()
            .find(|entry| entry.scope() == "s3id")
            .expect("should find the route access-ID binding")
            .key()
            .to_owned()
    };
    let original = key(&settings);
    let mut changed = settings.clone();
    changed.proxy.asset_routes[0].prefix = "/other/".into();
    assert_ne!(key(&changed), original);
    let mut changed = settings.clone();
    changed.proxy.asset_routes[0].origin_url = "https://different.example.com".into();
    assert_ne!(key(&changed), original);
    let mut changed = settings.clone();
    changed.proxy.asset_routes[0].origin_url = "https://bucket.example.com:8443".into();
    assert_ne!(key(&changed), original);
    let mut changed = settings.clone();
    changed.proxy.asset_routes[0].path_pattern = Some("^/assets/(.*)".into());
    changed.proxy.asset_routes[0].target_path = Some("/$1".into());
    assert_ne!(key(&changed), original);
    let mut changed = settings.clone();
    let Some(AssetOriginAuth::S3SigV4(auth)) = &mut changed.proxy.asset_routes[0].auth else {
        unreachable!()
    };
    auth.region = "us-west-2".into();
    assert_ne!(key(&changed), original);
    let mut changed = settings.clone();
    let Some(AssetOriginAuth::S3SigV4(auth)) = &mut changed.proxy.asset_routes[0].auth else {
        unreachable!()
    };
    auth.origin_query = Some(OriginQueryPolicy::Strip);
    assert_ne!(key(&changed), original);
    let plan =
        compile_bindings(&settings).expect("should compile all required S3 credential roles");
    let s3: Vec<_> = plan
        .entries()
        .iter()
        .filter(|entry| entry.scope().starts_with("s3"))
        .collect();
    assert_eq!(s3.len(), 3);
    assert_eq!(s3[0].subject(), s3[1].subject());
    assert_eq!(s3[1].subject(), s3[2].subject());
    assert_ne!(s3[0].key(), s3[1].key());
    assert_eq!(s3[0].subject()[7], "us-east-1");
    assert_eq!(s3[0].subject()[8], "preserve");
}

#[test]
fn s3_aliases_defaults_and_effective_query_policy_agree() {
    let route = |kind: &str| {
        serde_json::from_value::<ProxyAssetRoute>(json!({
            "prefix":"/assets/", "origin_url":"HTTPS://BUCKET.EXAMPLE.COM:443/",
            "auth":{"type":kind,"region":"us-east-1"}
        }))
        .expect("should deserialize the accepted S3 auth alias")
    };
    let mut a = Settings::default();
    a.proxy.asset_routes.push(route("s3_sigv4"));
    let mut b = Settings::default();
    b.proxy.asset_routes.push(route("s3_sig_v4"));
    assert_eq!(
        compile_bindings(&a).expect("should compile the first effective S3 policy"),
        compile_bindings(&b).expect("should compile the alias-equivalent S3 policy")
    );
    let Some(AssetOriginAuth::S3SigV4(auth)) = &mut b.proxy.asset_routes[0].auth else {
        unreachable!()
    };
    auth.origin_query = Some(OriginQueryPolicy::Preserve);
    assert_eq!(
        compile_bindings(&a).expect("should compile the default S3 query policy"),
        compile_bindings(&b).expect("should compile the equivalent explicit preserve policy")
    );
    b.proxy.asset_routes[0].image_optimizer = Some(
        serde_json::from_value(json!({"region":"us-east-1", "profile_set":"fictional"}))
            .expect("should deserialize the image-optimizer policy"),
    );
    let Some(AssetOriginAuth::S3SigV4(auth)) = &mut b.proxy.asset_routes[0].auth else {
        unreachable!()
    };
    auth.origin_query = None;
    assert_ne!(
        compile_bindings(&a).expect("should compile the non-optimized S3 route"),
        compile_bindings(&b).expect("should compile the image-optimizer effective query policy")
    );
}

#[test]
fn tinybird_authority_supports_https_ports_without_expanding_endpoint_syntax() {
    let mut settings = all_roles();
    let baseline =
        compile_bindings(&settings).expect("should compile the default Tinybird authority");
    settings.tinybird.api_host = "API.TINYBIRD.EXAMPLE.COM:443".into();
    assert_eq!(
        compile_bindings(&settings)
            .expect("should compile the equivalent explicit HTTPS default port"),
        baseline
    );
    assert!(
        validate_original_endpoints(
            &json!({"tinybird":{"api_host":"API.TINYBIRD.EXAMPLE.COM:443"}})
        )
        .is_ok()
    );
    settings.tinybird.api_host = "api.tinybird.example.com:8443".into();
    let changed =
        compile_bindings(&settings).expect("should compile the nondefault Tinybird HTTPS port");
    let entry = changed
        .entries()
        .iter()
        .find(|entry| entry.scope() == "tinybird")
        .expect("should find the port-specific Tinybird binding");
    assert_eq!(
        entry.target(),
        &CompiledTarget::Tinybird {
            endpoint: "https://api.tinybird.example.com:8443/v0/events".into(),
            dataset: "auction_events_raw".into()
        }
    );
    assert_ne!(
        entry.key(),
        baseline
            .entries()
            .iter()
            .find(|entry| entry.scope() == "tinybird")
            .expect("should find the baseline Tinybird binding")
            .key()
    );
    assert!(
        validate_original_endpoints(
            &json!({"tinybird":{"api_host":"api.tinybird.example.com:8443"}})
        )
        .is_ok()
    );
    for authority in [
        "https://api.tinybird.example.com",
        "api.tinybird.example.com/path",
        "@api.tinybird.example.com",
        "api.tinybird.example.com?",
        "api.tinybird.example.com#",
        "api.tinybird.example.com:0",
        "api.tinybird.example.com:65536",
        "api.tinybird.example.com:8443 ",
        "api.tinybird.example.com\\evil",
    ] {
        settings.tinybird.api_host = authority.into();
        assert!(compile_bindings(&settings).is_err());
        assert!(
            validate_original_endpoints(
                &json!({"tinybird":{"enabled":false,"api_host":authority}})
            )
            .is_err()
        );
    }
}

#[test]
fn handler_and_dataset_identity_are_exact_not_silently_trimmed() {
    let settings = all_roles();
    let keys = |settings: &Settings, scope: &str| {
        compile_bindings(settings)
            .expect("should compile the exact handler and dataset identities")
            .entries()
            .iter()
            .find(|entry| entry.scope() == scope)
            .expect("should find the requested identity scope")
            .key()
            .to_owned()
    };
    let mut changed = settings.clone();
    changed.handlers[0].path.push(' ');
    assert_ne!(keys(&changed, "handler"), keys(&settings, "handler"));
    let mut changed = settings.clone();
    changed.handlers[0].username = crate::redacted::Redacted::new("Jöhn ".into());
    assert_ne!(keys(&changed, "handler"), keys(&settings, "handler"));
    let mut changed = settings.clone();
    changed.tinybird.auction_dataset.push(' ');
    assert!(compile_bindings(&changed).is_err());
    let mut changed = settings.clone();
    changed.tinybird.auction_dataset = "other_events".into();
    assert_ne!(keys(&changed, "tinybird"), keys(&settings, "tinybird"));
}

#[test]
fn removed_reference_properties_reject_regardless_of_activation_or_value() {
    for mut value in [
        json!({"publisher":{"proxy_secret":null}}),
        json!({"ec":{"passphrase":"SENTINEL"}}),
        json!({"ec":{"partners":[{"api_token":"ts1_generated", "api_token_enabled":false}]}}),
        json!({"ec":{"partners":[{"ts_pull_token":"SENTINEL", "pull_sync_enabled":false}]}}),
        json!({"handlers":[{"password":"SENTINEL"}]}),
        json!({"trusted_client_ip":{"shared_secret":"SENTINEL"}}),
        json!({"tinybird":{"enabled":false,"auction_token_secret":"SENTINEL"}}),
        json!({"integrations":{"datadome":{"enabled":false,"server_side_key_secret_name":"SENTINEL"}}}),
        json!({"integrations":{"datadome":{"protection_test_bypass":{"enabled":false,"credential_secret_name":"SENTINEL"}}}}),
        json!({"proxy":{"asset_routes":[{"auth":{"type":"s3_sigv4","access_key_id":"SENTINEL"}}]}}),
        json!({"proxy":{"asset_routes":[{"auth":{"type":"s3_sig_v4","secret_access_key":"SENTINEL"}}]}}),
        json!({"proxy":{"asset_routes":[{"auth":{"type":"s3_sigv4","session_token":null,"session_token_enabled":false}}]}}),
    ] {
        let diagnostic = format!(
            "{:?}",
            reject_removed_references(&value)
                .expect_err("should reject the removed credential property")
        );
        assert!(!diagnostic.contains("SENTINEL"));
        assert!(!diagnostic.contains("ts1_generated"));
        assert!(diagnostic.contains("removed credential reference"));
        // Numeric overlay maps have the same protection.
        if let Some(array) = value.get_mut("handlers") {
            *array = json!({"0":{"password":"SENTINEL"}});
            assert!(reject_removed_references(&value).is_err());
        }
    }
    assert!(
        reject_removed_references(
            &json!({"tinybird":{"enabled":false,"access_token_secret":"discarded-placeholder"}})
        )
        .is_ok()
    );
    assert!(
        reject_removed_references(
            &json!({"handlers":"[{\"path\":\"^/\",\"password\":\"SENTINEL\"}]"})
        )
        .is_err()
    );
}

#[test]
fn malformed_dynamic_activation_fails_before_any_read() {
    for raw in [
        Value::Null,
        json!([]),
        json!({"enabled":"false"}),
        json!({"enabled":false,"enable_protection":"false"}),
        json!({"enabled":false,"protection_test_bypass":[]}),
    ] {
        let mut settings = Settings::default();
        settings.integrations.insert("datadome".into(), raw);
        let store = SpyStore {
            values: BTreeMap::new(),
            reads: Mutex::new(Vec::new()),
        };
        assert!(resolve(&settings, &store).is_err());
        assert!(
            store
                .reads
                .lock()
                .expect("should lock the native-read spy")
                .is_empty()
        );
    }
}

#[test]
fn original_preflight_covers_disabled_features_and_collection_overlays() {
    for value in [
        json!({"ec":{"partners":{"0":{"pull_sync_enabled":false,"pull_sync_url":"https://sync.example.com/a/../b"}}}}),
        json!({"tinybird":{"enabled":false,"api_host":" api.example.com"}}),
        json!({"integrations":{"datadome":{"enabled":false,"protection_api_origin":"https://protection.example.com?"}}}),
        json!({"proxy":{"asset_routes":[{"origin_url":"https://bucket.example.com/a/../", "auth":{"type":"s3_sigv4"}}]}}),
    ] {
        assert!(validate_original_endpoints(&value).is_err());
    }
}

#[test]
fn native_rendering_matches_actual_ts_bridges_and_spin_declarations() {
    let key = "ts1_2hsjuldwgmfhyotnokgn5mqrao6xnkyo3dwva33hez4vx43yvcjq";
    for adapter in ["fastly", "cloudflare"] {
        assert_eq!(
            render_native(adapter, SECRET_STORE_ID, key)
                .expect("should render the generated key in the flat native namespace")
                .native_name,
            key
        );
    }
    assert_eq!(
        render_native("axum", SECRET_STORE_ID, key)
            .expect("should render the Axum environment identity")
            .native_name,
        format!(
            "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_{}",
            key.to_uppercase()
        )
    );
    let spin = render_native("spin", SECRET_STORE_ID, key)
        .expect("should render both Spin declaration levels");
    assert_eq!(
        spin.native_name,
        format!("v_trusted_x5fserver_x5fsecrets_v_ts1_x5f{}", &key[4..])
    );
    assert_eq!(spin.native_name.len(), 92);
    assert_eq!(spin.declarations.len(), 2);
    assert!(spin.declarations[0].contains("secret = true"));
    assert!(spin.declarations[1].contains(&format!("{{{{ {} }}}}", spin.native_name)));
    for adapter in ["fastly", "axum", "cloudflare", "spin"] {
        assert!(render_native(adapter, "untrusted", key).is_err());
        assert!(render_native(adapter, SECRET_STORE_ID, "raw-reference").is_err());
    }
    assert!(render_native("unknown", SECRET_STORE_ID, key).is_err());
}

#[test]
fn renderer_tracks_trusted_native_mapping_without_changing_logical_authority() {
    let key = "ts1_2hsjuldwgmfhyotnokgn5mqrao6xnkyo3dwva33hez4vx43yvcjq";
    let mapped = "fictional_mapped_secrets";
    for adapter in ["axum", "spin"] {
        let binding = render_native_with_mapping(adapter, SECRET_STORE_ID, key, mapped)
            .expect("should render the selected trusted native store mapping");
        assert_eq!(binding.store_id, SECRET_STORE_ID);
        let expected = if adapter == "axum" {
            axum_secret_name(mapped, key)
        } else {
            spin_secret_name(mapped, key).expect("should encode the selected Spin store and key")
        };
        assert_eq!(binding.native_name, expected);
        assert_ne!(
            binding.native_name,
            render_native(adapter, SECRET_STORE_ID, key)
                .expect("should render the default logical-store mapping")
                .native_name
        );
        if adapter == "spin" {
            assert!(
                binding
                    .declarations
                    .iter()
                    .all(|declaration| declaration.contains(&binding.native_name))
            );
        }
    }
    for adapter in ["fastly", "cloudflare"] {
        assert_eq!(
            render_native_with_mapping(adapter, SECRET_STORE_ID, key, mapped)
                .expect("should render the flat namespace independently of the mapped store"),
            render_native(adapter, SECRET_STORE_ID, key)
                .expect("should render the flat namespace with the logical store")
        );
    }
}

#[test]
fn shared_native_mappers_preserve_existing_generic_bridge_semantics() {
    assert_eq!(
        axum_secret_name("trusted-server.secrets", "Mixed Case-Key"),
        "TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_MIXED_CASE_KEY"
    );
    assert_eq!(
        spin_secret_name("trusted_server_secrets", "fixture-Mixed.Key:one")
            .expect("should encode mixed-case and punctuation bytes in the Spin identity"),
        "v_trusted_x5fserver_x5fsecrets_v_fixture_x2d_x4dixed_x2e_x4bey_x3aone"
    );
    assert_ne!(
        spin_secret_name("store", "fixture_key")
            .expect("should encode the underscore-containing Spin key"),
        spin_secret_name("store", "fixture-key")
            .expect("should encode the hyphen-containing Spin key")
    );
    for invalid in [
        "",
        "1fixture",
        "Fixture",
        "fixture/key",
        "fixture value",
        "fïxture",
    ] {
        assert!(spin_secret_name("store", invalid).is_err());
    }
}

#[test]
fn resolved_plan_serialization_contains_only_metadata() {
    let settings = all_roles();
    let plan = compile_bindings(&settings).expect("should compile the secret-free binding plan");
    let serialized = serde_json::to_value(plan).expect("should serialize only binding metadata");
    assert_eq!(serialized["abi_version"], BINDING_ABI_VERSION);
    assert!(
        serialized["entries"]
            .as_array()
            .expect("should expose the serialized plan entries")
            .iter()
            .all(|entry| entry.get("credential").is_none())
    );
    assert!(
        serialized["entries"]
            .as_array()
            .expect("should expose the serialized plan entries")
            .iter()
            .all(|entry| entry.get("key").is_some())
    );
}
