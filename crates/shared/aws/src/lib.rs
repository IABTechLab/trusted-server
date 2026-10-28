//! What Amazon Web Services offers every crate that wants it: the `s3_sigv4`
//! origin signer, which signs a read from S3 with AWS Signature Version 4.
//!
//! A source's `auth` table selects it, for the publisher's origin or an asset
//! route's:
//!
//! ```toml
//! [source.origin]
//! url = "https://bucket.storage.example.com/site"
//!
//! [source.origin.auth]
//! type = "s3_sigv4"
//! region = "example-region-1"
//! access_key_id = "origin_access_key_id"
//! secret_access_key = "origin_secret_access_key"
//! # session_token = "origin_session_token"
//! # origin_query = "strip"
//! ```
//!
//! `region` is the region of the credential scope. `access_key_id`,
//! `secret_access_key` and `session_token` name keys in the default secret
//! store, which core looks up as the settings load, and the first two take
//! their own names when left out. `origin_query` is core's, see
//! `trusted_server_core::origin_auth`.
//!
//! The signer writes header-based authentication with `UNSIGNED-PAYLOAD`, so
//! it signs `GET` and `HEAD` reads and nothing with a body. The address must
//! be the host the service validates the signature for, and `https`, which
//! core requires of every signed source.

#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
        reason = "tests use direct diagnostics and panic-on-failure helpers"
    )
)]

mod sigv4;

use std::sync::Arc;
use std::time::SystemTime;

use error_stack::Report;
use http::{HeaderMap, Method};
use serde::Deserialize;
use serde_json::{Map, Value};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::IntegrationBuilder;
use trusted_server_core::origin_auth::{OriginSigner, OriginSignerModule, SignerSecret};
use trusted_server_core::redacted::Redacted;
use url::Url;

/// The id of this crate's builder, from the crate's folder.
pub const MODULE: &str = "shared.aws";

/// The name an `auth` table writes to select the signer.
pub const SIGNER: &str = "s3_sigv4";

/// The settings of the signer's table that name a secret, and the key name
/// each takes when left out.
const SECRETS: &[SignerSecret] = &[
    SignerSecret {
        name: "access_key_id",
        default: Some("access_key_id"),
    },
    SignerSecret {
        name: "secret_access_key",
        default: Some("secret_access_key"),
    },
    SignerSecret {
        name: "session_token",
        default: None,
    },
];

/// The signer, as a builder offers it.
pub static S3_SIGV4: OriginSignerModule = OriginSignerModule {
    name: SIGNER,
    secrets: SECRETS,
    build: build_signer,
};

/// The builder a deployment hands to an adapter, which offers the signer
/// whatever the settings say. It supplies no page module, and core checks
/// each `auth` table naming the signer when a deployment is validated. Each
/// adapter lists it among the modules it links.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::implementations(MODULE, env!("CARGO_PKG_NAME"))
        .with_origin_signer(&S3_SIGV4)
}

fn build_signer(
    settings: &Map<String, Value>,
    setting: &str,
) -> Result<Arc<dyn OriginSigner>, Report<TrustedServerError>> {
    let settings = S3SigV4Settings::read(settings, setting)?;
    Ok(Arc::new(S3SigV4Signer {
        region: settings.region,
        credentials: sigv4::Credentials {
            access_key_id: settings.access_key_id.expose().clone(),
            secret_access_key: settings.secret_access_key,
            session_token: settings.session_token,
        },
    }))
}

fn default_access_key_id() -> Redacted<String> {
    Redacted::new("access_key_id".to_string())
}

fn default_secret_access_key() -> Redacted<String> {
    Redacted::new("secret_access_key".to_string())
}

/// The signer's table. The credential fields hold key names in a pushed
/// configuration and the resolved values once the settings have loaded.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3SigV4Settings {
    /// The region of the credential scope.
    pub region: String,
    /// Accepted and discarded, because the store comes from the adapter's
    /// mapping.
    #[serde(default)]
    pub secret_store: Option<String>,
    /// The access key id.
    #[serde(default = "default_access_key_id")]
    pub access_key_id: Redacted<String>,
    /// The secret access key.
    #[serde(default = "default_secret_access_key")]
    pub secret_access_key: Redacted<String>,
    /// The session token of temporary credentials, when there is one.
    #[serde(default)]
    pub session_token: Option<Redacted<String>>,
}

