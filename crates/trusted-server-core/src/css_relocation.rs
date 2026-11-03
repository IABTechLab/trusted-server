//! A stylesheet a rehosting route serves from the reader's host, its own
//! addresses moved with it and its `[[serve]]` entry run on it, see
//! [`crate::rehost`].
//!
//! A stylesheet's relative and root-relative addresses resolve against the
//! address it is fetched from, so serving it from another address changes
//! what they name. Every `url()`, `src()`, `image-set()` candidate and
//! `@import` the browser would fetch is read with core's CSS tokenizer,
//! resolved against the stylesheet's upstream address, and written as the
//! address that fetches the same thing: a rehosting route's address where
//! one serves it, the absolute upstream address otherwise. A fragment-only
//! reference and `data:`, `blob:` and `about:` addresses are left as written.
//! An `@import` moves to a route only where that route moves stylesheets.

use bytes::Bytes;
use error_stack::Report;
use http::Method;
use url::Url;

use crate::cache_policy::{CachePolicy, CacheVisibility};
use crate::creative::{CssUrlContext, CssUrlPolicy, rewrite_css_urls};
use crate::error::TrustedServerError;
use crate::integrations::{IntegrationRegistry, IntegrationRequestState};
use crate::media_signing::OriginFreshness;
use crate::middleware::{MiddlewarePhase, Prepared, ResponseHeader};
use crate::module_context::{ModuleContext, ModuleRequest};
use crate::platform::RuntimeServices;
use crate::rehost::{PageAddress, Reference, RehostRoutes};
use crate::settings::Settings;
use crate::streaming_processor::StreamProcessor as _;

/// The largest stylesheet a rehosting route moves, before and after its
/// addresses are moved.
pub(crate) const MAX_STYLESHEET_BYTES: usize = 10 * 1024 * 1024;

/// Keep buffered bytes without copying. Streamed chunks are checked against
/// the remaining capacity by the shared upstream collector before appending.
pub(crate) async fn collect_stylesheet(
    body: edgezero_core::body::Body,
) -> Result<Bytes, Report<TrustedServerError>> {
    match body {
        edgezero_core::body::Body::Once(bytes) if bytes.len() <= MAX_STYLESHEET_BYTES => Ok(bytes),
        body => crate::integrations::collect_response_bounded(
            body,
            MAX_STYLESHEET_BYTES,
            "stylesheet_relocation",
        )
        .await
        .map(Bytes::from),
    }
}

/// A relocated answer cannot outlive its source or the route's policy.
/// Stale allowances and immutability do not transfer to these mutable URLs.
pub(crate) fn bounded_cache_policy(
    remaining: OriginFreshness,
    route: Option<CachePolicy>,
) -> CachePolicy {
    CachePolicy {
        visibility: route.map_or(CacheVisibility::Public, |policy| policy.visibility),
        browser_ttl: Some(
            route
                .and_then(|policy| policy.browser_ttl)
                .map_or(remaining.browser, |cap| remaining.browser.min(cap)),
        ),
        edge_ttl: Some(
            route
                .and_then(|policy| policy.edge_ttl)
                .map_or(remaining.shared, |cap| remaining.shared.min(cap)),
        ),
        stale_while_revalidate: None,
        stale_if_error: None,
        immutable: false,
    }
}

/// The route policy: each address resolved against the stylesheet's
/// upstream address and written as the address that fetches the same thing.
struct RouteAddresses<'a> {
    stylesheet: &'a Url,
    routes: &'a RehostRoutes,
    page: PageAddress<'a>,
}

impl CssUrlPolicy for RouteAddresses<'_> {
    fn replace(&mut self, value: &str, context: CssUrlContext) -> Option<String> {
        let value = value.trim();
        if value.is_empty() || value.starts_with('#') {
            return None;
        }
        let resolved = self.stylesheet.join(value).ok()?;
        if !matches!(resolved.scheme(), "http" | "https") {
            return None;
        }
        let reference = match context {
            CssUrlContext::Import => Reference::Style,
            CssUrlContext::Resource => Reference::Media,
        };
        Some(
            self.routes
                .rehosted(&resolved, reference, &self.page)
                .unwrap_or_else(|| resolved.to_string()),
        )
    }
}

