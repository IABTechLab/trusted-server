//! Encypher provenance for article pages and their media, `provenance.encypher`.
//!
//! A serve middleware finds the article in each HTML page a `[[serve]]` entry
//! names it for, sends its visible text to the Encypher signing API, and
//! embeds the returned invisible markers and a manifest meta tag. Plans are
//! cached in the runtime KV store, bound to a short-lived server policy.
//! Every failure passes the page through unchanged.
//!
//! Media is signed as its exact bytes through the Encypher media endpoint,
//! under the same policy and worker token: images, audio, video and fonts
//! core captures on the publisher and asset proxy paths, through the
//! module's media signer; media `data:` URIs in the same pages, and their
//! inline SVG as the standalone SVG document proven to parse to the tree the
//! browser built, which is the original bytes where they already are one;
//! and media `data:` URIs in the stylesheets under each `path` a
//! `[[serve]]` entry names `provenance.encypher.stylesheet` for, which the
//! publisher asserts no page loads with `integrity`. Stylesheets under no
//! such path are left as they are. A media item that cannot be signed is
//! delivered as it was and reported unsigned, never as signed.
//!
//! The middleware's preparation applies the request gates with no I/O: only
//! a `GET` of a `200` page on the configured domain, whose origin did not
//! mark it as one reader's, is held. The signing work runs in the stream
//! processor's asynchronous finish, which core awaits at the end of the
//! document. The module requires no permission and reads no gated value. It
//! sends the visible text of the page's article, the page address without
//! its query, and the bytes of the media it signs. A page whose origin set a
//! cookie or forbade shared caching is never sent, but the module cannot
//! tell a personalized article that carries neither, so a deployment places
//! it only on public article routes and has its origin mark every
//! personalized response private.
//!
//! Independently of signing admission, the applicable page middleware protects
//! original provenance-bearing or structurally uncertain inline SVG ranges.
//! Core holds the bounded original before its own rewrites, keeps those ranges
//! opaque through fetch/serve changes and restores their exact bytes before
//! publishing a template or reader response. The same classifier decides both
//! this protection and the module's descendant-edit refusals.

use std::any::Any;
use std::sync::Arc;

use error_stack::Report;
use serde::Deserialize;
use validator::{Validate, ValidationError};

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistration, ModuleSecretSetting, SuppliedMiddleware,
};
use trusted_server_core::middleware::{
    HTML_MEDIA_TYPE, Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase, Preparation,
};
use trusted_server_core::module_context::{FromModuleContext as _, ModuleCall, ModuleRequest};
use trusted_server_core::redacted::Redacted;
use trusted_server_core::settings::{IntegrationConfig, Settings};

mod api;
mod boundary;
mod data_uri;
mod embed;
mod embedded;
mod media_api;
mod prepared_probe;
mod processor;
mod svg_equivalence;
mod text;

use processor::{
    EncypherMedia, PageRequest, Signer, document_processor, is_excluded_path, is_excluded_prefix,
    stylesheet_processor,
};

/// The id the module's registration and its API backend carry.
const INTEGRATION_ID: &str = "provenance_encypher";

/// The name `[provenance] module` selects the module by and a `[[serve]]`
/// entry names its middleware by, from this crate's folder.
pub const MODULE: &str = "provenance.encypher";

/// The name a `[[serve]]` entry for stylesheets writes, which signs the media
/// `data:` URIs a stylesheet embeds.
pub const STYLESHEET_MIDDLEWARE: &str = "provenance.encypher.stylesheet";

/// The media type [`STYLESHEET_MIDDLEWARE`] works on.
const STYLESHEET_MEDIA_TYPE: &str = "text/css";