fn region_is_valid(region: &str) -> bool {
    region
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl S3SigV4Settings {
    /// Reads and checks the signer's table, where `setting` names it in a
    /// refusal.
    ///
    /// # Errors
    ///
    /// When the table has a key the signer does not know, a region it cannot
    /// sign with, or an empty credential.
    pub fn read(
        table: &Map<String, Value>,
        setting: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut settings: Self =
            serde_json::from_value(Value::Object(table.clone())).map_err(|error| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("[{setting}] could not be read by {SIGNER}: {error}"),
                })
            })?;
        settings.normalize();
        settings.check(setting)?;
        Ok(settings)
    }

    fn normalize(&mut self) {
        self.region = self.region.trim().to_string();
        if self.secret_store.take().is_some() {
            log::warn!(
                "{SIGNER} secret_store is deprecated and ignored; credentials resolve through the default secret store"
            );
        }
        self.access_key_id = Redacted::new(self.access_key_id.expose().trim().to_string());
        self.secret_access_key = Redacted::new(self.secret_access_key.expose().trim().to_string());
        self.session_token = self.session_token.take().and_then(|value| {
            let value = value.expose().trim().to_string();
            (!value.is_empty()).then(|| Redacted::new(value))
        });
    }

    fn check(&self, setting: &str) -> Result<(), Report<TrustedServerError>> {
        if self.region.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("[{setting}] {SIGNER} region must not be empty"),
            }));
        }
        if !region_is_valid(&self.region) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{setting}] {SIGNER} region must contain only lowercase letters, digits, and '-'"
                ),
            }));
        }
        if self.access_key_id.expose().is_empty() || self.secret_access_key.expose().is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("[{setting}] {SIGNER} credentials must not be empty"),
            }));
        }
        Ok(())
    }
}

/// Signs each request with the settings it was built from.
struct S3SigV4Signer {
    region: String,
    credentials: sigv4::Credentials,
}

impl OriginSigner for S3SigV4Signer {
    fn sign(
        &self,
        method: &Method,
        url: &Url,
        headers: &mut HeaderMap,
        now: SystemTime,
    ) -> Result<(), Report<TrustedServerError>> {
        sigv4::sign_headers(method, url, headers, &self.region, &self.credentials, now)
    }

    fn remove_namespace_headers(&self, headers: &mut HeaderMap) {
        sigv4::remove_namespace_headers(headers);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use edgezero_core::blob_envelope::BlobEnvelope;
    use http::{HeaderMap, HeaderValue, Method, header};
    use serde_json::json;
    use trusted_server_core::config::validate_settings_for_deploy_with;
    use trusted_server_core::config_payload::settings_from_config_blob_with;
    use trusted_server_core::inspect::MASK;
    use trusted_server_core::inspect::config::build_view;
    use trusted_server_core::integrations::IntegrationRegistry;
    use trusted_server_core::origin_auth::OriginAuth;
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };
    use trusted_server_core::platform::{PlatformError, PlatformSecretStore, StoreId, StoreName};
    use trusted_server_core::proxy::handle_asset_proxy_request;
    use trusted_server_core::settings::{OriginQueryPolicy, ProxyAssetRoute, Settings};
    use trusted_server_core::source::SourceRequest;
    use trusted_server_core::test_support::template::{
        template_with_resolved_required_secrets, uncomment_block,
    };
    use trusted_server_core::test_support::tests::{
        create_test_settings, edit_origin, set_route_auth,
    };

    use super::*;

    const STORAGE_URL: &str = "https://bucket.storage.example.com/site";
    const REGION: &str = "test-region-1";

    /// An `auth` table selecting the signer with resolved credentials, as
    /// the settings hold them once loaded.
    fn auth(origin_query: Option<OriginQueryPolicy>) -> OriginAuth {
        OriginAuth::new(SIGNER)
            .with_setting("region", REGION)
            .with_setting("access_key_id", "TESTACCESSKEYID")
            .with_setting("secret_access_key", "test-secret-access-key")
            .with_origin_query(origin_query)
    }

    fn settings_signing_the_origin(auth: OriginAuth) -> Settings {
        let mut settings = create_test_settings();
        edit_origin(&mut settings, |origin| {
            origin.url = STORAGE_URL.to_owned();
            origin.auth = Some(auth);
        });
        settings
    }

