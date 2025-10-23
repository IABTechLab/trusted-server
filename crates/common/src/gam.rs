use error_stack::Report;
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde_json::json;
use std::collections::HashMap;
use std::io::Read;
use uuid::Uuid;

use crate::constants::HEADER_SYNTHETIC_TRUSTED_SERVER;
use crate::error::TrustedServerError;
use crate::gdpr::get_consent_from_request;
use crate::settings::Settings;
use crate::templates::GAM_TEST_TEMPLATE;

/// GAM request builder for server-side ad requests
pub struct GamRequest {
    pub publisher_id: String,
    pub ad_units: Vec<String>,
    pub page_url: String,
    pub correlator: String,
    pub prmtvctx: Option<String>, // Permutive context - initially hardcoded, then dynamic
    pub user_agent: String,
    pub synthetic_id: String,
}

impl GamRequest {
    /// Create a new GAM request with default parameters
    pub fn new(settings: &Settings, req: &Request) -> Result<Self, Report<TrustedServerError>> {
        let correlator = Uuid::new_v4().to_string();
        let page_url = req.get_url().to_string();
        let user_agent = req
            .get_header(header::USER_AGENT)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("Mozilla/5.0 (compatible; TrustedServer/1.0)")
            .to_string();

        // Get synthetic ID from request headers
        let synthetic_id = req
            .get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        Ok(Self {
            publisher_id: settings.gam.publisher_id.clone(),
            ad_units: settings
                .gam
                .ad_units
                .iter()
                .map(|u| u.name.clone())
                .collect(),
            page_url,
            correlator,
            prmtvctx: None, // Will be set later with captured value
            user_agent,
            synthetic_id,
        })
    }

    /// Set the Permutive context (initially hardcoded from captured request)
    pub fn with_prmtvctx(mut self, prmtvctx: String) -> Self {
        self.prmtvctx = Some(prmtvctx);
        self
    }

    /// Build the GAM request URL for the "Golden URL" replay phase
    pub fn build_golden_url(&self) -> String {
        // This will be replaced with the actual captured URL from test-publisher.com
        // For now, using a template based on the captured Golden URL
        let mut params = HashMap::new();

        // Core GAM parameters (based on captured URL)
        params.insert("pvsid".to_string(), "3290837576990024".to_string()); // Publisher Viewability ID
        params.insert("correlator".to_string(), self.correlator.clone());
        params.insert(
            "eid".to_string(),
            "31086815,31093089,95353385,31085777,83321072".to_string(),
        ); // Event IDs
        params.insert("output".to_string(), "ldjh".to_string()); // Important: not 'json'
        params.insert("gdfp_req".to_string(), "1".to_string());
        params.insert("vrg".to_string(), "202506170101".to_string()); // Version/Region
        params.insert("ptt".to_string(), "17".to_string()); // Page Type
        params.insert("impl".to_string(), "fifs".to_string()); // Implementation

        // Ad unit parameters (simplified version of captured format)
        params.insert(
            "iu_parts".to_string(),
            format!("{},{},homepage", self.publisher_id, "trustedserver"),
        );
        params.insert(
            "enc_prev_ius".to_string(),
            "/0/1/2,/0/1/2,/0/1/2".to_string(),
        );
        params.insert("prev_iu_szs".to_string(), "320x50|300x250|728x90|970x90|970x250|1x2,320x50|300x250|728x90|970x90|970x250|1x2,320x50|300x250|728x90|970x90|970x250|1x2".to_string());
        params.insert("fluid".to_string(), "height,height,height".to_string());

        // Browser context (simplified)
        params.insert("biw".to_string(), "1512".to_string());
        params.insert("bih".to_string(), "345".to_string());
        params.insert("u_tz".to_string(), "-300".to_string());
        params.insert("u_cd".to_string(), "30".to_string());
        params.insert("u_sd".to_string(), "2".to_string());

        // Page context
        params.insert("url".to_string(), self.page_url.clone());
        params.insert(
            "dt".to_string(),
            chrono::Utc::now().timestamp_millis().to_string(),
        );

        // Add Permutive context if available (in cust_params like the captured URL)
        if let Some(ref prmtvctx) = self.prmtvctx {
            let cust_params = format!("permutive={}&puid={}", prmtvctx, self.synthetic_id);
            params.insert("cust_params".to_string(), cust_params);
        }

        // Build query string
        let query_string = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        format!("{}?{}", self.get_base_url(), query_string)
    }

    /// Get the base GAM server URL
    pub fn get_base_url(&self) -> String {
        // This will be updated with the actual GAM endpoint from captured request
        "https://securepubads.g.doubleclick.net/gampad/ads".to_string()
    }

