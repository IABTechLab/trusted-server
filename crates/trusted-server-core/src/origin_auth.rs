//! Signing of requests to an origin that answers only signed reads.
//!
//! A source's `auth` table selects a signer by `type` and holds the signer's
//! settings beside it:
//!
//! ```toml
//! [source.origin]
//! url = "https://storage.example.com/bucket/site"
//! auth = { type = "example_scheme", origin_query = "strip", key_name = "origin_key" }
//! ```
//!
//! Core reads `type`, which names a signer, and `origin_query`, which says
//! whether the reader's query is left off a signed request. The rest of the
//! table belongs to the signer. A crate offers a signer on its builder with
//! [`IntegrationBuilder::with_origin_signer`](crate::integrations::IntegrationBuilder::with_origin_signer),
//! and the sources are built from the settings when the registry is, which
//! is where a `type` nothing offers, or a table the signer cannot use, is
//! refused.
//!
//! A signer declares which of its settings hold the name of a secret, so
//! each is looked up in the default secret store as the settings load, at
//! both places an `auth` table can be written, and masked on the
//! configuration page.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use edgezero_core::app_config::{SecretField, SecretKind, SecretPathSegment};
use error_stack::Report;
use http::{HeaderMap, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::error::TrustedServerError;
use crate::integrations::IntegrationBuilder;
use crate::settings::{OriginQueryPolicy, Settings};

/// An `auth` table as a source writes it: the signer it names, whether the
/// reader's query is kept, and the signer's own settings.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct OriginAuth {
    /// The name of the signer, as a crate offers it.
    #[serde(rename = "type")]
    pub signer: String,
    /// Whether the reader's query is kept on the signed request. Left unset,
    /// the route's own default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_query: Option<OriginQueryPolicy>,
    /// The signer's settings, which core does not read.
    #[serde(flatten)]
    pub settings: Map<String, Value>,
}

impl fmt::Debug for OriginAuth {
    // The signer's settings hold its secrets once the settings have loaded,
    // so only their names are shown.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OriginAuth")
            .field("signer", &self.signer)
            .field("origin_query", &self.origin_query)
            .field("settings", &self.settings.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl OriginAuth {
    /// An `auth` table selecting `signer` with no settings.
    #[must_use]
    pub fn new(signer: impl Into<String>) -> Self {
        Self {
            signer: signer.into(),
            ..Self::default()
        }
    }

    /// The same table with `key` set to `value` among the signer's settings.
    #[must_use]
    pub fn with_setting(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.settings.insert(key.into(), value.into());
        self
    }

    /// The same table keeping or stripping the reader's query.
    #[must_use]
    pub fn with_origin_query(mut self, policy: Option<OriginQueryPolicy>) -> Self {
        self.origin_query = policy;
        self
    }

    pub(crate) fn normalize(&mut self) {
        self.signer = self.signer.trim().to_owned();
    }

    /// Refuses a table that names no signer, or names one in a form no
    /// crate could offer, naming `setting`.
    pub(crate) fn check(&self, setting: &str) -> Result<(), Report<TrustedServerError>> {
        if self.signer.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("[{setting}] names no signer. Write `type`"),
            }));
        }
        if !crate::module_name::is_valid(&self.signer) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{setting}] type `{}` is not a signer's name. A signer is named in lower \
                     case letters, digits, `_` or `-`",
                    self.signer
                ),
            }));
        }
        Ok(())
    }
}

