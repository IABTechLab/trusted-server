//! Asset routes that rehost a third party's media for the publisher's pages.
//!
//! A route that sets
//! [`rehost_page_references`](crate::settings::ProxyAssetRoute::rehost_page_references)
//! serves the files of one third-party folder, such as
//! `https://cdn.example.net/lib`, under its prefix on the reader's host. The
//! request path after the prefix is the path under that folder, see
//! [`target_path`], and the reader's query is passed on as it came.
//!
//! The publisher's pages are pointed at the route. An address on the folder,
//! resolved against the document's first effective base, in an attribute that fetches a
//! stylesheet or media, is written as the route's address on the reader's
//! host, see [`RehostRoutes::decide`], so the route serves it and the media
//! signer sees its bytes. A stylesheet moves only where a `[[serve]]` entry
//! for `text/css` covers its route path, because a stylesheet served from
//! another address must have its own addresses moved with it, which the
//! route does on that entry's path. An address is written only once the
//! route it would reach is proven to be this one and to fetch exactly the
//! address the page wrote, query included. An address the page protects with
//! `integrity` is never moved, and neither is anything that fails the proof.
//!
//! The publisher resolves the first effective HTML base from the bounded
//! original document before rewriting any reference, including earlier ones.

use error_stack::Report;
use url::Url;

use crate::error::TrustedServerError;
use crate::middleware::{MiddlewarePhase, PhaseEntries};
use crate::settings::{OriginQueryPolicy, ProxyAssetRoute, Settings, longest_prefix_index};

/// The media type a stylesheet route's `[[serve]]` entry names.
pub(crate) const STYLESHEET_MEDIA_TYPE: &str = "text/css";

/// The target path under the source's folder for `path`, a request path
/// that begins with a rehosting route's `prefix`: `/` and what follows the
/// prefix.
///
/// # Errors
///
/// When `path` does not begin with `prefix`, names the folder itself, or
/// has a segment that could leave the folder: `.` or `..`, written plainly
/// or percent-encoded, or an encoded `/` or `\`.
pub(crate) fn target_path(prefix: &str, path: &str) -> Result<String, Report<TrustedServerError>> {
    let refuse = |message: &str| {
        Report::new(TrustedServerError::BadRequest {
            message: format!("rehosted asset path {message}"),
        })
    };
    let suffix = path
        .strip_prefix(prefix)
        .ok_or_else(|| refuse("does not begin with the route prefix"))?;
    if suffix.is_empty() {
        return Err(refuse("names no file"));
    }
    if suffix.split('/').any(unsafe_segment) {
        return Err(refuse("has a segment that could leave the source folder"));
    }
    Ok(format!("/{suffix}"))
}

/// Whether a path segment could name something outside its folder once an
/// origin decodes it.
fn unsafe_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") || segment.contains('\\') {
        return true;
    }
    let decoded = lower.replace("%2e", ".");
    decoded == "." || decoded == ".."
}

/// Checks one route that sets `rehost_page_references`.
///
/// # Errors
///
/// When the route sets a path rewrite or the image optimizer, its prefix
/// does not end in `/` or is in the deployment's own namespace, or its
/// source is not core's `origin` over `https` with no `auth`, no
/// `host_header` naming another authority and no `directory_index`, or its
/// query policy is not `preserve`.
pub(crate) fn check_route(route: &ProxyAssetRoute) -> Result<(), Report<TrustedServerError>> {
    let refuse = |message: &str| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "proxy.asset_routes prefix `{}` sets rehost_page_references, so {message}",
                route.prefix
            ),
        })
    };
    if route.path_pattern.is_some() || route.target_path.is_some() {
        return Err(refuse(
            "it maps its own paths and sets no path_pattern or target_path",
        ));
    }
    if route.image_optimizer.is_some() {
        return Err(refuse(
            "it serves the bytes its source serves and sets no image_optimizer",
        ));
    }
    if !route.prefix.ends_with('/') {
        return Err(refuse("its prefix must end in `/`"));
    }
    if in_namespace(&route.prefix, crate::http_util::ROUTE_NAMESPACE) {
        return Err(refuse(
            "its prefix must not be in the deployment's own `/_ts` namespace",
        ));
    }
    let origin = route
        .origin_source()
        .ok_or_else(|| refuse("its source must be core's `origin`"))?;
    let url = crate::source::validate_url(&origin.url).map_err(|reason| refuse(&reason))?;
    if url.scheme() != "https" {
        return Err(refuse("its source url must use https"));
    }
    if origin.auth.is_some() {
        return Err(refuse("its source signs nothing and sets no auth"));
    }
    if origin
        .host_header
        .as_deref()
        .is_some_and(|host| !host.eq_ignore_ascii_case(&authority(&url)))
    {
        return Err(refuse(
            "its source sends the url's own authority, including its port, and sets no other host_header",
        ));
    }
    if origin.directory_index.is_some() {
        return Err(refuse("its source sets no directory_index"));
    }
    if route.origin_query_policy() != OriginQueryPolicy::Preserve {
        return Err(refuse("its source keeps the reader's query"));
    }
    Ok(())
}