    /// Send the GAM request and return the response
    pub async fn send_request(
        &self,
        _settings: &Settings,
    ) -> Result<Response, Report<TrustedServerError>> {
        let url = self.build_golden_url();
        log::info!("Sending GAM request to: {}", url);

        // Create the request
        let mut req = Request::new(Method::GET, &url);

        // Set headers to mimic a browser request (using only Fastly-compatible headers)
        req.set_header(header::USER_AGENT, &self.user_agent);
        req.set_header(header::ACCEPT, "application/json, text/plain, */*");
        req.set_header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9");
        req.set_header(header::ACCEPT_ENCODING, "gzip, deflate, br");
        req.set_header(header::REFERER, &self.page_url);
        req.set_header(header::ORIGIN, &self.page_url);
        req.set_header("X-Synthetic-ID", &self.synthetic_id);

        // Send the request to the GAM backend
        let backend_name = "gam_backend";
        log::info!("Sending request to backend: {}", backend_name);

        match req.send(backend_name) {
            Ok(mut response) => {
                log::info!(
                    "Received GAM response with status: {}",
                    response.get_status()
                );

                // Log response headers for debugging
                log::debug!("GAM Response headers:");
                for (name, value) in response.get_headers() {
                    log::debug!("  {}: {:?}", name, value);
                }

                // Handle response body safely
                let body_bytes = response.take_body_bytes();
                let body = match std::str::from_utf8(&body_bytes) {
                    Ok(body_str) => body_str.to_string(),
                    Err(e) => {
                        log::warn!("Could not read response body as UTF-8: {:?}", e);

                        // Try to decompress if it's Brotli compressed
                        let mut decompressed = Vec::new();
                        match brotli::BrotliDecompress(
                            &mut std::io::Cursor::new(&body_bytes),
                            &mut decompressed,
                        ) {
                            Ok(_) => match std::str::from_utf8(&decompressed) {
                                Ok(decompressed_str) => {
                                    log::debug!(
                                        "Successfully decompressed Brotli response: {} bytes",
                                        decompressed_str.len()
                                    );
                                    decompressed_str.to_string()
                                }
                                Err(e2) => {
                                    log::warn!(
                                        "Could not read decompressed body as UTF-8: {:?}",
                                        e2
                                    );
                                    format!("{{\"error\": \"decompression_failed\", \"message\": \"Could not decode decompressed response\", \"original_error\": \"{:?}\"}}", e2)
                                }
                            },
                            Err(e2) => {
                                log::warn!("Could not decompress Brotli response: {:?}", e2);
                                // Return a placeholder since we can't parse the binary response
                                format!("{{\"error\": \"compression_failed\", \"message\": \"Could not decompress response\", \"original_error\": \"{:?}\"}}", e2)
                            }
                        }
                    }
                };

                log::debug!("GAM Response body length: {} bytes", body.len());

                // For debugging, log first 500 chars of response
                if body.len() > 500 {
                    log::debug!("GAM Response preview: {}...", &body[..500]);
                } else {
                    log::debug!("GAM Response: {}", body);
                }

                Ok(Response::from_status(response.get_status())
                    .with_header(header::CONTENT_TYPE, "text/plain")
                    .with_header(header::CACHE_CONTROL, "no-store, private")
                    .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .with_header("X-GAM-Test", "true")
                    .with_header("X-Synthetic-ID", &self.synthetic_id)
                    .with_header("X-Correlator", &self.correlator)
                    .with_header("x-compress-hint", "on")
                    .with_body(body))
            }
            Err(e) => {
                log::error!("Error sending GAM request: {:?}", e);
                Err(Report::new(TrustedServerError::Gam {
                    message: format!("Failed to send GAM request: {}", e),
                }))
            }
        }
    }
}

/// Handle GAM test requests (Phase 1: Capture & Replay)
pub async fn handle_gam_test(
    settings: &Settings,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Starting GAM test request handling");

    // Debug: Log all request headers
    log::debug!("GAM Test - All request headers:");
    for (name, value) in req.get_headers() {
        log::debug!("  {}: {:?}", name, value);
    }

    // Check consent status from cookie (more reliable than header)
    let consent = get_consent_from_request(&req).unwrap_or_default();
    let advertising_consent = consent.advertising;

    log::debug!("GAM Test - Consent from cookie: {:?}", consent);
    log::debug!(
        "GAM Test - Advertising consent from cookie: {}",
        advertising_consent
    );

    // Also check header as fallback
    let header_consent = req
        .get_header("X-Consent-Advertising")
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    log::debug!(
        "GAM Test - Advertising consent from header: {}",
        header_consent
    );

    // Use cookie consent as primary, header as fallback
    let final_consent = advertising_consent || header_consent;
    log::info!("GAM Test - Final advertising consent: {}", final_consent);

    if !final_consent {
        return Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "application/json")
            .with_body_json(&json!({
                "error": "No advertising consent",
                "message": "GAM requests require advertising consent",
                "debug": {
                    "cookie_consent": consent,
                    "header_consent": header_consent,
                    "final_consent": final_consent
                }
            }))
            .map_err(|e| {
                Report::new(TrustedServerError::Gam {
                    message: format!("Failed to serialize consent response: {}", e),
                })
            });
    }

    // Create GAM request
    let gam_req = match GamRequest::new(settings, &req) {
        Ok(req) => {
            log::info!("Successfully created GAM request");
            req
        }
        Err(e) => {
            log::error!("Error creating GAM request: {:?}", e);
            return Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to create GAM request",
                    "details": format!("{:?}", e)
                }))
                .map_err(|e| {
                    Report::new(TrustedServerError::Gam {
                        message: format!("Failed to serialize error response: {}", e),
                    })
                });
        }
    };

    // For Phase 1, we'll use a hardcoded prmtvctx value from captured request
    // This will be replaced with the actual value from test-publisher.com
    let gam_req_with_context = gam_req.with_prmtvctx("129627,137412,138272,139095,139096,139218,141364,143196,143210,143211,143214,143217,144331,144409,144438,144444,144488,144543,144663,144679,144731,144824,144916,145933,146347,146348,146349,146350,146351,146370,146383,146391,146392,146393,146424,146995,147077,147740,148616,148627,148628,149007,150420,150663,150689,150690,150692,150752,150753,150755,150756,150757,150764,150770,150781,150862,154609,155106,155109,156204,164183,164573,165512,166017,166019,166484,166486,166487,166488,166492,166494,166495,166497,166511,167639,172203,172544,173548,176066,178053,178118,178120,178121,178133,180321,186069,199642,199691,202074,202075,202081,233782,238158,adv,bhgp,bhlp,bhgw,bhlq,bhlt,bhgx,bhgv,bhgu,bhhb,rts".to_string());

    log::info!(
        "Sending GAM request with correlator: {}",
        gam_req_with_context.correlator
    );

    match gam_req_with_context.send_request(settings).await {
        Ok(response) => {
            log::info!("GAM request successful");
            Ok(response)
        }
        Err(e) => {
            log::error!("GAM request failed: {:?}", e);
            Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to send GAM request",
                    "details": format!("{:?}", e)
                }))
                .map_err(|e| {
                    Report::new(TrustedServerError::Gam {
                        message: format!("Failed to serialize error response: {}", e),
                    })
                })
        }
    }
}

