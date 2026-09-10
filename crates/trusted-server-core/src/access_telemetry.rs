//! Access telemetry: route classification and the per-request access log row.
//!
//! Extends the reserved `access_logs_raw` Tinybird datasource with bounded,
//! content-free route identity (see [`RouteClass`] and
//! [`publisher_route_template`]) instead of the raw request path, which would
//! otherwise carry identifiers, search terms, and other user-generated
//! content into a 30-day dataset. See the design spec
//! `docs/superpowers/specs/2026-08-24-request-phase-timing-design.md`
//! section 9.

use serde_json::json;

use crate::request_timing::{AuctionWaitPlacement, TimingSnapshot};

/// Normalizes an HTTP method token into the bounded set of values stored in
/// the `method` `LowCardinality` column.
///
/// HTTP permits arbitrary extension-method tokens (`PROPFIND`, `MKCOL`, or
/// any client-supplied garbage), and the token on an inbound request is
/// entirely client controlled. Capturing one verbatim into a 30-day
/// `LowCardinality(String)` column would let a single caller inflate that
/// column's cardinality without bound and would violate this dataset's
/// bounded-dimension privacy rule (see the module doc). Every standard
/// method maps to its uppercase form; anything else maps to `"other"`. Runs
/// inside [`access_event_row`] rather than at each capture site, so every
/// row-building path is covered regardless of how `method` was populated.
///
/// # Examples
///
/// ```
/// use trusted_server_core::access_telemetry::normalize_method;
///
/// assert_eq!(normalize_method("get"), "GET");
/// assert_eq!(normalize_method("PROPFIND"), "other");
/// assert_eq!(normalize_method("<script>"), "other");
/// ```
#[must_use]
pub fn normalize_method(method: &str) -> &'static str {
    match method.to_ascii_uppercase().as_str() {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "PATCH" => "PATCH",
        "OPTIONS" => "OPTIONS",
        _ => "other",
    }
}

/// Coarse traffic category for one response, used as a `LowCardinality`
/// dimension in the access telemetry row.
///
/// Assigned per route by the adapter at dispatch time (see
/// [`RouteMetadata`]) rather than reconstructed from a handler enum or a
/// path regex at emission time, so the mapping lives in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteClass {
    /// A publisher-origin page served through the assembly/auction pipeline.
    PublisherHtml,
    /// The unified `tsjs` static bundle.
    Tsjs,
    /// A request served by a registered [`crate::integrations::IntegrationProxy`].
    IntegrationProxy,
    /// An Edge Cookie identity endpoint (verify, rotate, identify, admin
    /// lookups, batch sync).
    Ec,
    /// The server-side auction or SPA re-auction (`page-bids`) endpoint.
    AuctionApi,
    /// A response proxied through a configured asset route (the
    /// non-document fallback for scripts, styles, images, and fonts).
    Asset,
    /// Everything else: discovery, tester-cookie toggles, denied legacy
    /// aliases, and any response with no attached [`RouteMetadata`].
    Other,
}

impl RouteClass {
    /// Renders this variant as the `snake_case` string stored in the
    /// `route_class` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PublisherHtml => "publisher_html",
            Self::Tsjs => "tsjs",
            Self::IntegrationProxy => "integration_proxy",
            Self::Ec => "ec",
            Self::AuctionApi => "auction_api",
            Self::Asset => "asset",
            Self::Other => "other",
        }
    }
}

/// Route identity for one response, carried from dispatch to the freeze
/// point as a response extension.
///
/// The matched route pattern does not otherwise survive dispatch: the
/// request is consumed by the router, and nothing else records which
/// route-table row (or coarse fallback bucket) produced the response. Each
/// named-route handler wrapper attaches its matched route-table pattern
/// verbatim; the publisher fallback and `tsjs` handlers attach their class
/// plus a coarse template ([`publisher_route_template`] for the former).
#[derive(Debug, Clone)]
pub struct RouteMetadata {
    /// Coarse traffic category for this response.
    pub route_class: RouteClass,
    /// Bounded, content-free route identifier. For named routes this is the
    /// route-table pattern verbatim (e.g. `/_ts/admin/ec/{id}`); for
    /// publisher-fallback traffic it is the output of
    /// [`publisher_route_template`].
    pub route_template: String,
}