/// Signs a request to an origin.
///
/// The address and the headers are final when a signer is called, the `Host`
/// header among them, so the signature covers exactly what is sent.
pub trait OriginSigner: Send + Sync {
    /// Signs the request `headers` carry for `method` on `url` at `now`,
    /// replacing any earlier signature.
    ///
    /// # Errors
    ///
    /// When a header the signature needs is missing or cannot be written.
    fn sign(
        &self,
        method: &Method,
        url: &Url,
        headers: &mut HeaderMap,
        now: SystemTime,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Removes every header in the scheme's own namespace, so a header the
    /// reader chose cannot reach the origin beside the signature.
    fn remove_namespace_headers(&self, headers: &mut HeaderMap);
}

/// A setting of a signer's table that holds the name of a secret.
#[derive(Debug, Clone, Copy)]
pub struct SignerSecret {
    /// The setting's key in the `auth` table.
    pub name: &'static str,
    /// The key name read when the setting is left out, or none for a
    /// setting that is only looked up when written.
    pub default: Option<&'static str>,
}

/// Builds the signer `settings` configure, being the `auth` table without
/// the keys core reads, where `setting` names the table in a refusal.
pub type OriginSignerBuildFn =
    fn(&Map<String, Value>, &str) -> Result<Arc<dyn OriginSigner>, Report<TrustedServerError>>;

/// A signer a crate offers, which an `auth` table selects with
/// `type = "<name>"`.
pub struct OriginSignerModule {
    /// The name an `auth` table writes.
    pub name: &'static str,
    /// The settings of the signer's table that hold the name of a secret.
    pub secrets: &'static [SignerSecret],
    /// Builds the signer from its table.
    pub build: OriginSignerBuildFn,
}

impl fmt::Debug for OriginSignerModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OriginSignerModule")
            .field("name", &self.name)
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

/// The signing a location applies to each request: the signer and whether
/// the reader's query is kept.
#[derive(Clone)]
pub struct OriginSigning {
    signer: Arc<dyn OriginSigner>,
    origin_query: Option<OriginQueryPolicy>,
}

impl fmt::Debug for OriginSigning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OriginSigning")
            .field("origin_query", &self.origin_query)
            .finish_non_exhaustive()
    }
}

impl OriginSigning {
    /// Signing through `signer`, keeping or stripping the reader's query as
    /// `origin_query` says.
    #[must_use]
    pub fn new(signer: Arc<dyn OriginSigner>, origin_query: Option<OriginQueryPolicy>) -> Self {
        Self {
            signer,
            origin_query,
        }
    }

    /// Signs the request `headers` carry for `method` on `url`, now.
    ///
    /// # Errors
    ///
    /// As [`OriginSigner::sign`].
    pub fn sign(
        &self,
        method: &Method,
        url: &Url,
        headers: &mut HeaderMap,
    ) -> Result<(), Report<TrustedServerError>> {
        self.signer
            .sign(method, url, headers, crate::ec::std_system_time_now())
    }

    /// Removes every header in the signer's namespace, see
    /// [`OriginSigner::remove_namespace_headers`].
    pub fn remove_namespace_headers(&self, headers: &mut HeaderMap) {
        self.signer.remove_namespace_headers(headers);
    }

    /// Whether the reader's query is kept on the signed request, when the
    /// `auth` table says.
    #[must_use]
    pub fn origin_query_policy(&self) -> Option<OriginQueryPolicy> {
        self.origin_query
    }
}

/// The signers a deployment's builders offer, by name.
#[derive(Clone, Default)]
pub struct OriginSigners {
    offered: Vec<&'static OriginSignerModule>,
}

impl fmt::Debug for OriginSigners {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OriginSigners")
            .field("offered", &self.names())
            .finish()
    }
}

impl OriginSigners {
    /// The signers `offered`, in that order.
    #[must_use]
    pub fn new(offered: Vec<&'static OriginSignerModule>) -> Self {
        Self { offered }
    }

