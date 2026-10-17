//! What a deployment holds against the reader's own Edge Cookie, at
//! [`DATA_PAGE_PATH`].
//!
//! 1. Only a browser opening the address as a page is answered, which the
//!    browser marks with `Sec-Fetch-Mode: navigate` and
//!    `Sec-Fetch-Dest: document`.
//! 2. The page is sent with `Content-Security-Policy: sandbox`, which gives
//!    the document an origin of its own, so a page that opened it cannot
//!    read it.
//! 3. No identifier is shown.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{HeaderValue, Request, Response, StatusCode, header};
use serde_json::{Value, json};

use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_types::KvEntry;
use crate::ec::module::{AcceptedModules, EdgeCookieModule};
use crate::error::TrustedServerError;
use crate::response_privacy::RequiredHeaders;

use super::{MASK, render_page_with_lede};

/// The page showing what is held against the reader's own Edge Cookie.
pub const DATA_PAGE_PATH: &str = "/_ts/data";

const NOT_OPENED_AS_A_PAGE: &str = "This address shows a reader what is held against their own \
                                    browser, so it answers only a browser opening it as a page. \
                                    Open it from the address bar.\n";

/// `sandbox` gives the document an origin of its own, so a page that opened
/// it cannot read it, and nothing in it runs or loads but its own style.
const CONTENT_SECURITY_POLICY: &str = "sandbox; default-src 'none'; style-src 'unsafe-inline'; \
                                       frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// Answers [`DATA_PAGE_PATH`] for `req`.
///
/// `kv` is the identity graph the deployment keeps, or `None` when it keeps
/// none, and `module` is the Edge Cookie module it selects. The record is the
/// one stored against the identifier in the request's own Edge Cookie, named
/// after `public`, the request's public prefix.
/// Nothing is created or written for the reader.
///
/// # Errors
///
/// When the request's `Cookie` header cannot be read, or the store cannot be
/// read, or what it holds is not a record.
pub fn handle_data(
    kv: Option<&KvIdentityGraph>,
    module: Option<&dyn EdgeCookieModule>,
    public: &crate::prefix::PublicPrefix,
    req: &Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if !is_opened_as_a_page(req) {
        return Ok(protected(
            StatusCode::FORBIDDEN,
            "text/plain; charset=utf-8",
            NOT_OPENED_AS_A_PAGE.to_owned(),
        ));
    }
    let Some(kv) = kv else {
        return Ok(page(&nothing_held(
            "This deployment keeps no record against an Edge Cookie.",
        )));
    };
    let Some(ec_id) = crate::ec::request_cookie_ec(req, public)? else {
        return Ok(page(&nothing_held(
            "The request carried no Edge Cookie, so there is nothing to look up.",
        )));
    };
    let Some(kv_key) = AcceptedModules::active(module).canonical_kv_key(&ec_id) else {
        return Ok(page(&nothing_held(
            "The request's Edge Cookie is not one this deployment issued.",
        )));
    };
    let Some((entry, _generation)) = kv.get(&kv_key)? else {
        return Ok(page(&nothing_held(
            "This deployment holds no record against the request's Edge Cookie. A record \
             made in the last few seconds may not be readable yet.",
        )));
    };
    Ok(page(&held_payload(&entry)))
}

/// Whether `req` is a browser opening the address as a page of its own.
///
/// A browser sends `Sec-Fetch-*` only to a trustworthy origin, so every
/// browser on a plain `http://` host other than localhost fails this.
fn is_opened_as_a_page(req: &Request<EdgeBody>) -> bool {
    let is = |name: &str, expected: &str| {
        req.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case(expected))
    };
    is("sec-fetch-mode", "navigate") && is("sec-fetch-dest", "document")
}

