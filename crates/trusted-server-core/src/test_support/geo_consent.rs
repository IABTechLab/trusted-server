//! Geo-to-consent cases shared by application-path adapter runners.
//!
//! The hook receives raw source inputs, never a jurisdiction or decoded consent.
//! It must execute the real router and finalizer, consume the response, and
//! return actual response headers and requests received by its bidder transport.

use super::identity_conformance::RecordingStore;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::normalize_ec_id_for_kv;
use futures::executor::block_on;
use http::{HeaderMap, Method, StatusCode};
use serde_json::{Value, json};

use super::identity_conformance::tcf_consent_cookie;
use crate::settings::Settings;

static GRANTED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| tcf_consent_cookie(true));
static DENIED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| tcf_consent_cookie(false));

/// Local geo producer state to exercise through the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
pub enum Source {
    /// The configured producer receives country/subdivision input.
    Located,
    /// A forged source on an untrusted hop.
    Untrusted,
    /// A selected provider reports failure.
    Error,
    /// No geo provider is selected (native only).
    Unselected,
}

/// Expected current policy, independent of the implementation under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub enum Policy {
    /// GDPR opt-in.
    Gdpr,
    /// US state rules requiring a subdivision and a usable signal.
    UsState,
    /// A known location outside configured regulated jurisdictions.
    NonRegulated,
    /// No usable trusted location.
    Unknown,
}

/// One row of the shared location table.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Location {
    /// Stable case name.
    pub name: &'static str,
    /// Raw producer country, including deliberately invalid values.
    pub country: Option<&'static str>,
    /// Raw producer subdivision.
    pub region: Option<&'static str>,
    /// Producer outcome/trust control.
    pub source: Source,
    /// Independently specified policy expectation.
    pub policy: Policy,
    /// Fastly expectation when its local producer differs from protected `CloudFront`.
    #[serde(default)]
    pub fastly_policy: Option<Policy>,
    /// Fastly retains the country but drops this malformed subdivision.
    #[serde(default)]
    pub fastly_drop_region: bool,
}

/// Shared locations. US without subdivision deliberately preserves current policy.
pub static LOCATIONS: std::sync::LazyLock<Vec<Location>> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("fixtures/geo-consent.json"))
        .expect("should parse shared geo cases")
});

/// Consent signals sent through the actual request parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// No signal.
    None,
    /// Effective TCF Purpose 1.
    Purpose1,
    /// TCF without Purpose 1.
    Denied,
    /// GPP containing current TCF and US Privacy sections.
    GppTcf,
    /// GPP section 7 with US sale allowed.
    GppUs,
    /// GPP section 9 with US sale opted out.
    GppOptOut,
    /// US Privacy without sale opt-out.
    UsPrivacy,
    /// Global Privacy Control.
    Gpc,
}

impl Signal {
    /// Raw cookies to exercise extraction and forwarding.
    #[must_use]
    pub fn cookie(self) -> String {
        match self {
            Self::Purpose1 => GRANTED.clone(),
            Self::Denied => DENIED.clone(),
            Self::GppTcf => format!(
                "__gpp=DBACNY~{}~1YNN; __gpp_sid=2,6",
                GRANTED.trim_start_matches("euconsent-v2=")
            ),
            Self::GppUs => "__gpp=DBABLA~BVQqAAAAAgA.QA; __gpp_sid=7".to_owned(),
            Self::GppOptOut => "__gpp=DBABRg~BVVVVVVVVWA.AA; __gpp_sid=9".to_owned(),
            Self::UsPrivacy => "us_privacy=1YNN".to_owned(),
            Self::None | Self::Gpc => String::new(),
        }
    }

    fn grants_storage(self) -> bool {
        matches!(self, Self::Purpose1 | Self::GppTcf)
    }
}

/// Actual application entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Publisher document navigation, including SSAT.
    Document,
    /// Read-only EC endpoint.
    EcGet,
    /// Same-origin EC consent snapshot endpoint.
    EcPost,
    /// Programmatic server-side auction.
    Auction,
    /// Publisher asset request.
    Asset,
    /// Publisher API request.
    Api,
}

/// Raw input for the adapter hook.
pub struct Exchange<'a> {
    /// Producer row; never inject its expected policy into the application.
    pub location: Location,
    /// Consent representation to put on the request.
    pub signal: Signal,
    /// Route and Fetch Metadata to exercise.
    pub kind: Kind,
    /// Previously issued identity to replay.
    pub ec: Option<&'a str>,
}