    fn registry(settings: &Settings) -> Result<IntegrationRegistry, Report<TrustedServerError>> {
        IntegrationRegistry::with_registrations(settings, &[builder()])
    }

    /// The refusal a registry build ended in, or a panic saying `expected`
    /// when it built.
    fn refused(
        result: Result<IntegrationRegistry, Report<TrustedServerError>>,
        expected: &str,
    ) -> Report<TrustedServerError> {
        match result {
            Err(err) => err,
            Ok(_) => panic!("{expected}"),
        }
    }

    #[test]
    fn the_publisher_source_signs_through_the_signer_the_auth_names() {
        let settings = settings_signing_the_origin(auth(Some(OriginQueryPolicy::Strip)));
        let registry = registry(&settings).expect("should build the registry");
        let source = registry
            .publisher_source(&settings)
            .expect("should give the publisher's source");
        let headers = HeaderMap::new();
        let location = source
            .locate(&SourceRequest {
                method: &Method::GET,
                path: "/page",
                query: "",
                headers: &headers,
                removed_prefix: None,
            })
            .expect("should locate");
        assert!(location.query_is_stripped());

        let url = Url::parse("https://bucket.storage.example.com/site/page").expect("url");
        let mut sent = HeaderMap::new();
        sent.insert(
            header::HOST,
            HeaderValue::from_static("bucket.storage.example.com"),
        );
        sent.insert("x-amz-meta-reader", HeaderValue::from_static("chosen"));
        let signing = location.auth().expect("should sign");
        signing.remove_namespace_headers(&mut sent);
        signing
            .sign(&Method::GET, &url, &mut sent)
            .expect("should sign");

        let authorization = sent
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .expect("should write an Authorization header");
        assert!(
            authorization.starts_with("AWS4-HMAC-SHA256 Credential=TESTACCESSKEYID/")
                && authorization.contains(&format!("/{REGION}/s3/aws4_request")),
            "should sign with the settings' credentials and region: {authorization}"
        );
        assert_eq!(
            sent.get("x-amz-content-sha256"),
            Some(&HeaderValue::from_static("UNSIGNED-PAYLOAD"))
        );
        assert!(
            sent.get("x-amz-meta-reader").is_none(),
            "a reader's header in the scheme's namespace should be removed"
        );
    }