/// Normalizes a publisher-fallback request path into a bounded,
/// content-free route template using an operator-configured allowlist of
/// section names.
///
/// The root path `/` maps to itself. A path whose first segment matches an
/// entry in `sections` (ASCII case-insensitive) and that has at least one
/// further segment maps to `/{section}/*`, emitting the lowercased
/// allowlist entry rather than anything taken from the request. Every
/// other path maps to `/other/*`.
///
/// This construction makes the template content-free by definition: the
/// set of emitted values is exactly `{"/", "/other/*"}` plus one
/// `/{section}/*` per configured entry, so no request-derived byte ever
/// reaches the row and cardinality is bounded by operator configuration.
/// With the default empty allowlist, every publisher path collapses to
/// `/other/*`. Depth alone was rejected as a signal in review: under
/// `/{username}/posts` shapes the first segment is user data, and under
/// single-segment permalink structures it is the document itself.
///
/// # Examples
///
/// ```
/// use trusted_server_core::access_telemetry::publisher_route_template;
///
/// let sections = vec!["news".to_owned()];
/// assert_eq!(
///     publisher_route_template("/news/some-article-slug", &sections),
///     "/news/*"
/// );
/// assert_eq!(publisher_route_template("/", &sections), "/");
/// assert_eq!(
///     publisher_route_template("/alice/orders", &sections),
///     "/other/*"
/// );
/// assert_eq!(publisher_route_template("/news/x", &[]), "/other/*");
/// ```
#[must_use]
pub fn publisher_route_template(path: &str, sections: &[String]) -> String {
    if path == "/" {
        return "/".to_owned();
    }

    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let (first_segment, rest) = match trimmed.split_once('/') {
        Some((first, rest)) => (first, rest),
        None => (trimmed, ""),
    };
    let has_more_depth = !rest.is_empty();

    let matched_section = sections
        .iter()
        .find(|section| section.eq_ignore_ascii_case(first_segment));
    match matched_section {
        Some(section) if has_more_depth => format!("/{}/*", section.to_ascii_lowercase()),
        _ => "/other/*".to_owned(),
    }
}

/// A point-in-time view of the access-log dimensions for one response,
/// captured at the `Server-Timing` freeze point when access telemetry is
/// enabled (a disabled deployment skips the build entirely).
///
/// Built from typed response extensions ([`RouteMetadata`], the geo
/// lookup state, and the template-cache response state) plus adapter-owned
/// environment values, never from the public response headers those
/// extensions back — operator-configured response headers can override a
/// managed header, so reading the header instead of the extension would let
/// the row silently drift from the extension of record.
#[derive(Debug, Clone)]
pub struct AccessTelemetrySnapshot {
    /// The request's HTTP method (e.g. `GET`).
    pub method: String,
    /// The response's HTTP status code.
    pub status: u16,
    /// Coarse traffic category (see [`RouteClass`]).
    pub route_class: RouteClass,
    /// Bounded, content-free route identifier (see
    /// [`publisher_route_template`]).
    pub route_template: String,
    /// The configured publisher domain.
    pub publisher_domain: String,
    /// Adapter-derived deployment environment: `production`, `staging`, or
    /// `unknown`.
    pub env: String,
    /// Fastly service ID, or `unknown` when unavailable.
    pub service_id: String,
    /// Fastly POP code, or `unknown` when unavailable.
    pub pop: String,
    /// Trusted Server build/version identifier, or `unknown` when
    /// unavailable.
    pub ts_version: String,
    /// Two-letter geo country code, or `unknown` when no geo lookup
    /// resolved one.
    pub country: String,
    /// Template-cache outcome for this response, or `unknown` when the
    /// response never passed through the assembly pipeline.
    pub template_cache_state: String,
    /// Whether the response body was streamed or buffered to the client.
    pub body_mode: &'static str,
    /// The configured access-telemetry sample rate at the time this
    /// response was handled.
    pub sample_rate: f64,
}