/// `[provenance.encypher]`, the module's settings.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = "validate_hold_exceeds_timeout"))]
pub struct EncypherConfig {
    /// Encypher API origin, for example `https://api.example.com`. Must be https.
    #[validate(custom(function = validate_api_base_url))]
    pub api_base_url: String,
    /// Claimed publisher domain: a bare lowercase hostname without `www.`.
    #[validate(custom(function = validate_domain))]
    pub domain: String,
    /// The name of the secret-store entry holding the `cwk_` worker
    /// credential, resolved to the credential itself when settings load.
    pub worker_token: Redacted<String>,
    /// Optional article selector override: one unescaped `.class`, an exact
    /// `[attr=value]` (also single- or double-quoted), or an ASCII tag name.
    /// Empty means no override; a valid selector absent from a page keeps
    /// the detection fallback. At most 91 characters, so
    /// `override:{selector}` fits the API's 100-character limit.
    #[serde(default)]
    #[validate(length(max = 91), custom(function = validate_article_selector))]
    pub article_selector: String,
    /// Minimum visible article text, in Unicode scalars.
    #[serde(default = "default_min_text_length")]
    #[validate(range(min = 50, max = 50_000))]
    pub min_text_length: usize,
    /// Largest decoded document held for signing. Capped at
    /// `publisher.max_buffered_body_bytes`.
    #[serde(default = "default_max_document_bytes")]
    #[validate(range(min = 1024))]
    pub max_document_bytes: usize,
    /// First-byte timeout for each policy or sign call, in milliseconds. The
    /// only bound on a hung API on a runtime without a timer.
    #[serde(default = "default_timeout_ms")]
    #[validate(range(min = 250, max = 10_000))]
    pub timeout_ms: u64,
    /// Longest one document is held for signing, in milliseconds, on a
    /// runtime with a timer. Must exceed `timeout_ms`.
    #[serde(default = "default_max_hold_ms")]
    #[validate(range(min = 500, max = 15_000))]
    pub max_hold_ms: u64,
    /// Lifetime of a cached signing plan, in seconds.
    #[serde(default = "default_plan_ttl_seconds")]
    #[validate(range(min = 60, max = 31_536_000))]
    pub plan_ttl_seconds: u64,
    /// Lifetime of the server-authoritative policy, in seconds.
    #[serde(default = "default_policy_ttl_seconds")]
    #[validate(range(min = 60, max = 300))]
    pub policy_ttl_seconds: u64,
    /// Lifetime of a cached signing failure, in seconds.
    #[serde(default = "default_negative_ttl_seconds")]
    #[validate(range(min = 60, max = 86_400))]
    pub negative_ttl_seconds: u64,
    /// Largest media item signed, in bytes: a captured image or a decoded
    /// embedded item. Larger media is delivered unsigned. Bounded by what the
    /// endpoint embeds as source data when the source has no safe address.
    #[serde(default = "default_max_media_bytes")]
    #[validate(range(min = 1024, max = 8_000_000))]
    pub max_media_bytes: usize,
    /// Maximum uncached media-signing calls in one HTML or stylesheet pass.
    /// Cached assets do not consume calls. The document's existing time and
    /// output bounds still apply independently.
    #[serde(default = "default_max_media_sign_calls_per_document")]
    #[validate(range(min = 1, max = 128))]
    pub max_media_sign_calls_per_document: usize,
    /// Largest media signed through the prepared lane, in bytes: hashed in
    /// bounded chunks where it is delivered, never uploaded or held whole,
    /// and given a remote manifest its bytes are left unchanged by. `0`, the
    /// default, keeps the lane off, so no deployment starts hashing large
    /// files on its own; turning it on also needs the signed probe release
    /// and the prepared endpoints. Otherwise at least 16 MiB, and at most the
    /// prepared service's largest source. Media between `max_media_bytes`
    /// and this is the lane's. A bounded example is `1073741824` (1 GiB)
    /// with `max_large_media_ms = 60000`.
    #[serde(default = "default_max_large_media_bytes")]
    #[validate(custom(function = validate_max_large_media_bytes))]
    pub max_large_media_bytes: u64,
    /// Longest one large source may take, in milliseconds: core's hash of
    /// the whole source and the module's probe and API calls together. A
    /// source not signed in time is delivered unsigned.
    #[serde(default = "default_max_large_media_ms")]
    #[validate(range(min = 5_000, max = 600_000))]
    pub max_large_media_ms: u64,
}

