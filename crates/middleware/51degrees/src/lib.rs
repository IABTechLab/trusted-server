//! The 51Degrees client hints middleware, `51degrees.client_hints`.
//!
//! A browser sends its high entropy client hints only when the site asks for
//! them, with `Accept-CH` on the response and `Critical-CH` when the first
//! navigation should be retried carrying them, and they reach a third party
//! only when the top level page delegates them, by a `Delegate-CH` meta tag
//! or a `Permissions-Policy` header. A `Delegate-CH` tag added by JavaScript
//! is ignored, so the tag has to be in the HTML as served.
//!
//! No hint name is written down here. [`ClientHints`] asks the service,
//! through the shared crate and its cache, which hints it wants from the
//! request's browser as it filters a request for a page a `[[serve]]` entry
//! names it for, leaves the names in the request's state, and writes them on
//! that page, the first request included: the `Delegate-CH` tag in the head,
//! and `Accept-CH`, `Critical-CH` and `Permissions-Policy` on the response,
//! each keeping every line the response already carries under its name. The
//! tag's content follows 51Degrees' published example, lower case hint
//! names, a space and the origin, separated by semicolons.
//!
//! `Critical-CH` is written when `critical` in
//! `[middleware.51degrees.client_hints]` is on, which it is unless the table
//! turns it off.
//!
//! A browser keeps an origin's `Accept-CH` set until a response names
//! another, so `Accept-CH` and `Critical-CH` are left off the response to a
//! request that already carries every hint the service names, unless the
//! response names a set of its own, which would replace the one held. The tag
//! and `Permissions-Policy` apply to their own page only, and are written on
//! every page.

use std::sync::Arc;

use async_trait::async_trait;
use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::{BorrowedRequestInfo, RequestInfo};
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationDocumentState, IntegrationRegistration,
    IntegrationRequestFilter, IntegrationRequestState, ModuleSecretSetting, RequestFilterDecision,
    RequestFilterEffects, RequestFilterInput, SuppliedMiddleware,
};
use trusted_server_core::middleware::{
    HTML_MEDIA_TYPE, Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase,
    ResponseHeader, ResponseView,
};
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::redacted::Redacted;
use trusted_server_core::settings::Settings;
use trusted_server_shared_51degrees::client::CloudPipeline;
use trusted_server_shared_51degrees::config::{self, SharedConfig};
use trusted_server_shared_51degrees::engine_cache::{AnswerCache, CACHE};
use trusted_server_shared_51degrees::hints_engine::{
    ACCEPT_CH_VALUES, ClientHintsCall, ServiceHints, hint_names,
};
use trusted_server_shared_51degrees::license::license_key;
use validator::Validate;

/// The id the 51Degrees registration carries, under which the filter leaves
/// the hint names in the request's state.
pub use trusted_server_shared_51degrees::BUILDER_ID;

/// The name a `[[serve]]` entry selects the client hints middleware by, and
/// the name of its own table under `[middleware]`, from this crate's folder
/// with the entry after it.
pub const MODULE: &str = "51degrees.client_hints";

/// The properties the client hints middleware reads from the one answer the
/// deployment's pipeline holds for a request.
pub use trusted_server_shared_51degrees::hints_engine::ACCEPT_CH_VALUES as PROPERTIES;

/// The license key, named in `[shared.51degrees]` by its entry in the secret
/// store, which is in use whenever the table is present.
const SECRET_SETTINGS: &[ModuleSecretSetting] = &[ModuleSecretSetting {
    path: &["license_key"],
    in_use: always_in_use,
}];

fn always_in_use(_table: &serde_json::Map<String, serde_json::Value>) -> bool {
    true
}

/// The cache the service's answers are held in, which a deployment selects
/// and sizes in `[cache.51degrees-answers]`.
const CACHE_FEATURES: &[&str] = &[CACHE];

/// The page change the module supplies, as a `[[serve]]` entry names it.
const SUPPLIED_MIDDLEWARE: &[SuppliedMiddleware] =
    &[SuppliedMiddleware::on_html(MODULE, MiddlewarePhase::Serve)];

/// The builder a deployment installs to offer this middleware, which
/// `[shared.51degrees]` runs.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(BUILDER_ID, env!("CARGO_PKG_NAME"), register, validate)
        .with_shared_settings(config::vendor())
        .with_secret_settings(SECRET_SETTINGS)
        .with_cache_features(CACHE_FEATURES)
        .with_middleware(SUPPLIED_MIDDLEWARE)
}

/// Registers the client hints middleware and its request filter when
/// `[shared.51degrees]` is present.
///
/// # Errors
///
/// When the table cannot be read, has no license key, or names an endpoint
/// the cloud calls cannot be built on, or when the middleware's own table
/// cannot be read.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(shared) = config::read(settings)? else {
        return Ok(None);
    };
    // Fatal when absent, whatever the modules are used for. See `license_key`.
    let license_key = license_key(settings)?;
    let pipeline = pipeline(&shared, &license_key, settings)?;
    let middleware = Arc::new(ClientHints::new(
        &shared.endpoint,
        ClientHintsCall::on(pipeline),
        critical_from_settings(settings)?,
    ));
    Ok(Some(
        IntegrationRegistration::builder(BUILDER_ID)
            // The headers and the tag are the whole client side.
            .without_js()
            .with_request_filter(Arc::clone(&middleware) as Arc<dyn IntegrationRequestFilter>)
            .with_middleware(middleware)
            .build(),
    ))
}

/// Validates the configuration for deployment and reports whether
/// `[shared.51degrees]` runs this middleware.
///
/// # Errors
///
/// The same refusals as [`register`], so a deployment with no license key is
/// stopped by the configuration check rather than only when the state is
/// built.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    let Some(shared) = config::read(settings)? else {
        return Ok(false);
    };
    let license_key = license_key(settings)?;
    pipeline(&shared, &license_key, settings)?;
    critical_from_settings(settings)?;
    Ok(true)
}

/// The cloud pipeline the 51Degrees modules share, asking for the properties
/// this middleware reads, with its answers held in the cache
/// `[cache.51degrees-answers]` selects.
fn pipeline(
    shared: &SharedConfig,
    license_key: &Redacted<String>,
    settings: &Settings,
) -> Result<Arc<CloudPipeline>, Report<TrustedServerError>> {
    CloudPipeline::new(
        &shared.endpoint,
        license_key.expose(),
        shared.timeout_ms,
        AnswerCache::for_settings(settings),
    )
    .map(|pipeline| Arc::new(pipeline.wanting(ACCEPT_CH_VALUES)))
    .ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "[shared.{}] endpoint `{}` is not an address the cloud calls can be built on",
                config::vendor(),
                shared.endpoint
            ),
        })
    })
}