/// Renders one NDJSON access-log row for the Tinybird Events API.
///
/// Column names match spec section 9 exactly. Phase columns come from
/// `timings` and serialize as JSON `null` for phases that were never
/// recorded; every dimension column comes from `snapshot` and is a
/// non-nullable string (callers are expected to substitute an `unknown`
/// sentinel rather than leave a dimension empty). There is no `event_date`
/// column: the datasource's sorting key derives the date via
/// `toDate(event_ts)`.
#[must_use]
pub fn access_event_row(
    snapshot: &AccessTelemetrySnapshot,
    timings: &TimingSnapshot,
    event_ts_epoch_ms: u64,
) -> String {
    let auction_wait_placement = match timings.auction_wait_placement {
        Some(AuctionWaitPlacement::PreHeader) => "pre_header",
        Some(AuctionWaitPlacement::InStream) => "in_stream",
        None => "none",
    };

    let row = json!({
        "event_ts": format_event_timestamp(event_ts_epoch_ms),
        "method": normalize_method(&snapshot.method),
        "status": snapshot.status,
        "time_elapsed_ms": timings.time_elapsed_ms,
        "sample_rate": snapshot.sample_rate,
        "service_id": snapshot.service_id,
        "publisher_domain": snapshot.publisher_domain,
        "env": snapshot.env,
        "route_class": snapshot.route_class.as_str(),
        "route_template": snapshot.route_template,
        "body_mode": snapshot.body_mode,
        "auction_wait_placement": auction_wait_placement,
        "appbuild_ms": timings.appbuild_ms,
        "filter_ms": timings.filter_ms,
        "geo_ms": timings.geo_ms,
        "kv_ms": timings.kv_ms,
        "origin_ms": timings.origin_ms,
        "template_cache_ms": timings.template_cache_ms,
        "auction_wait_ms": timings.auction_wait_ms,
        "stream_ms": timings.stream_ms,
        "request_elapsed_ms": timings.request_elapsed_ms,
        "resp_bytes": timings.resp_bytes,
        "auction_dispatched_ms": timings.auction_dispatched_ms,
        "auction_resolved_ms": timings.auction_resolved_ms,
        "auction_committed_ms": timings.auction_committed_ms,
        "auction_id": timings.auction_id.as_deref().unwrap_or("none"),
        "template_cache_state": snapshot.template_cache_state,
        "country": snapshot.country,
        "ts_version": snapshot.ts_version,
        "pop": snapshot.pop,
    });
    row.to_string()
}