/// The prepared service's largest source, in bytes: the largest integer a
/// JSON number carries exactly, less the room it keeps for a manifest.
const MAX_PREPARED_SOURCE_BYTES: u64 = 9_007_199_246_352_383;
/// The smallest non-zero `max_large_media_bytes`, above every captured size.
const MIN_LARGE_MEDIA_BYTES: u64 = 16 * 1024 * 1024;

impl IntegrationConfig for EncypherConfig {}

fn default_min_text_length() -> usize {
    50
}

fn default_max_document_bytes() -> usize {
    5_000_000
}

fn default_timeout_ms() -> u64 {
    3_000
}

fn default_max_hold_ms() -> u64 {
    5_000
}

fn default_plan_ttl_seconds() -> u64 {
    604_800
}

fn default_policy_ttl_seconds() -> u64 {
    300
}

fn default_negative_ttl_seconds() -> u64 {
    300
}

fn default_max_media_bytes() -> usize {
    5_000_000
}

fn default_max_media_sign_calls_per_document() -> usize {
    8
}

fn default_max_large_media_bytes() -> u64 {
    0
}

fn default_max_large_media_ms() -> u64 {
    60_000
}

fn validate_max_large_media_bytes(value: u64) -> Result<(), ValidationError> {
    if value == 0 || (MIN_LARGE_MEDIA_BYTES..=MAX_PREPARED_SOURCE_BYTES).contains(&value) {
        Ok(())
    } else {
        Err(ValidationError::new("encypher_max_large_media_bytes"))
    }
}

fn validate_article_selector(value: &str) -> Result<(), ValidationError> {
    if value.is_empty() || boundary::is_valid_selector(value) {
        Ok(())
    } else {
        Err(ValidationError::new("encypher_article_selector"))
    }
}

fn validate_api_base_url(value: &str) -> Result<(), ValidationError> {
    let parsed =
        url::Url::parse(value).map_err(|_| ValidationError::new("encypher_api_base_url"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none_or(str::is_empty)
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ValidationError::new("encypher_api_base_url"));
    }
    Ok(())
}

fn validate_domain(value: &str) -> Result<(), ValidationError> {
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    };
    let valid = value.len() <= 253
        && value.contains('.')
        && !value.starts_with("www.")
        && value.split('.').all(valid_label);
    if valid {
        Ok(())
    } else {
        Err(ValidationError::new("encypher_domain"))
    }
}

fn validate_hold_exceeds_timeout(config: &EncypherConfig) -> Result<(), ValidationError> {
    if config.max_hold_ms > config.timeout_ms {
        Ok(())
    } else {
        Err(ValidationError::new("encypher_max_hold_ms"))
    }
}

/// The worker token, named in `[provenance.encypher]` by its entry in the
/// secret store, which is in use whenever the module is selected.
const SECRET_SETTINGS: &[ModuleSecretSetting] = &[ModuleSecretSetting {
    path: &["worker_token"],
    in_use: always_in_use,
}];

fn always_in_use(_table: &serde_json::Map<String, serde_json::Value>) -> bool {
    true
}

/// The changes the module supplies, as `[[serve]]` entries name them: the
/// page's article and embedded media, and a stylesheet's embedded media.
const SUPPLIED_MIDDLEWARE: &[SuppliedMiddleware] = &[
    SuppliedMiddleware::on_html(MODULE, MiddlewarePhase::Serve),
    SuppliedMiddleware::on(
        STYLESHEET_MIDDLEWARE,
        MiddlewarePhase::Serve,
        STYLESHEET_MEDIA_TYPE,
    ),
];

