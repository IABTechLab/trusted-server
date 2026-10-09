//! Allowlisted, bounded request context projection.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use chrono::{DateTime, Datelike as _, SecondsFormat, Timelike as _, Utc};
use error_stack::Report;

use crate::platform::{ClientInfo, GeoInfo};

use super::types::{TraceCookies, TraceNetwork, TraceRequestContextV1};

/// Bounded failure to represent the supplied capture clock in version one.
#[derive(Debug, derive_more::Display)]
#[display("invalid trace capture clock")]
pub struct ContextProjectionError;

impl core::error::Error for ContextProjectionError {}

/// Project frozen cookie health and trusted read-only network facts.
///
/// Full IPs, fingerprints, city, coordinates, and identity services never enter
/// the returned context. Unsupported optional facts remain absent. Invalid
/// platform strings are omitted with fixed field categories and no values.
///
/// # Errors
///
/// Returns a bounded error for a capture clock outside four-digit UTC years or
/// with leap-second serialization, which the version-one schema does not accept.
///
/// # Examples
///
/// ```
/// use chrono::Utc;
/// use http::HeaderMap;
/// use trusted_server_core::platform::ClientInfo;
/// use trusted_server_core::trace::{inspect_cookies, project_request_context};
/// let cookies = inspect_cookies(&HeaderMap::new(), None);
/// let context = project_request_context(&ClientInfo::default(), None, &cookies, Utc::now())?;
/// assert!(!context.cookies().observed_active());
/// # Ok::<(), error_stack::Report<trusted_server_core::trace::ContextProjectionError>>(())
/// ```
///
/// # Performance
///
/// Projection copies only bounded optional strings and the fixed cookie health.
pub fn project_request_context(
    client: &ClientInfo,
    geo: Option<&GeoInfo>,
    cookies: &TraceCookies,
    captured_at: DateTime<Utc>,
) -> Result<TraceRequestContextV1, Report<ContextProjectionError>> {
    if !(0..=9999).contains(&captured_at.year()) || captured_at.nanosecond() >= 1_000_000_000 {
        return Err(Report::new(ContextProjectionError));
    }
    Ok(TraceRequestContextV1 {
        schema_version: 1,
        captured_at: captured_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        network: TraceNetwork {
            masked_client_ip: client.client_ip.map(mask_ip),
            country: bounded_string(
                geo.map(|geo| geo.country.as_str()),
                2,
                true,
                "trace_network_country_omitted",
            ),
            region: bounded_string(
                geo.and_then(|geo| geo.region.as_deref()),
                32,
                false,
                "trace_network_region_omitted",
            ),
            asn: geo.and_then(|geo| geo.asn),
            tls_protocol: bounded_string(
                client.tls_protocol.as_deref(),
                32,
                false,
                "trace_network_tls_protocol_omitted",
            ),
            tls_cipher: bounded_string(
                client.tls_cipher.as_deref(),
                32,
                false,
                "trace_network_tls_cipher_omitted",
            ),
            edge_hostname: bounded_string(
                client.server_hostname.as_deref(),
                128,
                false,
                "trace_network_edge_hostname_omitted",
            ),
            edge_region: bounded_string(
                client.server_region.as_deref(),
                128,
                false,
                "trace_network_edge_region_omitted",
            ),
        },
        cookies: *cookies,
    })
}

fn mask_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => {
            let [first, second, third, _] = ip.octets();
            format!("{}/24", Ipv4Addr::new(first, second, third, 0))
        }
        IpAddr::V6(ip) => {
            let [first, second, third, ..] = ip.segments();
            format!("{}/48", Ipv6Addr::new(first, second, third, 0, 0, 0, 0, 0))
        }
    }
}