fn nothing_held(why: &str) -> Value {
    json!({
        "held": false,
        "why": why,
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// What the page shows for `entry`, being the record as it is stored with
/// each partner's identifier masked, and the two times in it written out.
fn held_payload(entry: &KvEntry) -> Value {
    let mut record = entry.clone();
    for id in record.ids.values_mut() {
        MASK.clone_into(&mut id.uid);
    }
    json!({
        "held": true,
        "identifier": MASK,
        "created": readable_time(entry.created),
        "consent_updated": readable_time(entry.consent.updated),
        "withdrawn": !entry.consent.ok,
        "record": record,
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// `unix_seconds` as RFC 3339 in UTC, or `None` beyond what a date can hold.
fn readable_time(unix_seconds: u64) -> Option<String> {
    let unix_seconds = i64::try_from(unix_seconds).ok()?;
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn page(payload: &Value) -> Response<EdgeBody> {
    let lede = format!(
        "What this deployment holds against the Edge Cookie your browser sent. Each \
         identifier shows as {MASK}."
    );
    protected(
        StatusCode::OK,
        "text/html; charset=utf-8",
        render_page_with_lede("Your data", &lede, payload),
    )
}

/// A response only the person looking at it can read, being never stored,
/// never framed, in an origin of its own, and offered to no other origin.
/// Its headers are [`RequiredHeaders`], so an operator's own headers of the
/// same names cannot replace them.
fn protected(status: StatusCode, content_type: &'static str, body: String) -> Response<EdgeBody> {
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, private"),
        )
        .header(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        )
        .header(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"))
        .header(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        )
        .header(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        )
        .header(
            "cross-origin-resource-policy",
            HeaderValue::from_static("same-origin"),
        )
        .body(EdgeBody::from(body.into_bytes()))
        .expect("should build the data response");
    let required = response.headers().clone();
    response.extensions_mut().insert(RequiredHeaders(required));
    response
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::consent::ConsentContext;
    use crate::ec::kv_types::{
        KvConsent, KvDevice, KvGeo, KvNetwork, KvPartnerId, KvPubProperties,
    };
    use crate::ec::module::HmacModule;
    use crate::redacted::Redacted;

    /// An identifier in the form the built-in HMAC module issues.
    const EC_ID: &str =
        "hmac~0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.aB3dE9";
    /// The part of [`EC_ID`] that no page may carry.
    const EC_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const PARTNER_UID: &str = "partner-uid-canary-1234";
    /// 14 March 2025 at midnight UTC.
    const CREATED: u64 = 1_741_910_400;

    fn module() -> HmacModule {
        HmacModule::new(Redacted::new("test-secret-key-32-bytes-minimum".to_owned()))
    }

    fn request(cookie: Option<&str>, mode: Option<&str>, dest: Option<&str>) -> Request<EdgeBody> {
        let mut builder = Request::builder()
            .method("GET")
            .uri(format!("https://publisher.example{DATA_PAGE_PATH}"));
        for (name, value) in [
            ("cookie", cookie),
            ("sec-fetch-mode", mode),
            ("sec-fetch-dest", dest),
        ] {
            if let Some(value) = value {
                builder = builder.header(name, value);
            }
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build the request")
    }

    /// A request as a browser sends it when a person opens the address.
    fn opened_as_a_page(cookie: Option<&str>) -> Request<EdgeBody> {
        request(cookie, Some("navigate"), Some("document"))
    }

    fn body_text(response: Response<EdgeBody>) -> String {
        let bytes = response
            .into_body()
            .into_bytes()
            .expect("a buffered body")
            .to_vec();
        String::from_utf8(bytes).expect("a text body")
    }

    /// A graph holding one record against [`EC_ID`], with one partner's
    /// identifier in it.
    fn graph_with_a_record() -> KvIdentityGraph {
        let graph = KvIdentityGraph::in_memory("test-ec-store");
        let module = module();
        let key = AcceptedModules::active(Some(&module))
            .canonical_kv_key(EC_ID)
            .expect("the test identifier should be one the module issues");
        let mut entry = KvEntry::new(
            &ConsentContext::default(),
            None,
            CREATED,
            "publisher.example",
        );
        entry.ids.insert(
            "partner.example.com".to_owned(),
            KvPartnerId {
                uid: PARTNER_UID.to_owned(),
            },
        );
        graph.create(&key, &entry).expect("should store the record");
        graph
    }

    #[test]
    fn only_a_browser_opening_the_page_is_answered() {
        let graph = graph_with_a_record();
        let module = module();
        let cookie = format!("ts-ec={EC_ID}");

        for (mode, dest, who) in [
            (None, None, "a tool that sends neither header"),
            (Some("cors"), Some("empty"), "a script's fetch"),
            (Some("same-origin"), Some("empty"), "a same-origin fetch"),
            (Some("navigate"), Some("iframe"), "a frame"),
            (Some("navigate"), Some("object"), "an embedded object"),
            (
                Some("navigate"),
                None,
                "a navigation that names no destination",
            ),
            (
                Some("no-cors"),
                Some("document"),
                "a request that is not a navigation",
            ),
        ] {
            let response = handle_data(
                Some(&graph),
                Some(&module),
                &crate::prefix::PublicPrefix::default(),
                &request(Some(&cookie), mode, dest),
            )
            .expect("should answer");

            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "should refuse {who}"
            );
            assert_eq!(
                body_text(response),
                NOT_OPENED_AS_A_PAGE,
                "should tell {who} nothing about the record"
            );
        }

        let response = handle_data(
            Some(&graph),
            Some(&module),
            &crate::prefix::PublicPrefix::default(),
            &opened_as_a_page(Some(&cookie)),
        )
        .expect("should answer");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should answer a browser opening the page"
        );
    }

    #[test]
    fn the_record_is_shown_with_every_identifier_masked() {
        let graph = graph_with_a_record();
        let module = module();
        let cookie = format!("ts-ec={EC_ID}");

        let response = handle_data(
            Some(&graph),
            Some(&module),
            &crate::prefix::PublicPrefix::default(),
            &opened_as_a_page(Some(&cookie)),
        )
        .expect("should answer");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let html = body_text(response);
        assert!(
            html.contains("\"held\": true"),
            "should say a record is held"
        );
        assert!(
            html.contains("partner.example.com"),
            "should name the partner that holds an identifier"
        );
        assert!(
            html.contains("\"created\": \"2025-03-14T00:00:00Z\""),
            "should say when the record was made in a form a person reads"
        );
        assert!(
            html.contains("publisher.example"),
            "should show the site the record was made on"
        );
        assert!(
            !html.contains(PARTNER_UID),
            "should not show the partner's identifier"
        );
        assert!(
            !html.contains(EC_HASH),
            "should not show the Edge Cookie identifier"
        );
        assert!(
            html.contains("\"uid\": \"XXXX\""),
            "should show that the partner's identifier is masked"
        );
        assert!(
            html.contains("Each identifier shows as XXXX."),
            "should tell the reader what the mask means"
        );
    }

    #[test]
    fn an_identifier_sent_in_another_case_reaches_its_own_record() {
        let graph = graph_with_a_record();
        let module = module();
        let cookie = format!("ts-ec=hmac~{}.aB3dE9", EC_HASH.to_ascii_uppercase());

        let html = body_text(
            handle_data(
                Some(&graph),
                Some(&module),
                &crate::prefix::PublicPrefix::default(),
                &opened_as_a_page(Some(&cookie)),
            )
            .expect("should answer"),
        );

        assert!(
            html.contains("\"held\": true"),
            "should read the record under the key it is stored under: {html}"
        );
    }

    /// Every field the record has is set here and listed below, so a field
    /// added to the record fails this test until it is decided whether the
    /// field is an identifier.
    #[test]
    fn every_field_of_the_record_is_shown_or_masked_by_decision() {
        fn leaves(value: &Value, path: &str, out: &mut BTreeSet<String>) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        let key = if path == "ids" { "<partner>" } else { key };
                        let path = if path.is_empty() {
                            key.to_owned()
                        } else {
                            format!("{path}.{key}")
                        };
                        leaves(child, &path, out);
                    }
                }
                _ => {
                    out.insert(path.to_owned());
                }
            }
        }
        let entry = KvEntry {
            v: 1,
            created: CREATED,
            consent: KvConsent {
                tcf: Some("tcf-string".to_owned()),
                gpp: Some("gpp-string".to_owned()),
                ok: true,
                updated: CREATED + 60,
            },
            geo: KvGeo {
                country: "GB".to_owned(),
                region: Some("ENG".to_owned()),
                asn: Some(64_496),
                dma: Some(807),
            },
            pub_properties: Some(KvPubProperties {
                origin_domain: "publisher.example".to_owned(),
                seen_domains: BTreeSet::from(["publisher.example".to_owned()]),
            }),
            device: Some(KvDevice {
                is_mobile: 0,
                ja4_class: Some("t13d1516h2".to_owned()),
                platform_class: Some("windows".to_owned()),
                h2_fp_hash: Some("0123456789ab".to_owned()),
                known_browser: Some(true),
            }),
            network: Some(KvNetwork {
                cluster_size: Some(3),
            }),
            ids: BTreeMap::from([(
                "partner.example.com".to_owned(),
                KvPartnerId {
                    uid: PARTNER_UID.to_owned(),
                },
            )]),
        };

        let payload = held_payload(&entry);

        let mut shown = BTreeSet::new();
        leaves(&payload["record"], "", &mut shown);
        assert_eq!(
            shown.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "consent.gpp",
                "consent.ok",
                "consent.tcf",
                "consent.updated",
                "created",
                "device.h2_fp_hash",
                "device.is_mobile",
                "device.ja4_class",
                "device.known_browser",
                "device.platform_class",
                "geo.asn",
                "geo.country",
                "geo.dma",
                "geo.region",
                "ids.<partner>.uid",
                "network.cluster_size",
                "pub_properties.origin_domain",
                "pub_properties.seen_domains",
                "v",
            ],
            "a field the record has gained should be shown or masked by decision"
        );
        assert_eq!(
            payload["record"]["ids"]["partner.example.com"]["uid"],
            json!(MASK),
            "the one identifier the record holds should be masked"
        );
        assert_eq!(payload["identifier"], json!(MASK));
        assert_eq!(payload["consent_updated"], json!("2025-03-14T00:01:00Z"));
        assert_eq!(payload["withdrawn"], json!(false));
        assert!(
            !payload.to_string().contains(PARTNER_UID),
            "should carry no identifier anywhere"
        );
    }

    /// The record and the answer the API reference shows.
    #[test]
    fn a_known_record_gives_the_documented_answer() {
        let made = 1_773_480_600;
        let entry = KvEntry {
            v: 1,
            created: made,
            consent: KvConsent {
                tcf: Some("CP...".to_owned()),
                gpp: None,
                ok: true,
                updated: made,
            },
            geo: KvGeo {
                country: "GB".to_owned(),
                region: Some("ENG".to_owned()),
                asn: None,
                dma: None,
            },
            pub_properties: Some(KvPubProperties {
                origin_domain: "example.com".to_owned(),
                seen_domains: BTreeSet::from(["example.com".to_owned()]),
            }),
            device: None,
            network: None,
            ids: BTreeMap::from([(
                "partner.example.com".to_owned(),
                KvPartnerId {
                    uid: PARTNER_UID.to_owned(),
                },
            )]),
        };

        assert_eq!(
            held_payload(&entry),
            json!({
                "consent_updated": "2026-03-14T09:30:00Z",
                "created": "2026-03-14T09:30:00Z",
                "held": true,
                "identifier": "XXXX",
                "record": {
                    "consent": { "ok": true, "tcf": "CP...", "updated": 1_773_480_600 },
                    "created": 1_773_480_600,
                    "geo": { "country": "GB", "region": "ENG" },
                    "ids": { "partner.example.com": { "uid": "XXXX" } },
                    "pub_properties": {
                        "origin_domain": "example.com",
                        "seen_domains": ["example.com"]
                    },
                    "v": 1
                },
                "version": env!("CARGO_PKG_VERSION"),
                "withdrawn": false
            })
        );
    }

    #[test]
    fn a_withdrawal_is_shown_as_what_is_held() {
        let payload = held_payload(&KvEntry::tombstone(CREATED));

        assert_eq!(payload["held"], json!(true));
        assert_eq!(
            payload["withdrawn"],
            json!(true),
            "should say the record is one of a withdrawal"
        );
    }

    #[test]
    fn a_reader_with_nothing_held_is_told_why() {
        let graph = graph_with_a_record();
        let module = module();
        let another = format!("ts-ec=hmac~{}.zZ9yY8", "f".repeat(64));

        for (kv, cookie, why) in [
            (None, Some("ts-ec=anything"), "keeps no record"),
            (Some(&graph), None, "carried no Edge Cookie"),
            (
                Some(&graph),
                Some("ts-ec=t0op~device-1.issued-monday"),
                "is not one this deployment issued",
            ),
            (
                Some(&graph),
                Some(another.as_str()),
                "holds no record against the request",
            ),
        ] {
            let response = handle_data(
                kv,
                Some(&module),
                &crate::prefix::PublicPrefix::default(),
                &opened_as_a_page(cookie),
            )
            .expect("should answer");

            assert_eq!(response.status(), StatusCode::OK, "{why}");
            let html = body_text(response);
            assert!(html.contains("\"held\": false"), "{why}: {html}");
            assert!(html.contains(why), "should say `{why}`: {html}");
            assert!(
                !html.contains("partner.example.com"),
                "should show nothing of another reader's record"
            );
        }
    }

    #[test]
    fn a_store_that_cannot_be_read_is_an_error_and_not_an_empty_answer() {
        let graph = KvIdentityGraph::failing("test-ec-store");
        let module = module();
        let cookie = format!("ts-ec={EC_ID}");

        let result = handle_data(
            Some(&graph),
            Some(&module),
            &crate::prefix::PublicPrefix::default(),
            &opened_as_a_page(Some(&cookie)),
        );

        assert!(
            result.is_err(),
            "should not tell the reader nothing is held when the store was not read"
        );
    }

    #[test]
    fn every_answer_is_for_the_reader_alone() {
        let graph = graph_with_a_record();
        let module = module();
        let cookie = format!("ts-ec={EC_ID}");
        let answered = handle_data(
            Some(&graph),
            Some(&module),
            &crate::prefix::PublicPrefix::default(),
            &opened_as_a_page(Some(&cookie)),
        )
        .expect("should answer");
        let refused = handle_data(
            Some(&graph),
            Some(&module),
            &crate::prefix::PublicPrefix::default(),
            &request(Some(&cookie), None, None),
        )
        .expect("should answer");

        for (name, response) in [("the page", answered), ("the refusal", refused)] {
            let headers = response.headers();
            assert_eq!(
                headers[header::CACHE_CONTROL],
                "no-store, private",
                "{name} should never be stored"
            );
            assert!(
                headers[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .expect("a text policy")
                    .starts_with("sandbox;"),
                "{name} should be in an origin of its own"
            );
            assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{name}");
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{name}");
            assert_eq!(headers[header::REFERRER_POLICY], "no-referrer", "{name}");
            assert_eq!(
                headers["cross-origin-resource-policy"], "same-origin",
                "{name}"
            );
            assert!(
                headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
                "{name} should be offered to no other origin"
            );
        }
    }

    #[test]
    fn the_page_does_not_offer_a_data_form() {
        let html = body_text(page(&nothing_held("nothing")));

        assert!(
            !html.contains(".json"),
            "should not point at a form the address does not have"
        );
        assert!(html.contains("<h1>Your data</h1>"));
    }
}
