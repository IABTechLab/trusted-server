//! Rewriting of publisher-origin URLs in proxied response headers.
//!
//! The publisher-page path rewrites origin URLs in the response body to the
//! serving host. Headers that carry URLs must follow the same mapping, or the
//! browser acts on them against the origin: it follows `Location` and `Refresh`
//! off the appliance, preloads `Link` targets from the origin, and enforces a
//! Content Security Policy that does not permit the rewritten resources.
//!
//! Two rules apply:
//!
//! - **Navigation and preload headers** (`Location`, `Content-Location`,
//!   `Refresh`, `Link`): an absolute or protocol-relative URL whose authority is
//!   the publisher origin is mapped to the serving host, using the same
//!   scheme mapping as the HTML body rewriter. Relative URLs and other hosts
//!   are left unchanged.
//! - **Content Security Policy** (`Content-Security-Policy`,
//!   `Content-Security-Policy-Report-Only`): every source expression that names
//!   the publisher origin gains a sibling expression for the serving host.
//!   Nothing is removed, so resources the origin still serves directly remain
//!   permitted. Host changes cannot authorize nonce-less inline inserts; that
//!   is out of scope here.
//!
//! Each response's own header values are rewritten, so per-page policies are
//! preserved. Operator `[response_headers]` overrides are applied later and
//! still take precedence.

use edgezero_core::http::{HeaderMap, HeaderName, HeaderValue, header};
use url::Url;

/// Headers whose value is a Content Security Policy.
const CSP_HEADERS: &[HeaderName] = &[
    header::CONTENT_SECURITY_POLICY,
    header::CONTENT_SECURITY_POLICY_REPORT_ONLY,
];

/// CSP directives whose values are report endpoints, not source lists.
///
/// Adding a serving-host endpoint would duplicate every violation report.
const CSP_NON_SOURCE_DIRECTIVES: &[&str] = &["report-uri", "report-to"];

/// The origin-to-serving-host mapping applied to response headers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OriginHeaderRewrite<'a> {
    /// Publisher origin authority (`host` or `host:port`).
    pub origin_host: &'a str,
    /// Serving host authority the browser addressed.
    pub request_host: &'a str,
    /// Scheme the browser used to reach the serving host.
    pub request_scheme: &'a str,
    /// Scheme Trusted Server uses to fetch the publisher origin.
    pub origin_scheme: &'a str,
    /// Path and query of the request being answered (`/page?x=1`).
    ///
    /// An origin redirect to this URL under a different scheme than
    /// [`Self::origin_scheme`] (for example, forcing HTTPS) is left unchanged.
    /// Rewriting it would send the browser back to the URL it just requested,
    /// Trusted Server would refetch the origin over the same scheme, and the
    /// origin would redirect again.
    pub request_path_and_query: &'a str,
    /// Whether the request being answered is a `GET` or `HEAD`.
    ///
    /// A same-scheme `Location` to the current URL is left unchanged for these
    /// methods when the response sets no cookie. The retry through Trusted
    /// Server would repeat the same request, so the origin would redirect again.
    pub request_is_get_or_head: bool,
}

