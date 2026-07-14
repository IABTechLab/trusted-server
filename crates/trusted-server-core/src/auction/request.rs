//! Canonical auction request data.
//!
//! Canonical request records are immutable snapshots built from an admitted
//! auction attempt. They keep attempt identity separate from EC identity and
//! normalize page-level URL data before provider-specific conversions.

use std::collections::BTreeMap;

use error_stack::{ensure, Report};
use url::Url;
use uuid::Uuid;

use crate::auction::admission::AuctionAdmission;
use crate::auction::context::ContextValue;
use crate::auction::types::{AdSlot, DeviceInfo, UserInfo};
use crate::auction::AuctionSource;
use crate::error::TrustedServerError;

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
    let page_url = url_without_fragment(admission.page_url());
    let referer = admission
        .request_metadata()
        .referer
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .map(|url| url_without_fragment(&url))
        .filter(|url| !url.as_str().is_empty());

    if let Some(referer) = &referer {
        ensure!(
            same_origin(referer, admission.publisher_origin()),
            TrustedServerError::Auction {
                message: "Canonical auction referer must match publisher origin".to_string(),
            }
        );
    }

    Ok(CanonicalAuctionRequest {
        auction_id: admission.auction_id(),
        source: admission.source(),
        page: CanonicalPage {
            publisher_origin: admission.publisher_origin().clone(),
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

    use crate::auction::admission::{admit_auction_http, finalize_admission};
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::ConsentContext;
    use crate::ec::EcContext;
    use crate::platform::ClientInfo;
    use crate::test_support::tests::create_test_settings;

    use super::*;

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