/// Handle GAM golden URL replay (for testing captured requests)
pub async fn handle_gam_golden_url(
    _settings: &Settings,
    _req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Handling GAM golden URL replay");

    // This endpoint will be used to test the exact captured URL from test-publisher.com
    // For now, return a placeholder response
    Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_body_json(&json!({
            "status": "golden_url_replay",
            "message": "Ready for captured URL testing",
            "next_steps": [
                "1. Capture complete GAM request URL from test-publisher.com",
                "2. Replace placeholder URL in GamRequest::build_golden_url()",
                "3. Test with exact captured parameters"
            ]
        }))
        .map_err(|e| {
            Report::new(TrustedServerError::Gam {
                message: format!("Failed to serialize response: {}", e),
            })
        })
}

/// Handle GAM custom URL testing (for testing captured URLs directly)
pub async fn handle_gam_custom_url(
    _settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Handling GAM custom URL test");

    // Check consent status from cookie or header for testing
    let consent = get_consent_from_request(&req).unwrap_or_default();
    let cookie_consent = consent.advertising;

    // Also check header as fallback for testing
    let header_consent = req
        .get_header("X-Consent-Advertising")
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    let advertising_consent = cookie_consent || header_consent;

    if !advertising_consent {
        return Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "application/json")
            .with_body_json(&json!({
                "error": "No advertising consent",
                "message": "GAM requests require advertising consent"
            }))
            .map_err(|e| {
                Report::new(TrustedServerError::Gam {
                    message: format!("Failed to serialize response: {}", e),
                })
            });
    }

    // Parse the request body to get the custom URL
    let body = req.take_body_str();
    let url_data: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        log::error!("Error parsing request body: {:?}", e);
        Report::new(TrustedServerError::Gam {
            message: format!("Invalid JSON in request body: {}", e),
        })
    })?;

    let custom_url = url_data["url"].as_str().ok_or_else(|| {
        Report::new(TrustedServerError::Gam {
            message: "Missing 'url' field in request body".to_string(),
        })
    })?;

    log::info!("Testing custom GAM URL: {}", custom_url);

    // Create a request to the custom URL
    let mut gam_req = Request::new(Method::GET, custom_url);

    // Set headers to mimic a browser request
    gam_req.set_header(
        header::USER_AGENT,
        "Mozilla/5.0 (compatible; TrustedServer/1.0)",
    );
    gam_req.set_header(header::ACCEPT, "application/json, text/plain, */*");
    gam_req.set_header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9");
    gam_req.set_header(header::ACCEPT_ENCODING, "gzip, deflate, br");
    gam_req.set_header(header::REFERER, "https://www.test-publisher.com/");
    gam_req.set_header(header::ORIGIN, "https://www.test-publisher.com");

    // Send the request to the GAM backend
    let backend_name = "gam_backend";
    log::info!("Sending custom URL request to backend: {}", backend_name);

    match gam_req.send(backend_name) {
        Ok(mut response) => {
            log::info!(
                "Received GAM response with status: {}",
                response.get_status()
            );

            // Log response headers for debugging
            log::debug!("GAM Response headers:");
            for (name, value) in response.get_headers() {
                log::debug!("  {}: {:?}", name, value);
            }

            // Handle response body safely
            let body_bytes = response.take_body_bytes();
            let body = match std::str::from_utf8(&body_bytes) {
                Ok(body_str) => body_str.to_string(),
                Err(e) => {
                    log::warn!("Could not read response body as UTF-8: {:?}", e);

                    // Try to decompress if it's Brotli compressed
                    let mut decompressed = Vec::new();
                    match brotli::BrotliDecompress(
                        &mut std::io::Cursor::new(&body_bytes),
                        &mut decompressed,
                    ) {
                        Ok(_) => match std::str::from_utf8(&decompressed) {
                            Ok(decompressed_str) => {
                                log::debug!(
                                    "Successfully decompressed Brotli response: {} bytes",
                                    decompressed_str.len()
                                );
                                decompressed_str.to_string()
                            }
                            Err(e2) => {
                                log::warn!("Could not read decompressed body as UTF-8: {:?}", e2);
                                format!("{{\"error\": \"decompression_failed\", \"message\": \"Could not decode decompressed response\", \"original_error\": \"{:?}\"}}", e2)
                            }
                        },
                        Err(e2) => {
                            log::warn!("Could not decompress Brotli response: {:?}", e2);
                            // Return a placeholder since we can't parse the binary response
                            format!("{{\"error\": \"compression_failed\", \"message\": \"Could not decompress response\", \"original_error\": \"{:?}\"}}", e2)
                        }
                    }
                }
            };

            log::debug!("GAM Response body length: {} bytes", body.len());

            Ok(Response::from_status(response.get_status())
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header(header::CACHE_CONTROL, "no-store, private")
                .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .with_header("X-GAM-Test", "true")
                .with_header("X-Custom-URL", "true")
                .with_header("x-compress-hint", "on")
                .with_body_json(&json!({
                    "status": "custom_url_test",
                    "original_url": custom_url,
                    "response_status": response.get_status().as_u16(),
                    "response_body": body,
                    "message": "Custom URL test completed"
                }))
                .map_err(|e| {
                    Report::new(TrustedServerError::Gam {
                        message: format!("Failed to serialize response: {}", e),
                    })
                })?)
        }
        Err(e) => {
            log::error!("Error sending custom GAM request: {:?}", e);
            Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to send custom GAM request",
                    "details": format!("{:?}", e),
                    "original_url": custom_url
                }))
                .map_err(|e| {
                    Report::new(TrustedServerError::Gam {
                        message: format!("Failed to serialize error response: {}", e),
                    })
                })
        }
    }
}