    /// The signers `builders` offer, or a refusal when two of them offer one
    /// name.
    ///
    /// # Errors
    ///
    /// Naming the signer and both builders.
    pub fn offered_by(builders: &[IntegrationBuilder]) -> Result<Self, Report<TrustedServerError>> {
        let mut offered: Vec<(&'static OriginSignerModule, &'static str)> = Vec::new();
        for builder in builders {
            let Some(module) = builder.origin_signer() else {
                continue;
            };
            if let Some((_, by)) = offered.iter().find(|(known, _)| known.name == module.name) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "`{}` and `{by}` both offer the origin signer `{}`",
                        builder.id(),
                        module.name
                    ),
                }));
            }
            offered.push((module, builder.id()));
        }
        Ok(Self::new(
            offered.into_iter().map(|(module, _)| module).collect(),
        ))
    }

    /// The names on offer, in the order offered.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.offered.iter().map(|module| module.name).collect()
    }

    /// The signer offered under `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'static OriginSignerModule> {
        self.offered
            .iter()
            .copied()
            .find(|module| module.name == name)
    }

    /// The signing `auth` selects, built from its table, or a refusal naming
    /// `setting`.
    ///
    /// # Errors
    ///
    /// When `auth` names a signer nothing offers, with the names on offer, or
    /// when the signer cannot use its table.
    pub fn build(
        &self,
        auth: &OriginAuth,
        setting: &str,
    ) -> Result<OriginSigning, Report<TrustedServerError>> {
        let Some(module) = self.get(&auth.signer) else {
            let names = self.names();
            let offer = if names.is_empty() {
                "It offers none".to_owned()
            } else {
                format!("The origin signers it offers are [{}]", names.join(", "))
            };
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{setting}] type `{}` names an origin signer this deployment does not \
                     offer. {offer}",
                    auth.signer
                ),
            }));
        };
        let signer = (module.build)(&auth.settings, setting)?;
        Ok(OriginSigning::new(signer, auth.origin_query))
    }
}

/// The `auth` table at `source.origin.auth` of `node`, a serialized
/// configuration or one of its asset routes.
fn auth_table_of(node: &mut Value) -> Option<&mut Map<String, Value>> {
    node.get_mut("source")?
        .get_mut("origin")?
        .get_mut("auth")?
        .as_object_mut()
}

/// Calls `visit` with every `auth` table a serialized configuration holds
/// and the path a refusal names: the publisher's source's, then each asset
/// route's.
fn each_auth_table(data: &mut Value, mut visit: impl FnMut(&str, &mut Map<String, Value>)) {
    if let Some(table) = auth_table_of(data) {
        visit("source.origin.auth", table);
    }
    let Some(routes) = data
        .get_mut("proxy")
        .and_then(|proxy| proxy.get_mut("asset_routes"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for (index, route) in routes.iter_mut().enumerate() {
        if let Some(table) = auth_table_of(route) {
            visit(
                &format!("proxy.asset_routes[{index}].source.origin.auth"),
                table,
            );
        }
    }
}

/// The signer `table` names, when `signers` offers it.
fn signer_of(
    table: &Map<String, Value>,
    signers: &OriginSigners,
) -> Option<&'static OriginSignerModule> {
    table
        .get("type")
        .and_then(Value::as_str)
        .and_then(|name| signers.get(name.trim()))
}

/// Writes the default key name of every secret setting an `auth` table
/// leaves out, in a serialized configuration whose secrets have not been
/// looked up yet, so the lookup finds the setting as a written one.
pub(crate) fn fill_secret_defaults(data: &mut Value, signers: &OriginSigners) {
    each_auth_table(data, |_, table| {
        let Some(module) = signer_of(table, signers) else {
            return;
        };
        for secret in module.secrets {
            if let Some(default) = secret.default
                && !table.contains_key(secret.name)
            {
                table.insert(secret.name.to_owned(), Value::String(default.to_owned()));
            }
        }
    });
}

/// The secret leaves the offered signers declare, at both places an `auth`
/// table can be written. Every level is optional, so a source that signs
/// with nothing, or leaves a setting out, is passed over.
pub(crate) fn secret_fields(signers: &OriginSigners) -> Vec<SecretField> {
    let optional = |name: &'static str| SecretPathSegment::OptionalField(Cow::Borrowed(name));
    let mut fields = Vec::new();
    for module in &signers.offered {
        for secret in module.secrets {
            let leaf = SecretPathSegment::Field(Cow::Borrowed(secret.name));
            fields.push(SecretField {
                kind: SecretKind::KeyInDefault,
                optional: true,
                path: vec![
                    optional("source"),
                    optional("origin"),
                    optional("auth"),
                    leaf.clone(),
                ],
            });
            fields.push(SecretField {
                kind: SecretKind::KeyInDefault,
                optional: true,
                path: vec![
                    optional("proxy"),
                    optional("asset_routes"),
                    SecretPathSegment::ArrayEach,
                    optional("source"),
                    optional("origin"),
                    optional("auth"),
                    leaf,
                ],
            });
        }
    }
    fields
}

/// Refuses an `auth` table whose written secret setting names no key, for
/// a check that runs while the settings still hold key names.
///
/// # Errors
///
/// Naming the setting's path.
pub(crate) fn check_key_references(
    settings: &Settings,
    signers: &OriginSigners,
) -> Result<(), Report<TrustedServerError>> {
    let mut data = serde_json::to_value(settings).map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!("the settings could not be serialized: {error}"),
        })
    })?;
    let mut blank = None;
    each_auth_table(&mut data, |path, table| {
        if blank.is_some() {
            return;
        }
        let Some(module) = signer_of(table, signers) else {
            return;
        };
        for secret in module.secrets {
            if let Some(value) = table.get(secret.name)
                && value.as_str().is_none_or(|key| key.trim().is_empty())
            {
                blank = Some(format!("{path}.{}", secret.name));
                return;
            }
        }
    });
    match blank {
        Some(path) => Err(Report::new(TrustedServerError::Configuration {
            message: format!("secret key reference at `{path}` must not be empty"),
        })),
        None => Ok(()),
    }
}

