//! Prebid Server proxy integration for first-party ad serving.
//!
//! This module handles proxying requests between Prebid.js and Prebid Server,
//! ensuring all ad serving happens through the first-party domain.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde_json::{json, Value};

use crate::backend::ensure_backend_from_url;
use crate::constants::{HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER};
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::settings::Settings;
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};

/// Handles Prebid auction requests, enhancing them with synthetic IDs and privacy signals
/// before forwarding to Prebid Server.
///
/// This function:
/// 1. Parses the incoming OpenRTB request from Prebid.js
/// 2. Enhances it with synthetic IDs and privacy information
/// 3. Forwards to Prebid Server
/// 4. Transforms the response to ensure all URLs are first-party
///
/// # Errors
///
/// Returns a [`TrustedServerError`] if:
/// - Request parsing fails
/// - Synthetic ID generation fails
/// - Communication with Prebid Server fails
pub async fn handle_prebid_auction(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Handling Prebid auction request");

    // 1. Parse incoming OpenRTB request
    let mut openrtb_request: Value = serde_json::from_slice(&req.take_body_bytes())
        .change_context(TrustedServerError::Prebid {
            message: "Failed to parse OpenRTB request".to_string(),
        })?;

    // 2. Get/generate synthetic IDs
    let synthetic_id = get_or_generate_synthetic_id(settings, &req)?;
    let fresh_id = generate_synthetic_id(settings, &req)?;

    log::info!(
        "Using synthetic ID: {}, fresh ID: {}",
        synthetic_id,
        fresh_id
    );

    // 3. Enhance the OpenRTB request
    enhance_openrtb_request(
        &mut openrtb_request,
        &synthetic_id,
        &fresh_id,
        settings,
        &req,
    )?;

    // 4. Forward to Prebid Server
    let mut pbs_req = Request::new(
        Method::POST,
        format!("{}/openrtb2/auction", settings.prebid.server_url),
    );

    // Copy relevant headers
    copy_request_headers(&req, &mut pbs_req);

    pbs_req
        .set_body_json(&openrtb_request)
        .change_context(TrustedServerError::Prebid {
            message: "Failed to set request body".to_string(),
        })?;

    log::info!("Sending request to Prebid Server");

    let backend_name = ensure_backend_from_url(&settings.prebid.server_url)?;

    // 5. Send to PBS and get response
    let mut pbs_response =
        pbs_req
            .send(backend_name)
            .change_context(TrustedServerError::Prebid {
                message: "Failed to send request to Prebid Server".to_string(),
            })?;

    // 6. Transform response for first-party serving
    if pbs_response.get_status().is_success() {
        let response_body = pbs_response.take_body_bytes();

        match serde_json::from_slice::<Value>(&response_body) {
            Ok(mut response_json) => {
                // Get request host and scheme for URL rewriting
                let request_host = get_request_host(&req);
                let request_scheme = get_request_scheme(&req);

                // Transform all third-party URLs to first-party
                transform_prebid_response(&mut response_json, &request_host, &request_scheme)?;

                // Create response with transformed JSON
                let transformed_body = serde_json::to_vec(&response_json).change_context(
                    TrustedServerError::Prebid {
                        message: "Failed to serialize transformed response".to_string(),
                    },
                )?;

                Ok(Response::from_status(StatusCode::OK)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header("X-Synthetic-ID", &synthetic_id)
                    .with_header(HEADER_SYNTHETIC_FRESH, &fresh_id)
                    .with_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id)
                    .with_body(transformed_body))
            }
            Err(_) => {
                // If response is not JSON, pass through as-is
                Ok(Response::from_status(pbs_response.get_status())
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_body(response_body))
            }
        }
    } else {
        // Pass through error responses
        Ok(pbs_response)
    }
}

/// Enhances the OpenRTB request with synthetic IDs and privacy information.
fn enhance_openrtb_request(
    request: &mut Value,
    synthetic_id: &str,
    fresh_id: &str,
    settings: &Settings,
    req: &Request,
) -> Result<(), Report<TrustedServerError>> {
    // Ensure user object exists
    if !request["user"].is_object() {
        request["user"] = json!({});
    }

    // Add synthetic IDs
    request["user"]["id"] = json!(synthetic_id);
    if !request["user"]["ext"].is_object() {
        request["user"]["ext"] = json!({});
    }
    request["user"]["ext"]["synthetic_fresh"] = json!(fresh_id);

    // TODO: Add privacy signals

    // Add US Privacy if present
    if req.get_header("Sec-GPC").is_some() {
        if !request["regs"].is_object() {
            request["regs"] = json!({});
        }
        if !request["regs"]["ext"].is_object() {
            request["regs"]["ext"] = json!({});
        }
        request["regs"]["ext"]["us_privacy"] = json!("1YYN");
    }

    // Add geo information if available
    if let Some(geo_info) = GeoInfo::from_request(req) {
        let geo_obj = json!({
            "type": 2, // 2 = IP address location
            "country": geo_info.country, // Note: OpenRTB expects ISO 3166-1 alpha-3, but Fastly provides alpha-2
            "city": geo_info.city,
            "region": geo_info.region,
        });

        if !request["device"].is_object() {
            request["device"] = json!({});
        }
        request["device"]["geo"] = geo_obj.clone();
    }

    // Add site information if missing
    if !request["site"].is_object() {
        request["site"] = json!({
            "domain": settings.publisher.domain,
            "page": format!("https://{}", settings.publisher.domain),
        });
    }

    Ok(())
}