/// The builder a deployment installs to offer this module.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(INTEGRATION_ID, env!("CARGO_PKG_NAME"), register, validate)
        .with_module_name(MODULE)
        .with_secret_settings(SECRET_SETTINGS)
        .with_middleware(SUPPLIED_MIDDLEWARE)
}

/// Refuses a document that selects the module and names its middleware in no
/// `[[serve]]` entry, where no page would ever be signed.
fn require_an_entry(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    let placed = settings
        .serve
        .entries()
        .iter()
        .any(|entry| entry.middleware.iter().any(|name| name == MODULE));
    if placed {
        return Ok(());
    }
    Err(Report::new(TrustedServerError::Settings {
        message: format!(
            "[provenance] selects `encypher`, but no [[serve]] entry names `{MODULE}`, so no \
             page would be signed. Add `{MODULE}` to the [[serve]] entry that covers the \
             article pages, with a `path` for each section to sign"
        ),
    }))
}

/// Refuses a `[[serve]]` entry that names the stylesheet middleware without
/// a `path` narrower than the whole site.
///
/// Signing a stylesheet's media changes its bytes, which breaks a page that
/// loads it with a subresource integrity value, and the middleware cannot
/// see the page that loads it. So it runs only on stylesheets the publisher
/// names by path, asserting none of them is loaded with `integrity`; a bare
/// `text/css` entry, or one for `/`, would rewrite every stylesheet.
fn require_scoped_stylesheet_entries(
    settings: &Settings,
) -> Result<(), Report<TrustedServerError>> {
    let unscoped = settings.serve.entries().iter().any(|entry| {
        entry
            .middleware
            .iter()
            .any(|name| name == STYLESHEET_MIDDLEWARE)
            && entry
                .path
                .as_deref()
                .is_none_or(|path| path.is_empty() || path == "/")
    });
    if !unscoped {
        return Ok(());
    }
    Err(Report::new(TrustedServerError::Settings {
        message: format!(
            "a [[serve]] entry names `{STYLESHEET_MIDDLEWARE}` for every path. Signing a \
             stylesheet's embedded media changes its bytes and breaks a page that loads it \
             with `integrity`, so give the entry a `path` for the stylesheets no page loads \
             with `integrity`"
        ),
    }))
}

/// The module's settings when a section selects it.
fn config(settings: &Settings) -> Result<Option<EncypherConfig>, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<EncypherConfig>(MODULE)? else {
        return Ok(None);
    };
    require_an_entry(settings)?;
    require_scoped_stylesheet_entries(settings)?;
    Ok(Some(config))
}

/// Validates the module's table and reports whether a section selects the
/// module.
///
/// # Errors
///
/// Returns an error when the table cannot be read or fails its validation, or
/// when no `[[serve]]` entry names the middleware.
pub fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(config(settings)?.is_some())
}

/// Registers the middleware and the media signer when a section selects the
/// module and its worker token is not blank.
///
/// # Errors
///
/// Returns the errors [`validate`] returns.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = config(settings)? else {
        return Ok(None);
    };
    if config.worker_token.expose().trim().is_empty() {
        log::warn!("encypher: not registered, because the worker token is empty");
        return Ok(None);
    }
    log::info!("encypher: registered for domain={}", config.domain);
    let max_document_bytes = config
        .max_document_bytes
        .min(settings.publisher.max_buffered_body_bytes);
    let signer = Arc::new(Signer::new(config, max_document_bytes));
    Ok(Some(
        IntegrationRegistration::builder(INTEGRATION_ID)
            .without_js()
            .with_middleware(Arc::new(Encypher {
                signer: Arc::clone(&signer),
            }))
            .with_middleware(Arc::new(EncypherStylesheet {
                signer: Arc::clone(&signer),
            }))
            .with_media_signer(Arc::new(EncypherMedia::new(signer)))
            .build(),
    ))
}

/// The middleware, which signs the article and the embedded media of each
/// admitted page.
pub struct Encypher {
    signer: Arc<Signer>,
}

