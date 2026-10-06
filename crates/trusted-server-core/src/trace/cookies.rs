//! Read-only health inspection of runtime-visible Cookie fields.

use edgezero_core::request::{Preservation, RequestIngress};
use http::{HeaderMap, header};

use crate::ec::generation::is_valid_ec_id;
use crate::ec::prebid_eids::parse_prebid_eids_cookie;

use super::types::{
    CookieHealth, InvalidCookieDetail, TraceCookies, UnavailableCookieDetail, ValidCookieDetail,
};

const MAX_COOKIE_HEADER_BYTES: usize = 16 * 1024;
const COOKIE_NAMES: [&str; 4] = ["ts-ec", "ts-eids", "ts-tester", "__Host-ts-console"];
const VALUE_LIMITS: [usize; 4] = [512, 8 * 1024, 16, 16];

/// Freeze redacted health from complete runtime-visible Cookie fields.
///
/// Aggregate size, actual UTF-8, then preservation ambiguity take precedence
/// over cookie-specific results. Missing fidelity does not reject readable
/// marker-free fields. This never mutates headers or accesses runtime services.
///
/// # Examples
///
/// ```
/// use http::{HeaderMap, HeaderValue, header};
/// let mut headers = HeaderMap::new();
/// headers.insert(header::COOKIE, HeaderValue::from_static("__Host-ts-console=1"));
/// let cookies = trusted_server_core::trace::inspect_cookies(&headers, None);
/// assert!(cookies.observed_active());
/// ```
///
/// # Performance
///
/// Inspection is linear in visible bytes, bounded to 16 KiB before parsing.
/// Only the existing bounded EID parser allocates while validating values.
#[must_use]
pub fn inspect_cookies(headers: &HeaderMap, ingress: Option<&RequestIngress>) -> TraceCookies {
    let fields = headers.get_all(header::COOKIE);
    let total = fields.iter().fold(0usize, |total, value| {
        total.saturating_add(value.as_bytes().len())
    });
    if total > MAX_COOKIE_HEADER_BYTES {
        return unavailable(UnavailableCookieDetail::HeaderTooLarge);
    }
    if fields
        .iter()
        .any(|value| core::str::from_utf8(value.as_bytes()).is_err())
    {
        return unavailable(UnavailableCookieDetail::HeaderNotUtf8);
    }
    let fidelity = ingress.map(|ingress| ingress.header_fidelity(&header::COOKIE));
    let octets_preserved =
        fidelity.is_some_and(|fidelity| fidelity.octets() == Preservation::Preserved);
    let multiplicity_preserved =
        fidelity.is_some_and(|fidelity| fidelity.field_multiplicity() == Preservation::Preserved);
    let text = || {
        fields
            .iter()
            .filter_map(|value| core::str::from_utf8(value.as_bytes()).ok())
    };
    if text().any(|value| {
        (!octets_preserved && value.contains('\u{fffd}'))
            || (!multiplicity_preserved && value.contains(','))
    }) {
        return unavailable(UnavailableCookieDetail::RuntimeHeaderAmbiguous);
    }
    let mut counts = [0usize; 4];
    let mut first_values = [None; 4];
    for field in text() {
        for segment in field.split(';') {
            let segment = segment.trim_matches(|character: char| character.is_ascii_whitespace());
            let (name, value) = match segment.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (
                    segment.split_ascii_whitespace().next().unwrap_or_default(),
                    None,
                ),
            };
            let Some(index) = COOKIE_NAMES.iter().position(|reserved| *reserved == name) else {
                continue;
            };
            if counts[index] == 0 {
                first_values[index] = value;
            }
            counts[index] += 1;
        }
    }
    TraceCookies::new(std::array::from_fn(|index| match counts[index] {
        0 => CookieHealth::absent(),
        1 => validate_value(index, first_values[index]),
        _ => CookieHealth::duplicate(),
    }))
}