    #[test]
    fn an_asset_route_signs_the_address_it_sends() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"ok".to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn trusted_server_core::platform::PlatformHttpClient>
            );
            let mut settings = create_test_settings();
            let mut route = ProxyAssetRoute::new("/assets/", STORAGE_URL);
            set_route_auth(&mut route, Some(auth(None)));
            settings.proxy.asset_routes.push(route);
            let registry = registry(&settings).expect("should build the registry");
            let source = registry
                .route_source(&settings, 0)
                .expect("should give the route's source");
            let request = http::Request::builder()
                .method(Method::GET)
                .uri("https://www.example.com/assets/app.js?v=2")
                .body(edgezero_core::body::Body::empty())
                .expect("should build the request");

            handle_asset_proxy_request(
                &settings,
                &services,
                request,
                &settings.proxy.asset_routes[0],
                source.as_ref(),
            )
            .await
            .expect("should proxy the signed asset request");

            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://bucket.storage.example.com/site/assets/app.js?v=2"],
                "the query is kept unless origin_query strips it"
            );
            let headers = stub.recorded_request_headers();
            let sent = &headers[0];
            let value = |name: &str| {
                sent.iter()
                    .find(|(sent_name, _)| sent_name == name)
                    .map(|(_, value)| value.as_str())
            };
            assert_eq!(value("host"), Some("bucket.storage.example.com"));
            assert!(
                value("authorization").is_some_and(|authorization| authorization
                    .starts_with("AWS4-HMAC-SHA256 Credential=TESTACCESSKEYID/")),
                "should sign the request sent: {:?}",
                value("authorization")
            );
            assert_eq!(value("x-amz-content-sha256"), Some("UNSIGNED-PAYLOAD"));
        });
    }

    #[test]
    fn a_table_the_signer_cannot_use_is_refused_at_startup_and_at_deploy_validation() {
        validate_settings_for_deploy_with(&settings_signing_the_origin(auth(None)), &[builder()])
            .expect("a table the signer can use should pass deploy validation");

        // A blank credential is caught as an empty key reference when the
        // deployment is validated, before the signer reads its table.
        for (table, at_startup, at_deploy) in [
            (
                OriginAuth::new(SIGNER).with_setting("region", ""),
                "[source.origin.auth] s3_sigv4 region must not be empty",
                "[source.origin.auth] s3_sigv4 region must not be empty",
            ),
            (
                OriginAuth::new(SIGNER).with_setting("region", "Test_Region"),
                "[source.origin.auth] s3_sigv4 region must contain only lowercase letters",
                "[source.origin.auth] s3_sigv4 region must contain only lowercase letters",
            ),
            (
                auth(None).with_setting("access_key_id", " "),
                "[source.origin.auth] s3_sigv4 credentials must not be empty",
                "secret key reference at `source.origin.auth.access_key_id` must not be empty",
            ),
            (
                auth(None).with_setting("secret_access_key", ""),
                "[source.origin.auth] s3_sigv4 credentials must not be empty",
                "secret key reference at `source.origin.auth.secret_access_key` must not be empty",
            ),
            (
                auth(None).with_setting("bucket", "site"),
                "unknown field `bucket`",
                "unknown field `bucket`",
            ),
            (
                OriginAuth::new(SIGNER),
                "missing field `region`",
                "missing field `region`",
            ),
        ] {
            let settings = settings_signing_the_origin(table.clone());
            let err = refused(registry(&settings), "should refuse the table at startup");
            assert!(
                format!("{err:?}").contains(at_startup),
                "{table:?} should be refused saying `{at_startup}`: {err:?}"
            );

            let err = validate_settings_for_deploy_with(&settings, &[builder()])
                .expect_err("should refuse the table when the deployment is validated");
            assert!(
                format!("{err:?}").contains(at_deploy),
                "{table:?} should fail deploy validation saying `{at_deploy}`: {err:?}"
            );
        }
    }

    #[test]
    fn an_asset_route_refusal_names_the_route() {
        let mut settings = create_test_settings();
        let mut route = ProxyAssetRoute::new("/assets/", STORAGE_URL);
        set_route_auth(
            &mut route,
            Some(OriginAuth::new(SIGNER).with_setting("region", "")),
        );
        settings.proxy.asset_routes.push(route);

        let err = refused(registry(&settings), "should refuse the route's table");
        assert!(
            format!("{err:?}").contains(
                "[proxy.asset_routes /assets/ source.origin.auth] s3_sigv4 region must not be empty"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn the_signer_is_refused_where_the_build_does_not_offer_it() {
        let settings = settings_signing_the_origin(auth(None));

        let err = refused(
            IntegrationRegistry::with_registrations(&settings, &[]),
            "should refuse a signer nothing offers",
        );
        assert!(
            format!("{err:?}").contains(
                "[source.origin.auth] type `s3_sigv4` names an origin signer this deployment does not offer"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn a_blank_session_token_is_no_token_and_a_deprecated_store_is_discarded() {
        let settings = S3SigV4Settings::read(
            &serde_json::from_value(json!({
                "region": " test-region-1 ",
                "secret_store": "old-store",
                "access_key_id": " TESTACCESSKEYID ",
                "secret_access_key": "test-secret-access-key",
                "session_token": " ",
            }))
            .expect("a table"),
            "source.origin.auth",
        )
        .expect("should read the table");

        assert_eq!(settings.region, "test-region-1");
        assert_eq!(settings.access_key_id.expose(), "TESTACCESSKEYID");
        assert!(settings.session_token.is_none());
        assert!(settings.secret_store.is_none());
    }

    #[test]
    fn the_documented_table_builds_the_signer() {
        let toml = uncomment_block(
            &template_with_resolved_required_secrets(),
            "[source.origin.auth]",
        );
        let settings = Settings::from_toml(&toml).expect("the documented table should parse");

        // The template selects modules this crate's tests do not link, so
        // the publisher's source is built on its own, as the registry builds
        // it, rather than through a registry.
        let location = settings
            .origin_source()
            .expect("the template should select the origin source")
            .location(
                &trusted_server_core::origin_auth::OriginSigners::new(vec![&S3_SIGV4]),
                "source.origin",
            )
            .expect("the documented table should build the signer");
        assert!(
            location.auth().is_some(),
            "the documented table should sign"
        );
    }

    /// Every value this store hands back starts with this.
    const CANARY: &str = "canary-";

    /// Resolves every key name to a value of its own, long enough for any
    /// minimum length a secret has.
    struct CanaryStore;

    impl PlatformSecretStore for CanaryStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            Ok(format!("{CANARY}{key}-0123456789abcdef").into_bytes())
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    /// Loads `settings` as a deployment does, with the signer's builder, so
    /// its secret settings are looked up in the canary store.
    fn load(settings: &Settings) -> Settings {
        let mut pushed = settings.clone();
        // The blob a push writes names the policy and carries its file.
        pushed.permissions = trusted_server_core::test_support::tests::sample_permissions();
        let data = serde_json::to_value(&pushed).expect("should serialize");
        let envelope = BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned());
        let envelope = serde_json::to_string(&envelope).expect("should serialize the envelope");
        settings_from_config_blob_with(
            &envelope,
            &CanaryStore,
            &StoreName::from("ts_secrets"),
            trusted_server_core::platform::PolicyOrigin::Blob,
            &[builder()],
        )
        .expect("should load the settings")
    }

    #[test]
    fn the_credentials_are_looked_up_at_both_auth_paths_and_masked_on_the_configuration_page() {
        let mut settings = create_test_settings();
        edit_origin(&mut settings, |origin| {
            origin.url = STORAGE_URL.to_owned();
            origin.auth = Some(
                OriginAuth::new(SIGNER)
                    .with_setting("region", REGION)
                    .with_setting("access_key_id", "origin-access")
                    .with_setting("secret_access_key", "origin-secret")
                    .with_setting("session_token", "origin-session"),
            );
        });
        let mut route = ProxyAssetRoute::new("/assets/", "https://assets.example.com");
        set_route_auth(
            &mut route,
            Some(OriginAuth::new(SIGNER).with_setting("region", REGION)),
        );
        settings.proxy.asset_routes.push(route);

        let loaded = load(&settings);

        let origin = loaded
            .origin_source()
            .and_then(|origin| origin.auth)
            .expect("should keep the origin's auth");
        assert_eq!(
            origin
                .settings
                .get("secret_access_key")
                .and_then(Value::as_str),
            Some("canary-origin-secret-0123456789abcdef")
        );
        assert_eq!(
            origin.settings.get("session_token").and_then(Value::as_str),
            Some("canary-origin-session-0123456789abcdef")
        );
        let route = loaded.proxy.asset_routes[0]
            .origin_source()
            .and_then(|origin| origin.auth)
            .expect("should keep the route's auth");
        assert_eq!(
            route.settings.get("access_key_id").and_then(Value::as_str),
            Some("canary-access_key_id-0123456789abcdef"),
            "a credential left out should be looked up under its own name"
        );
        assert_eq!(
            route
                .settings
                .get("secret_access_key")
                .and_then(Value::as_str),
            Some("canary-secret_access_key-0123456789abcdef")
        );
        assert!(
            !route.settings.contains_key("session_token"),
            "a session token left out stays left out"
        );

        let view = build_view(&loaded).expect("should build the configuration view");
        let rendered = serde_json::to_string(&view.settings).expect("should serialize");
        assert!(
            !rendered.contains(CANARY),
            "no credential should reach the configuration page: {rendered}"
        );
        for path in [
            "source.origin.auth.access_key_id",
            "source.origin.auth.secret_access_key",
            "source.origin.auth.session_token",
            "proxy.asset_routes[0].source.origin.auth.access_key_id",
            "proxy.asset_routes[0].source.origin.auth.secret_access_key",
        ] {
            assert!(
                view.masked.iter().any(|masked| masked == path),
                "{path} should be masked: {:?}",
                view.masked
            );
        }
        assert_eq!(
            view.settings["source"]["origin"]["auth"]["region"],
            json!(REGION),
            "the region is not a secret"
        );
        assert_eq!(
            view.settings["source"]["origin"]["auth"]["secret_access_key"],
            json!(MASK)
        );

        let registry = registry(&loaded).expect("the loaded settings should build the signer");
        assert!(registry.publisher_source(&loaded).is_ok());
    }
}