impl std::fmt::Debug for Encypher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Encypher").finish_non_exhaustive()
    }
}

/// What the preparation decided for one document: the page to sign, or none.
type Admission = Option<PageRequest>;

impl Middleware for Encypher {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == HTML_MEDIA_TYPE
    }

    fn source_protection(&self) -> Option<trusted_server_core::middleware::SourceProtection> {
        Some(embedded::source_protection)
    }

    /// Every path but an API, feed, admin or well-known path, or an asset,
    /// which is never an article.
    fn handles_path(&self, path: &str) -> bool {
        !is_excluded_path(path)
    }

    /// Serve only: the signed page is written on each reader's copy, so a
    /// failed signing is never stored for every reader.
    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    /// Applies the request gates, with no I/O, and keeps the page to sign.
    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        let admission = admit(&call, context, |page| self.signer.admit(page));
        Some(Box::pin(async move {
            Ok(Box::new(admission) as Box<dyn Any + Send>)
        }))
    }

    /// Holds the admitted page, with the security policies its response
    /// carries, which decide whether inline style media may be edited.
    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let Some(page) = context.take_prepared::<Admission>(MODULE).flatten() else {
            return MiddlewareAction::pass();
        };
        let policies = context
            .response_headers
            .and_then(embedded::content_security_policies);
        MiddlewareAction {
            stream: Some(Box::new(document_processor(&self.signer, page, policies))),
            ..MiddlewareAction::pass()
        }
    }
}

/// The stylesheet middleware, which signs the media `data:` URIs of each
/// admitted stylesheet.
pub struct EncypherStylesheet {
    signer: Arc<Signer>,
}

impl std::fmt::Debug for EncypherStylesheet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EncypherStylesheet")
            .finish_non_exhaustive()
    }
}

impl Middleware for EncypherStylesheet {
    fn middleware_id(&self) -> &'static str {
        STYLESHEET_MIDDLEWARE
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == STYLESHEET_MEDIA_TYPE
    }

    /// Every path but an API, feed, admin or well-known path. A `.css`
    /// suffix is what a stylesheet has.
    fn handles_path(&self, path: &str) -> bool {
        !is_excluded_prefix(path)
    }

    /// Serve only, as for pages.
    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    /// Applies the request gates, with no I/O, and keeps the stylesheet to
    /// sign.
    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        let admission = admit(&call, context, |page| self.signer.admit_stylesheet(page));
        Some(Box::pin(async move {
            Ok(Box::new(admission) as Box<dyn Any + Send>)
        }))
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let Some(page) = context
            .take_prepared::<Admission>(STYLESHEET_MIDDLEWARE)
            .flatten()
        else {
            return MiddlewareAction::pass();
        };
        MiddlewareAction {
            stream: Some(Box::new(stylesheet_processor(&self.signer, page))),
            ..MiddlewareAction::pass()
        }
    }
}

/// The document a request is for, when `gate` admits it.
fn admit(
    call: &ModuleCall<'_>,
    context: &MiddlewareContext<'_>,
    gate: impl FnOnce(PageRequest) -> Result<PageRequest, processor::Gate>,
) -> Admission {
    let request = ModuleRequest::from_module_context(call).ok()?;
    let removed_prefix = request.removed_prefix().unwrap_or_default();
    let page = PageRequest {
        method: request.method().clone(),
        // The address the reader asked for: a host that serves several
        // publishers by path signs each page for its own address.
        page_url: format!(
            "{}://{}{removed_prefix}{}",
            request.scheme(),
            request.host(),
            request.path()
        ),
        removed_prefix: removed_prefix.to_owned(),
        response_status: context.status.as_u16(),
        origin_response_is_private: context.origin_private,
    };
    match gate(page) {
        Ok(page) => Some(page),
        Err(gate) => {
            log::debug!("encypher: document not held, gate={}", gate.code());
            None
        }
    }
}

#[cfg(test)]
mod tests;