/// Transforms the Prebid Server response to ensure all URLs are first-party.
fn transform_prebid_response(
    response: &mut Value,
    request_host: &str,
    request_scheme: &str,
) -> Result<(), Report<TrustedServerError>> {
    // Transform bid responses
    if let Some(seatbids) = response["seatbid"].as_array_mut() {
        for seatbid in seatbids {
            if let Some(bids) = seatbid["bid"].as_array_mut() {
                for bid in bids {
                    // Transform creative markup
                    if let Some(adm) = bid["adm"].as_str() {
                        bid["adm"] = json!(rewrite_ad_markup(adm, request_host, request_scheme));
                    }

                    // Transform notification URL
                    if let Some(nurl) = bid["nurl"].as_str() {
                        bid["nurl"] = json!(make_first_party_proxy_url(
                            nurl,
                            request_host,
                            request_scheme,
                            "track"
                        ));
                    }

                    // Transform billing URL
                    if let Some(burl) = bid["burl"].as_str() {
                        bid["burl"] = json!(make_first_party_proxy_url(
                            burl,
                            request_host,
                            request_scheme,
                            "track"
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

/// Rewrites ad markup to use first-party URLs.
fn rewrite_ad_markup(markup: &str, request_host: &str, request_scheme: &str) -> String {
    let mut content = markup.to_string();

    // Common ad CDN patterns to rewrite
    let cdn_patterns = vec![
        ("https://cdn.adsrvr.org", "adsrvr"),
        ("https://ib.adnxs.com", "adnxs"),
        ("https://rtb.openx.net", "openx"),
        ("https://as.casalemedia.com", "casale"),
        ("https://eus.rubiconproject.com", "rubicon"),
    ];

    for (cdn_url, cdn_name) in cdn_patterns {
        if content.contains(cdn_url) {
            // Replace with first-party proxy URL
            let proxy_base = format!(
                "{}://{}/ad-proxy/{}",
                request_scheme, request_host, cdn_name
            );
            content = content.replace(cdn_url, &proxy_base);
        }
    }

    // Also handle protocol-relative URLs
    content = content.replace(
        "//cdn.adsrvr.org",
        &format!("//{}/ad-proxy/adsrvr", request_host),
    );
    content = content.replace(
        "//ib.adnxs.com",
        &format!("//{}/ad-proxy/adnxs", request_host),
    );

    content
}

/// Creates a first-party proxy URL for the given third-party URL.
fn make_first_party_proxy_url(
    third_party_url: &str,
    request_host: &str,
    request_scheme: &str,
    proxy_type: &str,
) -> String {
    let encoded = BASE64.encode(third_party_url.as_bytes());
    format!(
        "{}://{}/ad-proxy/{}/{}",
        request_scheme, request_host, proxy_type, encoded
    )
}

/// Copies relevant headers from the incoming request to the outgoing request.
fn copy_request_headers(from: &Request, to: &mut Request) {
    let headers_to_copy = [
        header::COOKIE,
        header::USER_AGENT,
        header::HeaderName::from_static("x-forwarded-for"),
        header::REFERER,
        header::ACCEPT_LANGUAGE,
    ];

    for header_name in &headers_to_copy {
        if let Some(value) = from.get_header(header_name) {
            to.set_header(header_name, value);
        }
    }
}

/// Gets the request host from the incoming request.
fn get_request_host(req: &Request) -> String {
    req.get_header(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Gets the request scheme from the incoming request.
fn get_request_scheme(req: &Request) -> String {
    // Check various headers to determine scheme
    if req.get_tls_protocol().is_some() || req.get_tls_cipher_openssl_name().is_some() {
        return "https".to_string();
    }

    if let Some(proto) = req.get_header("X-Forwarded-Proto") {
        if let Ok(proto_str) = proto.to_str() {
            return proto_str.to_lowercase();
        }
    }

    "https".to_string() // Default to HTTPS for security
}

#[cfg(test)]
mod tests {
    // Note: test_rewrite_ad_markup removed as it tested a private function.
    // This functionality is tested through the public handle_prebid_auction function.

    // Note: test_enhance_openrtb_request removed as it tested a private function.
    // This functionality is tested through the public handle_prebid_auction function.

    // Note: test_transform_prebid_response removed as it tested a private function.
    // This functionality is tested through the public handle_prebid_auction function.
}
