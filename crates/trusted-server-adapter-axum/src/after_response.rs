//! Axum registration for the shared guarded identity lookup runner.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::after_response::AfterResponseHandle;
use edgezero_core::error::EdgeError;
use edgezero_core::outbound::HttpClient;
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::after_response::AfterResponseLookups;
use trusted_server_core::ec::app::IdentityApp;

/// Registers eligible identity work on the request's bounded after-response scheduler.
///
/// Returns whether a task was registered. No work or no finalized follow-up
/// permit registers nothing. Move the context here: cloning it discards permits.
/// The task begins outbound requests only after `EdgeZero` releases the response.
/// Its complete lifetime is bounded by the existing after-response identity
/// budget and the production host's remaining shutdown grace.
///
/// # Errors
///
/// Returns an error if eligible work has no scheduler or registration fails.
/// Callers log the error and preserve the already assembled response.
///
/// # Examples
///
/// ```ignore
/// schedule_lookups(handle, work, ec_context, identity, client)?;
/// ```
pub fn schedule_lookups(
    handle: Option<&AfterResponseHandle>,
    work: Option<AfterResponseLookups>,
    mut ec_context: EcContext,
    identity: Arc<IdentityApp>,
    client: HttpClient,
) -> Result<bool, EdgeError> {
    let Some(work) = work else {
        return Ok(false);
    };
    let Some(permit) = ec_context.take_after_response_permit() else {
        return Ok(false);
    };
    let handle =
        handle.ok_or_else(|| EdgeError::validation("after-response scheduler unavailable"))?;
    let budget = Duration::from_millis(identity.settings().ec.identity_after_response_budget_ms);
    let client = client.with_proxy_marker(false);
    handle.schedule(budget, async move {
        work.run(&ec_context, permit, &identity, &client).await;
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests;
