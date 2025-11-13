//! Permutive SDK proxy for serving the real Permutive SDK through Trusted Server.
//!
//! This module handles fetching the Permutive SDK from their CDN and serving it
//! through the first-party domain, with caching support.

use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};

use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Handles requests for the Permutive SDK bundle.
///
/// This function:
/// 1. Checks if Permutive is configured
/// 2. Fetches the real SDK from Permutive's CDN
/// 3. Caches it (future: in KV store)
/// 4. Returns it with proper headers for browser caching
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - Permutive is not configured
/// - SDK fetch fails
/// - Backend communication fails
pub async fn handle_permutive_sdk(
    settings: &Settings,
    _req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Handling Permutive SDK request");

    // Get SDK URL from settings
    let sdk_url = settings.permutive.sdk_url().ok_or_else(|| {
        TrustedServerError::PermutiveSdk {
            message: "Permutive SDK URL not configured. Set organization_id and workspace_id in settings.".to_string(),
        }
    })?;

    log::info!("Fetching Permutive SDK from: {}", sdk_url);

    // TODO: Check KV store cache first (Phase 3)
    // For now, always fetch from origin

    // Fetch SDK from Permutive CDN
    let mut permutive_req = Request::new(Method::GET, &sdk_url);
    
    // Set headers
    permutive_req.set_header(header::USER_AGENT, "TrustedServer/1.0");
    permutive_req.set_header(header::ACCEPT, "application/javascript, */*");

    // Determine backend name from URL
    let backend_name = ensure_backend_from_url(&sdk_url)?;

    // Fetch from Permutive
    let mut permutive_response = permutive_req
        .send(backend_name)
        .change_context(TrustedServerError::PermutiveSdk {
            message: format!("Failed to fetch Permutive SDK from {}", sdk_url),
        })?;

    // Check if fetch was successful
    if !permutive_response.get_status().is_success() {
        log::error!(
            "Permutive SDK fetch failed with status: {}",
            permutive_response.get_status()
        );
        return Err(Report::new(TrustedServerError::PermutiveSdk {
            message: format!(
                "Permutive SDK returned error status: {}",
                permutive_response.get_status()
            ),
        }));
    }

    // Get the SDK body
    let sdk_body = permutive_response.take_body_bytes();

    log::info!(
        "Successfully fetched Permutive SDK: {} bytes",
        sdk_body.len()
    );

    // TODO: Cache in KV store (Phase 3)

    // Return SDK with proper caching headers
    let cache_ttl = settings.permutive.cache_ttl_seconds;
    
    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "application/javascript; charset=utf-8")
        .with_header(
            header::CACHE_CONTROL,
            format!("public, max-age={}, immutable", cache_ttl),
        )
        .with_header("X-Permutive-SDK-Proxy", "true")
        .with_header("X-SDK-Source", &sdk_url)
        .with_body(sdk_body))
}

#[cfg(test)]
mod tests {
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_sdk_url_generation() {
        let settings = create_test_settings();
        let sdk_url = settings.permutive.sdk_url();
        
        assert!(sdk_url.is_some());
        assert_eq!(
            sdk_url.unwrap(),
            "https://testorg.edge.permutive.app/test-workspace-web.js"
        );
    }

    #[test]
    fn test_sdk_url_missing_config() {
        let mut settings = create_test_settings();
        settings.permutive.organization_id = String::new();
        
        let sdk_url = settings.permutive.sdk_url();
        assert!(sdk_url.is_none());
    }
}
