//! Canonical auction request data.
//!
//! Canonical request records are immutable snapshots built from an admitted
//! auction attempt. They keep attempt identity separate from EC identity and
//! normalize page-level URL data before provider-specific conversions.

use std::collections::BTreeMap;

use error_stack::{ensure, Report, ResultExt};
use url::Url;
use uuid::Uuid;

use crate::auction::admission::AuctionAdmission;
use crate::auction::context::ContextValue;
use crate::auction::formats::{validated_ad_slots, AdRequest};
use crate::auction::identity::AuctionIdentity;
use crate::auction::types::{
    AdSlot, AuctionRequest, DeviceInfo, PublisherInfo, SiteInfo, UserInfo,
};
use crate::auction::validation::{validate_context, AuctionInputLimits};
use crate::auction::AuctionSource;
use crate::consent::ConsentContext;
use crate::creative_opportunities::CreativeOpportunitySlot;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::RequestInfo;
use crate::integrations::prebid::PrebidIntegrationConfig;
use crate::settings::Settings;

/// Canonical page metadata for an auction attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPage {
    pub publisher_origin: Url,
    pub page_url: Url,
    pub telemetry_path: String,
    pub referer: Option<Url>,
}

/// Currency used for canonical auction economics.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Currency {
    Usd,
}

/// Caller-supplied inputs that are not owned by admission.
#[derive(Debug, Clone)]
pub struct CanonicalAuctionInput {
    pub publisher_domain: String,
    pub account_id: Option<String>,
    pub user: UserInfo,
    pub device: Option<DeviceInfo>,
    pub slots: Vec<AdSlot>,
    pub context: BTreeMap<String, ContextValue>,
    pub currency: Currency,
}

/// Canonical auction request snapshot.
#[derive(Debug, Clone)]
pub struct CanonicalAuctionRequest {
    pub auction_id: Uuid,
    pub source: AuctionSource,
    pub page: CanonicalPage,
    pub publisher_domain: String,
    pub account_id: Option<String>,
    pub user: UserInfo,
    pub device: Option<DeviceInfo>,
    pub slots: Vec<AdSlot>,
    pub context: BTreeMap<String, ContextValue>,
    pub currency: Currency,
}

/// Build a canonical auction request from an admitted attempt.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when request metadata contains a referer that
/// does not share the publisher origin.
pub fn build_canonical_request(
    admission: &AuctionAdmission,
    input: CanonicalAuctionInput,
) -> Result<CanonicalAuctionRequest, Report<TrustedServerError>> {
    let referer = admission
        .request_metadata()
        .referer
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .map(|url| url_without_fragment(&url))
        .filter(|url| !url.as_str().is_empty());

    assemble_canonical_request(
        admission.auction_id(),
        admission.source(),
        admission.publisher_origin().clone(),
        admission.page_url(),
        referer,
        input,
    )
}

/// Build a canonical auction request for the initial-navigation proxy path.
///
/// Initial navigation runs before any admission record exists, so page
/// identity is derived from the trusted [`RequestInfo`] scheme/host and request
/// path. Each attempt receives a fresh `auction_id`; there is no referer to
/// carry through.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when the request scheme/host/path do not
/// form a valid publisher origin or page URL.
pub(crate) fn build_navigation_canonical_request(
    request_info: &RequestInfo,
    request_path: &str,
    input: CanonicalAuctionInput,
) -> Result<CanonicalAuctionRequest, Report<TrustedServerError>> {
    let publisher_origin = Url::parse(&format!("{}://{}", request_info.scheme, request_info.host))
        .change_context(TrustedServerError::Auction {
            message: "Invalid navigation publisher origin".to_string(),
        })?;
    let page_url = Url::parse(&format!(
        "{}://{}{}",
        request_info.scheme, request_info.host, request_path
    ))
    .change_context(TrustedServerError::Auction {
        message: "Invalid navigation page URL".to_string(),
    })?;

    assemble_canonical_request(
        Uuid::new_v4(),
        AuctionSource::InitialNavigation,
        publisher_origin,
        &page_url,
        None,
        input,
    )
}