impl OriginHeaderRewrite<'_> {
    fn is_active(&self) -> bool {
        !self.origin_host.is_empty()
            && !self.request_host.is_empty()
            && !self.request_scheme.is_empty()
            && !self.origin_host.eq_ignore_ascii_case(self.request_host)
    }

    /// Rewrite a URL reference that points at the publisher origin.
    ///
    /// Returns [`None`] when `url` is relative, names another host, or names the
    /// origin host with a different port.
    fn rewrite_url(&self, url: &str) -> Option<String> {
        let rest = strip_prefix_ignore_ascii_case(url, "https://")
            .or_else(|| strip_prefix_ignore_ascii_case(url, "http://"));
        if let Some(rest) = rest {
            let suffix = self.strip_origin_authority(rest)?;
            return Some(format!(
                "{}://{}{suffix}",
                self.request_scheme, self.request_host
            ));
        }

        let suffix = self.strip_origin_authority(url.strip_prefix("//")?)?;
        Some(format!("//{}{suffix}", self.request_host))
    }

    /// Rewrite a navigation target (`Location`, `Refresh`), unless rewriting
    /// it would loop.
    ///
    /// A target equal to the current request under a different scheme always
    /// loops, because Trusted Server refetches the origin over the same scheme.
    /// A same-scheme target equal to the current request (POST-redirect-GET, a
    /// cookie-setting redirect, a periodic reload) depends on state and is
    /// rewritten here; [`Self::rewrite_location`] adds the stateless case.
    ///
    /// `Set-Cookie` `Domain` attributes are not rewritten. A cookie-setting
    /// redirect to the current URL whose cookie names a `Domain` the serving
    /// host is not within is rejected by the browser, so the redirect repeats
    /// until the browser stops it.
    fn rewrite_navigation_url(&self, url: &str) -> Option<String> {
        let rewritten = self.rewrite_url(url)?;
        if self.is_scheme_change_to_current_request(url) {
            log::warn!(
                "Keeping origin scheme-change redirect to the current request URL; the \
                 browser leaves the serving host. Check that `publisher.origin_url` uses \
                 the scheme the origin enforces"
            );
            return None;
        }
        Some(rewritten)
    }

    /// Rewrite a `Location` target, unless rewriting it would loop.
    ///
    /// Beyond [`Self::rewrite_navigation_url`], a same-scheme redirect of a
    /// `GET` or `HEAD` to the current URL is left unchanged when the response
    /// sets no cookie. Nothing the browser sends on the retry would differ, so
    /// the origin is redirecting because of something Trusted Server sends
    /// unchanged, such as `publisher.origin_host_header_override`.
    fn rewrite_location(&self, url: &str, response_sets_cookie: bool) -> Option<String> {
        let rewritten = self.rewrite_navigation_url(url)?;
        if self.request_is_get_or_head && !response_sets_cookie && self.targets_current_request(url)
        {
            log::warn!(
                "Keeping origin redirect to the current request URL; the browser leaves \
                 the serving host. Check what the origin expects from the request, such \
                 as `publisher.origin_host_header_override`"
            );
            return None;
        }
        Some(rewritten)
    }

    fn is_scheme_change_to_current_request(&self, url: &str) -> bool {
        let Some((scheme, rest)) = url.split_once("://") else {
            return false;
        };
        if scheme.eq_ignore_ascii_case(self.origin_scheme) {
            return false;
        }
        if self.strip_origin_authority(rest).is_none() {
            return false;
        }
        self.targets_current_request(url)
    }

    /// Whether `url`, an absolute or protocol-relative URL on the origin,
    /// names the path and query of the current request.
    fn targets_current_request(&self, url: &str) -> bool {
        // Normalize the target the way the browser will before following it:
        // an empty path becomes `/`, dot segments are resolved, and the
        // fragment is dropped.
        let parsed = if url.starts_with("//") {
            Url::parse(&format!("{}:{url}", self.origin_scheme))
        } else {
            Url::parse(url)
        };
        let Ok(target) = parsed else {
            return false;
        };
        let (request_path, request_query) = match self.request_path_and_query.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (self.request_path_and_query, None),
        };
        target.path() == request_path && target.query() == request_query
    }

    /// Strip the origin authority from the start of `rest`, returning the
    /// path, query, and fragment that follow it.
    fn strip_origin_authority<'b>(&self, rest: &'b str) -> Option<&'b str> {
        let suffix = strip_prefix_ignore_ascii_case(rest, self.origin_host)?;
        let at_boundary = suffix.is_empty() || matches!(suffix.as_bytes()[0], b'/' | b'?' | b'#');
        at_boundary.then_some(suffix)
    }

    /// Build the serving-host CSP source for a source expression naming the
    /// origin, or [`None`] when the expression names something else.
    fn csp_source_for(&self, source: &str) -> Option<String> {
        if source.starts_with('\'') {
            return None;
        }
        if source.contains("://") {
            return self.rewrite_url(source);
        }
        let suffix = self.strip_origin_authority(source)?;
        Some(format!("{}{suffix}", self.request_host))
    }
}