fn unavailable(detail: UnavailableCookieDetail) -> TraceCookies {
    TraceCookies::new([CookieHealth::unavailable(detail); 4])
}

fn validate_value(index: usize, value: Option<&str>) -> CookieHealth {
    let Some(value) = value else {
        return CookieHealth::invalid(InvalidCookieDetail::Malformed);
    };
    if value.len() > VALUE_LIMITS[index] {
        return CookieHealth::invalid(InvalidCookieDetail::Oversized);
    }
    let (valid, detail, invalid) = match index {
        0 => (
            is_valid_ec_id(value),
            ValidCookieDetail::EcFormat,
            InvalidCookieDetail::Malformed,
        ),
        1 => (
            parse_prebid_eids_cookie(value).is_ok(),
            ValidCookieDetail::EidsFormat,
            InvalidCookieDetail::Malformed,
        ),
        2 => (
            value == "true",
            ValidCookieDetail::TesterValue,
            InvalidCookieDetail::UnsupportedValue,
        ),
        _ => (
            value == "1",
            ValidCookieDetail::DiagnosticsValue,
            InvalidCookieDetail::UnsupportedValue,
        ),
    };
    if valid {
        CookieHealth::valid(detail)
    } else {
        CookieHealth::invalid(invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use edgezero_core::request::{
        CapturedTarget, HeaderFidelity, Preservation, RequestIngress, TargetUnavailable,
    };
    use http::{HeaderMap, HeaderValue, header};
    use serde_json::{Value, json};

    const RESERVED: [(&str, &str, &str); 4] = [
        ("ts-ec", "ts_ec", "valid_ec_format"),
        ("ts-eids", "ts_eids", "valid_eids_format"),
        ("ts-tester", "ts_tester", "valid_tester_value"),
        (
            "__Host-ts-console",
            "diagnostics_session",
            "valid_diagnostics_value",
        ),
    ];

    fn headers(values: &[&[u8]]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                header::COOKIE,
                HeaderValue::from_bytes(value).expect("should construct visible Cookie bytes"),
            );
        }
        headers
    }

    fn ingress(
        common: HeaderFidelity,
        overrides: Vec<(http::HeaderName, HeaderFidelity)>,
    ) -> RequestIngress {
        RequestIngress::new(
            CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
            None,
            common,
            overrides,
        )
        .expect("should construct distinct field fidelity overrides")
    }

    fn fidelity(octets: Preservation, multiplicity: Preservation) -> HeaderFidelity {
        HeaderFidelity::new(
            octets,
            multiplicity,
            Preservation::Unavailable,
            Preservation::Transformed,
        )
    }

    fn inspect(headers: &HeaderMap, ingress: Option<&RequestIngress>) -> Value {
        serde_json::to_value(inspect_cookies(headers, ingress))
            .expect("should serialize redacted cookie health")
    }

    fn health(state: &str, detail: Option<&str>) -> Value {
        let mut health = json!({"state": state, "source": "request"});
        if let Some(detail) = detail {
            health["detail"] = json!(detail);
        }
        health
    }

    fn assert_all(actual: &Value, state: &str, detail: Option<&str>) {
        for (_, key, _) in RESERVED {
            assert_eq!(
                actual[key],
                health(state, detail),
                "should give {key} the aggregate or absent health result"
            );
        }
        assert_eq!(
            actual
                .as_object()
                .expect("should serialize an object")
                .len(),
            4,
            "should serialize only the four reserved cookie names"
        );
    }

    fn valid_values() -> [String; 4] {
        [
            format!("{}.aB1234", "a".repeat(64)),
            STANDARD.encode(
                serde_json::to_vec(&json!([
                    {"source": "identity.example", "uids": [{"id": "fictional-private-eid"}]}
                ]))
                .expect("should encode the fictional EID payload"),
            ),
            "true".to_owned(),
            "1".to_owned(),
        ]
    }

    #[test]
    fn valid_reserved_values_use_only_their_named_detail() {
        for ((name, key, detail), value) in RESERVED.into_iter().zip(valid_values()) {
            let value = format!("{name}={value}");
            let headers = headers(&[value.as_bytes()]);
            let before = headers.clone();
            let actual = inspect(&headers, None);
            assert_eq!(
                actual[key],
                health("present_valid", Some(detail)),
                "should use the canonical validator for {name}"
            );
            for (_, other, _) in RESERVED {
                if other != key {
                    assert_eq!(
                        actual[other],
                        health("absent", None),
                        "should not infer another cookie"
                    );
                }
            }
            assert_eq!(
                headers, before,
                "should not sanitize or mutate incoming fields"
            );
            let serialized = actual.to_string();
            assert!(
                !serialized.contains("fictional-private-eid")
                    && !serialized.contains(&"a".repeat(64)),
                "should serialize shape without values or IDs"
            );
        }
    }

    #[test]
    fn observed_active_requires_exactly_one_valid_frozen_session() {
        for (field, expected) in [
            ("__Host-ts-console=1", true),
            ("__Host-ts-console=invalid-example", false),
            ("__Host-ts-console", false),
            ("__Host-ts-console=1; __Host-ts-console=1", false),
            ("__Host-ts-console=1; unrelated=a,b", false),
            ("ts-tester=true", false),
            ("", false),
        ] {
            let cookies = inspect_cookies(&headers(&[field.as_bytes()]), None);
            assert_eq!(
                cookies.observed_active(),
                expected,
                "should derive activity only from frozen session health"
            );
        }
    }

    #[test]
    fn duplicates_win_over_malformed_valid_and_oversized_values() {
        for ((name, key, _), valid) in RESERVED.into_iter().zip(valid_values()) {
            for invalid in [
                "".to_owned(),
                "invalid-example".to_owned(),
                "x".repeat(9000),
            ] {
                let first = format!("{name}={valid}");
                let second = format!("{name}={invalid}");
                for fields in [
                    vec![first.clone(), second.clone()],
                    vec![format!("{first}; {second}")],
                    vec![format!("{name}; {first}")],
                    vec![second.clone(), first.clone()],
                ] {
                    let fields: Vec<&[u8]> = fields.iter().map(String::as_bytes).collect();
                    let actual = inspect(&headers(&fields), None);
                    assert_eq!(
                        actual[key],
                        health("duplicate", Some("multiple_values")),
                        "should count every exact occurrence before validating {name}"
                    );
                }
            }
        }
    }

    #[test]
    fn grammar_counts_malformed_reserved_tokens_and_keeps_values_complete() {
        for (name, key, _) in RESERVED {
            for segment in [name.to_owned(), format!("{name} invalid-example")] {
                let actual = inspect(&headers(&[segment.as_bytes()]), None);
                assert_eq!(
                    actual[key],
                    health("present_invalid", Some("malformed")),
                    "should count no-equals reserved tokens exactly"
                );
            }
            for segment in [
                format!("{name}-extra"),
                format!("{name} =true"),
                name.to_uppercase(),
            ] {
                let actual = inspect(&headers(&[segment.as_bytes()]), None);
                assert_all(&actual, "absent", None);
            }
        }
        let actual = inspect(&headers(&[b" bad pair; =invalid; unrelated==value; ts-tester=true=extra; __Host-ts-console=1=extra"]), None);
        assert_eq!(
            actual["ts_tester"],
            health("present_invalid", Some("unsupported_value")),
            "should retain additional equals in the tester value"
        );
        assert_eq!(
            actual["diagnostics_session"],
            health("present_invalid", Some("unsupported_value")),
            "should retain additional equals in the session value"
        );
        let actual = inspect(
            &headers(&[b"\t ts-tester=true ; __Host-ts-console= 1"]),
            None,
        );
        assert_eq!(
            actual["ts_tester"],
            health("present_valid", Some("valid_tester_value")),
            "should trim only whole-segment ASCII whitespace"
        );
        assert_eq!(
            actual["diagnostics_session"],
            health("present_invalid", Some("unsupported_value")),
            "should not normalize value whitespace after equals"
        );
    }

    #[test]
    fn canonical_rejections_and_visible_per_value_caps_remain_independent() {
        for ((name, key, _), limit) in RESERVED.into_iter().zip([512, 8192, 16, 16]) {
            for (value, detail) in [
                (
                    "x".repeat(limit),
                    if key == "ts_tester" || key == "diagnostics_session" {
                        "unsupported_value"
                    } else {
                        "malformed"
                    },
                ),
                ("x".repeat(limit + 1), "oversized"),
                ("é".repeat(limit / 2 + 1), "oversized"),
            ] {
                let field = format!("{name}={value}");
                let actual = inspect(&headers(&[field.as_bytes()]), None);
                assert_eq!(
                    actual[key],
                    health("present_invalid", Some(detail)),
                    "should enforce the UTF-8 value byte limit for {name}"
                );
                for (_, other, _) in RESERVED {
                    if other != key {
                        assert_eq!(
                            actual[other],
                            health("absent", None),
                            "should keep unrelated health independent"
                        );
                    }
                }
            }
        }
        let uppercase = format!("ts-ec={}.aB1234", "A".repeat(64));
        let actual = inspect(&headers(&[uppercase.as_bytes()]), None);
        assert_eq!(
            actual["ts_ec"],
            health("present_invalid", Some("malformed")),
            "should reuse canonical lowercase EC validation"
        );
    }

    #[test]
    fn visible_aggregate_cap_precedes_actual_utf8_and_ambiguity() {
        for length in [16384, 16385] {
            let field = format!("unrelated={}", "x".repeat(length - "unrelated=".len()));
            let actual = inspect(&headers(&[field.as_bytes()]), None);
            if length == 16384 {
                assert_all(&actual, "absent", None);
            } else {
                assert_all(&actual, "unavailable", Some("header_too_large"));
            }
        }
        let first = "x".repeat(8192);
        let second = "x".repeat(8192);
        assert_all(
            &inspect(&headers(&[first.as_bytes(), second.as_bytes()]), None),
            "absent",
            None,
        );
        assert_all(
            &inspect(&headers(&[first.as_bytes(), second.as_bytes(), b"x"]), None),
            "unavailable",
            Some("header_too_large"),
        );
        let oversized = "x".repeat(16385);
        assert_all(
            &inspect(&headers(&[b"\xff", b",", oversized.as_bytes()]), None),
            "unavailable",
            Some("header_too_large"),
        );
        assert_all(
            &inspect(&headers(&[b"\xff", b",", b"__Host-ts-console=1"]), None),
            "unavailable",
            Some("header_not_utf8"),
        );
    }

    #[test]
    fn all_non_preserved_statuses_and_missing_metadata_allow_marker_free_fields() {
        for status in [
            Preservation::Unknown,
            Preservation::Transformed,
            Preservation::Unavailable,
        ] {
            let metadata = ingress(fidelity(status, status), vec![]);
            for metadata in [None, Some(&metadata)] {
                assert_all(&inspect(&HeaderMap::new(), metadata), "absent", None);
                let actual = inspect(
                    &headers(&["__Host-ts-console=1; unrelated=readable-é".as_bytes()]),
                    metadata,
                );
                assert_eq!(
                    actual["diagnostics_session"],
                    health("present_valid", Some("valid_diagnostics_value")),
                    "should preserve ordinary activation without ambiguity markers"
                );
            }
        }
    }

    #[test]
    fn ambiguity_markers_anywhere_suppress_all_cookie_results_without_comma_splitting() {
        for status in [
            Preservation::Unknown,
            Preservation::Transformed,
            Preservation::Unavailable,
        ] {
            let metadata = ingress(fidelity(status, status), vec![]);
            for field in [
                "__Host-ts-console=1; unrelated=�",
                "__Host-ts-console=1; unrelated=\"�\"",
                "__Host-ts-console=1, __Host-ts-console=1",
                "__Host-ts-console=1; unrelated=\"a,b\"",
                "__Host-ts-console=1; malformed-unrelated,",
            ] {
                for metadata in [None, Some(&metadata)] {
                    assert_all(
                        &inspect(&headers(&[field.as_bytes()]), metadata),
                        "unavailable",
                        Some("runtime_header_ambiguous"),
                    );
                }
            }
        }
    }

    #[test]
    fn cookie_specific_axes_are_independent_and_order_is_not_a_prerequisite() {
        for (octets, multiplicity, field, expected) in [
            (
                Preservation::Preserved,
                Preservation::Unknown,
                "__Host-ts-console=1; unrelated=�",
                "present_valid",
            ),
            (
                Preservation::Unknown,
                Preservation::Preserved,
                "__Host-ts-console=1; unrelated=a,b",
                "present_valid",
            ),
            (
                Preservation::Unknown,
                Preservation::Preserved,
                "__Host-ts-console=1; unrelated=�",
                "unavailable",
            ),
            (
                Preservation::Preserved,
                Preservation::Unknown,
                "__Host-ts-console=1; unrelated=a,b",
                "unavailable",
            ),
        ] {
            let metadata = ingress(
                HeaderFidelity::default(),
                vec![(header::COOKIE, fidelity(octets, multiplicity))],
            );
            let actual = inspect(&headers(&[field.as_bytes()]), Some(&metadata));
            if expected == "unavailable" {
                assert_all(&actual, "unavailable", Some("runtime_header_ambiguous"));
            } else {
                assert_eq!(
                    actual["diagnostics_session"],
                    health("present_valid", Some("valid_diagnostics_value")),
                    "should use only the relevant Cookie preservation axis"
                );
            }
        }
        let preserved = fidelity(Preservation::Preserved, Preservation::Preserved);
        let metadata = ingress(
            preserved,
            vec![(header::CONTENT_TYPE, HeaderFidelity::default())],
        );
        let actual = inspect(
            &headers(&["unrelated=é,�; __Host-ts-console=1".as_bytes()]),
            Some(&metadata),
        );
        assert_eq!(
            actual["diagnostics_session"],
            health("present_valid", Some("valid_diagnostics_value")),
            "should permit preserved unrelated markers and valid other Unicode"
        );
        let metadata = ingress(
            HeaderFidelity::default(),
            vec![(header::CONTENT_TYPE, preserved)],
        );
        assert_all(
            &inspect(
                &headers(&["unrelated=�; __Host-ts-console=1".as_bytes()]),
                Some(&metadata),
            ),
            "unavailable",
            Some("runtime_header_ambiguous"),
        );
    }

    #[test]
    fn preserved_markers_follow_full_value_validation_and_exact_duplicate_counts() {
        let metadata = ingress(
            fidelity(Preservation::Preserved, Preservation::Preserved),
            vec![],
        );
        for (field, detail) in [
            ("__Host-ts-console=1, __Host-ts-console=1", "oversized"),
            ("__Host-ts-console=1,x", "unsupported_value"),
            ("__Host-ts-console=�", "unsupported_value"),
        ] {
            let actual = inspect(&headers(&[field.as_bytes()]), Some(&metadata));
            assert_eq!(
                actual["diagnostics_session"],
                health("present_invalid", Some(detail)),
                "should never comma-split or select a partial session value"
            );
        }
        let actual = inspect(
            &headers(&[b"__Host-ts-console=1", b"__Host-ts-console=1"]),
            Some(&metadata),
        );
        assert_eq!(
            actual["diagnostics_session"],
            health("duplicate", Some("multiple_values")),
            "should count exact preserved fields independently of order fidelity"
        );
        assert_all(
            &inspect(&headers(&[b"unrelated=\xff"]), Some(&metadata)),
            "unavailable",
            Some("header_not_utf8"),
        );
    }
}