/// Builds every `auth` table that names a signer `signers` offers, for a
/// check that runs while the settings still hold key names, so a table the
/// signer cannot use fails deploy validation and not the deployment's first
/// start. A table naming a signer nothing offers is left to the build that
/// offers it.
///
/// # Errors
///
/// The signer's refusal, naming the table as the settings load does.
pub(crate) fn check_tables(
    settings: &Settings,
    signers: &OriginSigners,
) -> Result<(), Report<TrustedServerError>> {
    let publisher = settings
        .origin_source()
        .and_then(|origin| origin.auth)
        .map(|auth| (auth, "source.origin.auth".to_owned()));
    let routes = settings.proxy.asset_routes.iter().filter_map(|route| {
        let auth = route.origin_source()?.auth?;
        Some((auth, format!("{}.origin.auth", route.source_setting())))
    });
    for (auth, setting) in publisher.into_iter().chain(routes) {
        if signers.get(&auth.signer).is_some() {
            signers.build(&auth, &setting)?;
        }
    }
    Ok(())
}

/// A signer for core's tests, which cannot link the crates that supply the
/// real ones.
///
/// Its table is `realm`, which it refuses empty, `key_name`, a secret with
/// a default key name, and `token_name`, a secret with none. It writes
/// `Authorization: Fixture <key> <method> <url> <host>`, so a test can
/// check exactly what was signed, and `x-fixture-signed`, and its namespace
/// is every `x-fixture-*` header.
#[cfg(test)]
pub(crate) mod fixture {
    use std::sync::Arc;
    use std::time::SystemTime;

    use error_stack::Report;
    use http::{HeaderMap, HeaderValue, Method, header};
    use serde::Deserialize;
    use serde_json::{Map, Value};
    use url::Url;

    use super::{OriginAuth, OriginSigner, OriginSignerModule, OriginSigners, SignerSecret};
    use crate::error::TrustedServerError;
    use crate::integrations::IntegrationBuilder;

    /// The name an `auth` table writes to select the fixture.
    pub(crate) const SIGNER: &str = "fixture_scheme";

    /// The key name `key_name` takes when left out.
    pub(crate) const DEFAULT_KEY_NAME: &str = "fixture_key";

    /// The id of the builder that offers the fixture.
    pub(crate) const INTEGRATION_ID: &str = "fixture-signer";

    pub(crate) const SECRETS: &[SignerSecret] = &[
        SignerSecret {
            name: "key_name",
            default: Some(DEFAULT_KEY_NAME),
        },
        SignerSecret {
            name: "token_name",
            default: None,
        },
    ];

    pub(crate) static MODULE: OriginSignerModule = OriginSignerModule {
        name: SIGNER,
        secrets: SECRETS,
        build,
    };

    /// A builder offering the fixture, for a registry or a settings load.
    pub(crate) fn builder() -> IntegrationBuilder {
        IntegrationBuilder::implementations(INTEGRATION_ID, "trusted-server-core")
            .with_origin_signer(&MODULE)
    }