/// Rewrite publisher-origin URLs in navigation, preload, and CSP headers.
///
/// Values that are not valid UTF-8, or whose rewrite would not be a valid
/// header value, are left unchanged. Multiple values of the same header keep
/// their order.
///
/// # Examples
///
/// ```ignore
/// let rewrite = OriginHeaderRewrite {
///     origin_host: "origin.example.com",
///     request_host: "www.example.com",
///     request_scheme: "https",
///     origin_scheme: "https",
///     request_path_and_query: "/page",
///     request_is_get_or_head: true,
/// };
/// rewrite_origin_urls_in_headers(response.headers_mut(), &rewrite);
/// ```
pub(crate) fn rewrite_origin_urls_in_headers(
    headers: &mut HeaderMap,
    rewrite: &OriginHeaderRewrite<'_>,
) {
    if !rewrite.is_active() {
        return;
    }

    let response_sets_cookie = headers.contains_key(header::SET_COOKIE);
    rewrite_header_values(headers, &header::LOCATION, |value| {
        rewrite.rewrite_location(value.trim(), response_sets_cookie)
    });
    rewrite_header_values(headers, &header::CONTENT_LOCATION, |value| {
        rewrite.rewrite_url(value.trim())
    });
    rewrite_header_values(headers, &header::REFRESH, |value| {
        rewrite_refresh(value, rewrite)
    });
    rewrite_header_values(headers, &header::LINK, |value| rewrite_link(value, rewrite));
    for name in CSP_HEADERS {
        rewrite_header_values(headers, name, |value| rewrite_csp(value, rewrite));
    }
}

fn rewrite_header_values(
    headers: &mut HeaderMap,
    name: &HeaderName,
    rewrite_value: impl Fn(&str) -> Option<String>,
) {
    let mut changed = false;
    let values: Vec<HeaderValue> = headers
        .get_all(name)
        .iter()
        .map(|value| {
            let rewritten = value
                .to_str()
                .ok()
                .and_then(&rewrite_value)
                .and_then(|rewritten| HeaderValue::from_str(&rewritten).ok());
            match rewritten {
                Some(rewritten) => {
                    changed = true;
                    rewritten
                }
                None => value.clone(),
            }
        })
        .collect();

    if !changed {
        return;
    }

    log::debug!("Rewriting publisher origin URLs in `{name}` response header");
    headers.remove(name);
    for value in values {
        headers.append(name.clone(), value);
    }
}

/// Rewrite the URL in a `Refresh` value such as `0; url=https://origin/next`.
///
/// Accepts the forms browsers parse: whitespace or a `;` or `,` separator, an optional
/// case-insensitive `url=` prefix, and an optionally quoted URL.
fn rewrite_refresh(value: &str, rewrite: &OriginHeaderRewrite<'_>) -> Option<String> {
    let bytes = value.as_bytes();
    let skip_whitespace = |mut index: usize| {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        index
    };

    let time_start = skip_whitespace(0);
    let mut index = time_start;
    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
        index += 1;
    }
    if index == time_start {
        return None;
    }
    if !matches!(bytes.get(index), Some(b';' | b','))
        && !bytes.get(index).is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    index = skip_whitespace(index);
    if matches!(bytes.get(index), Some(b';' | b',')) {
        index += 1;
    }
    index = skip_whitespace(index);

    if strip_prefix_ignore_ascii_case(&value[index..], "url").is_some() {
        let after_url = skip_whitespace(index + 3);
        if bytes.get(after_url) == Some(&b'=') {
            index = skip_whitespace(after_url + 1);
        }
    }

    let (start, end) = match bytes.get(index) {
        Some(&quote @ (b'"' | b'\'')) => {
            let start = index + 1;
            let end = value[start..]
                .find(char::from(quote))
                .map_or(value.len(), |offset| start + offset);
            (start, end)
        }
        Some(_) => (index, value.trim_end().len()),
        None => return None,
    };

    let rewritten = rewrite.rewrite_navigation_url(&value[start..end])?;
    Some(format!("{}{rewritten}{}", &value[..start], &value[end..]))
}