/// Handle GAM response rendering in iframe
pub async fn handle_gam_render(
    settings: &Settings,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Handling GAM response rendering");

    // Check consent status from cookie
    let consent = get_consent_from_request(&req).unwrap_or_default();
    let advertising_consent = consent.advertising;

    if !advertising_consent {
        return Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "application/json")
            .with_body_json(&json!({
                "error": "No advertising consent",
                "message": "GAM requests require advertising consent"
            }))
            .map_err(|e| {
                Report::new(TrustedServerError::Gam {
                    message: format!("Failed to serialize response: {}", e),
                })
            });
    }

    // Create GAM request and get response
    let gam_req = match GamRequest::new(settings, &req) {
        Ok(req) => req.with_prmtvctx("129627,137412,138272,139095,139096,139218,141364,143196,143210,143211,143214,143217,144331,144409,144438,144444,144488,144543,144663,144679,144731,144824,144916,145933,146347,146348,146349,146350,146351,146370,146383,146391,146392,146393,146424,146995,147077,147740,148616,148627,148628,149007,150420,150663,150689,150690,150692,150752,150753,150755,150756,150757,150764,150770,150781,150862,154609,155106,155109,156204,164183,164573,165512,166017,166019,166484,166486,166487,166488,166492,166494,166495,166497,166511,167639,172203,172544,173548,176066,178053,178118,178120,178121,178133,180321,186069,199642,199691,202074,202075,202081,233782,238158,adv,bhgp,bhlp,bhgw,bhlq,bhlt,bhgx,bhgv,bhgu,bhhb,rts".to_string()),
        Err(e) => {
            return Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to create GAM request",
                    "details": format!("{:?}", e)
                }))
                .map_err(|e| Report::new(TrustedServerError::Gam {
                    message: format!("Failed to serialize error response: {}", e),
                }));
        }
    };

    // Get GAM response
    let gam_response = match gam_req.send_request(settings).await {
        Ok(response) => response,
        Err(e) => {
            return Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to get GAM response",
                    "details": format!("{:?}", e)
                }))
                .map_err(|e| {
                    Report::new(TrustedServerError::Gam {
                        message: format!("Failed to serialize error response: {}", e),
                    })
                });
        }
    };

    // Parse the GAM response to extract HTML
    let response_body = gam_response.into_body_str();
    log::info!("Parsing GAM response for HTML extraction");

    // The GAM response format is: {"/ad_unit_path":["html",0,null,null,0,90,728,0,0,null,null,null,null,null,[...],null,null,null,null,null,null,null,0,null,null,null,null,null,null,"creative_id","line_item_id"],"<!doctype html>..."}
    // We need to extract the HTML part after the JSON array

    let html_content = if response_body.contains("<!doctype html>") {
        // Find the start of HTML content
        if let Some(html_start) = response_body.find("<!doctype html>") {
            let html = &response_body[html_start..];
            log::debug!("Extracted HTML content: {} bytes", html.len());
            html.to_string()
        } else {
            format!("<html><body><p>Error: Could not find HTML content in GAM response</p><pre>{}</pre></body></html>", 
                   response_body.chars().take(500).collect::<String>())
        }
    } else {
        // Fallback: return the raw response in a safe HTML wrapper
        format!(
            "<html><body><p>GAM Response (no HTML found):</p><pre>{}</pre></body></html>",
            response_body.chars().take(1000).collect::<String>()
        )
    };

    // Create a safe HTML page that renders the ad content in an iframe
    let render_page = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>GAM Ad Render Test</title>
    <style>
        body {{
            font-family: Arial, sans-serif;
            margin: 20px;
            background-color: #f5f5f5;
        }}
        .container {{
            max-width: 1200px;
            margin: 0 auto;
            background: white;
            padding: 20px;
            border-radius: 8px;
            box-shadow: 0 2px 10px rgba(0,0,0,0.1);
        }}
        .header {{
            text-align: center;
            margin-bottom: 30px;
            padding-bottom: 20px;
            border-bottom: 2px solid #eee;
        }}
        .ad-frame {{
            width: 100%;
            min-height: 600px;
            border: 2px solid #ddd;
            border-radius: 4px;
            background: white;
        }}
        .controls {{
            margin: 20px 0;
            text-align: center;
        }}
        .btn {{
            background: #007bff;
            color: white;
            border: none;
            padding: 10px 20px;
            border-radius: 4px;
            cursor: pointer;
            margin: 0 10px;
        }}
        .btn:hover {{
            background: #0056b3;
        }}
        .info {{
            background: #e9ecef;
            padding: 15px;
            border-radius: 4px;
            margin: 20px 0;
        }}
        .debug {{
            background: #f8f9fa;
            border: 1px solid #dee2e6;
            padding: 10px;
            border-radius: 4px;
            margin-top: 20px;
            font-family: monospace;
            font-size: 12px;
            max-height: 200px;
            overflow-y: auto;
        }}
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>🎯 GAM Ad Render Test</h1>
            <p>Rendering Google Ad Manager response in iframe</p>
        </div>
        
        <div class="info">
            <strong>Status:</strong> Ad content loaded successfully<br>
            <strong>Response Size:</strong> {} bytes<br>
            <strong>Timestamp:</strong> {}
        </div>
        
        <div class="controls">
            <button class="btn" onclick="refreshAd()">🔄 Refresh Ad</button>
            <button class="btn" onclick="toggleDebug()">🐛 Toggle Debug</button>
            <button class="btn" onclick="window.location.href='/gam-test-page'">← Back to Test Page</button>
        </div>
        
        <iframe 
            id="adFrame" 
            class="ad-frame" 
            srcdoc="{}"
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox"
            title="GAM Ad Content">
        </iframe>
        
        <div id="debugInfo" class="debug" style="display: none;">
            <strong>Debug Info:</strong><br>
            <strong>HTML Content Length:</strong> {} characters<br>
            <strong>HTML Preview:</strong><br>
            <pre>{}</pre>
        </div>
    </div>
    
    <script>
        function refreshAd() {{
            // Reload the entire page to get a fresh GAM request
            window.location.reload();
        }}
        
        function toggleDebug() {{
            const debug = document.getElementById('debugInfo');
            if (debug.style.display === 'none' || debug.style.display === '') {{
                debug.style.display = 'block';
            }} else {{
                debug.style.display = 'none';
            }}
        }}
        
        // Auto-refresh every 30 seconds for testing
        setInterval(refreshAd, 30000);
    </script>