/// `[middleware.51degrees.client_hints]`, the middleware's own options.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
struct ClientHintsSettings {
    /// Whether `Critical-CH` is written with `Accept-CH`.
    #[serde(default = "default_true")]
    critical: bool,
}

const fn default_true() -> bool {
    true
}

/// Whether `Critical-CH` is written, from `[middleware.51degrees.client_hints]`,
/// which is on unless the table says `critical = false`.
///
/// # Errors
///
/// When the table carries a key it does not know or a value that is not
/// `true` or `false`.
pub fn critical_from_settings(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(settings
        .middleware_config::<ClientHintsSettings>(MODULE)?
        .is_none_or(|options| options.critical))
}

/// The client hints delegation, written on each page a `[[serve]]` entry
/// names it for.
///
/// Serve only, because the list is the service's answer for one reader's
/// browser, and what the fetch phase leaves is stored for every reader.
#[derive(Debug)]
pub struct ClientHints {
    /// The origin the hints are delegated to, for example
    /// `https://cloud.example.com`. Derived from the configured endpoint, so
    /// a deployment using its own private cloud delegates to that instead.
    endpoint_origin: Option<String>,
    /// Asks which hints the service wants from the reader's browser, sending
    /// its `User-Agent` and `Sec-CH-UA` hints alone, through the cache, which
    /// holds one answer for each set of those values and so answers every
    /// reader on the same browser after the first.
    hints: ClientHintsCall,
    /// Whether `Critical-CH` is written with `Accept-CH`, so the browser
    /// retries the first navigation carrying its hints. On by default,
    /// because without it the first page view of a session is priced on a
    /// User-Agent alone, at the cost of one extra origin fetch at the start
    /// of a session. The retry cannot loop, because a browser retries only
    /// when the named hints were absent and the retried request carries them.
    critical: bool,
}

impl ClientHints {
    /// Creates the middleware for a configured endpoint and the shared
    /// crate's client hints question. An endpoint that is not a URL, or has
    /// no host, yields a middleware that writes nothing. Startup validation
    /// refuses such an endpoint, so this is only a fallback.
    #[must_use]
    pub fn new(endpoint: &str, hints: ClientHintsCall, critical: bool) -> Self {
        Self {
            endpoint_origin: endpoint_origin(endpoint),
            hints,
            critical,
        }
    }

    /// The names the service asks for, for the request's browser, or nothing
    /// when the service names nothing or did not answer. The call logs why.
    async fn names(
        &self,
        request_info: &dyn RequestInfo,
        services: &RuntimeServices,
    ) -> Vec<String> {
        match self.hints.accept_ch(request_info, services).await {
            Some(accept_ch) => hint_names(&accept_ch),
            None => Vec::new(),
        }
    }
}

/// Left beside the names when the request already carried every one of them,
/// so the browser holds them for the origin.
#[derive(Debug)]
struct HintsHeld;

/// The names the filter left for this request, which core copies into the
/// document state from the request's state on serve, or none for a document
/// with no request behind it.
fn names_in(document_state: &IntegrationDocumentState) -> Vec<String> {
    document_state
        .get::<ServiceHints>(BUILDER_ID)
        .map_or_else(Vec::new, |hints| hints.names().to_vec())
}

impl Middleware for ClientHints {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == HTML_MEDIA_TYPE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    /// Writes the delegation tag in the head, for the names the filter left
    /// for this request. A document with no names, or a fetch-phase
    /// document, gets nothing.
    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let head_inserts = delegation_for(
            self.endpoint_origin.as_deref(),
            context.request_host,
            &names_in(context.document_state),
        )
        .into_iter()
        .collect();
        MiddlewareAction {
            head_inserts,
            ..MiddlewareAction::pass()
        }
    }

    /// Writes `Accept-CH`, `Critical-CH` and `Permissions-Policy` for the
    /// names the filter left for this request, each keeping what the
    /// response already carries, with the first two left off where the
    /// browser holds the names already. The headers ask the browser to send
    /// the hints with its later requests as well as with the ones the tag
    /// covers.
    fn response_headers(
        &self,
        context: &MiddlewareContext<'_>,
        response: &ResponseView<'_>,
    ) -> Vec<ResponseHeader> {
        client_hint_headers(
            &names_in(context.document_state),
            self.critical,
            context
                .document_state
                .get::<HintsHeld>(BUILDER_ID)
                .is_some(),
            delegated_origin(self.endpoint_origin.as_deref(), context.request_host),
            response.headers,
        )
    }
}

/// Whether `request` is a navigation to a page, which is what the delegation
/// is for. An asset or an API call gets no call to the service.
fn asks_for_a_document(request: &http::Request<edgezero_core::body::Body>) -> bool {
    if request.method() != http::Method::GET && request.method() != http::Method::HEAD {
        return false;
    }
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    };
    header("sec-fetch-dest").eq_ignore_ascii_case("document")
        || header("accept").to_ascii_lowercase().contains("text/html")
}

/// Whether a `[[serve]]` entry whose path covers `path` names this
/// middleware, by the rule the headers step uses, so the service is asked
/// only for a page whose response the middleware writes on.
fn placed_for(settings: &Settings, path: &str) -> bool {
    settings
        .phase_entries(MiddlewarePhase::Serve)
        .for_path(path)
        .contains(&MODULE)
}

#[async_trait(?Send)]
impl IntegrationRequestFilter for ClientHints {
    fn integration_id(&self) -> &'static str {
        BUILDER_ID
    }

    /// Asks the service, through the cache, which hints it wants from this
    /// browser, and leaves their names in the request's state for the
    /// document. A request that is not a page navigation, one whose path no
    /// `[[serve]]` entry naming this middleware covers, and a service that
    /// names nothing or does not answer, leave nothing, and the page gets no
    /// tag and no header.
    async fn filter_request(
        &self,
        input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
        if asks_for_a_document(input.request)
            && placed_for(input.settings, input.request.uri().path())
        {
            let names = {
                // No address, because the hints depend on the browser alone.
                let evidence = BorrowedRequestInfo::new("", Some(input.request.headers()));
                self.names(&evidence, input.services).await
            };
            if !names.is_empty() {
                if carries_every(input.request.headers(), &names) {
                    IntegrationRequestState::insert(input.request, BUILDER_ID, HintsHeld);
                }
                IntegrationRequestState::insert(
                    input.request,
                    BUILDER_ID,
                    ServiceHints::new(names),
                );
            }
        }
        Ok(RequestFilterDecision::Continue(
            RequestFilterEffects::default(),
        ))
    }
}

