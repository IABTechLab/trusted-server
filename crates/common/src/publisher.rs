use error_stack::Report;
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};

use crate::constants::{
    HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_COMPRESS_HINT,
    HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};
use crate::cookies::create_synthetic_cookie;
use crate::error::TrustedServerError;
use crate::gdpr::{get_consent_from_request, GdprConsent};
use crate::geo::get_dma_code;
use crate::settings::Settings;
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
use crate::templates::{EDGEPUBS_TEMPLATE, HTML_TEMPLATE};

/// Handles the main page request.
///
/// Serves the main page with synthetic ID generation and ad integration.
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - Synthetic ID generation fails
/// - Response creation fails
pub fn handle_main_page(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!(
        "Using ad_partner_url: {}, counter_store: {}",
        settings.ad_server.ad_partner_url,
        settings.synthetic.counter_store,
    );

    // Add DMA code check to main page as well
    let dma_code = get_dma_code(&mut req);
    log::info!("Main page - DMA Code: {:?}", dma_code);

    // Check GDPR consent before proceeding
    let consent = match get_consent_from_request(&req) {
        Some(c) => c,
        None => {
            log::debug!("No GDPR consent found, using default");
            GdprConsent::default()
        }
    };
    if !consent.functional {
        // Return a version of the page without tracking
        return Ok(Response::from_status(StatusCode::OK)
            .with_body(
                HTML_TEMPLATE.replace("fetch('/prebid-test')", "console.log('Tracking disabled')"),
            )
            .with_header(header::CONTENT_TYPE, "text/html")
            .with_header(header::CACHE_CONTROL, "no-store, private"));
    }

    // Calculate fresh ID first using the synthetic module
    let fresh_id = generate_synthetic_id(settings, &req)?;

    // Check for existing Trusted Server ID in this specific order:
    // 1. X-Synthetic-Trusted-Server header
    // 2. Cookie
    // 3. Fall back to fresh ID
    let synthetic_id = get_or_generate_synthetic_id(settings, &req)?;

    log::info!(
        "Existing Trusted Server header: {:?}",
        req.get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
    );
    log::info!("Generated Fresh ID: {}", &fresh_id);
    log::info!("Using Trusted Server ID: {}", synthetic_id);

    // Create response with the main page HTML
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(HTML_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header(HEADER_SYNTHETIC_FRESH, fresh_id.as_str()) // Fresh ID always changes
        .with_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id) // Trusted Server ID remains stable
        .with_header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
        )
        .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .with_header("x-compress-hint", "on");

    // Copy geo headers from request to response
    for header_name in &[
        HEADER_X_GEO_CITY,
        HEADER_X_GEO_COUNTRY,
        HEADER_X_GEO_CONTINENT,
        HEADER_X_GEO_COORDINATES,
        HEADER_X_GEO_METRO_CODE,
        HEADER_X_GEO_INFO_AVAILABLE,
    ] {
        if let Some(value) = req.get_header(header_name) {
            response.set_header(header_name, value);
        }
    }

    // Only set cookies if we have consent
    if consent.functional {
        response.set_header(
            header::SET_COOKIE,
            create_synthetic_cookie(settings, &synthetic_id),
        );
    }

    // Debug: Print all request headers
    log::info!("All Request Headers:");
    for (name, value) in req.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Debug: Print the response headers
    log::info!("Response Headers:");
    for (name, value) in response.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Prevent caching
    response.set_header(header::CACHE_CONTROL, "no-store, private");

    Ok(response)
}

/// Handles the EdgePubs page request.
///
/// Serves the EdgePubs landing page with integrated ad slots.
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if response creation fails.
pub fn handle_edgepubs_page(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Serving EdgePubs landing page");

    // log_fastly::init_simple("mylogs", Info);

    // Add DMA code check
    let dma_code = get_dma_code(&mut req);
    log::info!("EdgePubs page - DMA Code: {:?}", dma_code);

    // Check GDPR consent
    let _consent = match get_consent_from_request(&req) {
        Some(c) => c,
        None => {
            log::debug!("No GDPR consent found for EdgePubs page, using default");
            GdprConsent::default()
        }
    };

    // Generate synthetic ID for EdgePubs page
    let fresh_id = generate_synthetic_id(settings, &req)?;

    // Get or generate Trusted Server ID
    let trusted_server_id = get_or_generate_synthetic_id(settings, &req)?;

    // Create response with EdgePubs template
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(EDGEPUBS_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header(header::CACHE_CONTROL, "no-store, private")
        .with_header(HEADER_X_COMPRESS_HINT, "on");

    // Add synthetic ID headers
    response.set_header(HEADER_SYNTHETIC_FRESH, &fresh_id);
    response.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &trusted_server_id);

    // Add DMA code header if available
    if let Some(dma) = dma_code {
        response.set_header(HEADER_X_GEO_METRO_CODE, dma);
    }

    // Set synthetic ID cookie
    let cookie = create_synthetic_cookie(settings, &trusted_server_id);
    response.set_header(header::SET_COOKIE, cookie);

    Ok(response)
}