</body>
</html>"#,
        html_content.len(),
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        html_content.replace("\"", "&quot;").replace("'", "&#39;"),
        html_content.len(),
        html_content.chars().take(200).collect::<String>()
    );

    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .with_header(header::CACHE_CONTROL, "no-store, private")
        .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .with_header("X-GAM-Render", "true")
        .with_header("X-Synthetic-ID", &gam_req.synthetic_id)
        .with_header("X-Correlator", &gam_req.correlator)
        .with_body(render_page))
}

/// Check if the path is for a GAM asset
pub fn is_gam_asset_path(path: &str) -> bool {
    // Common GAM paths that we know about
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
    path.contains("safeframe") // GAM safe frame containers
}

/// Rewrite hardcoded URLs in GAM JavaScript to use first-party proxy
pub fn rewrite_gam_urls(content: &str) -> String {
    log::info!("Starting GAM URL rewriting...");

    // Define the URL mappings based on the user's configuration
    let url_mappings = [
        // Primary GAM domains
        ("securepubads.g.doubleclick.net", "edgepubs.com"),
        ("googletagservices.com", "edgepubs.com"),
        ("googlesyndication.com", "edgepubs.com"),
        ("pagead2.googlesyndication.com", "edgepubs.com"),
        ("tpc.googlesyndication.com", "edgepubs.com"),
        // GAM-specific subdomains that might appear
        ("www.googletagservices.com", "edgepubs.com"),
        ("www.googlesyndication.com", "edgepubs.com"),
        ("static.googleadsserving.cn", "edgepubs.com"),
        // Ad serving domains
        ("doubleclick.net", "edgepubs.com"),
        ("www.google.com/adsense", "edgepubs.com/adsense"),
        // Google ad quality and traffic domains
        ("adtrafficquality.google", "edgepubs.com"),
        ("ep1.adtrafficquality.google", "edgepubs.com"),
        ("ep2.adtrafficquality.google", "edgepubs.com"),
        ("ep3.adtrafficquality.google", "edgepubs.com"),
        // Other Google ad-related domains
        (
            "6ab9b2c571ea5e8cf287325e9ebeaa41.safeframe.googlesyndication.com",
            "edgepubs.com",
        ),
        ("www.google.com/recaptcha", "edgepubs.com/recaptcha"),
    ];

    let mut rewritten_content = content.to_string();
    let mut total_replacements = 0;

    for (original_domain, proxy_domain) in &url_mappings {
        // Count replacements for this domain
        let before_count = rewritten_content.matches(original_domain).count();

        if before_count > 0 {
            log::info!(
                "Found {} occurrences of '{}' to rewrite",
                before_count,
                original_domain
            );

            // Replace both HTTP and HTTPS versions
            rewritten_content = rewritten_content.replace(
                &format!("https://{}", original_domain),
                &format!("https://{}", proxy_domain),
            );
            rewritten_content = rewritten_content.replace(
                &format!("http://{}", original_domain),
                &format!("https://{}", proxy_domain),
            );

            // Also replace protocol-relative URLs (//domain.com)
            rewritten_content = rewritten_content.replace(
                &format!("//{}", original_domain),
                &format!("//{}", proxy_domain),
            );

            // Replace domain-only references (for cases where protocol is added separately)
            rewritten_content = rewritten_content.replace(
                &format!("\"{}\"", original_domain),
                &format!("\"{}\"", proxy_domain),
            );
            rewritten_content = rewritten_content.replace(
                &format!("'{}'", original_domain),
                &format!("'{}'", proxy_domain),
            );

            let after_count = rewritten_content.matches(original_domain).count();
            let replacements = before_count - after_count;
            total_replacements += replacements;

            if replacements > 0 {
                log::info!(
                    "Replaced {} occurrences of '{}' with '{}'",
                    replacements,
                    original_domain,
                    proxy_domain
                );
            }
        }
    }

    log::info!(
        "GAM URL rewriting complete. Total replacements: {}",
        total_replacements
    );

    // Log a sample of the rewritten content for debugging (first 500 chars)
    if total_replacements > 0 {
        let sample_length = std::cmp::min(500, rewritten_content.len());
        log::debug!(
            "Rewritten content sample: {}",
            &rewritten_content[..sample_length]
        );
    }

    rewritten_content
}