/// Assemble the immutable canonical request from already-decided page identity
/// and caller-supplied inputs.
fn assemble_canonical_request(
    auction_id: Uuid,
    source: AuctionSource,
    publisher_origin: Url,
    page_url: &Url,
    referer: Option<Url>,
    input: CanonicalAuctionInput,
) -> Result<CanonicalAuctionRequest, Report<TrustedServerError>> {
    let page_url = url_without_fragment(page_url);

    if let Some(referer) = &referer {
        ensure!(
            same_origin(referer, &publisher_origin),
            TrustedServerError::Auction {
                message: "Canonical auction referer must match publisher origin".to_string(),
            }
        );
    }

    Ok(CanonicalAuctionRequest {
        auction_id,
        source,
        page: CanonicalPage {
            publisher_origin,
            telemetry_path: telemetry_path(&page_url),
            page_url,
            referer,
        },
        publisher_domain: input.publisher_domain,
        account_id: input.account_id,
        user: input.user,
        device: input.device,
        slots: input.slots,
        context: input.context,
        currency: input.currency,
    })
}

/// Build canonical inputs from server-side creative opportunity slots.
///
/// Shared by the initial-navigation and SPA page-bids paths, which both source
/// their slots from matched [`CreativeOpportunitySlot`]s and carry no
/// client-supplied auction context.
pub(crate) fn canonical_input_from_creative_opportunities(
    settings: &Settings,
    matched_slots: &[CreativeOpportunitySlot],
    identity: AuctionIdentity,
    consent: ConsentContext,
    device: Option<DeviceInfo>,
) -> CanonicalAuctionInput {
    let slots = matched_slots
        .iter()
        .map(CreativeOpportunitySlot::to_ad_slot)
        .collect();
    canonical_input(settings, slots, BTreeMap::new(), identity, consent, device)
}

/// Build canonical inputs from a validated `/auction` request body.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when slot or context validation fails.
pub(crate) fn canonical_input_from_ad_request(
    settings: &Settings,
    body: &AdRequest,
    identity: AuctionIdentity,
    consent: ConsentContext,
    device: Option<DeviceInfo>,
) -> Result<CanonicalAuctionInput, Report<TrustedServerError>> {
    let slots = validated_ad_slots(body)?;
    let context: BTreeMap<String, ContextValue> = validate_context(
        body.config.as_ref(),
        &settings.auction.allowed_context_keys,
        &AuctionInputLimits::default(),
    )?
    .into_iter()
    .collect();

    Ok(canonical_input(
        settings, slots, context, identity, consent, device,
    ))
}

/// Assemble the caller-owned portion of the canonical request shared by every
/// entry path.
fn canonical_input(
    settings: &Settings,
    slots: Vec<AdSlot>,
    context: BTreeMap<String, ContextValue>,
    identity: AuctionIdentity,
    consent: ConsentContext,
    device: Option<DeviceInfo>,
) -> CanonicalAuctionInput {
    CanonicalAuctionInput {
        publisher_domain: settings.publisher.domain.clone(),
        account_id: configured_account_id(settings),
        user: UserInfo {
            id: identity.ec_id,
            consent: Some(consent),
            eids: identity.eids,
        },
        device,
        slots,
        context,
        currency: Currency::Usd,
    }
}

/// Snapshot device data from request signals shared by every entry path.
///
/// Returns `None` only when no device signal is available so the canonical
/// request omits an empty device record.
pub(crate) fn auction_device_snapshot(
    user_agent: Option<String>,
    client_ip: Option<String>,
    geo: Option<GeoInfo>,
) -> Option<DeviceInfo> {
    if user_agent.is_none() && client_ip.is_none() && geo.is_none() {
        return None;
    }
    Some(DeviceInfo {
        user_agent,
        ip: client_ip,
        geo,
    })
}

/// Read the optional configured Prebid account ID.
///
/// The same configured value is used by every entry path so canonical requests
/// do not diverge on account identity.
fn configured_account_id(settings: &Settings) -> Option<String> {
    settings
        .integration_config::<PrebidIntegrationConfig>("prebid")
        .ok()
        .flatten()
        .and_then(|config| config.account_id)
}