/// Whether `path` is `namespace` or under it.
fn in_namespace(path: &str, namespace: &str) -> bool {
    path == namespace
        || path
            .strip_prefix(namespace)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Checks the rehosting routes together, each already checked by
/// [`check_route`]: no route's prefix overlaps a rehosting route's, no two
/// rehosting routes serve one folder or nested folders, and a probe address
/// on each makes the round trip.
///
/// # Errors
///
/// When two routes overlap or a probe fails the round trip.
pub(crate) fn check_routes(routes: &[ProxyAssetRoute]) -> Result<(), Report<TrustedServerError>> {
    let refuse = |message: String| Report::new(TrustedServerError::Configuration { message });
    let rehosting = RehostRoutes::of_parts(routes, PhaseEntries::default());
    for route in &rehosting.routes {
        for (index, other) in routes.iter().enumerate() {
            if index != route.index
                && (other.prefix.starts_with(&route.prefix)
                    || route.prefix.starts_with(&other.prefix))
            {
                return Err(refuse(format!(
                    "proxy.asset_routes prefix `{}` sets rehost_page_references, so no other \
                     route's prefix may begin it or begin with it, as `{}` does",
                    route.prefix, other.prefix
                )));
            }
        }
        for other in &rehosting.routes {
            if other.index != route.index && route.covers_origin_of(other) {
                return Err(refuse(format!(
                    "proxy.asset_routes prefixes `{}` and `{}` both rehost the folder of one \
                     source, or one inside the other",
                    route.prefix, other.prefix
                )));
            }
        }
        let probe = format!(
            "{}://{}{}/rehost-probe/x.css?q=1#f",
            route.origin.scheme(),
            authority(&route.origin),
            route.folder
        );
        let page = PageAddress {
            scheme: "https",
            host: "reader.invalid",
            removed_prefix: "",
            base: None,
        };
        let expected = format!(
            "https://reader.invalid{}rehost-probe/x.css?q=1#f",
            route.prefix
        );
        match rehosting.decide(&probe, Reference::Media, false, &page) {
            Decision::Rehost(emitted) if emitted == expected => {}
            other => {
                return Err(refuse(format!(
                    "proxy.asset_routes prefix `{}` failed its round trip: {probe} became \
                     {other:?}",
                    route.prefix
                )));
            }
        }
    }
    Ok(())
}

/// `url`'s host and any port it names.
fn authority(url: &Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_owned(),
    }
}

/// One rehosting route.
#[derive(Debug, Clone)]
struct Route {
    /// Its position among every asset route, which dispatch names.
    index: usize,
    prefix: String,
    /// The source's address.
    origin: Url,
    /// The source's folder, without a closing `/`, empty for the root.
    folder: String,
}

impl Route {
    /// Whether `other`'s folder is this route's, or inside or around it, on
    /// the same origin.
    fn covers_origin_of(&self, other: &Self) -> bool {
        same_origin(&self.origin, &other.origin)
            && (self.folder == other.folder
                || self.folder.starts_with(&format!("{}/", other.folder))
                || other.folder.starts_with(&format!("{}/", self.folder)))
    }
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// What the page's reader asked for, which the route's address is written
/// on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PageAddress<'a> {
    /// The reader's scheme.
    pub scheme: &'a str,
    /// The reader's host, with any port.
    pub host: &'a str,
    /// The path prefix request addressing removed, or empty.
    pub removed_prefix: &'a str,
    /// The document's effective base, read from its original markup.
    /// CSS and absolute startup probes need no document base.
    pub base: Option<&'a Url>,
}

/// What an address is fetched as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reference {
    /// A stylesheet, whose own addresses must move with it.
    Style,
    /// An image, a font, audio or video, fetched as it is.
    Media,
}

/// What [`RehostRoutes::decide`] decided about an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// No rehosting route serves it, so the page's other rewriting applies.
    Unmatched,
    /// A rehosting route serves its folder, or it is already a route's
    /// address, and it stays exactly as written: it is protected by
    /// `integrity`, is a stylesheet no `[[serve]]` entry moves, or fails the
    /// round trip. Nothing else rewrites it either.
    Keep,
    /// The route's address that replaces it.
    Rehost(String),
}