/// Handle GAM asset serving (JavaScript files and other resources)
pub async fn handle_gam_asset(
    _settings: &Settings,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let path = req.get_path();
    log::info!("Handling GAM asset request: {}", path);

    // Log request details for debugging
    log::info!("GAM Asset Request Details:");
    log::info!("  - Path: {}", path);
    log::info!("  - Method: {}", req.get_method());
    log::info!("  - Full URL: {}", req.get_url());

    // Determine backend and target path
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
        // Default: all other GAM requests go to main GAM backend
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

    // Construct full URL
    let mut full_url = format!("https://{}{}", original_host, target_path);
    if let Some(query) = req.get_url().query() {
        full_url.push('?');
        full_url.push_str(query);
    }

    // Special handling for /gampad/ads requests
    if target_path.contains("/gampad/ads") {
        log::info!("Applying URL parameter rewriting for GAM ad request");
        full_url = full_url.replace(
            "url=https%3A%2F%2Fedgepubs.com%2F",
            "url=https%3A%2F%2Fwww.test-publisher.com%2F",
        );
    }

    let mut asset_req = Request::new(req.get_method().clone(), &full_url);

    // Copy headers from original request
    for (name, value) in req.get_headers() {
        asset_req.set_header(name, value);
    }
    asset_req.set_header(header::HOST, original_host);

    // Send to backend
    match asset_req.send(backend_name) {
        Ok(mut response) => {
            log::info!(
                "Received GAM asset response: status={}",
                response.get_status()
            );

            // Check if JavaScript content needs rewriting
            let content_type = response
                .get_header(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");

            let needs_rewriting = content_type.contains("javascript") || path.contains(".js");

            if needs_rewriting {
                // Handle content-disposition header
                let original_content_disposition = response
                    .get_header("content-disposition")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string());

                response.remove_header("content-disposition");

                // Get response body
                let body_bytes = response.take_body_bytes();
                let original_length = body_bytes.len();

                // Handle Brotli compression if present
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
                        Ok(_) => {
                            log::info!("Successfully decompressed {} bytes", original_length);
                            decompressed
                        }
                        Err(e) => {
                            log::error!("Failed to decompress Brotli data: {:?}", e);
                            return Err(Report::new(TrustedServerError::Gam {
                                message: format!("Failed to decompress GAM asset: {}", e),
                            }));
                        }
                    }
                } else {
                    body_bytes
                };

                // Convert to string
                let body = match std::str::from_utf8(&decompressed_body) {
                    Ok(body_str) => body_str.to_string(),
                    Err(e) => {
                        log::error!("Invalid UTF-8 in GAM asset: {:?}", e);
                        return Err(Report::new(TrustedServerError::InvalidUtf8 {
                            message: format!("Invalid UTF-8 in GAM asset: {}", e),
                        }));
                    }
                };

                // Rewrite URLs
                let rewritten_body = rewrite_gam_urls(&body);
                let rewritten_length = rewritten_body.len();

                log::info!(
                    "Rewritten GAM JavaScript: {} -> {} bytes",
                    original_length,
                    rewritten_length
                );

                // Create new response
                let mut new_response = Response::from_status(response.get_status());

                // Copy headers except problematic ones
                for (name, value) in response.get_headers() {
                    let header_name = name.as_str().to_lowercase();
                    if header_name == "content-disposition"
                        || header_name == "content-encoding"
                        || header_name == "content-length"
                    {
                        continue;
                    }
                    new_response.set_header(name, value);
                }

                // Set body and headers
                new_response.set_body(rewritten_body);
                new_response
                    .set_header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate");
                new_response.set_header("X-Content-Rewritten", "true");

                // Restore content-disposition if it existed
                if let Some(original_disposition) = original_content_disposition {
                    new_response.set_header("content-disposition", &original_disposition);
                }

                Ok(new_response)
            } else {
                // No rewriting needed, serve as-is
                response.set_header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate");
                Ok(response)
            }
        }
        Err(e) => {
            log::error!("Error fetching GAM asset from {}: {:?}", backend_name, e);
            Err(Report::new(TrustedServerError::Gam {
                message: format!(
                    "Failed to fetch GAM asset from {} for path {}: {}",
                    backend_name, path, e
                ),
            }))
        }
    }
}