/// Why a rehosted stylesheet goes to the reader at its upstream address
/// instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum NotRelocated {
    /// The body is not UTF-8, or names another charset with `@charset`.
    #[display("stylesheet_not_utf8")]
    NotUtf8,
    /// The tokenizer refused it: nested too deep or grown too large.
    #[display("stylesheet_not_rewritable")]
    NotRewritable,
    /// A middleware of its `[[serve]]` entry refused it.
    #[display("stylesheet_refused")]
    Refused,
}

/// One stylesheet a rehosting route serves.
pub(crate) struct Stylesheet<'a> {
    pub settings: &'a Settings,
    pub services: &'a RuntimeServices,
    pub integration_registry: &'a IntegrationRegistry,
    /// The address the stylesheet was fetched from.
    pub upstream: &'a Url,
    /// Its `Content-Type`.
    pub content_type: &'a str,
    /// The route path the reader asked for, after any prefix request
    /// addressing removed.
    pub route_path: &'a str,
    pub page: PageAddress<'a>,
    /// The headers of the reader's response, as the middleware are told
    /// them.
    pub response_headers: &'a http::HeaderMap,
}

impl Stylesheet<'_> {
    /// `body` with its addresses moved and its `[[serve]]` entry's
    /// middleware run on it, with the headers those middleware write.
    ///
    /// # Errors
    ///
    /// Why the stylesheet cannot be served from the route.
    pub(crate) async fn relocate(
        &self,
        body: &Bytes,
    ) -> Result<(Vec<u8>, Vec<ResponseHeader>), NotRelocated> {
        let text = std::str::from_utf8(body).map_err(|_| NotRelocated::NotUtf8)?;
        if names_another_charset(text) {
            return Err(NotRelocated::NotUtf8);
        }
        let routes = RehostRoutes::of(self.settings);
        let relocated = rewrite_css_urls(
            &mut RouteAddresses {
                stylesheet: self.upstream,
                routes: &routes,
                page: self.page,
            },
            text,
            true,
            MAX_STYLESHEET_BYTES,
        )
        .map_err(|_| NotRelocated::NotRewritable)?;

        let prepared = Prepared::default();
        let request_state = IntegrationRequestState::default();
        let document = crate::publisher::TextResponse {
            content_type: self.content_type,
            status: http::StatusCode::OK,
            origin_private: false,
            response_headers: Some(self.response_headers),
            may_be_stored: false,
            request_path: self.route_path,
            request_host: self.page.host,
            request_scheme: self.page.scheme,
            origin_host: self.upstream.host_str().unwrap_or_default(),
            request_state: &request_state,
            max_buffered_script_bytes: self.settings.publisher.max_buffered_body_bytes,
            prepared: &prepared,
        };
        // Served to every reader alike, so prepared with no reader's state.
        let services = self.services.clone().without_connection();
        let removed_prefix =
            (!self.page.removed_prefix.is_empty()).then_some(self.page.removed_prefix);
        let method = Method::GET;
        let context = ModuleContext::new(
            ModuleRequest::new(&method, self.page.host, self.page.scheme, self.route_path)
                .with_removed_prefix(removed_prefix),
        )
        .with_settings(self.settings)
        .with_services(&services);
        crate::publisher::prepare_document(
            &document,
            &[(MiddlewarePhase::Serve, &context)],
            self.settings,
            self.integration_registry,
        )
        .await
        .map_err(|_| NotRelocated::Refused)?;
        let (mut processor, headers) = crate::publisher::middleware_stream_processors(
            &document,
            &[MiddlewarePhase::Serve],
            self.settings,
            self.integration_registry,
        )
        .map_err(|_| NotRelocated::Refused)?
        .into_middleware_processor();
        let signed = processor
            .finish(relocated.as_bytes(), self.services)
            .await
            .map_err(|_| NotRelocated::Refused)?;
        Ok((signed, headers))
    }
}

/// Whether a stylesheet's leading `@charset` names an encoding other than
/// UTF-8.
fn names_another_charset(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("@charset \"") else {
        return false;
    };
    let name = rest.split('"').next().unwrap_or_default();
    !name.eq_ignore_ascii_case("utf-8")
}

#[cfg(test)]
mod tests;