/// What a page's addresses are rehosted with: the deployment's rehosting
/// routes and the path prefix request addressing removed from the page's
/// own address.
#[derive(Debug, Clone, Default)]
pub struct PageRehost {
    pub(crate) routes: RehostRoutes,
    pub(crate) removed_prefix: String,
    pub(crate) base: Option<Url>,
}

/// The deployment's rehosting routes, with what decides whether a
/// stylesheet moves.
#[derive(Debug, Clone, Default)]
pub struct RehostRoutes {
    routes: Vec<Route>,
    /// Every asset route's prefix, in order, which dispatch matches.
    prefixes: Vec<String>,
    /// The `[[serve]]` entries.
    serve: PhaseEntries,
}

impl RehostRoutes {
    /// The rehosting routes `settings` configures.
    #[must_use]
    pub fn of(settings: &Settings) -> Self {
        Self::of_parts(
            &settings.proxy.asset_routes,
            settings.phase_entries(MiddlewarePhase::Serve).clone(),
        )
    }

    fn of_parts(routes: &[ProxyAssetRoute], serve: PhaseEntries) -> Self {
        let rehosting = routes
            .iter()
            .enumerate()
            .filter(|(_, route)| route.rehost_page_references)
            .filter_map(|(index, route)| {
                let origin = Url::parse(&route.origin_source()?.url).ok()?;
                let folder = origin.path().trim_end_matches('/').to_owned();
                Some(Route {
                    index,
                    prefix: route.prefix.clone(),
                    origin,
                    folder,
                })
            })
            .collect();
        Self {
            routes: rehosting,
            prefixes: routes.iter().map(|route| route.prefix.clone()).collect(),
            serve,
        }
    }

    /// Whether no route rehosts anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Whether a `[[serve]]` entry moves the stylesheet at `route_path`.
    fn stylesheet_approved(&self, route_path: &str) -> bool {
        !self
            .serve
            .for_response(STYLESHEET_MEDIA_TYPE, route_path)
            .is_empty()
    }

    /// What becomes of `value`, an address exactly as a page wrote it, read
    /// as `reference`. `protected` says the element carries `integrity`.
    pub(crate) fn decide(
        &self,
        value: &str,
        reference: Reference,
        protected: bool,
        page: &PageAddress<'_>,
    ) -> Decision {
        if self.routes.is_empty() {
            return Decision::Unmatched;
        }
        let scheme = page.base.map_or(page.scheme, Url::scheme);
        let resolved;
        let parts = if let Some(parts) = RawAddress::read(value, scheme) {
            parts
        } else {
            let Some(base) = page.base else {
                return Decision::Unmatched;
            };
            let Ok(url) = base.join(value) else {
                return Decision::Unmatched;
            };
            resolved = url;
            let Some(parts) = RawAddress::read(resolved.as_str(), scheme) else {
                return Decision::Unmatched;
            };
            parts
        };
        if self.is_route_address(&parts, page) {
            return Decision::Keep;
        }
        let Some(route) = self.route_for(&parts) else {
            return Decision::Unmatched;
        };
        if protected {
            return Decision::Keep;
        }
        self.rehost(route, &parts, reference, page)
            .map_or(Decision::Keep, Decision::Rehost)
    }

    /// The route's address for `url`, an absolute address a stylesheet
    /// names, resolved against the stylesheet's own address, or `None`
    /// where no rehosting route serves it or it fails the round trip.
    pub(crate) fn rehosted(
        &self,
        url: &Url,
        reference: Reference,
        page: &PageAddress<'_>,
    ) -> Option<String> {
        let parts = RawAddress::read(url.as_str(), page.scheme)?;
        if self.is_route_address(&parts, page) {
            return Some(url.as_str().to_owned());
        }
        let route = self.route_for(&parts)?;
        self.rehost(route, &parts, reference, page)
    }

    /// Whether `parts` is already a rehosting route's address on the
    /// reader's host, which is left as it is.
    fn is_route_address(&self, parts: &RawAddress<'_>, page: &PageAddress<'_>) -> bool {
        if !parts.scheme.eq_ignore_ascii_case(page.scheme)
            || !parts.authority.eq_ignore_ascii_case(page.host)
        {
            return false;
        }
        let Some(route_path) = parts.path.strip_prefix(page.removed_prefix) else {
            return false;
        };
        longest_prefix_index(self.prefixes.iter().map(String::as_str), route_path)
            .is_some_and(|index| self.routes.iter().any(|route| route.index == index))
    }