fn bounded_string(
    value: Option<&str>,
    limit: usize,
    ascii: bool,
    category: &'static str,
) -> Option<String> {
    let value = value?;
    if value.len() > limit || (ascii && !value.is_ascii()) || value.chars().any(|character| {
        character.is_control()
            || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }) {
        log::warn!("{category}");
        None
    } else {
        Some(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use chrono::{TimeZone as _, Utc};
    use http::{HeaderMap, HeaderValue, header};
    use serde_json::json;

    use crate::platform::{ClientInfo, GeoInfo};
    use crate::trace::cookies::inspect_cookies;

    fn captured_at() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 5, 12, 34, 56)
            .single()
            .expect("should construct a valid fixed UTC capture clock")
    }

    fn geo() -> GeoInfo {
        GeoInfo {
            city: "fictional-private-city".to_owned(),
            country: "US".to_owned(),
            continent: "fictional-private-continent".to_owned(),
            latitude: 12.345,
            longitude: 56.789,
            metro_code: 123,
            region: Some("CA".to_owned()),
            asn: Some(64512),
        }
    }

    #[test]
    fn trace_context_masks_and_omits_forbidden_fields() {
        let cookies = inspect_cookies(&HeaderMap::new(), None);
        for (ip, mask) in [
            (IpAddr::V4(Ipv4Addr::new(192, 0, 2, 129)), "192.0.2.0/24"),
            (
                IpAddr::V6(
                    "2001:db8:1234:5678::1"
                        .parse::<Ipv6Addr>()
                        .expect("should parse the documentation IPv6 address"),
                ),
                "2001:db8:1234::/48",
            ),
            (IpAddr::V6(Ipv6Addr::LOCALHOST), "::/48"),
        ] {
            let client = ClientInfo {
                client_ip: Some(ip),
                tls_protocol: Some("TLSv1.3".to_owned()),
                tls_cipher: Some("TLS_AES_128_GCM_SHA256".to_owned()),
                tls_ja4: Some("fictional-private-ja4".to_owned()),
                h2_fingerprint: Some("fictional-private-h2".to_owned()),
                server_hostname: Some("edge.example".to_owned()),
                server_region: Some("example-region".to_owned()),
            };
            let context = project_request_context(&client, Some(&geo()), &cookies, captured_at())
                .expect("should project valid read-only request facts");
            let actual = serde_json::to_value(context).expect("should serialize the context");
            assert_eq!(
                actual,
                json!({
                    "schema_version": 1,
                    "captured_at": "2026-10-05T12:34:56.000Z",
                    "network": {
                        "masked_client_ip": mask,
                        "country": "US",
                        "region": "CA",
                        "asn": 64512,
                        "tls_protocol": "TLSv1.3",
                        "tls_cipher": "TLS_AES_128_GCM_SHA256",
                        "edge_hostname": "edge.example",
                        "edge_region": "example-region"
                    },
                    "cookies": cookies
                }),
                "should emit only the exact network and context allowlist"
            );
            let serialized = actual.to_string();
            assert!(
                !serialized.contains("fictional-private")
                    && !serialized.contains("12.345")
                    && !serialized.contains("56.789")
                    && !serialized.contains(&ip.to_string()),
                "should omit full IP, fingerprints, city and coordinates"
            );
        }
    }

    #[test]
    fn missing_optional_facts_stay_absent_without_fallback_metadata() {
        let cookies = inspect_cookies(&HeaderMap::new(), None);
        let actual = serde_json::to_value(
            project_request_context(&ClientInfo::default(), None, &cookies, captured_at())
                .expect("should project unavailable optional facts"),
        )
        .expect("should serialize the context");
        assert_eq!(
            actual["network"],
            json!({}),
            "should not invent platform HTTP, POP, geo or IP fallback facts"
        );
    }

    #[test]
    fn invalid_optional_strings_are_omitted_without_shortening() {
        let cookies = inspect_cookies(&HeaderMap::new(), None);
        for invalid in [
            "x".repeat(129),
            "fictional\nprivate".to_owned(),
            "fictional\u{0085}private".to_owned(),
            "fictional\u{061c}private".to_owned(),
            "fictional\u{202e}private".to_owned(),
            "fictional\u{2069}private".to_owned(),
        ] {
            let client = ClientInfo {
                tls_protocol: Some(invalid.clone()),
                tls_cipher: Some(invalid.clone()),
                server_hostname: Some(invalid.clone()),
                server_region: Some(invalid.clone()),
                ..ClientInfo::default()
            };
            let mut geo = geo();
            geo.country = invalid.clone();
            geo.region = Some(invalid);
            let actual = serde_json::to_value(
                project_request_context(&client, Some(&geo), &cookies, captured_at())
                    .expect("should omit invalid optional fields"),
            )
            .expect("should serialize the context");
            assert_eq!(
                actual["network"],
                json!({"asn": 64512}),
                "should omit failing fields instead of shortening or exposing values"
            );
        }
    }

    #[test]
    fn optional_string_bounds_measure_utf8_bytes() {
        let cookies = inspect_cookies(&HeaderMap::new(), None);
        for oversized in [false, true] {
            let suffix = if oversized { "x" } else { "" };
            let short = format!("{}{suffix}", "é".repeat(16));
            let long = format!("{}{suffix}", "é".repeat(64));
            let client = ClientInfo {
                tls_protocol: Some(short.clone()),
                tls_cipher: Some(short.clone()),
                server_hostname: Some(long.clone()),
                server_region: Some(long.clone()),
                ..ClientInfo::default()
            };
            let mut geo = geo();
            geo.country = if oversized {
                "é".to_owned()
            } else {
                "US".to_owned()
            };
            geo.region = Some(short.clone());
            let actual = serde_json::to_value(
                project_request_context(&client, Some(&geo), &cookies, captured_at())
                    .expect("should apply per-field UTF-8 bounds"),
            )
            .expect("should serialize the context");
            if oversized {
                assert_eq!(
                    actual["network"],
                    json!({"asn": 64512}),
                    "should omit invalid ASCII or oversized UTF-8 fields"
                );
            } else {
                assert_eq!(
                    actual["network"],
                    json!({"asn": 64512, "country": "US", "region": short, "tls_protocol": short, "tls_cipher": short, "edge_hostname": long, "edge_region": long}),
                    "should retain values exactly at their byte limit"
                );
            }
        }
    }

    #[test]
    fn context_uses_frozen_cookie_inspection_after_header_sanitization() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-console=1; __Host-ts-console=invalid-example"),
        );
        let cookies = inspect_cookies(&headers, None);
        headers.remove(header::COOKIE);
        let actual = serde_json::to_value(
            project_request_context(&ClientInfo::default(), None, &cookies, captured_at())
                .expect("should project the frozen health"),
        )
        .expect("should serialize the context");
        assert_eq!(
            actual["cookies"]["diagnostics_session"],
            json!({"source":"request", "state":"duplicate", "detail":"multiple_values"}),
            "should never reread sanitized Cookie fields"
        );
    }

    #[test]
    fn invalid_capture_clock_representation_is_rejected() {
        let cookies = inspect_cookies(&HeaderMap::new(), None);
        for year in [-1, 10000] {
            let clock = Utc
                .with_ymd_and_hms(year, 1, 1, 0, 0, 0)
                .single()
                .expect("should construct a supported chrono extended year");
            assert!(
                project_request_context(&ClientInfo::default(), None, &cookies, clock).is_err(),
                "should reject years outside the exact four-digit UTC schema"
            );
        }
    }

    #[test]
    fn leap_second_clock_is_rejected_before_schema_serialization() {
        let cookies = inspect_cookies(&HeaderMap::new(), None);
        let clock = captured_at()
            .with_second(59)
            .expect("should set the last ordinary second")
            .with_nanosecond(1_000_000_000)
            .expect("should construct chrono's leap-second representation");
        assert!(
            project_request_context(&ClientInfo::default(), None, &cookies, clock).is_err(),
            "should reject second60 rather than serialize an invalid UTC schema timestamp"
        );
    }
}
