//! Strict request-cookie classification for shared template caching.

use std::collections::{HashMap, HashSet};

use crate::platform::TemplateCookieValue;

/// Whether the request can share a template and its explicit cookie dimensions.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TemplateCookieDecision {
    Bypass,
    Eligible(Vec<TemplateCookieValue>),
}

/// Validates exact cookie names across both configured policies.
///
/// # Errors
///
/// Returns an error naming the field and cookie when a name is invalid or repeated.
pub(crate) fn validate_cookie_names(
    key_names: &[String],
    bypass_names: &[String],
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for (field, names) in [
        ("template_cache_key_cookies", key_names),
        ("template_cache_bypass_cookies", bypass_names),
    ] {
        for name in names {
            if !is_cookie_name(name.as_bytes()) {
                return Err(format!("{field} contains invalid cookie name `{name}`"));
            }
            if !seen.insert(name.as_str()) {
                return Err(format!(
                    "{field} repeats cookie name `{name}` within or across cookie policies"
                ));
            }
        }
    }
    Ok(())
}

/// Classifies all request-cookie fields using validated configuration names.
///
/// Named policies bypass malformed or ambiguous input. Empty policies retain the
/// legacy header-presence rule. Admitted dimensions are sorted by exact name and
/// own only the configured key-cookie values, preserving their raw bytes.
pub(crate) fn evaluate_cookie_policy(
    headers: &http::HeaderMap,
    key_names: &[String],
    bypass_names: &[String],
    independent: bool,
) -> TemplateCookieDecision {
    if key_names.is_empty() && bypass_names.is_empty() {
        return if headers.contains_key(http::header::COOKIE) && !independent {
            TemplateCookieDecision::Bypass
        } else {
            TemplateCookieDecision::Eligible(Vec::new())
        };
    }

    let mut parsed = HashMap::<&[u8], &[u8]>::new();
    for field in headers.get_all(http::header::COOKIE) {
        for raw_pair in field.as_bytes().split(|byte| *byte == b';') {
            let pair = trim_pair(raw_pair);
            let Some(separator) = pair.iter().position(|byte| *byte == b'=') else {
                return TemplateCookieDecision::Bypass;
            };
            let (name, rest) = pair.split_at(separator);
            let value = &rest[1..];
            if !is_cookie_name(name) || !is_cookie_value(value) {
                return TemplateCookieDecision::Bypass;
            }
            if parsed.insert(name, value).is_some() {
                return TemplateCookieDecision::Bypass;
            }
            if bypass_names.iter().any(|item| item.as_bytes() == name) {
                return TemplateCookieDecision::Bypass;
            }
            if !independent && !key_names.iter().any(|item| item.as_bytes() == name) {
                return TemplateCookieDecision::Bypass;
            }
        }
    }

    let mut ordered_names: Vec<&String> = key_names.iter().collect();
    ordered_names.sort_unstable();
    TemplateCookieDecision::Eligible(
        ordered_names
            .into_iter()
            .map(|name| TemplateCookieValue {
                name: name.clone(),
                value: parsed.get(name.as_bytes()).map(|value| value.to_vec()),
            })
            .collect(),
    )
}

fn is_cookie_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(byte))
}

fn trim_pair(mut pair: &[u8]) -> &[u8] {
    while pair
        .first()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        pair = &pair[1..];
    }
    while pair.last().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
        pair = &pair[..pair.len() - 1];
    }
    pair
}

