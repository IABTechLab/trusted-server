use std::cell::{Cell, RefCell};
use std::future::pending;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use edgezero_core::app::Hooks as _;
use edgezero_core::blob_envelope::BlobEnvelope;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_SECRET_STORE_ID};
use trusted_server_core::request_signing::Keypair;

use super::*;

struct Fixture {
    config: BTreeMap<String, String>,
    variables: BTreeMap<String, Vec<u8>>,
    secret_reads: RefCell<Vec<String>>,
    pending_at: Option<&'static str>,
    polled: Cell<bool>,
    dropped: Rc<Cell<bool>>,
}

struct DropProbe(Rc<Cell<bool>>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

impl Fixture {
    fn new(signing: bool) -> Self {
        let data = serde_json::json!({
            "ec": { "passphrase": "ec_key" },
            "handlers": [{ "path": "^/_ts/admin", "username": "admin", "password": "admin_key" }],
            "publisher": {
                "domain": "publisher.example.com", "cookie_domain": ".publisher.example.com",
                "origin_url": "https://origin.publisher.example.com", "proxy_secret": "proxy_key"
            },
            "request_signing": if signing { serde_json::json!({
                "enabled": true, "config_store_id": "fixture-config", "secret_store_id": "fixture-secrets"
            }) } else { serde_json::Value::Null }
        });
        let envelope = BlobEnvelope::new(data, "2026-10-20T00:00:00Z".to_owned());
        let mut fixture = Self {
            config: BTreeMap::from([(
                CONFIG_BLOB_KEY.to_owned(),
                serde_json::to_string(&envelope).expect("should serialize fixture envelope"),
            )]),
            variables: BTreeMap::new(),
            secret_reads: RefCell::new(Vec::new()),
            pending_at: None,
            polled: Cell::new(false),
            dropped: Rc::new(Cell::new(false)),
        };
        let store = StoreName::from(DEFAULT_SECRET_STORE_ID);
        for (key, value) in [
            ("ec_key", "fixture-passphrase-32-bytes-minimum"),
            ("proxy_key", "fixture-proxy-secret-32-bytes-minimum"),
            ("admin_key", "fixture-admin-password"),
        ] {
            fixture.set_secret(&store, key, value.as_bytes().to_vec());
        }
        fixture
    }

    fn set_secret(&mut self, store: &StoreName, key: &str, value: Vec<u8>) {
        let variable = crate::platform::spin_secret_variable_name(store, key)
            .expect("should encode fixture variable");
        self.variables.insert(variable, value);
    }

    async fn pause(&self, stage: &'static str) {
        if self.pending_at == Some(stage) {
            let _probe = DropProbe(Rc::clone(&self.dropped));
            self.polled.set(true);
            pending::<()>().await;
        }
    }
}

#[async_trait::async_trait(?Send)]
impl Source for Fixture {
    async fn config_keys(&self) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.pause("keys").await;
        Ok(self.config.keys().cloned().collect())
    }

    async fn config(
        &self,
        key: &str,
        _: &MonotonicClock,
        _: Deadline,
    ) -> Result<Option<String>, Report<TrustedServerError>> {
        self.pause("config").await;
        Ok(self.config.get(key).cloned())
    }

    async fn secret(
        &self,
        store: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<TrustedServerError>> {
        self.pause("secret").await;
        let variable = crate::platform::spin_secret_variable_name(store, key)
            .expect("should encode existing secret naming contract");
        self.secret_reads.borrow_mut().push(variable.clone());
        self.variables
            .get(&variable)
            .cloned()
            .ok_or_else(|| failure("missing fixture secret"))
    }
}

async fn load_fixture(
    fixture: &Fixture,
) -> Result<
    (
        trusted_server_core::settings::Settings,
        Arc<SpinStoreSnapshot>,
    ),
    Report<TrustedServerError>,
> {
    let clock = MonotonicClock::default();
    let deadline = clock.deadline_after(BOOTSTRAP_BUDGET);
    load(
        fixture,
        CONFIG_BLOB_KEY,
        &StoreName::from(DEFAULT_SECRET_STORE_ID),
        &clock,
        deadline,
    )
    .await
}

#[tokio::test]
async fn envelope_and_encoded_secrets_build_an_initialized_app() {
    let mut fixture = Fixture::new(false);
    fixture
        .config
        .insert("cidr-list".to_owned(), "198.51.100.0/24".to_owned());
    let (settings, snapshot) = load_fixture(&fixture)
        .await
        .expect("should await finite startup inputs");
    assert!(
        fixture
            .secret_reads
            .borrow()
            .contains(&"v_trusted_x5fserver_x5fsecrets_v_ec_x5fkey".to_owned()),
        "should keep the encoded store/key variable contract"
    );
    assert_eq!(
        PlatformConfigStore::get(&*snapshot, &StoreName::from("custom-store"), "cidr-list")
            .expect("should preserve single-default-store config mapping"),
        "198.51.100.0/24"
    );
    fixture
        .config
        .insert("cidr-list".to_owned(), "203.0.113.0/24".to_owned());
    assert_eq!(
        PlatformConfigStore::get(&*snapshot, &StoreName::from("custom-store"), "cidr-list")
            .expect("should read immutable snapshot"),
        "198.51.100.0/24"
    );
    assert!(PlatformConfigStore::get(&*snapshot, &StoreName::from("default"), "absent").is_err());
    assert!(
        PlatformSecretStore::get_bytes(&*snapshot, &StoreName::from("other-store"), "ec_key")
            .is_err()
    );
    assert!(
        PlatformConfigStore::put(&*snapshot, &StoreId::from("default"), "key", "value").is_err()
    );
    assert!(
        PlatformSecretStore::create(&*snapshot, &StoreId::from("default"), "key", "value").is_err()
    );
    assert!(PlatformConfigStore::delete(&*snapshot, &StoreId::from("default"), "key").is_err());
    assert!(PlatformSecretStore::delete(&*snapshot, &StoreId::from("default"), "key").is_err());
    let app = assemble(settings, snapshot).expect("should assemble after successful bootstrap");
    assert_eq!(app.platform(), edgezero_adapter_spin::SPIN_PLATFORM);
    let request = request_builder()
        .uri("/health")
        .body(edgezero_core::body::Body::empty())
        .expect("should build health request");
    let response = app
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("should dispatch initialized health");
    assert_eq!(response.status(), edgezero_core::http::StatusCode::OK);
}

#[tokio::test]
async fn inactive_secret_references_do_not_trigger_provider_reads() {
    let mut fixture = Fixture::new(false);
    let envelope: BlobEnvelope = serde_json::from_str(
        fixture
            .config
            .get(CONFIG_BLOB_KEY)
            .expect("should have fixture envelope"),
    )
    .expect("should parse fixture envelope");
    let mut data = envelope.into_data();
    data["tinybird"] = serde_json::json!({
        "enabled": false, "auction_token_secret": "unused_auction", "access_token_secret": "unused_access"
    });
    fixture.config.insert(
        CONFIG_BLOB_KEY.to_owned(),
        serde_json::to_string(&BlobEnvelope::new(data, "2026-10-20T00:00:00Z".to_owned()))
            .expect("should serialize changed fixture envelope"),
    );
    load_fixture(&fixture)
        .await
        .expect("should ignore disabled Tinybird secret references");
    assert_eq!(
        fixture.secret_reads.borrow().len(),
        3,
        "should await only active config references"
    );
}

#[tokio::test]
async fn expired_bootstrap_never_polls_a_provider() {
    let fixture = Fixture::new(false);
    let clock = MonotonicClock::default();
    let deadline = Deadline::at_instant(clock.now());
    let store = StoreName::from(DEFAULT_SECRET_STORE_ID);
    let result = within_deadline(
        load(&fixture, CONFIG_BLOB_KEY, &store, &clock, deadline),
        &clock,
        deadline,
        |_| pending::<()>(),
    )
    .await;
    assert!(
        result.is_err(),
        "should reject expired bootstrap before first provider poll"
    );
    assert!(fixture.secret_reads.borrow().is_empty());
}

#[tokio::test]
async fn missing_required_secret_fails_before_a_dispatchable_app_exists() {
    let mut fixture = Fixture::new(false);
    fixture.variables.clear();
    let result = load_fixture(&fixture).await;
    assert!(
        result.is_err(),
        "should return bootstrap error, not a startup-error router"
    );
}

#[tokio::test]
async fn tampered_envelope_never_reads_secrets() {
    let mut fixture = Fixture::new(false);
    let envelope = fixture
        .config
        .get_mut(CONFIG_BLOB_KEY)
        .expect("should have envelope");
    *envelope = envelope.replace("publisher.example.com", "changed.example.com");
    assert!(
        load_fixture(&fixture).await.is_err(),
        "should reject integrity failure"
    );
    assert!(
        fixture.secret_reads.borrow().is_empty(),
        "should verify before any secret reads"
    );
}

#[tokio::test]
async fn signing_snapshot_preserves_active_and_inactive_public_keys_and_current_private_key() {
    let mut fixture = Fixture::new(true);
    let keypair = Keypair::generate();
    fixture
        .config
        .insert("current-kid".to_owned(), "kid-current".to_owned());
    fixture
        .config
        .insert("active-kids".to_owned(), "kid-current".to_owned());
    for kid in ["kid-current", "kid-retired"] {
        fixture.config.insert(
            kid.to_owned(),
            serde_json::to_string(&keypair.get_jwk(kid.to_owned()))
                .expect("should serialize public JWK"),
        );
    }
    fixture.set_secret(
        &StoreName::from(SIGNING_SECRET_STORE_NAME),
        "kid-current",
        STANDARD.encode(keypair.signing_key.to_bytes()).into_bytes(),
    );
    let (settings, snapshot) = load_fixture(&fixture)
        .await
        .expect("should load configured signing inputs");
    validate_signing(&snapshot).expect("should preserve current key parsing and matching");
    let mut request = request_builder()
        .uri("/")
        .body(edgezero_core::body::Body::empty())
        .expect("should build signing context");
    request.extensions_mut().insert(Arc::clone(&snapshot));
    let context = RequestContext::new(request, PathParams::default());
    let services = crate::platform::build_runtime_services(&context);
    let signature = RequestSigner::from_services(&services)
        .expect("should load signer")
        .sign(b"fixture payload")
        .expect("should sign fixture payload");
    assert!(
        verify_signature(b"fixture payload", &signature, "kid-retired", &services)
            .expect("should verify with stored non-active public KID")
    );
    assert!(
        PlatformConfigStore::get(
            &*snapshot,
            &StoreName::from(JWKS_CONFIG_STORE_NAME),
            "kid-retired"
        )
        .is_ok(),
        "should not limit verification to active-kids"
    );
    assert!(
        fixture
            .secret_reads
            .borrow()
            .contains(&"v_signing_x5fkeys_v_kid_x2dcurrent".to_owned()),
        "should preserve signing secret variable name"
    );
    assemble(settings, snapshot).expect("should retain signing-enabled app support");
}

#[tokio::test]
async fn signing_rejects_missing_private_key_and_mismatched_key_material() {
    let mut fixture = Fixture::new(true);
    fixture
        .config
        .insert("current-kid".to_owned(), "kid-current".to_owned());
    fixture
        .config
        .insert("active-kids".to_owned(), "kid-current".to_owned());
    let public = Keypair::generate();
    fixture.config.insert(
        "kid-current".to_owned(),
        serde_json::to_string(&public.get_jwk("kid-current".to_owned()))
            .expect("should serialize public key"),
    );
    assert!(
        load_fixture(&fixture).await.is_err(),
        "should fail before dispatch with missing signing secret"
    );
    let other = Keypair::generate();
    fixture.set_secret(
        &StoreName::from(SIGNING_SECRET_STORE_NAME),
        "kid-current",
        STANDARD.encode(other.signing_key.to_bytes()).into_bytes(),
    );
    assert!(
        load_fixture(&fixture).await.is_err(),
        "should reject public/private mismatch"
    );
}

#[tokio::test]
async fn pending_provider_work_times_out_and_is_dropped_at_every_bootstrap_stage() {
    for stage in ["config", "secret", "keys"] {
        let mut fixture = Fixture::new(false);
        fixture.pending_at = Some(stage);
        let expired = Arc::new(AtomicBool::new(false));
        let start = edgezero_core::time::MonotonicInstant::now();
        let end = start
            .checked_add(BOOTSTRAP_BUDGET)
            .expect("should make deadline");
        let clock_expired = Arc::clone(&expired);
        let clock = MonotonicClock::new(move || {
            if clock_expired.load(Ordering::SeqCst) {
                end
            } else {
                start
            }
        });
        let deadline = Deadline::at_instant(end);
        let store = StoreName::from(DEFAULT_SECRET_STORE_ID);
        let result = within_deadline(
            load(&fixture, CONFIG_BLOB_KEY, &store, &clock, deadline),
            &clock,
            deadline,
            |_| {
                let expired = Arc::clone(&expired);
                async move {
                    tokio::task::yield_now().await;
                    expired.store(true, Ordering::SeqCst);
                }
            },
        )
        .await;
        assert!(
            result.is_err(),
            "should bound a genuinely pending {stage} provider"
        );
        assert!(
            fixture.polled.get(),
            "should poll pending {stage} work before timing out"
        );
        assert!(
            fixture.dropped.get(),
            "should drop pending {stage} work on timeout"
        );
    }
}

#[tokio::test]
async fn snapshots_enforce_key_value_secret_and_aggregate_caps() {
    let mut fixture = Fixture::new(false);
    for index in 0..MAX_CONFIG_KEYS {
        fixture
            .config
            .insert(format!("key-{index}"), "value".to_owned());
    }
    assert!(
        load_fixture(&fixture).await.is_err(),
        "should cap key count including envelope"
    );
    let mut fixture = Fixture::new(false);
    fixture.config.insert(
        "large".to_owned(),
        "x".repeat(MAX_CONFIG_BYTES as usize + 1),
    );
    assert!(
        load_fixture(&fixture).await.is_err(),
        "should cap each materialized config value"
    );
    let mut fixture = Fixture::new(false);
    fixture.set_secret(
        &StoreName::from(DEFAULT_SECRET_STORE_ID),
        "ec_key",
        vec![b'x'; MAX_SECRET_BYTES + 1],
    );
    assert!(
        load_fixture(&fixture).await.is_err(),
        "should cap each materialized secret value"
    );
    let mut total = 0;
    charge(&mut total, MAX_SNAPSHOT_BYTES).expect("should allow exact aggregate cap");
    assert!(
        charge(&mut total, 1).is_err(),
        "should reject aggregate cap plus one"
    );
}

#[tokio::test]
async fn separate_invocations_keep_secrets_and_client_metadata_local() {
    let first = Fixture::new(false);
    let mut second = Fixture::new(false);
    second.set_secret(
        &StoreName::from(DEFAULT_SECRET_STORE_ID),
        "ec_key",
        b"second-fixture-passphrase-32-bytes".to_vec(),
    );
    let (_, first_snapshot) = load_fixture(&first)
        .await
        .expect("should load first invocation");
    let (_, second_snapshot) = load_fixture(&second)
        .await
        .expect("should load second invocation");
    for (snapshot, ip, expected) in [
        (
            first_snapshot,
            "198.51.100.8",
            b"fixture-passphrase-32-bytes-minimum".as_slice(),
        ),
        (
            second_snapshot,
            "203.0.113.9",
            b"second-fixture-passphrase-32-bytes".as_slice(),
        ),
    ] {
        let mut request = request_builder()
            .uri("/")
            .header("spin-client-addr", "192.0.2.1:1234")
            .header("spin-client-addr", format!("{ip}:5678"))
            .header("spin-full-url", "https://publisher.example.com/")
            .body(edgezero_core::body::Body::empty())
            .expect("should build invocation request");
        request.extensions_mut().insert(snapshot);
        let (mut parts, body) = request.into_parts();
        crate::app::normalize_spin_request(&mut parts);
        let context = RequestContext::new(
            edgezero_core::http::Request::from_parts(parts, body),
            edgezero_core::params::PathParams::default(),
        );
        let services = crate::platform::build_runtime_services(&context);
        assert_eq!(
            services.client_info().client_ip,
            Some(ip.parse().expect("should parse client IP")),
            "should use trusted invocation peer"
        );
        assert_eq!(
            services
                .secret_store()
                .get_bytes(&StoreName::from(DEFAULT_SECRET_STORE_ID), "ec_key")
                .expect("should read this invocation's secret"),
            expected,
            "should never reuse another invocation's snapshot"
        );
    }
}

#[test]
fn production_provider_bridge_has_no_synchronous_executor() {
    for source in [
        include_str!("../../platform.rs"),
        include_str!("../../app.rs"),
    ] {
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("should have production section");
        assert!(
            !production.contains("block_on"),
            "should never reintroduce production provider block_on"
        );
    }
    let startup = include_str!("../../startup.rs");
    assert!(
        !startup.contains("block_on"),
        "should await bootstrap providers with the host executor"
    );
    assert!(
        !startup.contains("static "),
        "should not retain live global SDK resources"
    );
    let entrypoint = include_str!("../../lib.rs");
    assert!(
        entrypoint.find("startup::initialize().await")
            < entrypoint.find("dispatch_app::<app::TrustedServerApp>"),
        "should propagate bootstrap failures before routed admission"
    );
    assert!(entrypoint.contains("startup::initialize().await"));
    assert!(entrypoint.contains("dispatch_app::<app::TrustedServerApp>"));
    assert!(entrypoint.contains("anyhow::Result<edgezero_adapter_spin::SpinResponse>"));
    let _ = TrustedServerApp::routes();
}