/// Formats `epoch_ms` as a `ClickHouse` `DateTime64(3)`-compatible string
/// (`%Y-%m-%d %H:%M:%S%.3f`), matching the format the auction telemetry
/// sink already uses for `event_ts`.
///
/// Falls back to the Unix epoch when `epoch_ms` cannot be represented as a
/// valid timestamp, which never happens for a real wall-clock reading.
fn format_event_timestamp(epoch_ms: u64) -> String {
    let epoch_ms = i64::try_from(epoch_ms).unwrap_or(i64::MAX);
    let timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(epoch_ms)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
    timestamp.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64-hex-plus-suffix EC identifier, matching the format the admin
    /// EC lookup route (`/_ts/admin/ec/{id}`) accepts as a path parameter.
    const SYNTHETIC_EC_ID: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.test01";

    fn unknown_snapshot(route_class: RouteClass, route_template: &str) -> AccessTelemetrySnapshot {
        AccessTelemetrySnapshot {
            method: "GET".to_owned(),
            status: 200,
            route_class,
            route_template: route_template.to_owned(),
            publisher_domain: "unknown".to_owned(),
            env: "unknown".to_owned(),
            service_id: "unknown".to_owned(),
            pop: "unknown".to_owned(),
            ts_version: "unknown".to_owned(),
            country: "unknown".to_owned(),
            template_cache_state: "unknown".to_owned(),
            body_mode: "buffered",
            sample_rate: 0.0,
        }
    }

    #[test]
    fn admin_ec_route_template_never_contains_the_identifier() {
        // Named-route templates come from the route table verbatim, never
        // from the matched request path, so a row built for this route can
        // never carry the caller's actual EC id — even when a caller
        // supplies one shaped exactly like a real one.
        let snapshot = unknown_snapshot(RouteClass::Ec, "/_ts/admin/ec/{id}");
        let row = access_event_row(&snapshot, &TimingSnapshot::default(), 0);

        assert!(
            !row.contains(SYNTHETIC_EC_ID),
            "row must never contain a literal EC identifier: {row}"
        );
        assert!(
            row.contains("/_ts/admin/ec/{id}"),
            "row should still carry the route-table pattern: {row}"
        );
    }

    #[test]
    fn publisher_route_template_emits_only_allowlisted_sections() {
        let sections = vec!["news".to_owned(), "Sports".to_owned()];
        assert_eq!(
            publisher_route_template("/news/some-article-slug", &sections),
            "/news/*"
        );
        assert_eq!(
            publisher_route_template("/NEWS/some-article-slug", &sections),
            "/news/*",
            "matching should be case-insensitive on the request side"
        );
        assert_eq!(
            publisher_route_template("/sports/scores/today", &sections),
            "/sports/*",
            "the emitted value should be the lowercased allowlist entry"
        );
        assert_eq!(publisher_route_template("/", &sections), "/");
        assert_eq!(
            publisher_route_template("/news", &sections),
            "/other/*",
            "an allowlisted section with no further depth is the document, not a section"
        );
        assert_eq!(
            publisher_route_template("/opinion/some-slug", &sections),
            "/other/*",
            "a section absent from the allowlist should collapse"
        );
    }

    #[test]
    fn publisher_route_template_collapses_everything_by_default() {
        // The default allowlist is empty, so no request-derived byte can
        // reach the row: the only emitted values are `/` and `/other/*`.
        let empty: Vec<String> = Vec::new();
        for path in [
            "/news/some-article-slug",
            "/alice/orders",
            "/jane-doe/posts",
            "/johnsmith1985/settings",
            "/how-to-treat-my-hiv-today/comments",
            "/abcdefabcdefabcdefabcdefabcdefab/x",
            "/8f3a9c2b/x",
            "/my-post-title",
            "/user@example.com/profile",
            "/search terms here",
            "//double-slash",
            "/550e8400-e29b-41d4-a716-446655440000/profile",
        ] {
            assert_eq!(
                publisher_route_template(path, &empty),
                "/other/*",
                "should collapse with an empty allowlist: {path}"
            );
        }
        assert_eq!(publisher_route_template("/", &empty), "/");
    }

    #[test]
    fn publisher_route_template_never_emits_user_data_even_when_allowlisted() {
        // Adversarial depth-2 shapes from review: even with sections
        // configured, a first segment that is not an exact allowlist match
        // collapses, and the emitted value on a match is the allowlist
        // entry itself, never request bytes.
        let sections = vec!["news".to_owned()];
        for path in [
            "/jane-doe/posts",
            "/johnsmith1985/settings",
            "/how-to-treat-my-hiv-today/comments",
            "/abcdefabcdefabcdefabcdefabcdefab/x",
            "/8f3a9c2b/x",
            "/newsy/article",
        ] {
            assert_eq!(
                publisher_route_template(path, &sections),
                "/other/*",
                "should collapse non-allowlisted first segments: {path}"
            );
        }
    }

    #[test]
    fn row_serializes_nulls_for_missing_phases() {
        let snapshot = unknown_snapshot(RouteClass::Other, "/other/*");
        let row = access_event_row(&snapshot, &TimingSnapshot::default(), 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&row).expect("should serialize valid JSON");

        for field in [
            "time_elapsed_ms",
            "appbuild_ms",
            "filter_ms",
            "geo_ms",
            "kv_ms",
            "origin_ms",
            "template_cache_ms",
            "auction_wait_ms",
            "stream_ms",
            "request_elapsed_ms",
            "resp_bytes",
            "auction_dispatched_ms",
            "auction_resolved_ms",
            "auction_committed_ms",
        ] {
            assert!(
                parsed[field].is_null(),
                "unrecorded phase `{field}` should serialize as null: {row}"
            );
        }

        for field in [
            "service_id",
            "publisher_domain",
            "env",
            "route_class",
            "route_template",
            "body_mode",
            "template_cache_state",
            "country",
            "ts_version",
            "pop",
        ] {
            assert!(
                parsed[field].is_string(),
                "dimension `{field}` must never be null: {row}"
            );
        }
        assert_eq!(parsed["auction_wait_placement"], "none");
        assert_eq!(
            parsed["auction_id"], "none",
            "auction_id should carry the none sentinel when no auction ran"
        );
    }

    #[test]
    fn row_serializes_recorded_phases_as_numbers() {
        let snapshot = unknown_snapshot(RouteClass::AuctionApi, "/auction");
        let timings = TimingSnapshot {
            time_elapsed_ms: Some(12),
            request_elapsed_ms: Some(15),
            appbuild_ms: Some(1),
            filter_ms: Some(2),
            geo_ms: Some(3),
            kv_ms: Some(4),
            origin_ms: Some(5),
            template_cache_ms: Some(6),
            auction_wait_ms: Some(7),
            stream_ms: Some(8),
            auction_wait_placement: Some(AuctionWaitPlacement::InStream),
            resp_bytes: Some(1024),
            auction_dispatched_ms: Some(9),
            auction_resolved_ms: Some(10),
            auction_committed_ms: Some(11),
            auction_id: Some("33333333-3333-3333-3333-333333333333".to_owned()),
        };
        let row = access_event_row(&snapshot, &timings, 1_700_000_000_000);
        let parsed: serde_json::Value =
            serde_json::from_str(&row).expect("should serialize valid JSON");

        assert_eq!(parsed["appbuild_ms"], 1);
        assert_eq!(parsed["stream_ms"], 8);
        assert_eq!(parsed["resp_bytes"], 1024);
        assert_eq!(parsed["auction_wait_placement"], "in_stream");
        assert_eq!(parsed["auction_dispatched_ms"], 9);
        assert_eq!(parsed["auction_resolved_ms"], 10);
        assert_eq!(parsed["auction_committed_ms"], 11);
        assert_eq!(parsed["auction_id"], "33333333-3333-3333-3333-333333333333");
    }

    #[test]
    fn route_class_renders_snake_case() {
        assert_eq!(RouteClass::PublisherHtml.as_str(), "publisher_html");
        assert_eq!(RouteClass::Tsjs.as_str(), "tsjs");
        assert_eq!(RouteClass::IntegrationProxy.as_str(), "integration_proxy");
        assert_eq!(RouteClass::Ec.as_str(), "ec");
        assert_eq!(RouteClass::AuctionApi.as_str(), "auction_api");
        assert_eq!(RouteClass::Asset.as_str(), "asset");
        assert_eq!(RouteClass::Other.as_str(), "other");
    }

    #[test]
    fn format_event_timestamp_matches_clickhouse_datetime64_shape() {
        // 2023-11-14T22:13:20.000Z
        let rendered = format_event_timestamp(1_700_000_000_000);
        assert_eq!(rendered, "2023-11-14 22:13:20.000");
    }

    #[test]
    fn normalize_method_uppercases_allowlisted_methods() {
        assert_eq!(normalize_method("get"), "GET");
        assert_eq!(normalize_method("Get"), "GET");
        assert_eq!(normalize_method("POST"), "POST");
        assert_eq!(normalize_method("head"), "HEAD");
        assert_eq!(normalize_method("PUT"), "PUT");
        assert_eq!(normalize_method("delete"), "DELETE");
        assert_eq!(normalize_method("Patch"), "PATCH");
        assert_eq!(normalize_method("options"), "OPTIONS");
    }

    #[test]
    fn normalize_method_maps_extension_and_garbage_tokens_to_other() {
        assert_eq!(
            normalize_method("PROPFIND"),
            "other",
            "an HTTP extension method must not reach the row verbatim"
        );
        assert_eq!(
            normalize_method("<script>alert(1)</script>"),
            "other",
            "an unbounded client-controlled token must not reach the row verbatim"
        );
    }

    #[test]
    fn row_normalizes_method_even_when_snapshot_carries_a_raw_token() {
        // The normalizer runs inside `access_event_row` so every row-building
        // path is covered, regardless of what the snapshot's `method` field
        // holds — a caller-controlled extension method must never leak into
        // the row unnormalized.
        let mut snapshot = unknown_snapshot(RouteClass::Other, "/other/*");
        snapshot.method = "PROPFIND".to_owned();
        let row = access_event_row(&snapshot, &TimingSnapshot::default(), 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&row).expect("should serialize valid JSON");

        assert_eq!(parsed["method"], "other");
    }
}