/// Rewrite every `<uri-reference>` in a `Link` value, and the candidate URLs
/// of quoted `imagesrcset` parameters.
///
/// `<` inside a quoted parameter value is not treated as a link target.
fn rewrite_link(value: &str, rewrite: &OriginHeaderRewrite<'_>) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = String::new();
    let mut copied_until = 0;
    let mut index = 0;
    let mut in_quotes = false;
    let mut changed = false;

    while index < bytes.len() {
        match bytes[index] {
            b'\\' if in_quotes => index += 1,
            b'"' if !in_quotes && is_imagesrcset_value(&value[..index]) => {
                let start = index + 1;
                let end = quoted_string_end(bytes, start);
                if let Some(rewritten) = rewrite_srcset(&value[start..end], rewrite) {
                    out.push_str(&value[copied_until..start]);
                    out.push_str(&rewritten);
                    copied_until = end;
                    changed = true;
                }
                index = end;
            }
            b'"' => in_quotes = !in_quotes,
            b'<' if !in_quotes => {
                let start = index + 1;
                let Some(offset) = value[start..].find('>') else {
                    break;
                };
                let end = start + offset;
                if let Some(rewritten) = rewrite.rewrite_url(value[start..end].trim()) {
                    out.push_str(&value[copied_until..start]);
                    out.push_str(&rewritten);
                    copied_until = end;
                    changed = true;
                }
                index = end;
            }
            _ => {}
        }
        index += 1;
    }

    if !changed {
        return None;
    }
    out.push_str(&value[copied_until..]);
    Some(out)
}

/// Whether the text before an opening quote ends with `imagesrcset=`.
fn is_imagesrcset_value(before_quote: &str) -> bool {
    before_quote
        .trim_end()
        .strip_suffix('=')
        .map(str::trim_end)
        .and_then(|name| {
            name.len()
                .checked_sub("imagesrcset".len())
                .map(|start| &name[start..])
        })
        .is_some_and(|name| name.eq_ignore_ascii_case("imagesrcset"))
}

/// Index of the closing quote of a quoted string whose content starts at
/// `start`, or the end of `bytes` when it is unterminated.
fn quoted_string_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 1,
            b'"' => return index,
            _ => {}
        }
        index += 1;
    }
    bytes.len()
}

/// Rewrite the URL of each comma-separated `srcset` candidate, keeping its
/// descriptor and spacing.
fn rewrite_srcset(srcset: &str, rewrite: &OriginHeaderRewrite<'_>) -> Option<String> {
    let mut changed = false;
    let candidates: Vec<String> = srcset
        .split(',')
        .map(|candidate| {
            let leading = candidate.len() - candidate.trim_start().len();
            let url_end = candidate[leading..]
                .find(|c: char| c.is_ascii_whitespace())
                .map_or(candidate.len(), |offset| leading + offset);
            match rewrite.rewrite_url(&candidate[leading..url_end]) {
                Some(rewritten) => {
                    changed = true;
                    format!(
                        "{}{rewritten}{}",
                        &candidate[..leading],
                        &candidate[url_end..]
                    )
                }
                None => candidate.to_string(),
            }
        })
        .collect();
    changed.then(|| candidates.join(","))
}

/// Add serving-host sources next to every source expression naming the origin.
///
/// Handles comma-separated policy lists and `;`-separated directives. A
/// serving-host source already present in the directive is not duplicated.
fn rewrite_csp(value: &str, rewrite: &OriginHeaderRewrite<'_>) -> Option<String> {
    let mut insertions: Vec<(usize, String)> = Vec::new();

    let mut directive_start = 0;
    for directive in value.split([';', ',']) {
        let offset = directive_start;
        directive_start += directive.len() + 1;

        let mut tokens = tokens_with_offsets(directive);
        let Some((_, name)) = tokens.next() else {
            continue;
        };
        if CSP_NON_SOURCE_DIRECTIVES
            .iter()
            .any(|skipped| name.eq_ignore_ascii_case(skipped))
        {
            continue;
        }

        let sources: Vec<(usize, &str)> = tokens.collect();
        let directive_insertions_start = insertions.len();
        for (token_offset, source) in &sources {
            let Some(added) = rewrite.csp_source_for(source) else {
                continue;
            };
            let already_present = sources
                .iter()
                .map(|(_, existing)| *existing)
                .chain(
                    insertions[directive_insertions_start..]
                        .iter()
                        .map(|(_, queued)| queued.as_str()),
                )
                .any(|existing| csp_sources_equal(existing, &added));
            if !already_present {
                insertions.push((offset + token_offset + source.len(), added));
            }
        }
    }

    if insertions.is_empty() {
        return None;
    }

    let extra: usize = insertions.iter().map(|(_, added)| added.len() + 1).sum();
    let mut out = String::with_capacity(value.len() + extra);
    let mut copied_until = 0;
    for (position, added) in insertions {
        out.push_str(&value[copied_until..position]);
        out.push(' ');
        out.push_str(&added);
        copied_until = position;
    }
    out.push_str(&value[copied_until..]);
    Some(out)
}

