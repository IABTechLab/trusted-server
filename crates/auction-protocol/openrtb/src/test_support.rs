//! Helpers for tests of a demand implementation that speaks `OpenRTB`, in
//! this crate and in an implementation's own.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use http::Request;
use trusted_server_core::auction::AuctionPlan;
use trusted_server_core::auction::types::AuctionRequest;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::request_signing::RequestSigner;

use crate::wire::BidRequest;

/// Builds the request the first demand source of `plan` would send once
/// `request` is routed to it, read back as the bid request it is, or `None`
/// when the source keeps no impression.
///
/// A test in an implementation's own crate checks what its field policy and
/// its extensions do to a request through this. The signature, when `signer`
/// is given, is made over the signing input core's test support fixes.
///
/// # Errors
///
/// Returns the error the request builder returns, or an auction error when
/// the body the driver built does not read back as a bid request.
pub fn build_for_first_source(
    plan: &AuctionPlan,
    request: AuctionRequest,
    inbound: &Request<EdgeBody>,
    transport_timeout_ms: u32,
    signer: Option<&RequestSigner>,
) -> Result<Option<BidRequest>, Report<TrustedServerError>> {
    let Some(outbound) = trusted_server_core::auction::test_support::build_for_first_source(
        plan,
        request,
        inbound,
        transport_timeout_ms,
        signer,
    )?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&outbound.body)
        .map(Some)
        .change_context(TrustedServerError::Auction {
            message: "the body the driver built does not read back as a bid request".to_string(),
        })
}