    /// The rehosting route whose folder holds `parts`.
    fn route_for(&self, parts: &RawAddress<'_>) -> Option<&Route> {
        let url = parts.url.as_ref()?;
        self.routes.iter().find(|route| {
            same_origin(url, &route.origin)
                && parts
                    .path
                    .strip_prefix(route.folder.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }

    /// `parts` written as `route`'s address on the reader's host, once
    /// dispatch is proven to reach `route` and the route is proven to fetch
    /// exactly `parts`, or `None`.
    fn rehost(
        &self,
        route: &Route,
        parts: &RawAddress<'_>,
        reference: Reference,
        page: &PageAddress<'_>,
    ) -> Option<String> {
        let suffix = parts
            .path
            .strip_prefix(route.folder.as_str())?
            .strip_prefix('/')?;
        let route_path = format!("{}{suffix}", route.prefix);
        if reference == Reference::Style && !self.stylesheet_approved(&route_path) {
            return None;
        }
        let dispatched =
            longest_prefix_index(self.prefixes.iter().map(String::as_str), &route_path);
        if dispatched != Some(route.index) {
            return None;
        }
        let target = target_path(&route.prefix, &route_path).ok()?;
        let fetched = crate::proxy::target_url_under(
            &route.origin,
            target,
            None,
            parts.query.unwrap_or_default(),
        )
        .ok()?;
        let wanted = parts.url.as_ref()?;
        let mut wanted_without_fragment = wanted.clone();
        wanted_without_fragment.set_fragment(None);
        if fetched.as_str() != wanted_without_fragment.as_str() {
            return None;
        }
        let mut emitted = format!(
            "{}://{}{}{route_path}",
            page.scheme, page.host, page.removed_prefix
        );
        if let Some(query) = parts.query {
            emitted.push('?');
            emitted.push_str(query);
        }
        if let Some(fragment) = parts.fragment {
            emitted.push('#');
            emitted.push_str(fragment);
        }
        Some(emitted)
    }
}

/// An absolute or protocol-relative address split as written, its bytes
/// kept, with its parsed form.
#[derive(Debug)]
struct RawAddress<'a> {
    scheme: &'a str,
    authority: &'a str,
    /// The path as written, `/` where none is.
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
    /// The address as the browser resolves it, or `None` where it does not
    /// parse.
    url: Option<Url>,
}

impl<'a> RawAddress<'a> {
    /// Reads `value`, trimmed as the browser trims an address, when it is
    /// absolute with an authority or protocol-relative. A protocol-relative
    /// address takes `document_scheme`, the scheme of the page it is on.
    fn read(value: &'a str, document_scheme: &'a str) -> Option<Self> {
        let value = value.trim_matches(|c: char| c.is_ascii_whitespace());
        let (scheme, after_scheme) = if let Some(rest) = value.strip_prefix("//") {
            (document_scheme, rest)
        } else {
            let (scheme, rest) = value.split_once("://")?;
            if scheme.is_empty()
                || !scheme
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
            {
                return None;
            }
            (scheme, rest)
        };
        let (rest, fragment) = match after_scheme.split_once('#') {
            Some((rest, fragment)) => (rest, Some(fragment)),
            None => (after_scheme, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((rest, query)) => (rest, Some(query)),
            None => (rest, None),
        };
        let (authority, path) = match rest.find('/') {
            Some(slash) => rest.split_at(slash),
            None => (rest, "/"),
        };
        if authority.is_empty() || authority.contains('@') {
            return None;
        }
        let url = Url::parse(&format!("{scheme}://{after_scheme}")).ok();
        Some(Self {
            scheme,
            authority,
            path,
            query,
            fragment,
            url,
        })
    }
}

/// The ranges of the addresses in a `srcset` or `imagesrcset` value, read
/// as a browser reads candidates: an address is a run without whitespace,
/// losing any commas it ends with, and its descriptors run to the next comma
/// outside parentheses.
pub(crate) fn srcset_address_ranges(value: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = value.as_bytes();
    let mut ranges = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        while at < bytes.len() && (bytes[at].is_ascii_whitespace() || bytes[at] == b',') {
            at += 1;
        }
        if at >= bytes.len() {
            break;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let mut end = at;
        while end > start && bytes[end - 1] == b',' {
            end -= 1;
        }
        if end > start {
            ranges.push(start..end);
        }
        if end < at {
            // The address ended the candidate with its commas.
            continue;
        }
        let mut depth = 0_usize;
        while at < bytes.len() {
            match bytes[at] {
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => break,
                _ => {}
            }
            at += 1;
        }
    }
    ranges
}

#[cfg(test)]
mod tests;
