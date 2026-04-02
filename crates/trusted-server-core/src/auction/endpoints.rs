//! HTTP endpoint handlers for auction requests.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};

use crate::auction::formats::AdRequest;
use crate::consent::gate_eids_by_consent;
use crate::ec::eids::{resolve_partner_ids, to_eids};
use crate::ec::kv::KvIdentityGraph;
use crate::ec::partner::PartnerStore;
use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::openrtb::Eid;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::formats::{convert_to_openrtb_response, convert_tsjs_to_auction_request};
use super::types::AuctionContext;
use super::AuctionOrchestrator;

/// Handle auction request from /auction endpoint.
///
/// This is the main entry point for running header bidding auctions.
/// It orchestrates bids from multiple providers (Prebid, APS, GAM, etc.) and returns
/// the winning bids in `OpenRTB` format with creative HTML inline in the `adm` field.
///
/// # Errors
///
/// Returns an error if:
/// - The request body cannot be parsed
/// - The auction request conversion fails (e.g., invalid ad units)
/// - The auction execution fails
/// - The response cannot be serialized
pub async fn handle_auction(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    kv: Option<&KvIdentityGraph>,
    partner_store: Option<&PartnerStore>,
    ec_context: &EcContext,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Parse request body
    let body: AdRequest = serde_json::from_slice(&req.take_body_bytes()).change_context(
        TrustedServerError::Auction {
            message: "Failed to parse auction request body".to_string(),
        },
    )?;

    log::info!(
        "Auction request received for {} ad units",
        body.ad_units.len()
    );

    // Story 5 middleware contract: auction is a read-only EC route.
    // It must not generate EC IDs; it only consumes pre-routed context.
    // Only forward the EC ID to auction partners when consent allows it.
    // A returning user may still have a ts-ec cookie but have since
    // withdrawn consent — forwarding that revoked ID to bidders would
    // defeat the consent gating.
    let ec_id = if ec_context.ec_allowed() {
        ec_context.ec_value().unwrap_or("")
    } else {
        ""
    };
    let consent_context = ec_context.consent().clone();
    let geo = ec_context.geo_info().cloned();

    // Resolve partner EIDs from the KV identity graph when the user has
    // a valid EC and both KV and partner stores are available.
    let eids = resolve_auction_eids(kv, partner_store, ec_context);

    // Convert tsjs request format to auction request
    let mut auction_request = convert_tsjs_to_auction_request(
        &body,
        settings,
        services,
        &req,
        consent_context,
        ec_id,
        geo,
    )?;

    // Apply consent gating to the resolved EIDs before attaching them to the
    // auction request. `gate_eids_by_consent` checks TCF Purpose 1 + 4.
    let had_eids = eids.as_ref().is_some_and(|v| !v.is_empty());
    auction_request.user.eids = gate_eids_by_consent(eids, auction_request.user.consent.as_ref());
    if had_eids && auction_request.user.eids.is_none() {
        log::debug!("Auction EIDs stripped by TCF consent gating");
    }

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &req,
        client_info: &services.client_info,
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
    convert_to_openrtb_response(&result, settings, &auction_request, ec_context.ec_allowed())
}

/// Resolves partner EIDs from the KV identity graph for bidstream decoration.
///
/// Returns `None` when any prerequisite is missing (no KV store, no partner
/// store, no EC, consent denied). On KV or partner-resolution errors, logs a
/// warning and returns empty EIDs so the auction can proceed in degraded mode.
fn resolve_auction_eids(
    kv: Option<&KvIdentityGraph>,
    partner_store: Option<&PartnerStore>,
    ec_context: &EcContext,
) -> Option<Vec<Eid>> {
    let kv = kv?;
    let partner_store = partner_store?;

    if !ec_context.ec_allowed() {
        return None;
    }

    let ec_id = ec_context.ec_value()?;

    let entry = match kv.get(ec_id) {
        Ok(Some((entry, _generation))) => entry,
        Ok(None) => return Some(Vec::new()),
        Err(err) => {
            log::warn!("Auction KV read failed for EC ID '{ec_id}': {err:?}");
            return Some(Vec::new());
        }
    };

    match resolve_partner_ids(partner_store, &entry) {
        Ok(resolved) => Some(to_eids(&resolved)),
        Err(err) => {
            log::warn!("Auction partner resolution failed: {err:?}");
            Some(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentContext;

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        EcContext::new_for_test(
            ec_value.map(str::to_owned),
            ConsentContext {
                jurisdiction,
                ..ConsentContext::default()
            },
        )
    }

    #[test]
    fn resolve_auction_eids_returns_none_without_kv() {
        let partner_store = PartnerStore::new("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let result = resolve_auction_eids(None, Some(&partner_store), &ec_context);
        assert!(result.is_none(), "should return None when KV is missing");
    }

    #[test]
    fn resolve_auction_eids_returns_none_without_partner_store() {
        let kv = KvIdentityGraph::new("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let result = resolve_auction_eids(Some(&kv), None, &ec_context);
        assert!(
            result.is_none(),
            "should return None when partner store is missing"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_none_when_consent_denied() {
        let kv = KvIdentityGraph::new("test_store");
        let partner_store = PartnerStore::new("test_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::Unknown, Some(&ec_id));

        let result = resolve_auction_eids(Some(&kv), Some(&partner_store), &ec_context);
        assert!(
            result.is_none(),
            "should return None when consent is denied"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_none_when_no_ec() {
        let kv = KvIdentityGraph::new("test_store");
        let partner_store = PartnerStore::new("test_store");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let result = resolve_auction_eids(Some(&kv), Some(&partner_store), &ec_context);
        assert!(
            result.is_none(),
            "should return None when no EC value is present"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_empty_on_kv_miss() {
        let kv = KvIdentityGraph::new("nonexistent_store");
        let partner_store = PartnerStore::new("nonexistent_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        // KV store doesn't exist, so the get() call will error — should return
        // empty Vec (degraded mode), not None.
        let result = resolve_auction_eids(Some(&kv), Some(&partner_store), &ec_context);
        let eids = result.expect("should return Some on KV error (degraded mode)");
        assert!(
            eids.is_empty(),
            "should return empty vec on KV error (degraded mode)"
        );
    }
}