fn is_cookie_value(value: &[u8]) -> bool {
    let payload = if value.first() == Some(&b'"') {
        if value.len() < 2 || value.last() != Some(&b'"') {
            return false;
        }
        &value[1..value.len() - 1]
    } else {
        value
    };
    payload
        .iter()
        .all(|byte| matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header::COOKIE};

    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn headers(fields: &[&[u8]]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for field in fields {
            headers.append(
                COOKIE,
                HeaderValue::from_bytes(field).expect("should accept fixture bytes"),
            );
        }
        headers
    }

    fn dimension(name: &str, value: Option<&[u8]>) -> TemplateCookieValue {
        TemplateCookieValue {
            name: name.to_owned(),
            value: value.map(<[u8]>::to_vec),
        }
    }

    #[test]
    fn template_cookie_policy_validates_names() {
        for (key, bypass, valid) in [
            (vec![], vec![], true),
            (vec!["AZaz09!#$%&'*+-.^_`|~"], vec!["session"], true),
            (vec!["session"], vec!["Session"], true),
            (vec!["ab_bucket", "ab_bucket"], vec![], false),
            (vec![], vec!["session", "session"], false),
            (vec!["session"], vec!["session"], false),
        ] {
            assert_eq!(
                validate_cookie_names(&names(&key), &names(&bypass)).is_ok(),
                valid,
                "should validate exact names and list uniqueness"
            );
        }
        for invalid in [
            "", "a b", " a", "a ", "a=b", "a;b", "a,b", "a:b", "a/b", "a\\b", "a\"b", "a\tb",
            "a\nb", "é",
        ] {
            for (key, bypass, field) in [
                (vec![invalid], vec![], "template_cache_key_cookies"),
                (vec![], vec![invalid], "template_cache_bypass_cookies"),
            ] {
                let error = validate_cookie_names(&names(&key), &names(&bypass))
                    .expect_err("should reject invalid cookie names");
                assert!(
                    error.contains(field) && error.contains(invalid),
                    "should identify the invalid configuration field and name"
                );
            }
        }
    }

    #[test]
    fn template_cookie_policy_legacy_boolean_matrix() {
        for fields in [
            vec![],
            vec![b"".as_slice()],
            vec![b"broken".as_slice()],
            vec![b"a=1; a=2".as_slice()],
            vec![b"a=\xff".as_slice()],
            vec![b"a=1".as_slice(), b"".as_slice()],
        ] {
            for independent in [false, true] {
                let expected = if fields.is_empty() || independent {
                    TemplateCookieDecision::Eligible(vec![])
                } else {
                    TemplateCookieDecision::Bypass
                };
                assert_eq!(
                    evaluate_cookie_policy(&headers(&fields), &[], &[], independent),
                    expected,
                    "should retain the legacy header-presence decision"
                );
            }
        }
    }

    #[test]
    fn template_cookie_policy_named_decision_matrix() {
        for (key, bypass) in [
            (vec!["ab_bucket"], vec![]),
            (vec![], vec!["session"]),
            (vec!["ab_bucket"], vec!["session"]),
        ] {
            for independent in [false, true] {
                for (field, expected_value, unknown, session) in [
                    (None, None, false, false),
                    (
                        Some("ab_bucket=A"),
                        Some(b"A".as_slice()),
                        key.is_empty(),
                        false,
                    ),
                    (
                        Some("ab_bucket="),
                        Some(b"".as_slice()),
                        key.is_empty(),
                        false,
                    ),
                    (Some("ts-ec=reader"), None, true, false),
                    (Some("session="), None, true, true),
                    (
                        Some("ab_bucket=A; session=login"),
                        Some(b"A".as_slice()),
                        true,
                        true,
                    ),
                    (
                        Some("ab_bucket=A; ts-ec=reader"),
                        Some(b"A".as_slice()),
                        true,
                        false,
                    ),
                    (Some("Ab_bucket=B"), None, true, false),
                    (Some("Session=login"), None, true, false),
                ] {
                    let fields = field
                        .map(|value| vec![value.as_bytes()])
                        .unwrap_or_default();
                    let expected = if (session && !bypass.is_empty()) || (unknown && !independent) {
                        TemplateCookieDecision::Bypass
                    } else {
                        TemplateCookieDecision::Eligible(
                            key.iter()
                                .map(|name| dimension(name, expected_value))
                                .collect(),
                        )
                    };
                    assert_eq!(
                        evaluate_cookie_policy(
                            &headers(&fields),
                            &names(&key),
                            &names(&bypass),
                            independent
                        ),
                        expected,
                        "should apply list membership and scoped independence"
                    );
                }
            }
        }
    }

    #[test]
    fn template_cookie_policy_preserves_values_and_sorts_dimensions() {
        let key = names(&["z", "ab_bucket", "A"]);
        for fields in [
            vec![b"z=a=b%2F; ab_bucket=\"A\"; A=".as_slice()],
            vec![
                b" \tA= \t; z=a=b%2F ".as_slice(),
                b"ab_bucket=\"A\"".as_slice(),
            ],
        ] {
            assert_eq!(
                evaluate_cookie_policy(&headers(&fields), &key, &[], false),
                TemplateCookieDecision::Eligible(vec![
                    dimension("A", Some(b"")),
                    dimension("ab_bucket", Some(b"\"A\"")),
                    dimension("z", Some(b"a=b%2F"))
                ]),
                "should preserve representation and ignore pair order and splitting"
            );
        }
        for value in [b"".as_slice(), b"\"\"", b"!#$%&'()*+-./0123456789:<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{|}~"] {
            let mut field = b"ab_bucket=".to_vec();
            field.extend_from_slice(value);
            assert_eq!(evaluate_cookie_policy(&headers(&[&field]), &names(&["ab_bucket"]), &[], false), TemplateCookieDecision::Eligible(vec![dimension("ab_bucket", Some(value))]), "should accept every cookie-octet and preserve quotes");
        }
    }

    #[test]
    fn template_cookie_policy_rejects_malformed_and_duplicate_fields() {
        for fields in [
            vec![b"".as_slice()],
            vec![b" \t"],
            vec![b"bare"],
            vec![b"=value"],
            vec![b"a=1;"],
            vec![b";a=1"],
            vec![b"a=1;;b=2"],
            vec![b"ab_bucket =A"],
            vec![b"ab_bucket= A"],
            vec![b"a:b=1"],
            vec![b"a=\""],
            vec![b"a=\"x"],
            vec![b"a=x\""],
            vec![b"a=\"x\"y\""],
            vec![b"a=x y"],
            vec![b"a=x\ty"],
            vec![b"a=x,y"],
            vec![b"a=x\\y"],
            vec![b"a=\xff"],
            vec![b"\xff=1"],
            vec![b"a=1; a=1"],
            vec![b"a=1; a=2"],
            vec![b"a=1", b"a=1"],
            vec![b"a=1", b"a=2"],
            vec![b"ab_bucket=A", b""],
            vec![b"ab_bucket=A", b"a=\xff"],
            vec![b"ab_bucket=A", b"session="],
        ] {
            for independent in [false, true] {
                assert_eq!(
                    evaluate_cookie_policy(
                        &headers(&fields),
                        &names(&["ab_bucket"]),
                        &names(&["session"]),
                        independent
                    ),
                    TemplateCookieDecision::Bypass,
                    "should reject ambiguous or malformed cookie input"
                );
            }
            assert_eq!(
                evaluate_cookie_policy(&headers(&fields), &[], &[], true),
                TemplateCookieDecision::Eligible(vec![]),
                "should preserve legacy independent handling for the same malformed input"
            );
        }
    }
}