impl Exchange<'_> {
    /// HTTP method and path for this entry point.
    #[must_use]
    pub fn target(&self) -> (Method, &'static str) {
        match self.kind {
            Kind::Document => (Method::GET, "/article"),
            Kind::EcGet => (Method::GET, "/_ts/api/v1/ec"),
            Kind::EcPost => (Method::POST, "/_ts/api/v1/ec"),
            Kind::Auction => (Method::POST, "/auction"),
            Kind::Asset => (Method::GET, "/image.png"),
            Kind::Api => (Method::GET, "/api/example"),
        }
    }

    /// Raw request headers, including same-origin proof for EC POST.
    #[must_use]
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut cookie = self.signal.cookie();
        if let Some(ec) = self.ec {
            cookie.push_str(&format!("; ts-ec={ec}"));
        }
        let mut headers = vec![
            ("host", "publisher.example.com".to_owned()),
            (
                "user-agent",
                "Mozilla/5.0 AppleWebKit/537.36 Chrome/146.0.0.0 Safari/537.36".to_owned(),
            ),
            ("cookie", cookie),
            ("origin", "https://publisher.example.com".to_owned()),
            ("content-type", "application/json".to_owned()),
            ("sec-fetch-site", "same-origin".to_owned()),
        ];
        if self.kind == Kind::Document {
            headers.extend([
                ("sec-fetch-dest", "document".to_owned()),
                ("sec-fetch-mode", "navigate".to_owned()),
                ("accept", "text/html".to_owned()),
            ]);
        }
        if self.signal == Signal::Gpc {
            headers.push(("sec-gpc", "1".to_owned()));
        }
        headers
    }

    /// Wire body for the selected route.
    #[must_use]
    pub fn body(&self) -> String {
        if self.kind == Kind::Auction {
            json!({"adUnits":[{"code":"example-slot","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}).to_string()
        } else if self.kind == Kind::EcPost {
            json!({}).to_string()
        } else {
            String::new()
        }
    }
}

/// Outside observations after consuming the complete finalized response.
pub struct Observed {
    /// Actual HTTP status.
    pub status: StatusCode,
    /// Actual finalized response headers, including every Set-Cookie.
    pub headers: HeaderMap,
    /// `OpenRTB` bodies actually received by the bidder transport.
    pub bids: Vec<Value>,
}

/// Adapter hook for future Cloudflare/Spin runners.
pub trait Adapter {
    /// Drives real middleware, routing, source resolution and finalization.
    fn exchange(&mut self, request: &Exchange<'_>) -> Observed;
    /// Inspects authoritative storage, which has no response-header signal.
    fn record_is_live(&self, identity: &str) -> bool;
}

/// Reads the recording store after a completed response.
///
/// This is the suite's internal observation: cookie retention alone cannot
/// distinguish a live record from a silent tombstone.
///
/// # Panics
/// Panics if the fixture store cannot be read.
#[must_use]
pub fn record_is_live(store: &RecordingStore, identity: &str) -> bool {
    block_on(KvIdentityGraph::new(store.clone()).get(&normalize_ec_id_for_kv(identity)))
        .expect("should inspect authoritative fixture record")
        .is_some_and(|(record, _)| record.consent.ok)
}

/// Configures identical actual bidder and HTML slot behavior on each runner.
///
/// # Panics
/// Panics if the static fictional configuration is invalid.
pub fn configure(settings: &mut Settings) {
    settings.consent.check_expiration = false;
    settings.auction = serde_json::from_value(json!({"enabled":true,"providers":{"example-bidder":{"protocol":"openrtb-2.6","profile":"standard","endpoint":BIDDER,"routing":"all_eligible"}}})).expect("should configure capturing bidder");
    settings.creative_opportunities = Some(serde_json::from_value(json!({"gam_network_id":"12345","slot":[{"id":"example-slot","page_patterns":["/*"],"formats":[{"width":300,"height":250,"media_type":"banner"}]}]})).expect("should configure HTML auction slot"));
}

/// Fictional bidder transport destination.
pub const BIDDER: &str = "https://bidder.example.com/bid";

fn cookies(response: &Observed) -> Vec<String> {
    response
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().expect("should decode cookie").to_owned())
        .filter(|value| value.starts_with("ts-ec="))
        .collect()
}

/// Executes the shared matrix with independent policy expectations.
///
/// `canonical` selects the current protected Axum profile (`true`) or Fastly
/// profile (`false`), including casing and table-specified producer projections.
/// Expectations are applied after exchange; the adapter always receives raw input.
/// `unselected` is false on Fastly, whose local lookup has no provider selector.
///
/// # Panics
/// Panics on a violated application-path expectation.
pub fn verify(adapter: &mut impl Adapter, canonical: bool, unselected: bool) {
    for &location in LOCATIONS.iter() {
        if location.source == Source::Unselected && !unselected {
            continue;
        }
        for signal in [
            Signal::None,
            Signal::Purpose1,
            Signal::Denied,
            Signal::GppTcf,
            Signal::GppUs,
            Signal::GppOptOut,
            Signal::UsPrivacy,
            Signal::Gpc,
        ] {
            for kind in [
                Kind::Document,
                Kind::EcGet,
                Kind::EcPost,
                Kind::Auction,
                Kind::Asset,
                Kind::Api,
            ] {
                for returning in [false, true] {
                    let identity = returning.then(|| seed_identity(adapter));
                    verify_exchange(
                        adapter,
                        &Exchange {
                            location,
                            signal,
                            kind,
                            ec: identity.as_deref(),
                        },
                        canonical,
                    );
                }
            }
        }
    }
}

fn seed_identity(adapter: &mut impl Adapter) -> String {
    let seed = adapter.exchange(&Exchange {
        location: *LOCATIONS
            .iter()
            .find(|location| location.name == "jp")
            .expect("should have non-regulated seed case"),
        signal: Signal::None,
        kind: Kind::EcPost,
        ec: None,
    });
    let identity = cookies(&seed)
        .first()
        .expect("should issue seed EC")
        .split(';')
        .next()
        .expect("should have cookie pair")
        .trim_start_matches("ts-ec=")
        .to_owned();
    assert!(
        adapter.record_is_live(&identity),
        "should persist live seed before replay"
    );
    identity
}

fn verify_exchange(adapter: &mut impl Adapter, request: &Exchange<'_>, canonical: bool) {
    let observed = adapter.exchange(request);
    // Expected producer projection is applied only after the real exchange.
    // Never pass these expectations into ingress or application services.
    let mut expected = Exchange { ..*request };
    if !canonical {
        expected.location.policy = request
            .location
            .fastly_policy
            .unwrap_or(request.location.policy);
        if request.location.fastly_drop_region {
            expected.location.region = None;
        }
    }
    let request = &expected;
    let context = format!(
        "{} {:?} {:?} returning={}",
        request.location.name,
        request.signal,
        request.kind,
        request.ec.is_some()
    );
    assert!(
        !observed.status.is_server_error(),
        "should not fail request: {context}"
    );
    assert!(
        observed.status.is_success(),
        "should accept request: {context}, status={}",
        observed.status
    );
    verify_identity(adapter, request, &observed, &context);
    verify_dispatch(request, &observed, &context);
    verify_geo(request, &observed, canonical, &context);
}

fn verify_identity(
    adapter: &impl Adapter,
    request: &Exchange<'_>,
    observed: &Observed,
    context: &str,
) {
    let location = request.location;
    let signal = request.signal;
    let kind = request.kind;
    let identity = request.ec;
    let returning = identity.is_some();
    let withdrawal = returning
        && match location.policy {
            Policy::Gdpr => signal == Signal::Denied,
            Policy::UsState => {
                matches!(signal, Signal::Denied | Signal::Gpc | Signal::GppOptOut)
            }
            Policy::NonRegulated | Policy::Unknown => false,
        };
    // EC GET is read-only and does not run identity finalization.
    let withdrawal = withdrawal && kind != Kind::EcGet;
    let allowed = match location.policy {
        Policy::Gdpr => signal.grants_storage(),
        Policy::UsState => {
            signal.grants_storage() || matches!(signal, Signal::GppUs | Signal::UsPrivacy)
        }
        Policy::NonRegulated => true,
        Policy::Unknown => false,
    };
    let ec = cookies(observed);
    let expired = ec.iter().any(|cookie| cookie.contains("Max-Age=0"));
    assert_eq!(
        expired, withdrawal,
        "should apply jurisdiction-specific withdrawal: {context}; cookies={ec:?}"
    );
    if !returning {
        assert_eq!(
            ec.iter().any(|cookie| !cookie.contains("Max-Age=0")),
            allowed && matches!(kind, Kind::Document | Kind::EcPost),
            "should gate EC creation: {context}; cookies={ec:?}"
        );
    } else if !withdrawal {
        assert!(
            ec.iter().all(|cookie| cookie.split(';').next()
                == Some(format!("ts-ec={}", identity.expect("should replay seed")).as_str())),
            "should retain existing EC: {context}; cookies={ec:?}"
        );
    }
    if let Some(identity) = identity {
        assert_eq!(
            adapter.record_is_live(identity),
            !withdrawal,
            "should preserve or withdraw authoritative record: {context}"
        );
    }
}

fn verify_dispatch(request: &Exchange<'_>, observed: &Observed, context: &str) {
    let location = request.location;
    let signal = request.signal;
    let kind = request.kind;
    let auction = signal != Signal::Denied
        && (signal.grants_storage() || !matches!(location.policy, Policy::Gdpr | Policy::Unknown));
    assert_eq!(
        observed.bids.len(),
        usize::from(auction && matches!(kind, Kind::Auction | Kind::Document)),
        "should gate actual bidder dispatch: {context}"
    );
}

fn verify_geo(request: &Exchange<'_>, observed: &Observed, canonical: bool, context: &str) {
    let location = request.location;
    let located = location.policy != Policy::Unknown;
    let expected_country = location.country.map(|value| {
        if canonical {
            value.to_ascii_uppercase()
        } else {
            value.to_owned()
        }
    });
    let expected_region = location.region.map(|value| {
        if canonical {
            value.to_ascii_uppercase()
        } else {
            value.to_owned()
        }
    });
    assert_eq!(
        observed
            .headers
            .get("x-geo-info-available")
            .and_then(|value| value.to_str().ok()),
        Some(if located { "true" } else { "false" }),
        "should report managed availability: {context}"
    );
    for name in ["country", "region"] {
        let expected = if !located {
            None
        } else if name == "country" {
            expected_country.as_deref()
        } else {
            expected_region.as_deref()
        };
        assert_eq!(
            observed
                .headers
                .get(format!("x-geo-{name}"))
                .and_then(|value| value.to_str().ok()),
            expected,
            "should report same producer {name}: {context}"
        );
    }
    for bid in &observed.bids {
        assert_eq!(
            bid["device"]["geo"]["country"].as_str(),
            located.then_some(expected_country.as_deref()).flatten(),
            "should export same country: {context}"
        );
        assert_eq!(
            bid["device"]["geo"]["region"].as_str(),
            located.then_some(expected_region.as_deref()).flatten(),
            "should export same subdivision: {context}"
        );
        verify_privacy_fields(bid, location, request.signal, context);
    }
}

fn verify_privacy_fields(bid: &Value, location: Location, signal: Signal, context: &str) {
    if signal.grants_storage() {
        assert_eq!(
            bid["regs"]["gdpr"],
            json!(1),
            "should forward effective EU consent: {context}"
        );
    }
    if matches!(signal, Signal::GppUs | Signal::GppOptOut | Signal::GppTcf) {
        let raw = signal.cookie();
        let gpp = raw
            .trim_start_matches("__gpp=")
            .split(';')
            .next()
            .expect("should have GPP");
        assert_eq!(
            bid["regs"]["gpp"],
            json!(gpp),
            "should forward GPP: {context}"
        );
        assert_eq!(
            bid["regs"]["gpp_sid"],
            if signal == Signal::GppTcf {
                json!([2, 6])
            } else if signal == Signal::GppUs {
                json!([7])
            } else {
                json!([9])
            },
            "should forward relevant GPP sections: {context}"
        );
    }
    if !signal.grants_storage() {
        let expected = if signal == Signal::None {
            Value::Null
        } else {
            json!(0)
        };
        assert_eq!(
            bid["regs"]["gdpr"], expected,
            "should forward current non-GDPR regs semantics: {context}"
        );
    }
    if signal == Signal::Gpc {
        assert_eq!(
            bid["regs"]["us_privacy"],
            if location.policy == Policy::UsState {
                json!("1YYN")
            } else {
                Value::Null
            },
            "should synthesize US Privacy only for applicable GPC jurisdiction: {context}"
        );
    }
    if signal == Signal::UsPrivacy {
        assert_eq!(
            bid["regs"]["us_privacy"],
            json!("1YNN"),
            "should forward US Privacy: {context}"
        );
    }
}