/// The names among `hints` that are tokens, being lower case letters, digits
/// and hyphens. A name that is not one never reaches a header or the tag,
/// because the tag's content sits inside a double quoted attribute.
fn tokens(hints: &[String]) -> Vec<&str> {
    hints
        .iter()
        .map(String::as_str)
        .filter(|hint| {
            !hint.is_empty()
                && hint
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
        .collect()
}

/// Whether `headers` carry every one of `names`. A browser sends a high
/// entropy hint only once a response from the origin has named it in
/// `Accept-CH`, and the low entropy ones always, so only the whole list shows
/// the set is held. A header counts whatever its value, because
/// `Sec-CH-UA-Model` is an empty string on a desktop.
fn carries_every(headers: &http::HeaderMap, names: &[String]) -> bool {
    !names.is_empty() && names.iter().all(|name| headers.contains_key(name.as_str()))
}

/// `Accept-CH` naming `hints`, with `Critical-CH` naming the same when
/// `critical` is on, and `Permissions-Policy` delegating each to `origin`
/// when there is one, or nothing for no hints.
///
/// Each keeps every line `existing` carries under that name, joined into one
/// list, with the hints added after it, because core replaces all of those
/// lines with the one header written here. So a policy the publisher's origin
/// sets is kept and the hints it already asks for are still asked for.
/// `Accept-CH` and `Critical-CH` name the same hints, because a browser
/// retries only for the hints it was told are critical. Both are left off
/// when `held`, unless `existing` names an `Accept-CH` set of its own, which
/// would replace the one the browser holds.
fn client_hint_headers(
    hints: &[String],
    critical: bool,
    held: bool,
    origin: Option<&str>,
    existing: &http::HeaderMap,
) -> Vec<ResponseHeader> {
    let hints = tokens(hints);
    if hints.is_empty() {
        return Vec::new();
    }
    let said = |name: &str| {
        let lines: Vec<&str> = existing
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        (!lines.is_empty()).then(|| lines.join(", "))
    };
    let mut headers = Vec::new();
    let mut write = |name: &'static str, value: String| {
        if let Ok(value) = http::HeaderValue::from_str(&value) {
            headers.push((http::HeaderName::from_static(name), value));
        }
    };
    let accept_ch = said("accept-ch");
    if !held || accept_ch.is_some() {
        write("accept-ch", with_hints(accept_ch.as_deref(), &hints));
        if critical {
            write(
                "critical-ch",
                with_hints(said("critical-ch").as_deref(), &hints),
            );
        }
    }
    if let Some(origin) = origin {
        write(
            "permissions-policy",
            with_delegation(said("permissions-policy").as_deref(), origin, &hints),
        );
    }
    headers
}

/// The comma separated list `existing` holds with each of `hints` it does
/// not already name added after it.
fn with_hints(existing: Option<&str>, hints: &[&str]) -> String {
    let mut names: Vec<&str> = existing
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
        })
        .into_iter()
        .flatten()
        .collect();
    for hint in hints {
        if !names.iter().any(|name| name.eq_ignore_ascii_case(hint)) {
            names.push(*hint);
        }
    }
    names.join(", ")
}

/// The policy `existing` holds with a delegation of each of `hints` to
/// `origin` added after it, as `ch-ua-model=(self "https://cloud.example.com")`,
/// which keeps the hint for the page's own origin as well. In a policy the
/// later member of a name wins, so the delegation stands where the
/// publisher's own policy named the hint.
fn with_delegation(existing: Option<&str>, origin: &str, hints: &[&str]) -> String {
    let mut members: Vec<String> = existing
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|member| !member.is_empty())
        })
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect();
    for hint in hints {
        members.push(format!(r#"{}=(self "{origin}")"#, policy_feature(hint)));
    }
    members.join(", ")
}

/// The `Permissions-Policy` feature that governs the hint `hint`, which is
/// the hint's name without its `sec-` prefix, under `ch-`: `sec-ch-ua-model`
/// is `ch-ua-model`, and a legacy hint such as `dpr` is `ch-dpr`.
fn policy_feature(hint: &str) -> String {
    let name = hint.strip_prefix("sec-").unwrap_or(hint);
    if name.starts_with("ch-") {
        name.to_owned()
    } else {
        format!("ch-{name}")
    }
}

/// The origin part of `endpoint`, with the port where there is one.
fn endpoint_origin(endpoint: &str) -> Option<String> {
    url::Url::parse(endpoint).ok().and_then(|url| {
        let host = url.host_str()?;
        let scheme = url.scheme();
        Some(url.port().map_or_else(
            || format!("{scheme}://{host}"),
            |port| format!("{scheme}://{host}:{port}"),
        ))
    })
}

/// The origin the hints are delegated to from a page served on
/// `request_host`, or nothing when there is no endpoint or when the endpoint
/// is the page's own origin, which leaves no third party to delegate to.
fn delegated_origin<'a>(origin: Option<&'a str>, request_host: &str) -> Option<&'a str> {
    let origin = origin?;
    let same_origin = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .is_some_and(|host| host.eq_ignore_ascii_case(request_host));
    (!same_origin).then_some(origin)
}