impl CanonicalAuctionRequest {
    /// Project this canonical request into the legacy [`AuctionRequest`] still
    /// consumed by the orchestrator and provider integrations.
    ///
    /// The canonical request remains the source of truth: the legacy `id` is
    /// the canonical `auction_id`, and page/publisher/site/user/slot/context
    /// data are copied without recomputation. Provider serialization migrates
    /// to consume the canonical request directly in a later task.
    pub(crate) fn to_auction_request(&self) -> AuctionRequest {
        let page = self.page.page_url.as_str().to_string();
        AuctionRequest {
            id: self.auction_id.to_string(),
            slots: self.slots.clone(),
            publisher: PublisherInfo {
                domain: self.publisher_domain.clone(),
                page_url: Some(page.clone()),
            },
            user: self.user.clone(),
            device: self.device.clone(),
            site: Some(SiteInfo {
                domain: self.publisher_domain.clone(),
                page,
            }),
            context: self
                .context
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }
}

fn url_without_fragment(url: &Url) -> Url {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    normalized
}

fn telemetry_path(page_url: &Url) -> String {
    let path = page_url.path();
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use edgezero_core::body::Body as EdgeBody;
    use http::{header, Method, Request};

    use std::collections::HashMap;

    use serde_json::json;

    use crate::auction::admission::{admit_auction_http, finalize_admission};
    use crate::auction::formats::{AdRequest, AdUnit, BannerUnit, BidConfig, MediaTypes};
    use crate::auction::identity::AuctionIdentity;
    use crate::auction::types::MediaType;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::ConsentContext;
    use crate::creative_opportunities::{
        CreativeOpportunityFormat, CreativeOpportunitySlot, PrebidSlotParams, SlotProviders,
    };
    use crate::ec::EcContext;
    use crate::http_util::RequestInfo;
    use crate::platform::ClientInfo;
    use crate::test_support::tests::create_test_settings;

    use super::*;

    fn admitted_attempt(source: AuctionSource, page_url: &str) -> AuctionAdmission {
        let settings = create_test_settings();
        let ec_context = EcContext::new_for_test(
            Some("ec-value".to_string()),
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..ConsentContext::default()
            },
        );
        let client_info = ClientInfo {
            client_ip: None,
            tls_protocol: None,
            tls_cipher: None,
            tls_ja4: None,
            h2_fingerprint: None,
            server_hostname: None,
            server_region: None,
        };
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example/auction")
            .header(header::ORIGIN, "https://publisher.example");
        builder = match source {
            AuctionSource::AuctionApi => builder
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-tsjs-auction", "1"),
            AuctionSource::SpaNavigation => builder.header("x-tsjs-page-bids", "1"),
            AuctionSource::InitialNavigation => builder,
        };
        let request = builder
            .body(EdgeBody::empty())
            .expect("should build admitted auction request");
        let draft = admit_auction_http(&settings, source, &request, &ec_context, &client_info)
            .expect("should admit auction request");
        finalize_admission(
            draft,
            Url::parse(page_url).expect("should parse admitted page URL"),
        )
    }

    fn equivalent_identity() -> AuctionIdentity {
        AuctionIdentity {
            ec_id: Some("ec-value".to_string()),
            eids: None,
        }
    }

