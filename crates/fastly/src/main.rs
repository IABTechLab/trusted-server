use std::env;
use std::io::Read;

use fastly::geo::geo_lookup;
use fastly::http::{header, Method, StatusCode};
use fastly::KVStore;
use fastly::{Error, Request, Response};
use log_fastly::Logger;
use serde_json::json;

mod error;
use crate::error::to_error_response;

use trusted_server_common::constants::{
    HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_COMPRESS_HINT,
    HEADER_X_CONSENT_ADVERTISING, HEADER_X_FORWARDED_FOR, HEADER_X_GEO_CITY,
    HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};
use trusted_server_common::cookies::create_synthetic_cookie;
use trusted_server_common::gam::{
    handle_gam_custom_url, handle_gam_golden_url, handle_gam_passthrough, handle_gam_render, handle_gam_test, handle_server_side_ad,
};
// Note: TrustedServerError is used internally by the common crate
use trusted_server_common::gdpr::{
    get_consent_from_request, handle_consent_request, handle_data_subject_request, GdprConsent,
};
use trusted_server_common::models::AdResponse;
use trusted_server_common::prebid::PrebidRequest;
use trusted_server_common::privacy::PRIVACY_TEMPLATE;
use trusted_server_common::settings::Settings;
use trusted_server_common::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
use trusted_server_common::templates::{EDGEPUBS_TEMPLATE, GAM_TEST_TEMPLATE, HTML_TEMPLATE};
use trusted_server_common::why::WHY_TEMPLATE;

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    init_logger();

    // Log service version first
    log::info!(
        "FASTLY_SERVICE_VERSION: {}",
        std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    // Print Settings only once at the beginning
    let settings = match Settings::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            return Ok(to_error_response(e));
        }
    };
    log::info!("Settings {settings:?}");
    // Print User IP address immediately after Fastly Service Version
    let client_ip = req
        .get_client_ip_addr()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    log::info!("User IP: {}", client_ip);

    futures::executor::block_on(async {
        match (req.get_method(), req.get_path()) {
            (&Method::GET, "/") => handle_edgepubs_page(&settings, req),
            (&Method::GET, "/auburndao") => handle_main_page(&settings, req),
            (&Method::GET, "/ad-creative") => handle_ad_request(&settings, req),
            // Direct asset serving for partner domains (like auburndao.com approach)
            (&Method::GET, path) if is_partner_asset_path(path) => {
                handle_partner_asset(&settings, req).await
            }
            // Specific routes MUST come before catch-all patterns
            (&Method::GET, "/prebid-test") => handle_prebid_test(&settings, req).await,
            (&Method::GET, "/gam-test") => handle_gam_test(&settings, req).await,
            (&Method::GET, "/gam-golden-url") => handle_gam_golden_url(&settings, req).await,
            (&Method::POST, "/gam-test-custom-url") => handle_gam_custom_url(&settings, req).await,
            (&Method::GET, "/gam-render") => handle_gam_render(&settings, req).await,
            (&Method::POST, "/gam-passthrough") => handle_gam_passthrough(&settings, req).await,
            (&Method::GET, "/server-side-ad") => handle_server_side_ad(&settings, req).await,
            
            // Static asset endpoints with long cache
            (&Method::GET, "/static/styles.css") => handle_static_css(),
            (&Method::GET, "/static/app.js") => handle_static_js(),
            
            (&Method::GET, "/gam-test-page") => Ok(Response::from_status(StatusCode::OK)
                .with_body(GAM_TEST_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header("x-compress-hint", "on")),
            // Encoded path routing for domain proxying
            (&Method::GET, path) if is_encoded_path(path) => {
                handle_encoded_path(&settings, req).await
            }
            // GAM asset serving (separate from Equativ, checked after encoded paths)
            (&Method::GET, path) if is_text_response_path(path) => {
                handle_gam_asset(&settings, req).await
            },
            (&Method::GET, "/gdpr/consent") => handle_consent_request(&settings, req),
            (&Method::POST, "/gdpr/consent") => handle_consent_request(&settings, req),
            (&Method::GET, "/gdpr/data") => handle_data_subject_request(&settings, req),
            (&Method::DELETE, "/gdpr/data") => handle_data_subject_request(&settings, req),
            (&Method::GET, "/privacy-policy") => Ok(Response::from_status(StatusCode::OK)
                .with_body(PRIVACY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header(HEADER_X_COMPRESS_HINT, "on")),
            (&Method::GET, "/why-trusted-server") => Ok(Response::from_status(StatusCode::OK)
                .with_body(WHY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header(HEADER_X_COMPRESS_HINT, "on")),
            _ => Ok(Response::from_status(StatusCode::NOT_FOUND)
                .with_body("Not Found")
                .with_header(header::CONTENT_TYPE, "text/plain")
                .with_header(HEADER_X_COMPRESS_HINT, "on")),
        }
    })
}

fn get_dma_code(req: &mut Request) -> Option<String> {
    // Debug: Check if we're running in Fastly environment
    log::info!("Fastly Environment Check:");
    log::info!(
        "  FASTLY_POP: {}",
        std::env::var("FASTLY_POP").unwrap_or_else(|_| "not in Fastly".to_string())
    );
    log::info!(
        "  FASTLY_REGION: {}",
        std::env::var("FASTLY_REGION").unwrap_or_else(|_| "not in Fastly".to_string())
    );

    // Get detailed geo information using geo_lookup
    if let Some(geo) = req.get_client_ip_addr().and_then(geo_lookup) {
        log::info!("Geo Information Found:");

        // Set all available geo information in headers
        let city = geo.city();
        req.set_header(HEADER_X_GEO_CITY, city);
        log::info!("  City: {}", city);

        let country = geo.country_code();
        req.set_header(HEADER_X_GEO_COUNTRY, country);
        log::info!("  Country: {}", country);

        req.set_header(HEADER_X_GEO_CONTINENT, format!("{:?}", geo.continent()));
        log::info!("  Continent: {:?}", geo.continent());

        req.set_header(
            HEADER_X_GEO_COORDINATES,
            format!("{},{}", geo.latitude(), geo.longitude()),
        );
        log::info!("  Location: ({}, {})", geo.latitude(), geo.longitude());

        // Get and set the metro code (DMA)
        let metro_code = geo.metro_code();
        req.set_header(HEADER_X_GEO_METRO_CODE, metro_code.to_string());
        log::info!("Found DMA/Metro code: {}", metro_code);
        return Some(metro_code.to_string());
    } else {
        log::info!("No geo information available for the request");
        req.set_header(HEADER_X_GEO_INFO_AVAILABLE, "false");
    }

    // If no metro code is found, log all request headers for debugging
    log::info!("No DMA/Metro code found. All request headers:");
    for (name, value) in req.get_headers() {
        log::info!("  {}: {:?}", name, value);
    }

    None
}

/// Handles the EdgePubs page request.
///
/// Serves the EdgePubs landing page with integrated ad slots.
///
/// # Errors
///
/// Returns a Fastly [`Error`] if response creation fails.
fn handle_edgepubs_page(settings: &Settings, mut req: Request) -> Result<Response, Error> {
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
    let fresh_id = match generate_synthetic_id(settings, &req) {
        Ok(id) => id,
        Err(e) => return Ok(to_error_response(e)),
    };

    // Get or generate Trusted Server ID
    let trusted_server_id = match get_or_generate_synthetic_id(settings, &req) {
        Ok(id) => id,
        Err(e) => return Ok(to_error_response(e)),
    };

    // Create response with EdgePubs template - cache base HTML for 10 minutes
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(EDGEPUBS_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .with_header(header::CACHE_CONTROL, "public, max-age=600, s-maxage=600") // 10 min cache
        .with_header("ETag", &format!("\"edgepubs-v68-{}\"", fresh_id[..8].to_string())) // Version-based ETag
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

/// Check if the path is for an Equativ asset that should be served directly (like auburndao.com)
fn is_partner_asset_path(path: &str) -> bool {
    // Only handle Equativ/Smart AdServer assets for now
    path.contains("/diff/") ||          // Equativ assets
    path.ends_with(".png") ||           // Images
    path.ends_with(".jpg") ||           // Images
    path.ends_with(".gif") // Images
}

/// Handles direct asset serving for partner domains (like auburndao.com).
///
/// Fetches assets from original partner domains and serves them as first-party content.
/// This bypasses ad blockers and Safari ITP by making all assets appear to come from edgepubs.com.
///
/// # Errors
///
/// Returns a Fastly [`Error`] if asset fetching fails.
async fn handle_partner_asset(_settings: &Settings, req: Request) -> Result<Response, Error> {
    let path = req.get_path();
    println!("=== HANDLING PARTNER ASSET: {} ===", path);
    log::info!("Handling partner asset request: {}", path);

    // Only handle Equativ/Smart AdServer assets (matching auburndao.com approach)
    let (backend_name, original_host) = ("equativ_sascdn_backend", "creatives.sascdn.com");

    log::info!(
        "Serving asset from backend: {} (original host: {})",
        backend_name,
        original_host
    );

    // Construct full URL using the original host and path
    let full_url = format!("https://{}{}", original_host, path);
    log::info!("Fetching asset URL: {}", full_url);

    let mut asset_req = Request::new(req.get_method().clone(), &full_url);

    // Copy all headers from original request
    for (name, value) in req.get_headers() {
        asset_req.set_header(name, value);
    }

    // Set the Host header to the original domain for proper routing
    asset_req.set_header(header::HOST, original_host);

    // Send to appropriate backend
    match asset_req.send(backend_name) {
        Ok(mut response) => {
            // Match auburndao.com cache control exactly
            let cache_control = "max-age=31536000";

            // No content rewriting needed for Equativ assets (they're mostly images)
            // This matches the auburndao.com approach of serving assets directly

            // Match auburndao.com headers exactly - no modifications
            response.set_header(header::CACHE_CONTROL, cache_control);

            // Don't modify any other headers - keep them exactly as auburndao.com gets them

            println!("=== ASSET RESPONSE HEADERS FOR {} ===", path);
            for (name, value) in response.get_headers() {
                println!("  {}: {:?}", name, value);
            }

            // No special CORB handling needed for Equativ image assets

            log::info!(
                "Partner asset served successfully, cache-control: {}",
                cache_control
            );
            Ok(response)
        }
        Err(e) => {
            log::error!(
                "Error fetching partner asset from {} (original host: {}): {:?}",
                backend_name,
                original_host,
                e
            );
            Ok(Response::from_status(StatusCode::NOT_FOUND)
                .with_header(header::CONTENT_TYPE, "text/plain")
                .with_header("X-Original-Host", original_host)
                .with_header("X-Backend-Used", backend_name)
                .with_body(format!("Asset not found: Unable to fetch from {} (original: {})\\nPath: {}\\nError: {:?}", backend_name, original_host, path, e)))
        }
    }
}

/// Check if the path uses encoded domain routing
fn is_encoded_path(path: &str) -> bool {
    // Match our encoded path patterns (10-char hex codes)
    if let Some(first_segment) = path.strip_prefix('/').and_then(|p| p.split('/').next()) {
        first_segment.len() == 10 && first_segment.chars().all(|c| c.is_ascii_hexdigit())
    } else {
        false
    }
}

/// Check if the path is for a GAM asset (separate from Equativ)
fn is_text_response_path(path: &str) -> bool {
    // Broaden matching to catch all text content that might contain domain references

    // JavaScript and web assets
    path.ends_with(".js") ||
    path.ends_with(".css") ||
    path.ends_with(".html") ||
    path.ends_with(".htm") ||
    path.ends_with(".json") ||
    path.ends_with(".xml") ||

    // Google/GAM specific paths
    path.contains("/tag/js/") ||        // Google Tag Manager/GAM scripts
    path.contains("/pagead/") ||        // GAM ad serving and interactions
    path.contains("/gtag/js") ||        // Google Analytics/GAM gtag scripts
    path.contains("/gampad/") ||        // GAM ad requests (gampad/ads)
    path.contains("/bg/") ||            // GAM background scripts
    path.contains("/sodar") ||          // GAM traffic quality checks
    path.contains("/getconfig/") ||     // GAM configuration requests
    path.contains("/generate_204") ||   // GAM tracking pixels
    path.contains("/recaptcha/") ||     // reCAPTCHA requests
    path.contains("/static/topics/") || // GAM topics framework

    // Broader Google domain patterns
    path.contains("google") ||          // Any Google service
    path.contains("doubleclick") ||     // All DoubleClick variations
    path.contains("syndication") ||     // Google ad syndication

    // API and configuration endpoints
    path.contains("/api/") ||           // API responses may contain URLs
    path.contains("/config/") ||        // Configuration files
    path.contains("/ads/") ||           // Ad-related content

    // Default: apply rewriting to most text content to be comprehensive
    !path.ends_with(".png") && 
    !path.ends_with(".jpg") && 
    !path.ends_with(".jpeg") && 
    !path.ends_with(".gif") && 
    !path.ends_with(".ico") && 
    !path.ends_with(".woff") && 
    !path.ends_with(".woff2") && 
    !path.ends_with(".ttf") && 
    !path.ends_with(".eot") && 
    !path.ends_with(".svg") &&
    !path.ends_with(".mp4") &&
    !path.ends_with(".webm") &&
    !path.ends_with(".pdf")
}

/// Handles GAM asset serving (completely separate from Equativ)
/// Rewrite hardcoded URLs in content to use first-party proxy domains
fn rewrite_gam_urls(content: &str) -> String {
    log::info!("Starting precise domain rewriting with encoded paths...");

    let mut rewritten_content = content.to_string();
    let mut total_replacements = 0;

    // Define precise domain-to-encoded-path mappings (order matters - longest first to avoid conflicts)
    let domain_mappings = [
        // Specific subdomains FIRST (to avoid conflicts with broader matches)
        ("securepubads.g.doubleclick.net", "edgepubs.com/d4f8a2b1c3"),
        ("googleads.g.doubleclick.net", "edgepubs.com/f9a2d3e7c1"), 
        ("stats.g.doubleclick.net", "edgepubs.com/c7e1f4a9d2"),
        ("cm.g.doubleclick.net", "edgepubs.com/e3d8f2a5c9"),
        ("pagead2.googlesyndication.com", "edgepubs.com/a8f3d9e2c4"),
        ("tpc.googlesyndication.com", "edgepubs.com/b8d4e1f5a2"),
        ("safeframe.googlesyndication.com", "edgepubs.com/g2h7f9a4e1"),
        ("ep1.adtrafficquality.google", "edgepubs.com/a7e3f9d2c8"),
        ("ep2.adtrafficquality.google", "edgepubs.com/b4f7a8e1d5"),
        ("ep3.adtrafficquality.google", "edgepubs.com/c9a4f2e7b3"),
        
        // Main domains LAST (after all subdomains processed)
        ("googlesyndication.com", "edgepubs.com/e7f2a9c4d1"),
        ("googletagservices.com", "edgepubs.com/f3a8d9e2c7"),
        ("adtrafficquality.google", "edgepubs.com/d6c9f4a2e8"),
    ];

    // Process domains in order (specific subdomains first, then general domains)
    for (original_domain, encoded_path) in &domain_mappings {
        if rewritten_content.contains(original_domain) {
            let before_len = rewritten_content.len();
            
            // Replace with multiple URL patterns for thoroughness
            rewritten_content = rewritten_content
                .replace(&format!("https://{}", original_domain), &format!("https://{}", encoded_path))
                .replace(&format!("http://{}", original_domain), &format!("https://{}", encoded_path))
                .replace(&format!("//{}", original_domain), &format!("//{}", encoded_path))
                .replace(&format!("\"{}\"", original_domain), &format!("\"{}\"", encoded_path))
                .replace(&format!("'{}'", original_domain), &format!("'{}'", encoded_path));
            
            let after_len = rewritten_content.len();
            if before_len != after_len {
                total_replacements += 1;
                log::info!("Rewrote domain: {} -> {}", original_domain, encoded_path);
            }
        }
    }

    log::info!(
        "Precise domain rewriting complete. {} domains processed",
        total_replacements
    );

    rewritten_content
}

/// Handle requests to encoded domain paths
async fn handle_encoded_path(_settings: &Settings, req: Request) -> Result<Response, Error> {
    let path = req.get_path();
    log::info!("Handling encoded path request: {}", path);
    
    // Extract encoded domain and remaining path
    let (encoded_domain, backend_path) = if let Some(stripped) = path.strip_prefix('/') {
        if let Some((first, rest)) = stripped.split_once('/') {
            (first, format!("/{}", rest))
        } else {
            (stripped, "/".to_string())
        }
    } else {
        return Ok(Response::from_status(StatusCode::BAD_REQUEST)
            .with_body("Invalid encoded path"));
    };
    
    // Map encoded domains to backends (now including version 48 backends)
    let (backend_name, original_domain) = match encoded_domain {
        // Existing backends (version 47)
        "d4f8a2b1c3" => ("gam_backend", "securepubads.g.doubleclick.net"),
        "a8f3d9e2c4" => ("GAM_javascript_backend", "pagead2.googlesyndication.com"),
        "b8d4e1f5a2" => ("tpc_googlesyndication_backend", "tpc.googlesyndication.com"),
        "e7f2a9c4d1" => ("pagead2_googlesyndication_backend", "googlesyndication.com"),
        "f3a8d9e2c7" => ("GTS_services_backend", "googletagservices.com"),
        
        // New backends (version 48)
        "a7e3f9d2c8" => ("adtraffic_backend", "ep1.adtrafficquality.google"),
        "b4f7a8e1d5" => ("adtraffic_ep2_backend", "ep2.adtrafficquality.google"),
        "c9a4f2e7b3" => ("adtraffic_ep3_backend", "ep3.adtrafficquality.google"),
        "g2h7f9a4e1" => ("safeframe_backend", "safeframe.googlesyndication.com"),
        
        // Additional domains that might route to existing backends
        "f9a2d3e7c1" => ("gam_backend", "googleads.g.doubleclick.net"), // Route to gam_backend
        "c7e1f4a9d2" => ("gam_backend", "stats.g.doubleclick.net"), // Route to gam_backend  
        "e3d8f2a5c9" => ("gam_backend", "cm.g.doubleclick.net"), // Route to gam_backend
        "d6c9f4a2e8" => ("adtraffic_backend", "adtrafficquality.google"), // Route to adtraffic_backend
        
        // Fallback to default GAM backend for unknown codes
        _ => {
            log::warn!("Unknown encoded domain '{}', routing to default GAM backend", encoded_domain);
            ("gam_backend", "securepubads.g.doubleclick.net")
        }
    };
    
    log::info!(
        "Routing encoded path '{}' to backend '{}' ({}{})",
        encoded_domain, backend_name, original_domain, backend_path
    );
    
    // Construct the full target URL
    let mut target_url = format!("https://{}{}", original_domain, backend_path);
    if let Some(query) = req.get_url().query() {
        target_url = format!("{}?{}", target_url, query);
    }
    
    log::info!("Full target URL: {}", target_url);
    
    // Create backend request
    let mut backend_req = Request::new(req.get_method().clone(), &target_url);
    
    // Copy headers from original request
    for (name, value) in req.get_headers() {
        backend_req.set_header(name, value);
    }
    backend_req.set_header("host", original_domain);
    
    // Send request to backend
    match backend_req.send(backend_name) {
        Ok(mut response) => {
            log::info!(
                "Encoded path response: status={}, content-type={:?}",
                response.get_status(),
                response.get_header("content-type")
            );
            
            // Apply domain rewriting to response body if it's text content
            let content_type = response.get_header("content-type")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
                
            if content_type.contains("javascript") || content_type.contains("text") || backend_path.ends_with(".js") {
                // Handle compressed content properly (like original GAM asset handler)
                let body_bytes = response.take_body_bytes();
                
                // Check if content is compressed
                let decompressed_body = if response.get_header("content-encoding")
                    .and_then(|h| h.to_str().ok()) == Some("br") {
                    
                    log::info!("Detected Brotli compression in encoded path response, decompressing...");
                    let mut decompressed = Vec::new();
                    match brotli::Decompressor::new(&body_bytes[..], 4096)
                        .read_to_end(&mut decompressed) {
                        Ok(_) => decompressed,
                        Err(e) => {
                            log::error!("Failed to decompress encoded path response: {:?}", e);
                            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                                .with_body("Failed to decompress response"));
                        }
                    }
                } else {
                    body_bytes
                };
                
                // Convert to string safely
                match std::str::from_utf8(&decompressed_body) {
                    Ok(body_str) => {
                        // Debug GAM response content
                        log::info!("GAM encoded path response analysis:");
                        log::info!("  - Body length: {} chars", body_str.len());
                        log::info!("  - Contains 'creative': {}", body_str.contains("creative"));
                        log::info!("  - Contains 'img': {}", body_str.contains("img"));
                        log::info!("  - Contains 'src=': {}", body_str.contains("src="));
                        log::info!("  - Contains 'http': {}", body_str.contains("http"));
                        if body_str.len() > 400 {
                            log::info!("  - First 200 chars: {}", &body_str[0..200]);
                            log::info!("  - Last 200 chars: {}", &body_str[body_str.len()-200..]);
                        } else {
                            log::info!("  - Full content (short): {}", body_str);
                        }
                        
                        let rewritten_body = rewrite_gam_urls(body_str);
                        response.set_body(rewritten_body);
                        // Remove compression headers since we're serving uncompressed
                        response.remove_header("content-encoding");
                        response.remove_header("content-length");
                        log::info!("Applied domain rewriting to encoded path response");
                    }
                    Err(e) => {
                        log::error!("Encoded path response contains non-UTF-8 data: {:?}", e);
                        // Return original binary content without rewriting
                        response.set_body(decompressed_body);
                        log::warn!("Served encoded path response without domain rewriting (binary content)");
                    }
                }
            }
            
            Ok(response)
        }
        Err(e) => {
            log::error!("Error fetching encoded path from {}: {:?}", backend_name, e);
            Ok(Response::from_status(StatusCode::BAD_GATEWAY)
                .with_body(format!("Failed to fetch from encoded path: {}", encoded_domain)))
        }
    }
}

async fn handle_gam_asset(_settings: &Settings, req: Request) -> Result<Response, Error> {
    let path = req.get_path();
    println!("=== HANDLING GAM ASSET: {} ===", path);
    log::info!("Handling GAM asset request: {}", path);

    // Enhanced logging for GAM requests
    log::info!("GAM Asset Request Details:");
    log::info!("  - Path: {}", path);
    log::info!("  - Method: {}", req.get_method());
    log::info!("  - Full URL: {}", req.get_url());

    // Log all request headers for debugging
    log::info!("GAM Asset Request Headers:");
    for (name, value) in req.get_headers() {
        log::info!("  {}: {:?}", name, value);
    }

    // Log query parameters if any
    if let Some(query) = req.get_url().query() {
        log::info!("GAM Asset Query Parameters: {}", query);
    }

    // For domain-level proxying, we assume all GAM requests go to the main GAM backend
    // unless we detect specific patterns that need different backends
    let (backend_name, original_host, target_path) = if path.contains("/tag/js/test.js") {
        // Special case: our renamed test.js should map to the original gpt.js
        (
            "gam_backend",
            "securepubads.g.doubleclick.net",
            "/tag/js/gpt.js".to_string(),
        )
    } else if path.contains("/pagead/") && path.contains("googlesyndication") {
        (
            "pagead2_googlesyndication_backend",
            "pagead2.googlesyndication.com",
            path.to_string(),
        )
    } else {
        // Default: all other GAM requests go to main GAM backend with original path
        (
            "gam_backend",
            "securepubads.g.doubleclick.net",
            path.to_string(),
        )
    };

    log::info!(
        "Serving GAM asset from backend: {} (original host: {})",
        backend_name,
        original_host
    );

    // Construct full URL using the original host and target path (may be different from request path)
    let mut full_url = format!("https://{}{}", original_host, target_path);

    // Add query string if present
    if let Some(query) = req.get_url().query() {
        full_url.push('?');
        full_url.push_str(query);
    }

    // Special handling for /gampad/ads requests - rewrite URL parameter to use autoblog.com
    if target_path.contains("/gampad/ads") {
        log::info!("Applying URL parameter rewriting for GAM ad request");
        // Change url=https%3A%2F%2Fedgepubs.com%2F to url=https%3A%2F%2Fwww.autoblog.com%2F
        full_url = full_url.replace(
            "url=https%3A%2F%2Fedgepubs.com%2F",
            "url=https%3A%2F%2Fwww.autoblog.com%2F",
        );
        log::info!("Rewrote URL parameter from edgepubs.com to www.autoblog.com");
    }

    log::info!(
        "Fetching GAM asset URL: {} (original request: {})",
        full_url,
        path
    );

    let mut asset_req = Request::new(req.get_method().clone(), &full_url);

    // Copy all headers from original request
    for (name, value) in req.get_headers() {
        asset_req.set_header(name, value);
    }

    // Set the Host header to the original domain for proper routing
    asset_req.set_header(header::HOST, original_host);

    // Send to appropriate GAM backend
    log::info!(
        "Sending GAM asset request to backend '{}' for URL: {}",
        backend_name,
        full_url
    );
    match asset_req.send(backend_name) {
        Ok(mut response) => {
            log::info!(
                "Received GAM asset response: status={}, content-length={:?}",
                response.get_status(),
                response.get_header(header::CONTENT_LENGTH)
            );
            // Check if this is a JavaScript response that needs content rewriting
            let content_type = response
                .get_header(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");

            // Enable rewriting for JavaScript content
            let needs_rewriting = content_type.contains("javascript") || path.contains(".js");
            log::info!(
                "Content rewriting enabled for JavaScript: {}",
                needs_rewriting
            );

            log::info!(
                "GAM asset content-type: {}, needs_rewriting: {}",
                content_type,
                needs_rewriting
            );

            if needs_rewriting {
                // Step 1: Capture original content-disposition header before processing
                let original_content_disposition = response
                    .get_header("content-disposition")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string());

                log::info!(
                    "Captured original content-disposition: {:?}",
                    original_content_disposition
                );

                // Step 2: Remove content-disposition header so we can process the response properly
                response.remove_header("content-disposition");
                log::info!("Step 2: Temporarily removed content-disposition header for processing");

                // Get the response body as bytes first
                let body_bytes = response.take_body_bytes();
                let original_length = body_bytes.len();

                log::info!(
                    "Original GAM JavaScript body length: {} bytes",
                    original_length
                );

                // Check if we need to decompress Brotli data
                let decompressed_body = if response
                    .get_header("content-encoding")
                    .and_then(|h| h.to_str().ok())
                    == Some("br")
                {
                    log::info!("Detected Brotli compression, decompressing...");
                    let mut decompressed = Vec::new();
                    match brotli::Decompressor::new(&body_bytes[..], 4096)
                        .read_to_end(&mut decompressed)
                    {
                        Ok(bytes_read) => {
                            log::info!(
                                "Successfully decompressed {} bytes to {} bytes",
                                original_length,
                                bytes_read
                            );
                            decompressed
                        }
                        Err(e) => {
                            log::error!("Failed to decompress Brotli data: {:?}", e);
                            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                                .with_body(format!("Failed to decompress GAM asset: {:?}", e)));
                        }
                    }
                } else {
                    log::info!("No compression detected, using raw bytes");
                    body_bytes
                };

                // Now safely convert decompressed data to string
                let body = match std::str::from_utf8(&decompressed_body) {
                    Ok(body_str) => {
                        log::info!("Successfully decoded decompressed response as UTF-8");
                        body_str.to_string()
                    }
                    Err(e) => {
                        log::error!("Decompressed response contains non-UTF-8 data: {:?}", e);
                        return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                            .with_body(format!(
                                "Invalid UTF-8 in decompressed GAM asset: {:?}",
                                e
                            )));
                    }
                };

                // Log a sample of the original content for debugging (safely)
                let sample = if body.len() > 200 {
                    // Find a safe character boundary near 200 bytes
                    match body.char_indices().nth(100) {
                        // Get first ~100 characters instead of 200 bytes
                        Some((byte_idx, _)) => &body[..byte_idx],
                        None => &body[..std::cmp::min(50, body.len())], // Fallback to very short sample
                    }
                } else {
                    &body
                };
                log::info!("Original content sample: {}", sample);

                // Rewrite hardcoded URLs to use first-party proxy (now applied to all text content)
                let rewritten_body = rewrite_gam_urls(&body);
                let rewritten_length = rewritten_body.len();

                log::info!(
                    "Rewritten GAM JavaScript body length: {} bytes (diff: {})",
                    rewritten_length,
                    rewritten_length as i32 - original_length as i32
                );

                // Log a sample of the rewritten content for comparison (safely)
                if rewritten_length != original_length {
                    let rewritten_sample = if rewritten_body.len() > 200 {
                        // Find a safe character boundary near 200 bytes
                        match rewritten_body.char_indices().nth(100) {
                            Some((byte_idx, _)) => &rewritten_body[..byte_idx],
                            None => &rewritten_body[..std::cmp::min(50, rewritten_body.len())],
                        }
                    } else {
                        &rewritten_body
                    };
                    log::info!("Rewritten content sample: {}", rewritten_sample);
                } else {
                    log::warn!(
                        "No content changes detected - rewriting may not have found target URLs"
                    );
                }

                // Create new response with rewritten content
                let mut new_response = Response::from_status(response.get_status());

                // Copy headers from original response, but filter out problematic ones
                for (name, value) in response.get_headers() {
                    let header_name = name.as_str().to_lowercase();
                    // Skip headers that would cause problems with rewritten content
                    if header_name == "content-disposition" {
                        log::info!("Skipping problematic header: {}: {:?}", name, value);
                        continue;
                    }
                    // Skip compression headers since our rewritten content is uncompressed
                    if header_name == "content-encoding" || header_name == "content-length" {
                        log::info!(
                            "Skipping compression header for rewritten content: {}: {:?}",
                            name,
                            value
                        );
                        continue;
                    }
                    new_response.set_header(name, value);
                }

                // Set rewritten content (uncompressed to avoid timeout issues)
                // Note: compression headers are already filtered out above
                new_response.set_body(rewritten_body);
                log::info!("Sending rewritten content uncompressed to avoid Fastly timeout limits");

                // Disable caching during development/debugging
                let cache_control = "no-cache, no-store, must-revalidate";
                new_response.set_header(header::CACHE_CONTROL, cache_control);
                new_response.set_header("Pragma", "no-cache");
                new_response.set_header("Expires", "0");

                // Step 3: Restore original content-disposition header if it existed
                if let Some(original_disposition) = original_content_disposition {
                    new_response.set_header("content-disposition", &original_disposition);
                    log::info!(
                        "Step 3: Restored original content-disposition header: {}",
                        original_disposition
                    );
                }

                // Add debug headers
                new_response.set_header("X-Content-Rewritten", "true");
                new_response.set_header("X-Original-Length", &original_length.to_string());
                new_response.set_header("X-Rewritten-Length", &rewritten_length.to_string());

                println!(
                    "=== GAM ASSET RESPONSE HEADERS FOR {} (REWRITTEN) ===",
                    path
                );
                log::info!("GAM Asset Response Headers (Rewritten):");
                for (name, value) in new_response.get_headers() {
                    println!("  {}: {:?}", name, value);
                    log::info!("  {}: {:?}", name, value);
                }

                log::info!("GAM JavaScript asset rewritten and served successfully");
                Ok(new_response)
            } else {
                // No rewriting needed, serve as-is but disable caching for debugging
                let cache_control = "no-cache, no-store, must-revalidate";
                response.set_header(header::CACHE_CONTROL, cache_control);
                response.set_header("Pragma", "no-cache");
                response.set_header("Expires", "0");

                println!("=== GAM ASSET RESPONSE HEADERS FOR {} ===", path);
                log::info!("GAM Asset Response Headers (No Rewriting):");
                for (name, value) in response.get_headers() {
                    println!("  {}: {:?}", name, value);
                    log::info!("  {}: {:?}", name, value);
                }

                log::info!(
                    "GAM asset served successfully, cache-control: {}",
                    cache_control
                );
                Ok(response)
            }
        }
        Err(e) => {
            log::error!(
                "Error fetching GAM asset from {} (original host: {}): {:?}",
                backend_name,
                original_host,
                e
            );
            log::error!("GAM Asset Error Details:");
            log::error!("  - Backend: {}", backend_name);
            log::error!("  - Original Host: {}", original_host);
            log::error!("  - Full URL: {}", full_url);
            log::error!("  - Path: {}", path);
            log::error!("  - Error: {:?}", e);

            println!("=== GAM ASSET ERROR DETAILS ===");
            println!("Backend: {}", backend_name);
            println!("Original Host: {}", original_host);
            println!("Full URL: {}", full_url);
            println!("Error: {:?}", e);
            Ok(Response::from_status(StatusCode::NOT_FOUND)
                .with_header(header::CONTENT_TYPE, "text/plain")
                .with_header("X-Original-Host", original_host)
                .with_header("X-Backend-Used", backend_name)
                .with_header("X-Full-URL", &full_url)
                .with_body(format!("GAM Asset not found: Unable to fetch from {} (original: {})\\nPath: {}\\nFull URL: {}\\nError: {:?}", backend_name, original_host, path, full_url, e)))
        }
    }
}

/// Handles the main page request.
///
/// Serves the main page with synthetic ID generation and ad integration.
///
/// # Errors
///
/// Returns a Fastly [`Error`] if response creation fails.
fn handle_main_page(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    log::info!(
        "Using ad_partner_url: {}, counter_store: {}",
        settings.ad_server.ad_partner_url,
        settings.synthetic.counter_store,
    );

    // log_fastly::init_simple("mylogs", Info);

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
    let fresh_id = match generate_synthetic_id(settings, &req) {
        Ok(id) => id,
        Err(e) => return Ok(to_error_response(e)),
    };

    // Check for existing Trusted Server ID in this specific order:
    // 1. X-Synthetic-Trusted-Server header
    // 2. Cookie
    // 3. Fall back to fresh ID
    let synthetic_id = match get_or_generate_synthetic_id(settings, &req) {
        Ok(id) => id,
        Err(e) => return Ok(to_error_response(e)),
    };

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
        "X-Geo-City",
        "X-Geo-Country",
        "X-Geo-Continent",
        "X-Geo-Coordinates",
        "X-Geo-Metro-Code",
        "X-Geo-Info-Available",
    ] {
        if let Some(value) = req.get_header(*header_name) {
            response.set_header(*header_name, value);
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

/// Handles ad creative requests.
///
/// Processes ad requests with synthetic ID and consent checking.
///
/// # Errors
///
/// Returns a Fastly [`Error`] if response creation fails.
fn handle_ad_request(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    // Check GDPR consent to determine if we should serve personalized or non-personalized ads
    let _consent = match get_consent_from_request(&req) {
        Some(c) => c,
        None => {
            log::debug!("No GDPR consent found in ad request, using default");
            GdprConsent::default()
        }
    };
    let advertising_consent = req
        .get_header(HEADER_X_CONSENT_ADVERTISING)
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Add DMA code extraction
    let dma_code = get_dma_code(&mut req);

    log::info!("Client location - DMA Code: {:?}", dma_code);

    // Log headers for debugging
    let client_ip = req
        .get_client_ip_addr()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let x_forwarded_for = req
        .get_header(HEADER_X_FORWARDED_FOR)
        .map(|h| h.to_str().unwrap_or("Unknown"));

    log::info!("Client IP: {}", client_ip);
    log::info!("X-Forwarded-For: {}", x_forwarded_for.unwrap_or("None"));
    log::info!("Advertising consent: {}", advertising_consent);

    // Generate synthetic ID only if we have consent
    let synthetic_id = if advertising_consent {
        match generate_synthetic_id(settings, &req) {
            Ok(id) => id,
            Err(e) => return Ok(to_error_response(e)),
        }
    } else {
        // Use a generic ID for non-personalized ads
        "non-personalized".to_string()
    };

    // Only track visits if we have consent
    if advertising_consent {
        // Increment visit counter in KV store
        log::info!("Opening KV store: {}", settings.synthetic.counter_store);
        if let Ok(Some(store)) = KVStore::open(settings.synthetic.counter_store.as_str()) {
            log::info!("Fetching current count for synthetic ID: {}", synthetic_id);
            let current_count: i32 = store
                .lookup(&synthetic_id)
                .map(|mut val| match String::from_utf8(val.take_body_bytes()) {
                    Ok(s) => {
                        log::info!("Value from KV store: {}", s);
                        Some(s)
                    }
                    Err(e) => {
                        log::error!("Error converting bytes to string: {}", e);
                        None
                    }
                })
                .map(|opt_s| {
                    log::info!("Parsing string value: {:?}", opt_s);
                    opt_s.and_then(|s| s.parse().ok())
                })
                .unwrap_or_else(|_| {
                    log::info!("No existing count found, starting at 0");
                    None
                })
                .unwrap_or(0);

            let new_count = current_count + 1;
            log::info!("Incrementing count from {} to {}", current_count, new_count);

            if let Err(e) = store.insert(&synthetic_id, new_count.to_string().as_bytes()) {
                log::error!("Error updating KV store: {:?}", e);
            }
        }
    }

    // Modify the ad server URL construction to include DMA code if available
    let ad_server_url = if advertising_consent {
        let mut url = settings
            .ad_server
            .sync_url
            .replace("{{synthetic_id}}", &synthetic_id);
        if let Some(dma) = dma_code {
            url = format!("{}&dma={}", url, dma);
        }
        url
    } else {
        // Use a different URL or parameter for non-personalized ads
        settings
            .ad_server
            .sync_url
            .replace("{{synthetic_id}}", "non-personalized")
    };

    log::info!("Sending request to backend: {}", ad_server_url);

    // Add header logging here
    let mut ad_req = Request::get(ad_server_url);

    // Add consent information to the ad request
    ad_req.set_header(
        HEADER_X_CONSENT_ADVERTISING,
        if advertising_consent { "true" } else { "false" },
    );

    log::info!("Request headers to Equativ:");
    for (name, value) in ad_req.get_headers() {
        log::info!("  {}: {:?}", name, value);
    }

    match ad_req.send(settings.ad_server.ad_partner_url.as_str()) {
        Ok(mut res) => {
            log::info!(
                "Received response from backend with status: {}",
                res.get_status()
            );

            // Extract Fastly PoP from the Compute environment
            let fastly_pop = env::var("FASTLY_POP").unwrap_or_else(|_| "unknown".to_string());
            let fastly_cache_generation =
                env::var("FASTLY_CACHE_GENERATION").unwrap_or_else(|_| "unknown".to_string());
            let fastly_customer_id =
                env::var("FASTLY_CUSTOMER_ID").unwrap_or_else(|_| "unknown".to_string());
            let fastly_hostname =
                env::var("FASTLY_HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
            let fastly_region = env::var("FASTLY_REGION").unwrap_or_else(|_| "unknown".to_string());
            let fastly_service_id =
                env::var("FASTLY_SERVICE_ID").unwrap_or_else(|_| "unknown".to_string());
            let fastly_trace_id =
                env::var("FASTLY_TRACE_ID").unwrap_or_else(|_| "unknown".to_string());

            log::info!("Fastly POP: {}", fastly_pop);
            log::info!("Fastly Compute Variables:");
            log::info!("  - FASTLY_CACHE_GENERATION: {}", fastly_cache_generation);
            log::info!("  - FASTLY_CUSTOMER_ID: {}", fastly_customer_id);
            log::info!("  - FASTLY_HOSTNAME: {}", fastly_hostname);
            log::info!("  - FASTLY_POP: {}", fastly_pop);
            log::info!("  - FASTLY_REGION: {}", fastly_region);
            log::info!("  - FASTLY_SERVICE_ID: {}", fastly_service_id);
            //log::info!("  - FASTLY_SERVICE_VERSION: {}", fastly_service_version);
            log::info!("  - FASTLY_TRACE_ID: {}", fastly_trace_id);

            // Log all response headers
            log::info!("Response headers from Equativ:");
            for (name, value) in res.get_headers() {
                log::info!("  {}: {:?}", name, value);
            }

            if res.get_status().is_success() {
                let body = res.take_body_str();
                log::info!("Backend response body: {}", body);

                // Parse the JSON response and extract opid
                if let Ok(ad_response) = serde_json::from_str::<AdResponse>(&body) {
                    // Look for the callback with type "impression"
                    if let Some(callback) = ad_response
                        .callbacks
                        .iter()
                        .find(|c| c.callback_type == "impression")
                    {
                        // Extract opid from the URL
                        if let Some(opid) = callback
                            .url
                            .split('&')
                            .find(|&param| param.starts_with("opid="))
                            .and_then(|param| param.split('=').nth(1))
                        {
                            log::info!("Found opid: {}", opid);

                            // Store in opid KV store
                            log::info!(
                                "Attempting to open KV store: {}",
                                settings.synthetic.opid_store
                            );
                            match KVStore::open(settings.synthetic.opid_store.as_str()) {
                                Ok(Some(store)) => {
                                    log::info!("Successfully opened KV store");
                                    match store.insert(&synthetic_id, opid.as_bytes()) {
                                        Ok(_) => log::info!(
                                            "Successfully stored opid {} for synthetic ID: {}",
                                            opid,
                                            synthetic_id
                                        ),
                                        Err(e) => {
                                            log::error!("Error storing opid in KV store: {:?}", e)
                                        }
                                    }
                                }
                                Ok(None) => {
                                    log::warn!(
                                        "KV store returned None: {}",
                                        settings.synthetic.opid_store
                                    );
                                }
                                Err(e) => {
                                    log::error!(
                                        "Error opening KV store '{}': {:?}",
                                        settings.synthetic.opid_store,
                                        e
                                    );
                                }
                            };
                        }
                    }
                }

                // Return the JSON response with CORS headers - NO CACHE for ad content
                let mut response = Response::from_status(StatusCode::OK)
                    .with_header(header::CONTENT_TYPE, "application/json; charset=utf-8")
                    .with_header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate, private")
                    .with_header("Pragma", "no-cache") // HTTP/1.0 compatibility
                    .with_header("Expires", "0") // Prevent any proxy caching
                    .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .with_header(
                        header::ACCESS_CONTROL_EXPOSE_HEADERS,
                        "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
                    )
                    .with_header(HEADER_X_COMPRESS_HINT, "on")
                    .with_body(body);

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

                Ok(response)
            } else {
                log::warn!("Backend returned non-success status");
                Ok(Response::from_status(StatusCode::NO_CONTENT)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header(HEADER_X_COMPRESS_HINT, "on")
                    .with_body("{}"))
            }
        }
        Err(e) => {
            log::error!("Error making backend request: {:?}", e);
            Ok(Response::from_status(StatusCode::NO_CONTENT)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header(HEADER_X_COMPRESS_HINT, "on")
                .with_body("{}"))
        }
    }
}

/// Handles the prebid test route with detailed error logging
async fn handle_prebid_test(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    log::info!("Starting prebid test request handling");

    // Check consent status from headers
    let advertising_consent = req
        .get_header(HEADER_X_CONSENT_ADVERTISING)
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Calculate fresh ID and synthetic ID only if we have advertising consent
    let (fresh_id, synthetic_id) = if advertising_consent {
        match (
            generate_synthetic_id(settings, &req),
            get_or_generate_synthetic_id(settings, &req),
        ) {
            (Ok(fresh), Ok(synth)) => (fresh, synth),
            (Err(e), _) | (_, Err(e)) => {
                log::error!("Failed to generate IDs: {:?}", e);
                return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_body_json(&json!({
                        "error": "Failed to generate IDs",
                        "details": format!("{:?}", e)
                    }))?);
            }
        }
    } else {
        // Use non-personalized IDs when no consent
        (
            "non-personalized".to_string(),
            "non-personalized".to_string(),
        )
    };

    log::info!(
        "Existing Trusted Server header: {:?}",
        req.get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
    );
    log::info!("Generated Fresh ID: {}", &fresh_id);
    log::info!("Using Trusted Server ID: {}", synthetic_id);
    log::info!("Advertising consent: {}", advertising_consent);

    // Set both IDs as headers
    req.set_header(HEADER_SYNTHETIC_FRESH, &fresh_id);
    req.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id);
    req.set_header(
        HEADER_X_CONSENT_ADVERTISING,
        if advertising_consent { "true" } else { "false" },
    );

    log::info!(
        "Using Trusted Server ID: {}, Fresh ID: {}",
        synthetic_id,
        fresh_id
    );

    let prebid_req = match PrebidRequest::new(settings, &req) {
        Ok(req) => {
            log::info!(
                "Successfully created PrebidRequest with synthetic ID: {}",
                req.synthetic_id
            );
            req
        }
        Err(e) => {
            log::error!("Error creating PrebidRequest: {:?}", e);
            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to create prebid request",
                    "details": format!("{:?}", e)
                }))?);
        }
    };

    log::info!("Attempting to send bid request to Prebid Server at prebid_backend");

    match prebid_req.send_bid_request(settings, &req).await {
        Ok(mut prebid_response) => {
            log::info!("Received response from Prebid Server");
            log::info!("Response status: {}", prebid_response.get_status());

            log::info!("Response headers:");
            for (name, value) in prebid_response.get_headers() {
                log::info!("  {}: {:?}", name, value);
            }

            let body = prebid_response.take_body_str();
            log::info!("Response body: {}", body);

            Ok(Response::from_status(StatusCode::OK)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header("X-Prebid-Test", "true")
                .with_header("X-Synthetic-ID", &prebid_req.synthetic_id)
                .with_header(
                    "X-Consent-Advertising",
                    if advertising_consent { "true" } else { "false" },
                )
                .with_header(HEADER_X_COMPRESS_HINT, "on")
                .with_body(body))
        }
        Err(e) => {
            log::error!("Error sending bid request: {:?}", e);
            log::error!("Backend name used: prebid_backend");
            Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to send bid request",
                    "details": format!("{:?}", e),
                    "backend": "prebid_backend"
                }))?)
        }
    }
}
fn init_logger() {
    let logger = Logger::builder()
        .default_endpoint("tslog")
        .echo_stdout(true)
        .max_level(log::LevelFilter::Debug)
        .build()
        .expect("Failed to build Logger");

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}  {} {}",
                chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                record.level(),
                message
            ))
        })
        .chain(Box::new(logger) as Box<dyn log::Log>)
        .apply()
        .expect("Failed to initialize logger");
}

/// Handle static CSS with long cache
fn handle_static_css() -> Result<Response, Error> {
    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .with_header(header::CACHE_CONTROL, "public, max-age=86400, s-maxage=86400") // 24 hour cache
        .with_header("ETag", "\"css-v68\"")
        .with_header(HEADER_X_COMPRESS_HINT, "on")
        .with_body(include_str!("../../../static/styles.css")))
}

/// Handle static JS with long cache  
fn handle_static_js() -> Result<Response, Error> {
    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "application/javascript; charset=utf-8")
        .with_header(header::CACHE_CONTROL, "public, max-age=86400, s-maxage=86400") // 24 hour cache
        .with_header("ETag", "\"js-v68\"")
        .with_header(HEADER_X_COMPRESS_HINT, "on")
        .with_body(include_str!("../../../static/app.js")))
}