/// Whether two CSP source expressions are the same source.
///
/// Scheme and host compare case-insensitively; the path is case-sensitive, as
/// in CSP path matching, so `/A.js` and `/a.js` stay distinct.
fn csp_sources_equal(left: &str, right: &str) -> bool {
    let (left_authority, left_path) = split_csp_source_path(left);
    let (right_authority, right_path) = split_csp_source_path(right);
    left_authority.eq_ignore_ascii_case(right_authority) && left_path == right_path
}

/// Split a CSP source expression into its scheme-and-host part and its path.
fn split_csp_source_path(source: &str) -> (&str, &str) {
    let authority_start = source.find("://").map_or(0, |index| index + 3);
    source[authority_start..]
        .find('/')
        .map_or((source, ""), |offset| {
            source.split_at(authority_start + offset)
        })
}

fn tokens_with_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split(|c: char| c.is_ascii_whitespace())
        .filter(|token| !token.is_empty())
        .map(move |token| (token.as_ptr() as usize - text.as_ptr() as usize, token))
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const REWRITE: OriginHeaderRewrite<'static> = OriginHeaderRewrite {
        origin_host: "origin.example.com",
        request_host: "www.example.com",
        request_scheme: "https",
        origin_scheme: "http",
        request_path_and_query: "/current?page=1",
        request_is_get_or_head: false,
    };

    fn rewritten(name: &HeaderName, values: &[&str]) -> Vec<String> {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                name.clone(),
                HeaderValue::from_str(value).expect("should build test header value"),
            );
        }
        rewrite_origin_urls_in_headers(&mut headers, &REWRITE);
        headers
            .get_all(name)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .expect("should keep header value ascii")
                    .to_string()
            })
            .collect()
    }

    fn single(name: &HeaderName, value: &str) -> String {
        rewritten(name, &[value]).remove(0)
    }

    #[test]
    fn location_rewrites_absolute_origin_urls() {
        assert_eq!(
            single(&header::LOCATION, "http://origin.example.com/landing.html"),
            "https://www.example.com/landing.html",
            "should map an http origin redirect to the serving host"
        );
        assert_eq!(
            single(&header::LOCATION, "HTTPS://Origin.Example.com?next=1#top"),
            "https://www.example.com?next=1#top",
            "should match scheme and host case-insensitively"
        );
        assert_eq!(
            single(&header::LOCATION, "https://origin.example.com"),
            "https://www.example.com",
            "should rewrite a bare origin URL"
        );
        assert_eq!(
            single(&header::CONTENT_LOCATION, "//origin.example.com/canonical"),
            "//www.example.com/canonical",
            "should keep protocol-relative URLs protocol-relative"
        );
    }

    #[test]
    fn location_leaves_other_targets_unchanged() {
        for value in [
            "/relative/path",
            "landing.html",
            "https://other.example.com/landing.html",
            "https://cdn.origin.example.com/landing.html",
            "https://origin.example.com.example.net/landing.html",
            "https://origin.example.com:8443/landing.html",
            "https://other.example.com/?return=https://origin.example.com/",
            "ftp://origin.example.com/file",
        ] {
            assert_eq!(
                single(&header::LOCATION, value),
                value,
                "should not rewrite `{value}`"
            );
        }
    }

    #[test]
    fn origin_with_port_requires_matching_port() {
        let rewrite = OriginHeaderRewrite {
            origin_host: "127.0.0.1:8301",
            request_host: "127.0.0.1:3031",
            request_scheme: "http",
            origin_scheme: "http",
            request_path_and_query: "/",
            request_is_get_or_head: false,
        };
        assert_eq!(
            rewrite.rewrite_url("http://127.0.0.1:8301/landing.html"),
            Some("http://127.0.0.1:3031/landing.html".to_string()),
            "should map the origin authority including its port"
        );
        assert_eq!(
            rewrite.rewrite_url("http://127.0.0.1:83010/landing.html"),
            None,
            "should not match a longer port"
        );
        assert_eq!(
            rewrite.rewrite_url("http://127.0.0.1/landing.html"),
            None,
            "should not match the host without the origin port"
        );
    }

    #[test]
    fn navigation_headers_keep_scheme_change_to_current_request() {
        for value in [
            "https://origin.example.com/current?page=1",
            "HTTPS://origin.example.com/current?page=1#top",
        ] {
            assert_eq!(
                single(&header::LOCATION, value),
                value,
                "should keep scheme-change redirect `{value}` to avoid a loop"
            );
        }
        assert_eq!(
            single(
                &header::REFRESH,
                "0; url=https://origin.example.com/current?page=1"
            ),
            "0; url=https://origin.example.com/current?page=1",
            "should keep a scheme-change refresh to the same URL"
        );
    }

    #[test]
    fn navigation_headers_rewrite_same_scheme_targets_to_current_request() {
        assert_eq!(
            single(
                &header::LOCATION,
                "http://origin.example.com/current?page=1"
            ),
            "https://www.example.com/current?page=1",
            "should rewrite a POST-redirect-GET to the same URL"
        );
        assert_eq!(
            single(&header::LOCATION, "//origin.example.com/current?page=1"),
            "//www.example.com/current?page=1",
            "should rewrite a protocol-relative self target"
        );
        assert_eq!(
            single(
                &header::REFRESH,
                "60; url=http://origin.example.com/current?page=1"
            ),
            "60; url=https://www.example.com/current?page=1",
            "should keep a periodic reload on the serving host"
        );
        assert_eq!(
            single(
                &header::LOCATION,
                "https://origin.example.com/current?page=2"
            ),
            "https://www.example.com/current?page=2",
            "should still rewrite a redirect to a different query"
        );
        assert_eq!(
            single(
                &header::CONTENT_LOCATION,
                "https://origin.example.com/current?page=1"
            ),
            "https://www.example.com/current?page=1",
            "should rewrite non-navigation self references"
        );
    }

    #[test]
    fn location_keeps_stateless_get_redirect_to_current_request() {
        let rewrite = OriginHeaderRewrite {
            request_is_get_or_head: true,
            ..REWRITE
        };
        let location_after_rewrite = |headers: &[(HeaderName, &str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in headers {
                map.append(
                    name.clone(),
                    HeaderValue::from_str(value).expect("should build test header value"),
                );
            }
            rewrite_origin_urls_in_headers(&mut map, &rewrite);
            map.get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
                .expect("should keep the Location header")
        };

        for value in [
            "http://origin.example.com/current?page=1",
            "//origin.example.com/current?page=1",
            "http://origin.example.com/a/../current?page=1#top",
        ] {
            assert_eq!(
                location_after_rewrite(&[(header::LOCATION, value)]),
                value,
                "should keep a cookieless GET redirect `{value}` to the current URL"
            );
        }
        assert_eq!(
            location_after_rewrite(&[
                (header::LOCATION, "http://origin.example.com/current?page=1"),
                (header::SET_COOKIE, "gate=1; Path=/"),
            ]),
            "https://www.example.com/current?page=1",
            "should rewrite a cookie-setting GET redirect to the current URL"
        );
        assert_eq!(
            location_after_rewrite(&[(
                header::LOCATION,
                "http://origin.example.com/current?page=2"
            )]),
            "https://www.example.com/current?page=2",
            "should rewrite a GET redirect to a different URL"
        );
        assert_eq!(
            rewrite_refresh("60; url=http://origin.example.com/current?page=1", &rewrite),
            Some("60; url=https://www.example.com/current?page=1".to_string()),
            "should keep a periodic GET reload on the serving host"
        );
    }

    #[test]
    fn navigation_headers_normalize_scheme_change_targets_before_comparing() {
        let rewrite = OriginHeaderRewrite {
            request_path_and_query: "/?x=1",
            ..REWRITE
        };
        assert_eq!(
            rewrite.rewrite_navigation_url("https://origin.example.com?x=1"),
            None,
            "should treat an empty path before a query as `/`"
        );
        assert_eq!(
            rewrite_refresh("0; url=https://origin.example.com?x=1", &rewrite),
            None,
            "should keep a scheme-change refresh with an empty path"
        );
        for value in [
            "https://origin.example.com/a/../current?page=1",
            "https://origin.example.com/./current?page=1#top",
        ] {
            assert_eq!(
                single(&header::LOCATION, value),
                value,
                "should resolve dot segments in `{value}` before comparing"
            );
        }
        assert_eq!(
            single(
                &header::REFRESH,
                "0; url=https://origin.example.com/a/../current?page=1"
            ),
            "0; url=https://origin.example.com/a/../current?page=1",
            "should resolve dot segments in a refresh target before comparing"
        );
        assert_eq!(
            single(
                &header::LOCATION,
                "https://origin.example.com/a/../other?page=1"
            ),
            "https://www.example.com/a/../other?page=1",
            "should still rewrite a normalized target that differs"
        );
    }

    #[test]
    fn navigation_to_origin_root_is_compared_as_slash() {
        let rewrite = OriginHeaderRewrite {
            request_path_and_query: "/",
            ..REWRITE
        };
        assert_eq!(
            rewrite.rewrite_navigation_url("https://origin.example.com"),
            None,
            "should treat an empty path as `/`"
        );
    }

    #[test]
    fn refresh_rewrites_url_part() {
        assert_eq!(
            single(
                &header::REFRESH,
                "0; url=https://origin.example.com/refreshed.html"
            ),
            "0; url=https://www.example.com/refreshed.html",
            "should rewrite the url= target"
        );
        assert_eq!(
            single(
                &header::REFRESH,
                "5,URL = 'http://origin.example.com/next' "
            ),
            "5,URL = 'https://www.example.com/next' ",
            "should handle comma separator, spacing, case, and quotes"
        );
        assert_eq!(
            single(&header::REFRESH, "1.5; \"//origin.example.com/next\""),
            "1.5; \"//www.example.com/next\"",
            "should handle a quoted URL without url="
        );
        assert_eq!(
            single(&header::REFRESH, "0 url=https://origin.example.com/next"),
            "0 url=https://www.example.com/next",
            "should accept whitespace as the separator"
        );
        assert_eq!(
            single(&header::REFRESH, "3; https://origin.example.com/next"),
            "3; https://www.example.com/next",
            "should handle a bare URL without url="
        );
    }

    #[test]
    fn refresh_leaves_other_values_unchanged() {
        for value in [
            "30",
            "0; url=/relative",
            "0; url=https://other.example.com/next",
            "0x; url=https://origin.example.com/next",
            "; url=https://origin.example.com/next",
        ] {
            assert_eq!(
                single(&header::REFRESH, value),
                value,
                "should not rewrite `{value}`"
            );
        }
    }

    #[test]
    fn link_rewrites_each_origin_target() {
        assert_eq!(
            single(
                &header::LINK,
                "<http://origin.example.com/preload.css>; rel=preload; as=style, \
                 </local.js>; rel=preload; as=script, \
                 <https://cdn.example.net/font.woff2>; rel=preload; as=font, \
                 <//origin.example.com/next>; rel=prefetch"
            ),
            "<https://www.example.com/preload.css>; rel=preload; as=style, \
             </local.js>; rel=preload; as=script, \
             <https://cdn.example.net/font.woff2>; rel=preload; as=font, \
             <//www.example.com/next>; rel=prefetch",
            "should rewrite origin targets and keep params and other targets"
        );
    }

    #[test]
    fn link_rewrites_imagesrcset_candidates() {
        assert_eq!(
            single(
                &header::LINK,
                "<https://origin.example.com/a.jpg>; rel=preload; as=image; \
                 imagesrcset=\"https://origin.example.com/a.jpg 1x, /b.jpg 2x, \
                 https://cdn.example.net/c.jpg 3x\"; imagesizes=\"100vw\""
            ),
            "<https://www.example.com/a.jpg>; rel=preload; as=image; \
             imagesrcset=\"https://www.example.com/a.jpg 1x, /b.jpg 2x, \
             https://cdn.example.net/c.jpg 3x\"; imagesizes=\"100vw\"",
            "should rewrite origin srcset candidates and keep the rest"
        );
    }

    #[test]
    fn link_ignores_angle_brackets_in_quoted_params() {
        let value = "</a.css>; rel=preload; title=\"x <https://origin.example.com/b> \\\" y\"";
        assert_eq!(
            single(&header::LINK, value),
            value,
            "should not treat quoted text as a link target"
        );
    }

    #[test]
    fn link_rewrites_every_value_and_keeps_order() {
        assert_eq!(
            rewritten(
                &header::LINK,
                &[
                    "</one.js>; rel=preload; as=script",
                    "<https://origin.example.com/two.css>; rel=preload; as=style",
                ],
            ),
            vec![
                "</one.js>; rel=preload; as=script".to_string(),
                "<https://www.example.com/two.css>; rel=preload; as=style".to_string(),
            ],
            "should rewrite per value and preserve ordering"
        );
    }

    #[test]
    fn csp_adds_serving_host_beside_origin_sources() {
        assert_eq!(
            single(
                &header::CONTENT_SECURITY_POLICY,
                "default-src http://origin.example.com 'unsafe-inline'; \
                 img-src origin.example.com/images/ data:; script-src 'self'"
            ),
            "default-src http://origin.example.com https://www.example.com 'unsafe-inline'; \
             img-src origin.example.com/images/ www.example.com/images/ data:; script-src 'self'",
            "should add the serving host and keep the origin sources"
        );
    }

    #[test]
    fn csp_adds_one_serving_host_source_for_equivalent_origin_sources() {
        assert_eq!(
            single(
                &header::CONTENT_SECURITY_POLICY,
                "img-src http://origin.example.com https://origin.example.com"
            ),
            "img-src http://origin.example.com https://www.example.com https://origin.example.com",
            "should not add the same serving-host source twice"
        );
    }

    #[test]
    fn csp_keeps_path_case_when_deduplicating() {
        assert_eq!(
            single(
                &header::CONTENT_SECURITY_POLICY,
                "script-src https://origin.example.com/A.js https://origin.example.com/a.js"
            ),
            "script-src https://origin.example.com/A.js https://www.example.com/A.js \
             https://origin.example.com/a.js https://www.example.com/a.js",
            "should add a serving-host source for each path that differs only by case"
        );
        assert_eq!(
            single(
                &header::CONTENT_SECURITY_POLICY,
                "script-src https://origin.example.com/A.js https://WWW.Example.com/a.js \
                 https://origin.example.com/a.js"
            ),
            "script-src https://origin.example.com/A.js https://www.example.com/A.js \
             https://WWW.Example.com/a.js https://origin.example.com/a.js",
            "should match an existing serving-host source by host case-insensitively \
             and by path case-sensitively"
        );
    }

    #[test]
    fn csp_handles_policy_lists_and_report_only() {
        assert_eq!(
            single(
                &header::CONTENT_SECURITY_POLICY_REPORT_ONLY,
                "img-src https://origin.example.com, connect-src https://origin.example.com/api"
            ),
            "img-src https://origin.example.com https://www.example.com, \
             connect-src https://origin.example.com/api https://www.example.com/api",
            "should rewrite each policy in a comma-separated list"
        );
    }

    #[test]
    fn csp_does_not_duplicate_or_touch_unrelated_sources() {
        for value in [
            "default-src https://origin.example.com https://www.example.com",
            "default-src 'self' https://cdn.example.net *.origin.example.com",
            "default-src 'self'; report-uri https://origin.example.com/csp-report",
            "img-src https://origin.example.com:8443",
        ] {
            assert_eq!(
                single(&header::CONTENT_SECURITY_POLICY, value),
                value,
                "should leave `{value}` unchanged"
            );
        }
    }

    #[test]
    fn skips_rewrite_when_hosts_match_or_are_missing() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::LOCATION,
            HeaderValue::from_static("https://origin.example.com/next"),
        );
        for rewrite in [
            OriginHeaderRewrite {
                request_host: "ORIGIN.example.com",
                ..REWRITE
            },
            OriginHeaderRewrite {
                request_host: "",
                ..REWRITE
            },
            OriginHeaderRewrite {
                origin_host: "",
                ..REWRITE
            },
        ] {
            rewrite_origin_urls_in_headers(&mut headers, &rewrite);
            assert_eq!(
                headers.get(header::LOCATION),
                Some(&HeaderValue::from_static("https://origin.example.com/next")),
                "should leave headers untouched for {rewrite:?}"
            );
        }
    }

    #[test]
    fn leaves_unrelated_headers_untouched() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        headers.insert(
            "x-origin",
            HeaderValue::from_static("https://origin.example.com/"),
        );
        let before = headers.clone();
        rewrite_origin_urls_in_headers(&mut headers, &REWRITE);
        assert_eq!(headers, before, "should only rewrite URL-bearing headers");
    }
}