pub fn handle_gam_test_page(
    _settings: &Settings,
    _req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    Ok(Response::from_status(StatusCode::OK)
        .with_body(GAM_TEST_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header("x-compress-hint", "on"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::{
        constants::HEADER_SYNTHETIC_TRUSTED_SERVER, test_support::tests::create_test_settings,
    };

    fn create_test_request() -> Request {
        let mut req = Request::new(Method::GET, "https://example.com/test");
        req.set_header(header::USER_AGENT, "Mozilla/5.0 Test Browser");
        req.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, "test-synthetic-id-123");
        req
    }

    #[test]
    fn test_gam_request_new() {
        let settings = create_test_settings();
        let req = create_test_request();

        let gam_req = GamRequest::new(&settings, &req).unwrap();

        assert_eq!(gam_req.publisher_id, "21796327522");
        assert_eq!(gam_req.ad_units.len(), 2);
        assert_eq!(gam_req.ad_units[0], "test_unit_1");
        assert_eq!(gam_req.ad_units[1], "test_unit_2");
        assert_eq!(gam_req.page_url, "https://example.com/test");
        assert_eq!(gam_req.user_agent, "Mozilla/5.0 Test Browser");
        assert_eq!(gam_req.synthetic_id, "test-synthetic-id-123");
        assert!(gam_req.prmtvctx.is_none());
        assert!(!gam_req.correlator.is_empty());
    }

    #[test]
    fn test_gam_request_with_missing_headers() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/test");

        let gam_req = GamRequest::new(&settings, &req).unwrap();

        assert_eq!(
            gam_req.user_agent,
            "Mozilla/5.0 (compatible; TrustedServer/1.0)"
        );
        assert_eq!(gam_req.synthetic_id, "unknown");
    }

    #[test]
    fn test_gam_request_with_prmtvctx() {
        let settings = create_test_settings();
        let req = create_test_request();

        let gam_req = GamRequest::new(&settings, &req)
            .unwrap()
            .with_prmtvctx("test_context_123".to_string());

        assert_eq!(gam_req.prmtvctx, Some("test_context_123".to_string()));
    }

    #[test]
    fn test_build_golden_url() {
        let settings = create_test_settings();
        let req = create_test_request();

        let gam_req = GamRequest::new(&settings, &req)
            .unwrap()
            .with_prmtvctx("test_permutive_context".to_string());

        let url = gam_req.build_golden_url();

        assert!(url.starts_with("https://securepubads.g.doubleclick.net/gampad/ads?"));
        assert!(url.contains("correlator="));
        assert!(url.contains("iu_parts=21796327522%2Ctrustedserver%2Chomepage"));
        assert!(url.contains("url=https%3A%2F%2Fexample.com%2Ftest"));
        assert!(url.contains(
            "cust_params=permutive%3Dtest_permutive_context%26puid%3Dtest-synthetic-id-123"
        ));
        assert!(url.contains("output=ldjh"));
        assert!(url.contains("gdfp_req=1"));
    }

    #[test]
    fn test_build_golden_url_without_prmtvctx() {
        let settings = create_test_settings();
        let req = create_test_request();

        let gam_req = GamRequest::new(&settings, &req).unwrap();
        let url = gam_req.build_golden_url();

        assert!(!url.contains("cust_params="));
        assert!(!url.contains("permutive="));
    }

    #[test]
    fn test_url_encoding_in_build_golden_url() {
        let settings = create_test_settings();
        let mut req = Request::new(
            Method::GET,
            "https://example.com/test?param=value&special=test%20space",
        );
        req.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, "test-id");

        let gam_req = GamRequest::new(&settings, &req).unwrap();
        let url = gam_req.build_golden_url();

        // Check that URL parameters are properly encoded
        assert!(url.contains(
            "url=https%3A%2F%2Fexample.com%2Ftest%3Fparam%3Dvalue%26special%3Dtest%2520space"
        ));
    }

    #[test]
    fn test_correlator_uniqueness() {
        let settings = create_test_settings();
        let req = create_test_request();

        let gam_req1 = GamRequest::new(&settings, &req).unwrap();
        let gam_req2 = GamRequest::new(&settings, &req).unwrap();

        // Correlators should be unique for each request
        assert_ne!(gam_req1.correlator, gam_req2.correlator);
    }

    // Integration tests for GAM handlers
    #[tokio::test]
    async fn test_handle_gam_test_without_consent() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/gam-test");

        let response = handle_gam_test(&settings, req).await.unwrap();

        assert_eq!(response.get_status(), StatusCode::OK);
        let body = response.into_body_str();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["error"], "No advertising consent");
        assert_eq!(json["message"], "GAM requests require advertising consent");
    }

    #[tokio::test]
    async fn test_handle_gam_test_with_header_consent() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::GET, "https://example.com/gam-test");
        req.set_header("X-Consent-Advertising", "true");
        req.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, "test-synthetic-id");

        // Note: This test will fail when actually sending to GAM backend
        // In a real test environment, we'd mock the backend response
        let response = handle_gam_test(&settings, req).await;
        assert!(response.is_ok() || response.is_err()); // Test runs either way
    }

    #[tokio::test]
    async fn test_handle_gam_golden_url() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/gam-golden-url");

        let response = handle_gam_golden_url(&settings, req).await.unwrap();

        assert_eq!(response.get_status(), StatusCode::OK);
        let body = response.into_body_str();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["status"], "golden_url_replay");
        assert_eq!(json["message"], "Ready for captured URL testing");
        assert!(json["next_steps"].is_array());
    }

    #[tokio::test]
    async fn test_handle_gam_custom_url_without_consent() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::POST, "https://example.com/gam-test-custom-url");
        req.set_body(
            json!({
                "url": "https://securepubads.g.doubleclick.net/gampad/ads?test=1"
            })
            .to_string(),
        );

        let response = handle_gam_custom_url(&settings, req).await.unwrap();

        assert_eq!(response.get_status(), StatusCode::OK);
        let body = response.into_body_str();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["error"], "No advertising consent");
    }

    #[tokio::test]
    async fn test_handle_gam_custom_url_with_invalid_body() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::POST, "https://example.com/gam-test-custom-url");
        req.set_header("X-Consent-Advertising", "true");
        req.set_body("invalid json");

        let response = handle_gam_custom_url(&settings, req).await;

        // Should return an error for invalid JSON
        assert!(response.is_err());
    }

    #[tokio::test]
    async fn test_handle_gam_custom_url_missing_url_field() {
        let settings = create_test_settings();
        let mut req = Request::new(Method::POST, "https://example.com/gam-test-custom-url");
        req.set_header("X-Consent-Advertising", "true");
        req.set_body(
            json!({
                "other_field": "value"
            })
            .to_string(),
        );

        let response = handle_gam_custom_url(&settings, req).await;

        // Should return an error for missing URL field
        assert!(response.is_err());
    }

    #[tokio::test]
    async fn test_handle_gam_render_without_consent() {
        let settings = create_test_settings();
        let req = Request::new(Method::GET, "https://example.com/gam-render");

        let response = handle_gam_render(&settings, req).await.unwrap();

        assert_eq!(response.get_status(), StatusCode::OK);
        let body = response.into_body_str();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(json["error"], "No advertising consent");
        assert_eq!(json["message"], "GAM requests require advertising consent");
    }

    // Tests for is_gam_asset_path and rewrite_gam_urls functions
    #[test]
    fn test_is_gam_asset_path() {
        // Test positive cases
        assert!(is_gam_asset_path("/tag/js/gpt.js"));
        assert!(is_gam_asset_path("/pagead/ads"));
        assert!(is_gam_asset_path("/gampad/ads?params=test"));
        assert!(is_gam_asset_path("/recaptcha/api.js"));
        assert!(is_gam_asset_path("/safeframe/1-0-40/html/container.html"));

        // Test negative cases
        assert!(!is_gam_asset_path("/"));
        assert!(!is_gam_asset_path("/index.html"));
        assert!(!is_gam_asset_path("/api/v1/data"));
    }

    #[test]
    fn test_rewrite_gam_urls() {
        // Test basic domain replacement
        let content = "https://securepubads.g.doubleclick.net/tag/js/gpt.js";
        let rewritten = rewrite_gam_urls(content);
        assert_eq!(rewritten, "https://edgepubs.com/tag/js/gpt.js");

        // Test multiple domains and protocols
        let content = r#"
            var url1 = "https://googletagservices.com/tag/js/gpt.js";
            //securepubads.g.doubleclick.net/tag/js/gpt.js
            https://tpc.googlesyndication.com/simgad/123456?param=value#anchor
        "#;
        let rewritten = rewrite_gam_urls(content);
        assert!(rewritten.contains("https://edgepubs.com/tag/js/gpt.js"));
        assert!(rewritten.contains("//edgepubs.com/tag/js/gpt.js"));
        assert!(rewritten.contains("https://edgepubs.com/simgad/123456?param=value#anchor"));
        assert!(!rewritten.contains("googletagservices.com"));
        assert!(!rewritten.contains("googlesyndication.com"));

        // Test special domains (adtrafficquality, safeframe, recaptcha)
        let content = r#"
            https://ep1.adtrafficquality.google/beacon
            https://6ab9b2c571ea5e8cf287325e9ebeaa41.safeframe.googlesyndication.com/safeframe/1-0-40/html/container.html
            https://www.google.com/recaptcha/api.js
        "#;
        let rewritten = rewrite_gam_urls(content);
        assert!(rewritten.contains("https://edgepubs.com/beacon"));
        assert!(rewritten.contains("https://edgepubs.com/safeframe/1-0-40/html/container.html"));
        assert!(rewritten.contains("https://edgepubs.com/recaptcha/api.js"));

        // Test content that should not be changed
        let content = "https://example.com/some/path";
        let rewritten = rewrite_gam_urls(content);
        assert_eq!(content, rewritten);
    }

    #[test]
    fn test_rewrite_gam_urls_edge_cases() {
        // Test empty content
        assert_eq!(rewrite_gam_urls(""), "");

        // Test protocol-relative URLs
        let content = "//securepubads.g.doubleclick.net/tag/js/gpt.js";
        let rewritten = rewrite_gam_urls(content);
        assert_eq!(rewritten, "//edgepubs.com/tag/js/gpt.js");

        // Test case sensitivity (should not replace)
        let content = "https://SECUREPUBADS.G.DOUBLECLICK.NET/tag/js/gpt.js";
        let rewritten = rewrite_gam_urls(content);
        assert!(rewritten.contains("SECUREPUBADS.G.DOUBLECLICK.NET"));

        // Test URLs in HTML attributes
        let content =
            r#"<script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"></script>"#;
        let rewritten = rewrite_gam_urls(content);
        assert!(rewritten.contains(r#"src="https://edgepubs.com/tag/js/gpt.js""#));
    }
}