    /// The fixture, offered on its own.
    pub(crate) fn signers() -> OriginSigners {
        OriginSigners::new(vec![&MODULE])
    }

    /// An `auth` table selecting the fixture in `realm`, with the default
    /// key name.
    pub(crate) fn auth(realm: &str) -> OriginAuth {
        OriginAuth::new(SIGNER).with_setting("realm", realm)
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FixtureSettings {
        realm: String,
        #[serde(default = "default_key_name")]
        key_name: String,
        #[serde(default)]
        token_name: Option<String>,
    }

    fn default_key_name() -> String {
        DEFAULT_KEY_NAME.to_owned()
    }

    #[derive(Debug)]
    pub(crate) struct FixtureSigner {
        realm: String,
        key: String,
        token: Option<String>,
    }

    fn build(
        settings: &Map<String, Value>,
        setting: &str,
    ) -> Result<Arc<dyn OriginSigner>, Report<TrustedServerError>> {
        let read: FixtureSettings = serde_json::from_value(Value::Object(settings.clone()))
            .map_err(|error| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("[{setting}] could not be read by {SIGNER}: {error}"),
                })
            })?;
        if read.realm.trim().is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("[{setting}] {SIGNER} realm must not be empty"),
            }));
        }
        Ok(Arc::new(FixtureSigner {
            realm: read.realm,
            key: read.key_name,
            token: read.token_name,
        }))
    }

    impl OriginSigner for FixtureSigner {
        fn sign(
            &self,
            method: &Method,
            url: &Url,
            headers: &mut HeaderMap,
            _now: SystemTime,
        ) -> Result<(), Report<TrustedServerError>> {
            let host = headers
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    Report::new(TrustedServerError::Proxy {
                        message: "missing Host header to sign".to_owned(),
                    })
                })?
                .to_owned();
            let value = expected_authorization(&self.key, method, url.as_str(), &host);
            headers.insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(&value).map_err(|error| {
                    Report::new(TrustedServerError::InvalidHeaderValue {
                        message: format!("fixture signature: {error}"),
                    })
                })?,
            );
            headers.insert("x-fixture-signed", HeaderValue::from_static("1"));
            headers.insert(
                "x-fixture-realm",
                HeaderValue::from_str(&self.realm).map_err(|error| {
                    Report::new(TrustedServerError::InvalidHeaderValue {
                        message: format!("fixture realm: {error}"),
                    })
                })?,
            );
            if let Some(token) = &self.token {
                headers.insert(
                    "x-fixture-token",
                    HeaderValue::from_str(token).map_err(|error| {
                        Report::new(TrustedServerError::InvalidHeaderValue {
                            message: format!("fixture token: {error}"),
                        })
                    })?,
                );
            }
            Ok(())
        }

        fn remove_namespace_headers(&self, headers: &mut HeaderMap) {
            let names: Vec<_> = headers
                .keys()
                .filter(|name| name.as_str().starts_with("x-fixture-"))
                .cloned()
                .collect();
            for name in names {
                headers.remove(&name);
            }
        }
    }

    /// The `Authorization` the fixture writes with `key` for `method` on
    /// `url` sent with `host`.
    pub(crate) fn expected_authorization(
        key: &str,
        method: &Method,
        url: &str,
        host: &str,
    ) -> String {
        format!("Fixture {key} {method} {url} {host}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::integrations::IntegrationBuilder;

    fn table(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(table) => table,
            _ => unreachable!("the tests write objects"),
        }
    }

    #[test]
    fn an_auth_table_reads_its_type_and_query_policy_and_keeps_the_rest() {
        let auth: OriginAuth = serde_json::from_value(json!({
            "type": "fixture_scheme",
            "origin_query": "strip",
            "realm": "site",
            "key_name": "origin_key",
        }))
        .expect("should read the table");

        assert_eq!(auth.signer, "fixture_scheme");
        assert_eq!(auth.origin_query, Some(OriginQueryPolicy::Strip));
        assert_eq!(
            auth.settings,
            table(json!({ "realm": "site", "key_name": "origin_key" })),
            "the signer's settings should be kept apart from core's keys"
        );
        assert_eq!(
            serde_json::to_value(&auth).expect("should serialize"),
            json!({
                "type": "fixture_scheme",
                "origin_query": "strip",
                "realm": "site",
                "key_name": "origin_key",
            }),
            "the table should serialize as written"
        );
    }

    #[test]
    fn an_auth_table_debug_shows_setting_names_and_no_values() {
        let auth = fixture::auth("site").with_setting("key_name", "canary-key-value");

        let debug = format!("{auth:?}");

        assert!(debug.contains("fixture_scheme") && debug.contains("key_name"));
        assert!(
            !debug.contains("canary-key-value"),
            "a setting's value may be a secret: {debug}"
        );
    }

    #[test]
    fn an_auth_table_must_name_a_signer_in_a_form_a_crate_could_offer() {
        for (signer, expected) in [
            ("", "names no signer"),
            ("Not A Name", "is not a signer's name"),
        ] {
            let err = OriginAuth::new(signer)
                .check("source.origin.auth")
                .expect_err("should refuse");
            assert!(
                format!("{err:?}").contains(expected),
                "`{signer}` should be refused saying `{expected}`: {err:?}"
            );
        }
        OriginAuth::new("fixture_scheme")
            .check("source.origin.auth")
            .expect("a valid name passes");
    }

    #[test]
    fn a_type_nothing_offers_is_refused_with_the_names_on_offer() {
        let err = fixture::signers()
            .build(&OriginAuth::new("other_scheme"), "source.origin.auth")
            .expect_err("should refuse a type nothing offers");
        let message = format!("{err:?}");
        assert!(
            message.contains("[source.origin.auth] type `other_scheme` names an origin signer this deployment does not offer")
                && message.contains("[fixture_scheme]"),
            "should name the setting and the signers on offer: {message}"
        );

        let err = OriginSigners::default()
            .build(&OriginAuth::new("other_scheme"), "source.origin.auth")
            .expect_err("should refuse with nothing on offer");
        assert!(
            format!("{err:?}").contains("It offers none"),
            "should say nothing is on offer: {err:?}"
        );
    }

    #[test]
    fn a_table_the_signer_cannot_use_is_refused_naming_the_setting() {
        for (auth, expected) in [
            (
                fixture::auth(""),
                "[source.origin.auth] fixture_scheme realm must not be empty",
            ),
            (
                fixture::auth("site").with_setting("bucket", "x"),
                "unknown field `bucket`",
            ),
            (OriginAuth::new(fixture::SIGNER), "missing field `realm`"),
        ] {
            let err = fixture::signers()
                .build(&auth, "source.origin.auth")
                .expect_err("should refuse the table");
            assert!(
                format!("{err:?}").contains(expected),
                "should say `{expected}`: {err:?}"
            );
        }
    }

    #[test]
    fn a_built_signing_signs_the_final_address_and_host() {
        let signing = fixture::signers()
            .build(
                &fixture::auth("site").with_origin_query(Some(OriginQueryPolicy::Strip)),
                "source.origin.auth",
            )
            .expect("should build");
        let url = Url::parse("https://storage.example.com/bucket/site/page").expect("url");
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::HOST,
            http::HeaderValue::from_static("storage.example.com"),
        );
        headers.insert("x-fixture-reader", http::HeaderValue::from_static("chosen"));

        signing.remove_namespace_headers(&mut headers);
        signing
            .sign(&Method::GET, &url, &mut headers)
            .expect("should sign");

        assert_eq!(
            signing.origin_query_policy(),
            Some(OriginQueryPolicy::Strip)
        );
        assert_eq!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some(
                fixture::expected_authorization(
                    fixture::DEFAULT_KEY_NAME,
                    &Method::GET,
                    url.as_str(),
                    "storage.example.com",
                )
                .as_str()
            )
        );
        assert!(
            headers.get("x-fixture-reader").is_none(),
            "a reader's header in the namespace should be removed"
        );
    }

    #[test]
    fn two_builders_offering_one_signer_are_refused() {
        let err = OriginSigners::offered_by(&[
            fixture::builder(),
            IntegrationBuilder::implementations("second", "elsewhere")
                .with_origin_signer(&fixture::MODULE),
        ])
        .expect_err("should refuse the second offer");
        assert!(
            format!("{err:?}").contains(
                "`second` and `fixture-signer` both offer the origin signer `fixture_scheme`"
            ),
            "{err:?}"
        );

        let offered = OriginSigners::offered_by(&[fixture::builder()]).expect("one offer");
        assert_eq!(offered.names(), vec![fixture::SIGNER]);
    }

    #[test]
    fn secret_defaults_are_written_into_each_auth_table_naming_the_signer() {
        let mut data = json!({
            "source": { "module": "origin", "origin": {
                "url": "https://storage.example.com",
                "auth": { "type": "fixture_scheme", "realm": "site" },
            }},
            "proxy": { "asset_routes": [
                { "prefix": "/a/", "source": { "module": "origin", "origin": {
                    "url": "https://a.example.com",
                    "auth": { "type": "fixture_scheme", "realm": "a", "key_name": "written" },
                }}},
                { "prefix": "/b/", "source": { "module": "origin", "origin": {
                    "url": "https://b.example.com",
                    "auth": { "type": "other_scheme", "realm": "b" },
                }}},
                { "prefix": "/c/", "source": { "module": "origin", "origin": {
                    "url": "https://c.example.com",
                }}},
            ]},
        });

        fill_secret_defaults(&mut data, &fixture::signers());

        assert_eq!(
            data["source"]["origin"]["auth"]["key_name"],
            json!(fixture::DEFAULT_KEY_NAME),
            "a left out setting with a default should take it"
        );
        assert!(
            data["source"]["origin"]["auth"].get("token_name").is_none(),
            "a setting with no default should stay left out"
        );
        assert_eq!(
            data["proxy"]["asset_routes"][0]["source"]["origin"]["auth"]["key_name"],
            json!("written"),
            "a written setting should be kept"
        );
        assert!(
            data["proxy"]["asset_routes"][1]["source"]["origin"]["auth"]
                .get("key_name")
                .is_none(),
            "a table naming another signer should be left alone"
        );
    }

    #[test]
    fn secret_fields_are_declared_at_both_auth_paths() {
        let declared: Vec<String> = secret_fields(&fixture::signers())
            .iter()
            .map(SecretField::dotted_path)
            .collect();

        assert_eq!(
            declared,
            vec![
                "source.origin.auth.key_name",
                "proxy.asset_routes[*].source.origin.auth.key_name",
                "source.origin.auth.token_name",
                "proxy.asset_routes[*].source.origin.auth.token_name",
            ]
        );
    }

    #[test]
    fn a_written_secret_setting_naming_no_key_is_refused_naming_its_path() {
        use crate::test_support::tests::{create_test_settings, edit_origin, set_route_auth};

        let mut settings = create_test_settings();
        edit_origin(&mut settings, |origin| {
            origin.url = "https://storage.example.com/site".to_owned();
            origin.auth = Some(fixture::auth("site").with_setting("key_name", " "));
        });
        let err = check_key_references(&settings, &fixture::signers())
            .expect_err("should refuse a blank key name");
        assert!(
            format!("{err:?}").contains(
                "secret key reference at `source.origin.auth.key_name` must not be empty"
            ),
            "{err:?}"
        );

        let mut settings = create_test_settings();
        let mut route =
            crate::settings::ProxyAssetRoute::new("/assets/", "https://assets.example.com");
        set_route_auth(
            &mut route,
            Some(fixture::auth("assets").with_setting("token_name", "")),
        );
        settings.proxy.asset_routes.push(route);
        let err = check_key_references(&settings, &fixture::signers())
            .expect_err("should refuse a blank token name");
        assert!(
            format!("{err:?}").contains(
                "secret key reference at `proxy.asset_routes[0].source.origin.auth.token_name` must not be empty"
            ),
            "{err:?}"
        );

        let mut settings = create_test_settings();
        edit_origin(&mut settings, |origin| {
            origin.url = "https://storage.example.com/site".to_owned();
            origin.auth = Some(fixture::auth("site"));
        });
        check_key_references(&settings, &fixture::signers())
            .expect("a left out setting names its default key");
    }
}