/// The tag delegating `hints` from a page served on `request_host`, or nothing
/// when there is no origin to delegate to or no hint to delegate.
///
/// No hints means the service named none for this browser or has not
/// answered, and the tag is not guessed at.
fn delegation_for(origin: Option<&str>, request_host: &str, hints: &[String]) -> Option<String> {
    let origin = delegated_origin(origin, request_host)?;
    let pairs: Vec<String> = tokens(hints)
        .iter()
        .map(|hint| format!("{hint} {origin}"))
        .collect();
    if pairs.is_empty() {
        return None;
    }
    Some(format!(
        r#"<meta http-equiv="Delegate-CH" content="{}">"#,
        pairs.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use edgezero_core::body::Body;
    use trusted_server_core::html_processor::test_support::page_settings;
    use trusted_server_core::integrations::{
        IntegrationDocumentState, IntegrationRegistry, RequestFilterInput,
    };
    use trusted_server_core::middleware::{MiddlewareContext, Prepared, test_support};
    use trusted_server_shared_51degrees::test_services::{
        MemoryKvStore, ScriptedHttpClient, services_with,
    };

    use super::*;

    const CLOUD: &str = "https://cloud.example.com/api/v4/json";
    const CLOUD_ORIGIN: &str = "https://cloud.example.com";

    /// The names the live service's `Accept-CH` gives a publisher's license
    /// key for Chrome, lower case.
    const SERVICE_NAMES: &[&str] = &[
        "sec-ch-ua",
        "sec-ch-ua-full-version-list",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
        "sec-ch-ua-model",
        "sec-ch-ua-platform-version",
    ];

    /// What the data endpoint answers for a license key naming the three
    /// `Accept-CH` properties.
    const DATA: &str = r#"{"device":{
        "setheaderbrowseraccept-ch":"Sec-CH-UA,Sec-CH-UA-Platform",
        "setheaderhardwareaccept-ch":"Sec-CH-UA-Model",
        "setheaderplatformaccept-ch":"Sec-CH-UA-Platform-Version"}}"#;

    /// A deployment's `[shared.51degrees]`, with `rest` after it.
    fn settings_with(rest: &str) -> Settings {
        page_settings(&format!(
            "[shared.51degrees]\nendpoint = \"{CLOUD}\"\nlicense_key = \"a-secret-license-key\"\n\n{rest}"
        ))
    }

    fn hints_call() -> ClientHintsCall {
        ClientHintsCall::new(
            CLOUD,
            "a-secret-license-key",
            1000,
            trusted_server_shared_51degrees::test_services::answer_cache(),
        )
        .expect("the endpoint is a URL")
    }

    fn middleware(endpoint: &str) -> ClientHints {
        ClientHints::new(endpoint, hints_call(), true)
    }

    /// The tags the middleware writes for `names`, as the service left them,
    /// on a page served on `www.example.com`.
    fn delegation(endpoint: &str, names: &[&str]) -> Vec<String> {
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        delegation_for(
            endpoint_origin(endpoint).as_deref(),
            "www.example.com",
            &names,
        )
        .into_iter()
        .collect()
    }

    /// A serve-phase context on `www.example.com` whose document state holds
    /// `names` as core seeds them from the request's state, or nothing for an
    /// empty list.
    fn serve_context<'a>(
        state: &'a IntegrationDocumentState,
        prepared: &'a Prepared,
        names: &[&str],
    ) -> MiddlewareContext<'a> {
        if !names.is_empty() {
            state.get_or_insert_with(BUILDER_ID, || {
                ServiceHints::new(names.iter().map(|name| (*name).to_owned()).collect())
            });
        }
        MiddlewareContext {
            phase: MiddlewarePhase::Serve,
            request_host: "www.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            platform: test_support::platform(),
            document_state: state,
            max_buffered_script_bytes: 1 << 20,
            prepared,
        }
    }

    /// The headers `middleware` sets on a `200` response carrying `existing`,
    /// for a document whose state holds `names`, as name and value.
    fn headers_for(
        middleware: &ClientHints,
        names: &[&str],
        existing: &[(&'static str, &'static str)],
    ) -> Vec<(String, String)> {
        headers_with(middleware, names, existing, false)
    }

    /// As [`headers_for`], for a document whose request carried every name
    /// when `held`.
    fn headers_with(
        middleware: &ClientHints,
        names: &[&str],
        existing: &[(&'static str, &'static str)],
        held: bool,
    ) -> Vec<(String, String)> {
        let state = IntegrationDocumentState::default();
        if held {
            state.get_or_insert_with(BUILDER_ID, || HintsHeld);
        }
        let prepared = Prepared::default();
        let context = serve_context(&state, &prepared, names);
        let mut headers = http::HeaderMap::new();
        for (name, value) in existing {
            headers.append(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_static(value),
            );
        }
        let view = ResponseView {
            status: http::StatusCode::OK,
            headers: &headers,
        };
        middleware
            .response_headers(&context, &view)
            .into_iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    fn pairs(headers: &[(&str, &str)]) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn module_constant_is_the_crate_folder_with_the_entry() {
        assert_eq!(
            MODULE,
            trusted_server_core::module_name::short_form(
                trusted_server_core::settings::MIDDLEWARE_TYPE,
                trusted_server_core::module_name!("client_hints")
            ),
            "should be named by the folder this crate lives in, with the entry after it"
        );
    }

    // The headers on a document.

    #[test]
    fn the_names_the_filter_left_are_written_as_the_three_headers() {
        let headers = headers_for(&middleware(CLOUD), &["sec-ch-ua", "sec-ch-ua-model"], &[]);

        assert_eq!(
            headers,
            pairs(&[
                ("accept-ch", "sec-ch-ua, sec-ch-ua-model"),
                ("critical-ch", "sec-ch-ua, sec-ch-ua-model"),
                (
                    "permissions-policy",
                    r#"ch-ua=(self "https://cloud.example.com"), ch-ua-model=(self "https://cloud.example.com")"#
                ),
            ]),
            "should ask the browser for the names the service wants, on this response too, \
             and delegate each to the service"
        );
    }

    #[test]
    fn critical_off_writes_accept_ch_without_a_retry() {
        let headers = headers_for(
            &ClientHints::new(CLOUD, hints_call(), false),
            &["sec-ch-ua"],
            &[],
        );

        assert_eq!(
            headers,
            pairs(&[
                ("accept-ch", "sec-ch-ua"),
                (
                    "permissions-policy",
                    r#"ch-ua=(self "https://cloud.example.com")"#
                ),
            ]),
            "should ask without demanding a retry when critical is off"
        );
    }

    #[test]
    fn a_document_with_no_names_gets_no_header_and_no_tag() {
        let state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = serve_context(&state, &prepared, &[]);

        assert!(
            middleware(CLOUD).create(&context).is_pass(),
            "should write nothing for a document the filter left no names for"
        );
        assert!(headers_for(&middleware(CLOUD), &[], &[]).is_empty());
    }

    /// What the response already says under each name is kept, with the
    /// hints added after it, so the origin's own policy and the hints it
    /// asks for on its own survive.
    #[test]
    fn what_the_response_already_says_is_kept() {
        let headers = headers_for(
            &middleware(CLOUD),
            &["sec-ch-ua-model"],
            &[
                ("accept-ch", "Sec-CH-Prefers-Color-Scheme, sec-ch-ua-model"),
                ("permissions-policy", "camera=(), ch-ua-model=()"),
            ],
        );

        assert_eq!(
            headers,
            pairs(&[
                ("accept-ch", "Sec-CH-Prefers-Color-Scheme, sec-ch-ua-model"),
                ("critical-ch", "sec-ch-ua-model"),
                (
                    "permissions-policy",
                    r#"camera=(), ch-ua-model=(), ch-ua-model=(self "https://cloud.example.com")"#
                ),
            ]),
            "a hint the origin already asks for is named once, and the delegation comes \
             after the origin's own member so it is the one that stands"
        );
    }

    /// A response may send each header on several lines, and core replaces
    /// them all with the one written, so every line is kept, in order.
    #[test]
    fn every_line_the_response_sends_is_kept() {
        let headers = headers_for(
            &middleware(CLOUD),
            &["sec-ch-ua-model"],
            &[
                ("accept-ch", "Sec-CH-Prefers-Color-Scheme"),
                ("accept-ch", "Sec-CH-Viewport-Width"),
                ("critical-ch", "Sec-CH-Prefers-Color-Scheme"),
                ("critical-ch", "Sec-CH-Viewport-Width"),
                ("permissions-policy", "camera=()"),
                ("permissions-policy", "geolocation=(self)"),
            ],
        );

        assert_eq!(
            headers,
            pairs(&[
                (
                    "accept-ch",
                    "Sec-CH-Prefers-Color-Scheme, Sec-CH-Viewport-Width, sec-ch-ua-model"
                ),
                (
                    "critical-ch",
                    "Sec-CH-Prefers-Color-Scheme, Sec-CH-Viewport-Width, sec-ch-ua-model"
                ),
                (
                    "permissions-policy",
                    r#"camera=(), geolocation=(self), ch-ua-model=(self "https://cloud.example.com")"#
                ),
            ])
        );
    }

    /// A policy feature is the hint's name without `sec-`, under `ch-`.
    #[test]
    fn a_hint_is_governed_by_its_policy_feature() {
        for (hint, feature) in [
            ("sec-ch-ua-model", "ch-ua-model"),
            ("sec-ch-ua-full-version-list", "ch-ua-full-version-list"),
            ("sec-ch-prefers-color-scheme", "ch-prefers-color-scheme"),
            ("dpr", "ch-dpr"),
            ("viewport-width", "ch-viewport-width"),
        ] {
            assert_eq!(policy_feature(hint), feature, "{hint}");
        }
    }

    /// An endpoint on the page's own origin leaves no third party to
    /// delegate to, so the hints are asked for and nothing is delegated.
    #[test]
    fn an_endpoint_on_the_pages_own_origin_delegates_nothing() {
        let headers = headers_for(
            &middleware("https://www.example.com/api/v4/json"),
            &["sec-ch-ua-model"],
            &[],
        );

        assert_eq!(
            headers,
            pairs(&[
                ("accept-ch", "sec-ch-ua-model"),
                ("critical-ch", "sec-ch-ua-model"),
            ])
        );
    }

    // The tag.

    #[test]
    fn the_tag_names_every_hint_the_service_asks_for() {
        assert_eq!(
            delegation(CLOUD, SERVICE_NAMES),
            vec![format!(
                r#"<meta http-equiv="Delegate-CH" content="sec-ch-ua {CLOUD_ORIGIN}; sec-ch-ua-full-version-list {CLOUD_ORIGIN}; sec-ch-ua-mobile {CLOUD_ORIGIN}; sec-ch-ua-platform {CLOUD_ORIGIN}; sec-ch-ua-model {CLOUD_ORIGIN}; sec-ch-ua-platform-version {CLOUD_ORIGIN}">"#
            )],
            "the service's list, in its order, with nothing added and nothing dropped"
        );
    }

    #[test]
    fn the_tag_matches_the_shape_the_vendor_publishes() {
        let inserts = delegation(CLOUD, &["sec-ch-ua-model", "sec-ch-ua-platform-version"]);

        assert_eq!(
            inserts,
            vec![format!(
                r#"<meta http-equiv="Delegate-CH" content="sec-ch-ua-model {CLOUD_ORIGIN}; sec-ch-ua-platform-version {CLOUD_ORIGIN}">"#
            )],
            "the shape is taken from 51Degrees' own published example, and a browser \
             that does not recognize the content ignores the whole tag silently"
        );
    }

    #[test]
    fn a_hint_nothing_here_has_heard_of_is_still_delegated() {
        // The list is the service's, so a hint it starts asking for is
        // delegated the day it does, with no change here.
        assert_eq!(
            delegation(CLOUD, &["sec-ch-ua-something-new"]),
            vec![format!(
                r#"<meta http-equiv="Delegate-CH" content="sec-ch-ua-something-new {CLOUD_ORIGIN}">"#
            )],
            "a list of what to delegate would have dropped this silently"
        );
    }

    #[test]
    fn no_answer_from_the_service_writes_no_tag() {
        // A service that named nothing for this browser, or did not answer,
        // leaves no list, and the tag is not guessed at.
        assert!(delegation(CLOUD, &[]).is_empty());
    }

    #[test]
    fn a_document_with_no_request_behind_it_writes_nothing() {
        // The fetch phase prepares nothing for one reader, and nothing the
        // middleware writes may depend on one reader there.
        let state = IntegrationDocumentState::default();
        let context = test_support::context(MiddlewarePhase::Fetch, &state);

        assert!(middleware(CLOUD).create(&context).is_pass());
    }

    #[test]
    fn a_name_that_is_not_a_token_never_reaches_the_tag_or_a_header() {
        // The content sits inside a double quoted attribute, so a name
        // carrying a quote would end the attribute early.
        let names = ["sec-ch-ua-model\" onload=x", "sec-ch-ua-arch"];
        let state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = serve_context(&state, &prepared, &names);

        assert_eq!(
            middleware(CLOUD).create(&context).head_inserts,
            vec![format!(
                r#"<meta http-equiv="Delegate-CH" content="sec-ch-ua-arch {CLOUD_ORIGIN}">"#
            )]
        );
        assert_eq!(
            headers_for(&middleware(CLOUD), &names, &[])[0],
            ("accept-ch".to_owned(), "sec-ch-ua-arch".to_owned())
        );
    }

    /// The tag delegates to the origin of the configured endpoint, so a
    /// publisher running a private cloud delegates the hints to that rather
    /// than to the public service. An endpoint on the page's own origin leaves
    /// no third party to delegate to, and one that is not a URL writes nothing
    /// either.
    #[test]
    fn the_tag_delegates_to_the_configured_endpoints_origin() {
        for (case, endpoint, origin) in [
            ("the public cloud", CLOUD, Some(CLOUD_ORIGIN)),
            (
                "a private cloud",
                "https://cloud.publisher.example/api/v4/json",
                Some("https://cloud.publisher.example"),
            ),
            (
                // An origin missing its port delegates to a different one.
                "a private cloud on a port of its own",
                "http://127.0.0.1:8080/api/v4/json",
                Some("http://127.0.0.1:8080"),
            ),
            (
                "the page's own origin",
                "https://www.example.com/api/v4/json",
                None,
            ),
            ("an endpoint that is not a URL", "not-a-url", None),
            ("an endpoint with spaces in it", "not a url", None),
        ] {
            let expected: Vec<String> = origin
                .map(|origin| {
                    format!(r#"<meta http-equiv="Delegate-CH" content="sec-ch-ua-model {origin}">"#)
                })
                .into_iter()
                .collect();
            assert_eq!(
                delegation(endpoint, &["sec-ch-ua-model"]),
                expected,
                "{case}"
            );
        }
    }

    #[test]
    fn the_delegation_runs_on_serve_alone() {
        // What the fetch phase leaves is stored for every reader, and the
        // list is the service's answer for one reader's browser.
        assert_eq!(middleware(CLOUD).phases(), &[MiddlewarePhase::Serve]);
    }

    // The filter.

    /// A first page view in Chrome, before anything has asked for a high
    /// entropy hint.
    fn first_view() -> http::Request<Body> {
        let mut request = first_view_from("Mozilla/5.0");
        for hint in ["sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform"] {
            request.headers_mut().insert(
                http::HeaderName::from_static(hint),
                http::HeaderValue::from_static("\"something\""),
            );
        }
        request
    }

    /// A first page view from `user_agent`, carrying no client hint at all,
    /// which is the request the delegation exists for.
    fn first_view_from(user_agent: &'static str) -> http::Request<Body> {
        edgezero_core::http::request_builder()
            .method("GET")
            .uri("https://www.example.com/")
            .header("host", "www.example.com")
            .header("accept", "text/html,application/xhtml+xml")
            .header("sec-fetch-dest", "document")
            .header("user-agent", user_agent)
            .body(Body::empty())
            .expect("should build the request")
    }

    /// A request for a script, which the delegation is not for.
    fn an_asset() -> http::Request<Body> {
        edgezero_core::http::request_builder()
            .method("GET")
            .uri("https://www.example.com/app.js")
            .header("host", "www.example.com")
            .header("accept", "*/*")
            .header("sec-fetch-dest", "script")
            .header("user-agent", "Mozilla/5.0")
            .body(Body::empty())
            .expect("should build the request")
    }

    /// A `[[serve]]` entry naming the middleware on every HTML page.
    const PLACED: &str =
        "[[serve]]\nmedia_type = \"text/html\"\nmiddleware = [\"51degrees.client_hints\"]\n";

    /// What the filter leaves in `request`'s state, asked as core asks it,
    /// where an entry names the middleware on every page.
    async fn left_by_the_filter(
        middleware: &ClientHints,
        services: &RuntimeServices,
        request: &mut http::Request<Body>,
    ) -> Vec<String> {
        left_in(&settings_with(PLACED), middleware, services, request).await
    }

    /// What the filter leaves in `request`'s state for a deployment with
    /// `settings`.
    async fn left_in(
        settings: &Settings,
        middleware: &ClientHints,
        services: &RuntimeServices,
        request: &mut http::Request<Body>,
    ) -> Vec<String> {
        let decision = middleware
            .filter_request(RequestFilterInput {
                settings,
                services,
                request,
                geo_info: None,
                permissions: None,
                is_integration_route: false,
            })
            .await
            .expect("the filter refuses nothing");
        assert!(matches!(decision, RequestFilterDecision::Continue(_)));
        IntegrationRequestState::of(request)
            .get::<ServiceHints>(BUILDER_ID)
            .map_or_else(Vec::new, |hints| hints.names().to_vec())
    }

    /// What the filter leaves for a first page view.
    async fn prepared(middleware: &ClientHints, services: &RuntimeServices) -> Vec<String> {
        left_by_the_filter(middleware, services, &mut first_view()).await
    }

    #[tokio::test]
    async fn an_asset_request_asks_the_service_nothing_and_leaves_nothing() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        let names = left_by_the_filter(&middleware(CLOUD), &services, &mut an_asset()).await;

        assert!(names.is_empty());
        assert!(client.sent().is_empty());
    }

    /// A first page view of `path`.
    fn first_view_of(path: &str) -> http::Request<Body> {
        let mut request = first_view();
        *request.uri_mut() = format!("https://www.example.com{path}")
            .parse()
            .expect("should build the address");
        request
    }

    /// The table alone runs the filter, and with no entry naming the
    /// middleware no page gets the headers, so no page asks the service.
    #[tokio::test]
    async fn a_table_with_no_entry_asks_the_service_nothing() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        let names = left_in(
            &settings_with(""),
            &middleware(CLOUD),
            &services,
            &mut first_view(),
        )
        .await;

        assert!(names.is_empty());
        assert!(client.sent().is_empty(), "no entry names the middleware");
    }

    /// An entry's `path` scopes the call as it scopes the headers, so a page
    /// the entry covers asks the service and one it does not asks nothing.
    #[tokio::test]
    async fn an_entrys_path_scopes_the_call_to_the_service() {
        let settings = settings_with(concat!(
            "[[serve]]\n",
            "media_type = \"text/html\"\n",
            "path = \"/news/\"\n",
            "middleware = [\"51degrees.client_hints\"]\n",
        ));
        for (path, covered) in [("/", false), ("/newsletter", false), ("/news/today", true)] {
            let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
            let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

            let names = left_in(
                &settings,
                &middleware(CLOUD),
                &services,
                &mut first_view_of(path),
            )
            .await;

            assert_eq!(client.sent().len(), usize::from(covered), "{path}");
            assert_eq!(names.is_empty(), !covered, "{path}");
        }
    }

    /// The filter hands the service the reader's browser alone, so neither
    /// the reader's address nor a cookie reaches the request sent.
    #[tokio::test]
    async fn the_filter_sends_no_address_and_no_cookie() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()))
            .with_client_info(trusted_server_core::platform::ClientInfo {
                client_ip: Some("2.125.160.216".parse().expect("should parse the address")),
                ..Default::default()
            });
        let mut request = first_view();
        request.headers_mut().insert(
            http::header::COOKIE,
            http::HeaderValue::from_static("51D_ScreenPixelsWidth=1080; ts-ec=an-id"),
        );

        let names = left_by_the_filter(&middleware(CLOUD), &services, &mut request).await;

        assert!(!names.is_empty());
        let sent = client.sent();
        assert_eq!(sent.len(), 1);
        let body = &sent[0].body;
        assert!(
            body.contains("user-agent=") && body.contains("sec-ch-ua-mobile="),
            "{body}"
        );
        for absent in [
            "client-ip",
            "2.125.160.216",
            "51D_",
            "1080",
            "ts-ec",
            "an-id",
        ] {
            assert!(
                !body.contains(absent),
                "{absent} reached the service: {body}"
            );
        }
    }

    #[tokio::test]
    async fn a_first_view_is_given_the_services_list() {
        // The case the tag exists for. The browser has been asked for nothing
        // yet, and the list comes from the service, not from the request.
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client, Arc::new(MemoryKvStore::default()));

        let names = prepared(&middleware(CLOUD), &services).await;

        assert_eq!(
            names,
            vec![
                "sec-ch-ua".to_owned(),
                "sec-ch-ua-platform".to_owned(),
                "sec-ch-ua-model".to_owned(),
                "sec-ch-ua-platform-version".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn the_held_answer_serves_the_next_request_without_a_call() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));
        let middleware = middleware(CLOUD);

        for _ in 0..2 {
            assert!(!prepared(&middleware, &services).await.is_empty());
        }

        assert_eq!(
            client.sent().len(),
            1,
            "the answer, once, and the second page reads the held answer"
        );
    }

    #[tokio::test]
    async fn a_service_naming_nothing_leaves_nothing_to_delegate() {
        // The data call answers an empty body, as a service that names no
        // hints for this browser does.
        let client = Arc::new(ScriptedHttpClient::new(&["{}"]));
        let services = services_with(client, Arc::new(MemoryKvStore::default()));

        assert!(prepared(&middleware(CLOUD), &services).await.is_empty());
    }

    /// What the filter leaves is what the page carries, through the document
    /// state core seeds from the request's state on serve.
    #[tokio::test]
    async fn what_the_filter_leaves_is_what_the_page_carries() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client, Arc::new(MemoryKvStore::default()));
        let middleware = middleware(CLOUD);
        let names = prepared(&middleware, &services).await;
        let expected: Vec<&str> = names.iter().map(String::as_str).collect();
        let state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = serve_context(&state, &prepared, &expected);

        let inserts = middleware.create(&context).head_inserts;

        assert_eq!(inserts, delegation(CLOUD, &expected));
        assert_eq!(inserts.len(), 1);
    }

    /// What the service answers for a browser engine that has no client
    /// hints: `Unknown` for all three properties.
    const DATA_NO_HINTS: &str = r#"{"device":{
        "setheaderbrowseraccept-ch":"Unknown",
        "setheaderhardwareaccept-ch":"Unknown",
        "setheaderplatformaccept-ch":"Unknown"}}"#;

    const GECKO: &str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0";
    const WEBKIT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                          (KHTML, like Gecko) Version/17.6 Safari/605.1.15";
    const CHROMIUM: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                            (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

    /// The page and the headers a first view from `user_agent` gets, when
    /// the service answers `data`: the Delegate-CH inserts and the
    /// `Accept-CH` header, each empty when the service names no hint.
    async fn delegation_for_browser(
        user_agent: &'static str,
        data: &str,
    ) -> (Vec<String>, Option<String>) {
        let client = Arc::new(ScriptedHttpClient::new(&[data]));
        let services = services_with(client, Arc::new(MemoryKvStore::default()));
        let middleware = middleware(CLOUD);
        let mut request = first_view_from(user_agent);

        let names = left_by_the_filter(&middleware, &services, &mut request).await;

        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = serve_context(&state, &prepared, &names);
        let inserts = middleware.create(&context).head_inserts;
        let accept_ch = headers_for(&middleware, &names, &[])
            .into_iter()
            .find(|(name, _)| name == "accept-ch")
            .map(|(_, value)| value);
        (inserts, accept_ch)
    }

    /// The service is asked server side on the first request, before the
    /// browser has sent a single hint, and a Chromium browser is given the
    /// delegation and the headers from that answer.
    #[tokio::test]
    async fn a_chromium_browser_is_given_the_delegation_on_its_first_request() {
        let (inserts, accept_ch) = delegation_for_browser(CHROMIUM, DATA).await;

        assert_eq!(inserts.len(), 1, "{inserts:?}");
        assert!(
            inserts[0].contains(r#"http-equiv="Delegate-CH""#),
            "{inserts:?}"
        );
        assert_eq!(
            accept_ch.as_deref(),
            Some("sec-ch-ua, sec-ch-ua-platform, sec-ch-ua-model, sec-ch-ua-platform-version")
        );
    }

    /// A browser engine with no client hints is never given the tag or the
    /// headers, because the service answers `Unknown` for it and nothing
    /// here keeps a list of its own.
    #[tokio::test]
    async fn a_gecko_browser_is_never_given_client_hint_headers() {
        let (inserts, accept_ch) = delegation_for_browser(GECKO, DATA_NO_HINTS).await;

        assert!(inserts.is_empty(), "{inserts:?}");
        assert_eq!(accept_ch, None);
    }

    #[tokio::test]
    async fn a_webkit_browser_is_never_given_client_hint_headers() {
        let (inserts, accept_ch) = delegation_for_browser(WEBKIT, DATA_NO_HINTS).await;

        assert!(inserts.is_empty(), "{inserts:?}");
        assert_eq!(accept_ch, None);
    }

    // A browser that holds the hints.

    /// The names `DATA` gives.
    const DATA_NAMES: &[&str] = &[
        "sec-ch-ua",
        "sec-ch-ua-platform",
        "sec-ch-ua-model",
        "sec-ch-ua-platform-version",
    ];

    /// A Chromium page request carrying `hints`, as a browser does once a
    /// response from the origin named them in `Accept-CH`. Each is the empty
    /// string Chromium sends for `Sec-CH-UA-Model` on a desktop.
    fn later_view(hints: &[&str]) -> http::Request<Body> {
        let mut request = first_view_from(CHROMIUM);
        for hint in hints {
            request.headers_mut().insert(
                http::HeaderName::from_bytes(hint.as_bytes()).expect("a hint is a header name"),
                http::HeaderValue::from_static("\"\""),
            );
        }
        request
    }

    /// Whether the filter marks `request` as carrying every name the service
    /// answers with `DATA`.
    async fn held_after_filter(mut request: http::Request<Body>) -> bool {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client, Arc::new(MemoryKvStore::default()));

        let names = left_by_the_filter(&middleware(CLOUD), &services, &mut request).await;

        assert!(!names.is_empty(), "the service names hints for Chromium");
        IntegrationRequestState::of(&request)
            .get::<HintsHeld>(BUILDER_ID)
            .is_some()
    }

    #[tokio::test]
    async fn a_request_carrying_every_hint_is_held() {
        assert!(held_after_filter(later_view(DATA_NAMES)).await);
    }

    #[tokio::test]
    async fn a_request_missing_one_hint_is_not_held() {
        // As when the service starts naming a hint, so the headers replace
        // the set the browser holds.
        assert!(!held_after_filter(later_view(&DATA_NAMES[..3])).await);
    }

    #[tokio::test]
    async fn the_hints_sent_unasked_do_not_count_as_held() {
        // Chromium sends these three on every request, asked or not.
        assert!(!held_after_filter(first_view()).await);
    }

    #[test]
    fn a_held_set_gets_the_policy_and_the_tag_without_accept_ch() {
        let headers = headers_with(
            &middleware(CLOUD),
            &["sec-ch-ua", "sec-ch-ua-model"],
            &[],
            true,
        );
        let state = IntegrationDocumentState::default();
        state.get_or_insert_with(BUILDER_ID, || HintsHeld);
        let prepared = Prepared::default();
        let context = serve_context(&state, &prepared, &["sec-ch-ua-model"]);

        assert_eq!(
            headers,
            pairs(&[(
                "permissions-policy",
                r#"ch-ua=(self "https://cloud.example.com"), ch-ua-model=(self "https://cloud.example.com")"#
            )]),
            "the browser holds the set, and the policy applies to this page alone"
        );
        assert_eq!(
            middleware(CLOUD).create(&context).head_inserts.len(),
            1,
            "the tag applies to this page alone"
        );
    }

    #[test]
    fn a_response_naming_its_own_set_still_gets_the_hints_added() {
        // The response's set would replace the one the browser holds.
        let headers = headers_with(
            &middleware(CLOUD),
            &["sec-ch-ua-model"],
            &[("accept-ch", "Sec-CH-Prefers-Color-Scheme")],
            true,
        );

        assert_eq!(
            headers,
            pairs(&[
                ("accept-ch", "Sec-CH-Prefers-Color-Scheme, sec-ch-ua-model"),
                ("critical-ch", "sec-ch-ua-model"),
                (
                    "permissions-policy",
                    r#"ch-ua-model=(self "https://cloud.example.com")"#
                ),
            ])
        );
    }

    #[test]
    fn a_hint_counts_as_carried_whatever_its_value() {
        let mut headers = http::HeaderMap::new();
        headers.insert("sec-ch-ua-model", http::HeaderValue::from_static("\"\""));

        assert!(carries_every(&headers, &["sec-ch-ua-model".to_owned()]));
        assert!(!carries_every(&headers, &[]), "no names is never held");
    }

    // The tables.

    #[test]
    fn critical_is_on_unless_the_table_turns_it_off() {
        assert!(critical_from_settings(&settings_with("")).expect("no table is on"));
        assert!(
            critical_from_settings(&settings_with(
                "[middleware.51degrees.client_hints]\ncritical = true\n"
            ))
            .expect("the table says on")
        );
        assert!(
            !critical_from_settings(&settings_with(
                "[middleware.51degrees.client_hints]\ncritical = false\n"
            ))
            .expect("the table says off")
        );
    }

    /// A misspelt key in the middleware's own table fails the deploy check
    /// naming the table and the key, where it would otherwise be ignored.
    #[test]
    fn the_middlewares_table_refuses_a_key_it_does_not_know() {
        let settings =
            settings_with("[middleware.51degrees.client_hints]\nno_such_setting = true\n");

        let message = format!(
            "{:?}",
            critical_from_settings(&settings)
                .expect_err("the table carries a key it does not know")
        );

        assert!(
            message.contains("[middleware.51degrees.client_hints]")
                && message.contains("no_such_setting"),
            "the message has to name the table and the key: {message}"
        );
        assert!(
            validate(&settings).is_err(),
            "and the deploy check refuses it"
        );
    }

    #[test]
    fn a_configuration_without_the_shared_table_registers_nothing() {
        let settings = page_settings("");

        assert!(
            register(&settings)
                .expect("should read a document that selects nothing")
                .is_none()
        );
        assert!(!validate(&settings).expect("should read a document that selects nothing"));
    }

    #[test]
    fn a_shared_table_with_no_license_key_is_refused() {
        let settings = page_settings(&format!("[shared.51degrees]\nendpoint = \"{CLOUD}\"\n"));

        let message = format!(
            "{:?}",
            register(&settings)
                .err()
                .expect("there is no key to present")
        );

        assert!(
            message.contains("[shared.51degrees] has no `license_key`"),
            "{message}"
        );
        assert!(validate(&settings).is_err());
    }

    #[test]
    fn the_shared_table_registers_the_filter_and_the_middleware() {
        let registration = register(&settings_with(""))
            .expect("should read the table")
            .expect("the table runs the builder");

        assert_eq!(registration.integration_id, BUILDER_ID);
        assert!(
            registration
                .middleware
                .iter()
                .any(|middleware| middleware.middleware_id() == MODULE),
            "the middleware should be offered for an entry to write"
        );
        assert_eq!(
            registration.request_filters.len(),
            1,
            "the filter that asks the service"
        );
        assert!(
            registration.js_disabled,
            "the headers and the tag are the whole client side"
        );
    }

    #[test]
    fn the_description_is_what_the_shared_table_registers() {
        trusted_server_core::test_support::middleware::assert_described_middleware_is_registered(
            &settings_with(""),
            &linked_builders(),
            BUILDER_ID,
        );
    }

    /// The builders of the two 51Degrees modules, the client hints
    /// middleware's and the loading cache's, as a deployment that wants the
    /// answers held links them.
    fn linked_builders() -> [IntegrationBuilder; 2] {
        [builder(), trusted_server_loading_cache_51degrees::builder()]
    }

    /// The middleware the serve phase's headers step asks for a page, which
    /// is none where no entry names it, so a deployment that names no
    /// `[[serve]]` entry sends no client hints headers.
    #[test]
    fn no_entry_means_no_headers() {
        let unplaced = settings_with("");
        let registry = IntegrationRegistry::with_registrations(&unplaced, &linked_builders())
            .expect("should build the registry");
        assert!(
            registry
                .middleware_in(MiddlewarePhase::Serve)
                .contains(&MODULE),
            "the table runs the middleware whether or not an entry places it"
        );
        assert!(
            registry
                .middleware_for_path(unplaced.phase_entries(MiddlewarePhase::Serve), "/")
                .is_empty(),
            "and the headers step asks nothing where no entry names it"
        );

        let placed = settings_with(
            "[[serve]]\nmedia_type = \"text/html\"\nmiddleware = [\"51degrees.client_hints\"]\n",
        );
        let registry = IntegrationRegistry::with_registrations(&placed, &linked_builders())
            .expect("should build the registry");
        let asked: Vec<&str> = registry
            .middleware_for_path(placed.phase_entries(MiddlewarePhase::Serve), "/")
            .iter()
            .map(|middleware| middleware.middleware_id())
            .collect();

        assert_eq!(asked, [MODULE]);
    }

    /// One `[shared.51degrees]` table runs the middleware, and a linked
    /// loading cache module holds the answers it asks for, in the cache this
    /// builder declares, with no line in the document or with
    /// `[cache.51degrees-answers]` naming the module.
    #[test]
    fn the_shared_table_runs_the_middleware_and_a_linked_cache_holds_its_answers() {
        for (case, rest) in [
            ("no cache table", ""),
            (
                "the cache table naming the module",
                "[cache.51degrees-answers]\nmodule = \"51degrees\"\nstore_seconds = 600\n",
            ),
        ] {
            let settings = settings_with(rest);
            let registry = IntegrationRegistry::with_registrations(&settings, &linked_builders())
                .expect("should build the registry");
            let caches = trusted_server_core::integrations::loading_caches_from(
                &settings,
                &linked_builders(),
            )
            .expect("the declared cache selects the one loading cache module");

            assert!(registry.integration_runs(BUILDER_ID), "{case}");
            assert!(
                caches.for_feature(CACHE).keeps_values(),
                "{case}: the linked loading cache should hold the answers"
            );
        }
    }

    /// A `[cache.<name>]` table for a cache nothing declares is refused, so
    /// a misspelt table fails the deploy check and not the first request.
    #[test]
    fn a_misspelt_cache_table_is_refused_naming_the_declared_cache() {
        let settings = settings_with("[cache.51degrees-answer]\nmodule = \"51degrees\"\n");

        let message = format!(
            "{:?}",
            trusted_server_core::integrations::loading_caches_from(&settings, &linked_builders())
                .expect_err("nothing declares the misspelt cache")
        );

        assert!(
            message.contains("[cache.51degrees-answer]") && message.contains(CACHE),
            "{message}"
        );
    }
}