    fn equivalent_consent() -> ConsentContext {
        ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        }
    }

    fn equivalent_device() -> Option<DeviceInfo> {
        Some(DeviceInfo {
            user_agent: Some("Mozilla/5.0".to_string()),
            ip: Some("203.0.113.7".to_string()),
            geo: None,
        })
    }

    fn equivalent_creative_opportunity_slot() -> CreativeOpportunitySlot {
        let mut bidders = HashMap::new();
        bidders.insert("appnexus".to_string(), json!({ "placementId": 123 }));
        CreativeOpportunitySlot {
            id: "atf".to_string(),
            gam_unit_path: None,
            div_id: None,
            page_patterns: vec!["/article".to_string()],
            formats: vec![CreativeOpportunityFormat {
                width: 300,
                height: 250,
                media_type: MediaType::Banner,
            }],
            floor_price: Some(0.5),
            targeting: HashMap::from([("pos".to_string(), "atf".to_string())]),
            providers: SlotProviders {
                aps: None,
                prebid: Some(PrebidSlotParams { bidders }),
            },
            compiled_patterns: Vec::new(),
        }
    }

    fn equivalent_ad_request() -> AdRequest {
        AdRequest {
            version: Some(2),
            page_url: Some("https://publisher.example/article".to_string()),
            ad_units: vec![AdUnit {
                code: "atf".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![300, 250]],
                    }),
                    video: None,
                    native: None,
                }),
                bids: Some(vec![BidConfig {
                    bidder: "appnexus".to_string(),
                    params: json!({ "placementId": 123 }),
                }]),
                floor_usd: Some(0.5),
                targeting: BTreeMap::from([("pos".to_string(), json!("atf"))]),
            }],
            config: None,
            eids: None,
        }
    }

    fn assert_canonical_fields_match(a: &CanonicalAuctionRequest, b: &CanonicalAuctionRequest) {
        assert_eq!(
            a.page.publisher_origin.as_str(),
            b.page.publisher_origin.as_str(),
            "publisher_origin should match"
        );
        assert_eq!(
            a.page.page_url.as_str(),
            b.page.page_url.as_str(),
            "page_url should match"
        );
        assert_eq!(
            a.page.telemetry_path, b.page.telemetry_path,
            "telemetry_path should match"
        );
        assert_eq!(
            a.page.referer.as_ref().map(Url::as_str),
            b.page.referer.as_ref().map(Url::as_str),
            "referer should match"
        );
        assert_eq!(
            a.publisher_domain, b.publisher_domain,
            "publisher_domain should match"
        );
        assert_eq!(a.account_id, b.account_id, "account_id should match");
        assert_eq!(a.currency, b.currency, "currency should match");
        assert_eq!(a.user.id, b.user.id, "user.id should match");
        assert_eq!(
            serde_json::to_value(&a.user.eids).expect("should serialize eids"),
            serde_json::to_value(&b.user.eids).expect("should serialize eids"),
            "user.eids should match"
        );
        assert_eq!(
            serde_json::to_value(&a.device).expect("should serialize device"),
            serde_json::to_value(&b.device).expect("should serialize device"),
            "device should match"
        );
        assert_eq!(
            serde_json::to_value(&a.slots).expect("should serialize slots"),
            serde_json::to_value(&b.slots).expect("should serialize slots"),
            "slots should match"
        );
        assert_eq!(
            serde_json::to_value(&a.context).expect("should serialize context"),
            serde_json::to_value(&b.context).expect("should serialize context"),
            "context should match"
        );
    }

    #[test]
    fn equivalent_paths_build_same_request() {
        let settings = create_test_settings();
        let page_url = "https://publisher.example/article";
        let co_slots = vec![equivalent_creative_opportunity_slot()];
        let body = equivalent_ad_request();

        // Initial navigation builds without an admission from RequestInfo.
        let request_info = RequestInfo {
            host: "publisher.example".to_string(),
            scheme: "https".to_string(),
        };
        let initial = build_navigation_canonical_request(
            &request_info,
            "/article",
            canonical_input_from_creative_opportunities(
                &settings,
                &co_slots,
                equivalent_identity(),
                equivalent_consent(),
                equivalent_device(),
            ),
        )
        .expect("should build initial navigation request");

        // Page-bids and /auction build from their admitted attempts.
        let page_bids = build_canonical_request(
            &admitted_attempt(AuctionSource::SpaNavigation, page_url),
            canonical_input_from_creative_opportunities(
                &settings,
                &co_slots,
                equivalent_identity(),
                equivalent_consent(),
                equivalent_device(),
            ),
        )
        .expect("should build page-bids request");

        let auction = build_canonical_request(
            &admitted_attempt(AuctionSource::AuctionApi, page_url),
            canonical_input_from_ad_request(
                &settings,
                &body,
                equivalent_identity(),
                equivalent_consent(),
                equivalent_device(),
            )
            .expect("should build /auction canonical input"),
        )
        .expect("should build /auction request");

        assert_eq!(initial.source, AuctionSource::InitialNavigation);
        assert_eq!(page_bids.source, AuctionSource::SpaNavigation);
        assert_eq!(auction.source, AuctionSource::AuctionApi);

        assert_ne!(initial.auction_id, page_bids.auction_id);
        assert_ne!(initial.auction_id, auction.auction_id);
        assert_ne!(page_bids.auction_id, auction.auction_id);

        assert_canonical_fields_match(&initial, &page_bids);
        assert_canonical_fields_match(&initial, &auction);
    }

    #[test]
    fn to_auction_request_preserves_canonical_fields() {
        let settings = create_test_settings();
        let canonical = build_canonical_request(
            &admitted_attempt(
                AuctionSource::AuctionApi,
                "https://publisher.example/article",
            ),
            canonical_input_from_ad_request(
                &settings,
                &equivalent_ad_request(),
                equivalent_identity(),
                equivalent_consent(),
                equivalent_device(),
            )
            .expect("should build /auction canonical input"),
        )
        .expect("should build canonical request");

        let legacy = canonical.to_auction_request();

        assert_eq!(
            legacy.id,
            canonical.auction_id.to_string(),
            "legacy id should be the canonical auction UUID"
        );
        assert_eq!(legacy.publisher.domain, canonical.publisher_domain);
        assert_eq!(
            legacy.publisher.page_url.as_deref(),
            Some(canonical.page.page_url.as_str())
        );
        assert_eq!(
            legacy.site.as_ref().map(|site| site.page.as_str()),
            Some(canonical.page.page_url.as_str())
        );
        assert_eq!(legacy.user.id, canonical.user.id);
        assert_eq!(
            serde_json::to_value(&legacy.slots).expect("should serialize legacy slots"),
            serde_json::to_value(&canonical.slots).expect("should serialize canonical slots"),
        );
    }

    fn admitted_attempt_with_ec(
        ec_id: &str,
        page_url: &str,
        referer: Option<&str>,
    ) -> AuctionAdmission {
        let settings = create_test_settings();
        let ec_context = EcContext::new_for_test(
            Some(ec_id.to_string()),
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..ConsentContext::default()
            },
        );
        let client_info = ClientInfo {
            client_ip: None,
            tls_protocol: None,
            tls_cipher: None,
            tls_ja4: None,
            h2_fingerprint: None,
            server_hostname: None,
            server_region: None,
        };
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example/auction")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://publisher.example")
            .header("x-tsjs-auction", "1");
        if let Some(referer) = referer {
            builder = builder.header(header::REFERER, referer);
        }
        let request = builder
            .body(EdgeBody::empty())
            .expect("should build admitted auction request");
        let draft = admit_auction_http(
            &settings,
            AuctionSource::AuctionApi,
            &request,
            &ec_context,
            &client_info,
        )
        .expect("should admit auction request");
        finalize_admission(
            draft,
            Url::parse(page_url).expect("should parse admitted page URL"),
        )
    }

    fn input_with_ec(ec_id: &str) -> CanonicalAuctionInput {
        CanonicalAuctionInput {
            publisher_domain: "publisher.example".to_string(),
            account_id: None,
            user: UserInfo {
                id: Some(ec_id.to_string()),
                consent: None,
                eids: None,
            },
            device: None,
            slots: Vec::new(),
            context: BTreeMap::new(),
            currency: Currency::Usd,
        }
    }

    #[test]
    fn canonical_request_preserves_fresh_admission_id() {
        let first_admission =
            admitted_attempt_with_ec("ec-value", "https://publisher.example/a", None);
        let second_admission =
            admitted_attempt_with_ec("ec-value", "https://publisher.example/a", None);

        let first = build_canonical_request(&first_admission, input_with_ec("ec-value"))
            .expect("should build first request");
        let second = build_canonical_request(&second_admission, input_with_ec("ec-value"))
            .expect("should build second request");

        assert_ne!(first.auction_id, second.auction_id);
        assert_eq!(first.auction_id, first_admission.auction_id());
        assert_ne!(first.auction_id.to_string(), "ec-value");
    }

    #[test]
    fn canonical_request_normalizes_page_and_same_origin_referer() {
        let admission = admitted_attempt_with_ec(
            "ec-value",
            "https://publisher.example/article?utm=1#section",
            Some("https://publisher.example/referring-page?x=1#fragment"),
        );

        let canonical = build_canonical_request(&admission, input_with_ec("ec-value"))
            .expect("should build canonical request");

        assert_eq!(
            canonical.page.page_url.as_str(),
            "https://publisher.example/article?utm=1"
        );
        assert_eq!(canonical.page.telemetry_path, "/article");
        assert_eq!(
            canonical
                .page
                .referer
                .as_ref()
                .expect("should keep same-origin referer")
                .as_str(),
            "https://publisher.example/referring-page?x=1"
        );
    }

    #[test]
    fn canonical_request_rejects_cross_origin_referer() {
        let admission = admitted_attempt_with_ec(
            "ec-value",
            "https://publisher.example/article",
            Some("https://evil.example/referring-page"),
        );

        let result = build_canonical_request(&admission, input_with_ec("ec-value"));

        assert!(
            result.is_err(),
            "cross-origin referer should not enter canonical page metadata"
        );
    }
}
