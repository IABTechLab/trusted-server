//! HTTP endpoint handlers for auction requests.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Request, Response};

use crate::auction::formats::AdRequest;
use crate::consent;
use crate::consent::kv::ConsentKvOps;
use crate::error::TrustedServerError;
use crate::integrations::collect_body_bounded;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::formats::{convert_to_openrtb_response, convert_tsjs_to_auction_request};
use super::types::AuctionContext;
use super::AuctionOrchestrator;

const AUCTION_MAX_BODY_BYTES: usize = 256 * 1024;

/// Handle auction request from /auction endpoint.
///
/// This is the main entry point for running header bidding auctions.
/// It orchestrates bids from multiple providers (Prebid, APS, GAM, etc.) and returns
/// the winning bids in `OpenRTB` format with creative HTML inline in the `adm` field.
///
/// When `kv_ops` is provided, consent processing may load fallback consent
/// from KV and write cookie-sourced consent changes back through that
/// implementation. When `kv_ops` is `None`, consent processing remains
/// request-local and skips KV fallback/write-through.
///
/// # Errors
///
/// Returns an error if:
/// - The request body cannot be parsed
/// - Request-scoped consent preparation fails
/// - The auction request conversion fails (e.g., invalid ad units)
/// - The auction execution fails
/// - The response cannot be serialized
pub async fn handle_auction(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
    kv_ops: Option<&dyn ConsentKvOps>,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let (parts, body) = req.into_parts();

    // Parse request body — use a bounded read so streaming bodies cannot exhaust memory.
    let body_bytes = collect_body_bounded(body, AUCTION_MAX_BODY_BYTES, "auction")
        .await
        .change_context(TrustedServerError::Auction {
            message: "Failed to read auction request body".to_string(),
        })?;
    let body: AdRequest =
        serde_json::from_slice(&body_bytes).change_context(TrustedServerError::Auction {
            message: "Failed to parse auction request body".to_string(),
        })?;

    log::info!(
        "Auction request received for {} ad units",
        body.ad_units.len()
    );

    let http_req = Request::from_parts(parts, EdgeBody::empty());

    let consent_state =
        consent::prepare_request_consent_state(settings, services, &http_req, kv_ops)
            .change_context(TrustedServerError::Auction {
                message: "Failed to prepare request consent state".to_string(),
            })?;

    // Convert tsjs request format to auction request
    let auction_request = convert_tsjs_to_auction_request(
        &body,
        settings,
        services,
        &http_req,
        consent_state.consent_context,
        &consent_state.synthetic_id,
        consent_state.geo,
    )?;

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &http_req,
        client_info: services.client_info(),
        timeout_ms: settings.auction.timeout_ms,
        provider_responses: None,
        services,
    };

    // Run the auction
    let result = orchestrator
        .run_auction(&auction_request, &context, services)
        .await
        .change_context(TrustedServerError::Auction {
            message: "Auction orchestration failed".to_string(),
        })?;

    log::info!(
        "Auction completed: {} providers, {} winning bids, {}ms total",
        result.provider_responses.len(),
        result.winning_bids.len(),
        result.total_time_ms
    );

    // Convert to OpenRTB response format with inline creative HTML
    convert_to_openrtb_response(&result, settings, &auction_request)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;
    use std::time::Duration;

    use edgezero_core::body::Body as EdgeBody;
    use http::header;
    use http::Request;
    use serde_json::json;

    use super::handle_auction;
    use crate::auction::AuctionOrchestrator;
    use crate::auction_config_types::AuctionConfig;
    use crate::consent::kv::{ConsentKvOps, KvConsentEntry};
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;

    #[derive(Default)]
    struct StubConsentKvOps {
        loads: Mutex<Vec<String>>,
        saves: Mutex<HashMap<String, KvConsentEntry>>,
    }

    impl StubConsentKvOps {
        fn load_keys(&self) -> Vec<String> {
            self.loads.lock().expect("should lock load keys").clone()
        }

        fn saved_entries(&self) -> HashMap<String, KvConsentEntry> {
            self.saves
                .lock()
                .expect("should lock saved entries")
                .clone()
        }
    }

    impl ConsentKvOps for StubConsentKvOps {
        fn load_entry(&self, key: &str) -> Option<KvConsentEntry> {
            self.loads
                .lock()
                .expect("should lock load keys")
                .push(key.to_string());
            None
        }

        fn save_entry_with_ttl(&self, key: &str, entry: &KvConsentEntry, _ttl: Duration) {
            self.saves
                .lock()
                .expect("should lock saved entries")
                .insert(key.to_string(), entry.clone());
        }

        fn delete_entry(&self, _key: &str) {}
    }

    fn no_providers_orchestrator() -> AuctionOrchestrator {
        AuctionOrchestrator::new(AuctionConfig {
            enabled: true,
            providers: Vec::new(),
            mediator: None,
            timeout_ms: 50,
            creative_store: "creative_store".to_string(),
            allowed_context_keys: HashSet::new(),
        })
    }

    fn build_auction_request(cookie_header: Option<&str>) -> Request<EdgeBody> {
        let mut req = Request::builder()
            .method("POST")
            .uri("https://publisher.example/auction")
            .header(header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(
                serde_json::to_vec(&json!({
                    "adUnits": [{
                        "code": "slot-1",
                        "mediaTypes": {
                            "banner": {
                                "sizes": [[300, 250]]
                            }
                        }
                    }]
                }))
                .expect("should serialize auction request body"),
            ))
            .expect("should build auction request");

        if let Some(cookie_header) = cookie_header {
            req.headers_mut().insert(
                header::COOKIE,
                header::HeaderValue::from_str(cookie_header).expect("should build cookie header"),
            );
        }

        req
    }

    #[test]
    fn handle_auction_attempts_kv_fallback_when_cookie_signals_are_absent() {
        let settings = create_test_settings();
        let orchestrator = no_providers_orchestrator();
        let kv = StubConsentKvOps::default();

        let err = futures::executor::block_on(handle_auction(
            &settings,
            &orchestrator,
            &noop_services(),
            Some(&kv),
            build_auction_request(None),
        ))
        .expect_err("should fail later because no providers are configured");

        let _ = err;
        assert_eq!(
            kv.load_keys().len(),
            1,
            "should try loading consent from KV when request has no cookie signals"
        );
    }

    #[test]
    fn handle_auction_persists_cookie_consent_to_kv() {
        let settings = create_test_settings();
        let orchestrator = no_providers_orchestrator();
        let kv = StubConsentKvOps::default();

        let err = futures::executor::block_on(handle_auction(
            &settings,
            &orchestrator,
            &noop_services(),
            Some(&kv),
            build_auction_request(Some("euconsent-v2=CPXxGfAPXxGfA")),
        ))
        .expect_err("should fail later because no providers are configured");

        let _ = err;

        let saved_entries = kv.saved_entries();
        assert_eq!(
            saved_entries.len(),
            1,
            "should persist cookie-sourced consent to KV before auction execution"
        );
        let entry = saved_entries
            .values()
            .next()
            .expect("should have a saved consent entry");
        assert_eq!(
            entry.raw_tc_string.as_deref(),
            Some("CPXxGfAPXxGfA"),
            "should write the raw TC string from cookies into the KV entry"
        );
    }
}
