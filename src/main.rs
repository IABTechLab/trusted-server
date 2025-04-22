use fastly::http::{header, Method, StatusCode};
use fastly::KVStore;
use fastly::{Error, Request, Response};
use fastly::geo::geo_lookup;
use log::LevelFilter::Info;
use serde_json::json;
use std::env;

mod constants;
mod cookies;
use constants::{SYNTH_HEADER_FRESH, SYNTH_HEADER_POTSI};
mod models;
use models::AdResponse;
mod prebid;
use prebid::PrebidRequest;
mod settings;
use settings::Settings;
mod synthetic;
use synthetic::generate_synthetic_id;
mod templates;
use templates::HTML_TEMPLATE;
mod gdpr;
use gdpr::get_consent_from_request;
mod privacy;
use privacy::PRIVACY_TEMPLATE;
mod why;
use why::WHY_TEMPLATE;

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    let settings = Settings::new().unwrap();
    println!("Settings {settings:?}");

    futures::executor::block_on(async {
        println!(
            "FASTLY_SERVICE_VERSION: {}",
            std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
        );

        match (req.get_method(), req.get_path()) {
            (&Method::GET, "/") => handle_main_page(&settings, req),
            (&Method::GET, "/ad-creative") => handle_ad_request(&settings, req),
            (&Method::GET, "/prebid-test") => handle_prebid_test(&settings, req).await,
            (&Method::GET, "/gdpr/consent") => gdpr::handle_consent_request(&settings, req),
            (&Method::POST, "/gdpr/consent") => gdpr::handle_consent_request(&settings, req),
            (&Method::GET, "/gdpr/data") => gdpr::handle_data_subject_request(&settings, req),
            (&Method::DELETE, "/gdpr/data") => gdpr::handle_data_subject_request(&settings, req),
            (&Method::GET, "/privacy-policy") => Ok(Response::from_status(StatusCode::OK)
                .with_body(PRIVACY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header("x-compress-hint", "on")),
            (&Method::GET, "/why-trusted-server") => Ok(Response::from_status(StatusCode::OK)
                .with_body(WHY_TEMPLATE)
                .with_header(header::CONTENT_TYPE, "text/html")
                .with_header("x-compress-hint", "on")),
            _ => Ok(Response::from_status(StatusCode::NOT_FOUND)
                .with_body("Not Found")
                .with_header(header::CONTENT_TYPE, "text/plain")
                .with_header("x-compress-hint", "on")),
        }
    })
}

fn get_dma_code(req: &mut Request) -> Option<String> {
    // Debug: Check if we're running in Fastly environment
    println!("Fastly Environment Check:");
    println!("  FASTLY_POP: {}", std::env::var("FASTLY_POP").unwrap_or_else(|_| "not in Fastly".to_string()));
    println!("  FASTLY_REGION: {}", std::env::var("FASTLY_REGION").unwrap_or_else(|_| "not in Fastly".to_string()));
    
    // Get detailed geo information using geo_lookup
    if let Some(geo) = req.get_client_ip_addr().and_then(geo_lookup) {
        println!("Geo Information Found:");
        
        // Set all available geo information in headers
        let city = geo.city();
        req.set_header("X-Geo-City", city);
        println!("  City: {}", city);
        
        let country = geo.country_code();
        req.set_header("X-Geo-Country", country);
        println!("  Country: {}", country);
        
        req.set_header("X-Geo-Continent", format!("{:?}", geo.continent()));
        println!("  Continent: {:?}", geo.continent());
        
        req.set_header("X-Geo-Coordinates", format!("{},{}", geo.latitude(), geo.longitude()));
        println!("  Location: ({}, {})", geo.latitude(), geo.longitude());
        
        // Get and set the metro code (DMA)
        let metro_code = geo.metro_code();
        req.set_header("X-Geo-Metro-Code", metro_code.to_string());
        println!("Found DMA/Metro code: {}", metro_code);
        return Some(metro_code.to_string());
    } else {
        println!("No geo information available for the request");
        req.set_header("X-Geo-Info-Available", "false");
    }

    // If no metro code is found, log all request headers for debugging
    println!("No DMA/Metro code found. All request headers:");
    for (name, value) in req.get_headers() {
        println!("  {}: {:?}", name, value);
    }

    None
}

fn handle_main_page(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    println!(
        "Using ad_partner_url: {}, counter_store: {}",
        settings.ad_server.ad_partner_url, settings.synthetic.counter_store,
    );

    log_fastly::init_simple("mylogs", Info);

    // Add DMA code check to main page as well
    let dma_code = get_dma_code(&mut req);
    println!("Main page - DMA Code: {:?}", dma_code);

    // Check GDPR consent before proceeding
    let consent = get_consent_from_request(&req).unwrap_or_default();
    if !consent.functional {
        // Return a version of the page without tracking
        return Ok(Response::from_status(StatusCode::OK)
            .with_body(HTML_TEMPLATE.replace("fetch('/prebid-test')", "console.log('Tracking disabled')"))
            .with_header(header::CONTENT_TYPE, "text/html")
            .with_header(header::CACHE_CONTROL, "no-store, private"));
    }

    // Calculate fresh ID first using the synthetic module
    let fresh_id = synthetic::generate_synthetic_id(settings, &req);

    // Check for existing POTSI ID in this specific order:
    // 1. X-Synthetic-Potsi header
    // 2. Cookie
    // 3. Fall back to fresh ID
    let synthetic_id = synthetic::get_or_generate_synthetic_id(settings, &req);

    println!(
        "Existing POTSI header: {:?}",
        req.get_header(SYNTH_HEADER_POTSI)
    );
    println!("Generated Fresh ID: {}", fresh_id);
    println!("Using POTSI ID: {}", synthetic_id);

    // Create response with the main page HTML
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(HTML_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header(SYNTH_HEADER_FRESH, &fresh_id) // Fresh ID always changes
        .with_header(SYNTH_HEADER_POTSI, &synthetic_id) // POTSI ID remains stable
        .with_header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
        )
        .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .with_header("x-compress-hint", "on");

    // Copy geo headers from request to response
    for header_name in &["X-Geo-City", "X-Geo-Country", "X-Geo-Continent", "X-Geo-Coordinates", "X-Geo-Metro-Code", "X-Geo-Info-Available"] {
        if let Some(value) = req.get_header(*header_name) {
            response.set_header(*header_name, value);
        }
    }

    // Only set cookies if we have consent
    if consent.functional {
        response.set_header(
            header::SET_COOKIE,
            cookies::create_synthetic_cookie(&synthetic_id),
        );
    }

    // Debug: Print all request headers
    println!("All Request Headers:");
    for (name, value) in req.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Debug: Print the response headers
    println!("Response Headers:");
    for (name, value) in response.get_headers() {
        log::info!("{}: {:?}", name, value);
    }

    // Prevent caching
    response.set_header(header::CACHE_CONTROL, "no-store, private");

    Ok(response)
}

fn handle_ad_request(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    // Check GDPR consent to determine if we should serve personalized or non-personalized ads
    let consent = get_consent_from_request(&req).unwrap_or_default();
    let advertising_consent = req.get_header("X-Consent-Advertising")
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Add DMA code extraction
    let dma_code = get_dma_code(&mut req);
    
    println!("Client location - DMA Code: {:?}", dma_code);

    // Log headers for debugging
    let client_ip = req
        .get_client_ip_addr()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let x_forwarded_for = req
        .get_header("x-forwarded-for")
        .map(|h| h.to_str().unwrap_or("Unknown"));

    println!("Client IP: {}", client_ip);
    println!("X-Forwarded-For: {}", x_forwarded_for.unwrap_or("None"));
    println!("Advertising consent: {}", advertising_consent);

    // Generate synthetic ID only if we have consent
    let synthetic_id = if advertising_consent {
        generate_synthetic_id(settings, &req)
    } else {
        // Use a generic ID for non-personalized ads
        "non-personalized".to_string()
    };

    // Only track visits if we have consent
    if advertising_consent {
        // Increment visit counter in KV store
        println!("Opening KV store: {}", settings.synthetic.counter_store);
        if let Ok(Some(store)) = KVStore::open(settings.synthetic.counter_store.as_str()) {
            println!("Fetching current count for synthetic ID: {}", synthetic_id);
            let current_count: i32 = store
                .lookup(&synthetic_id)
                .and_then(|mut val| {
                    match String::from_utf8(val.take_body_bytes()) {
                        Ok(s) => {
                            println!("Value from KV store: {}", s);
                            Ok(Some(s))
                        }
                        Err(e) => {
                            println!("Error converting bytes to string: {}", e);
                            Ok(None)
                        }
                    }
                })
                .and_then(|opt_s| {
                    println!("Parsing string value: {:?}", opt_s);
                    Ok(opt_s.and_then(|s| s.parse().ok()))
                })
                .unwrap_or_else(|_| {
                    println!("No existing count found, starting at 0");
                    None
                })
                .unwrap_or(0);

            let new_count = current_count + 1;
            println!("Incrementing count from {} to {}", current_count, new_count);

            if let Err(e) = store.insert(&synthetic_id, new_count.to_string().as_bytes()) {
                println!("Error updating KV store: {:?}", e);
            }
        }
    }

    // Modify the ad server URL construction to include DMA code if available
    let ad_server_url = if advertising_consent {
        let mut url = settings.ad_server.sync_url.replace("{{synthetic_id}}", &synthetic_id);
        if let Some(dma) = dma_code {
            url = format!("{}&dma={}", url, dma);
        }
        url
    } else {
        // Use a different URL or parameter for non-personalized ads
        settings.ad_server.sync_url.replace("{{synthetic_id}}", "non-personalized")
    };

    println!("Sending request to backend: {}", ad_server_url);

    // Add header logging here
    let mut ad_req = Request::get(ad_server_url);
    
    // Add consent information to the ad request
    ad_req.set_header("X-Consent-Advertising", if advertising_consent { "true" } else { "false" });
    
    println!("Request headers to Equativ:");
    for (name, value) in ad_req.get_headers() {
        println!("  {}: {:?}", name, value);
    }

    match ad_req.send(settings.ad_server.ad_partner_url.as_str()) {
        Ok(mut res) => {
            println!(
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
            // let fastly_service_version = env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| "unknown".to_string());
            let fastly_trace_id =
                env::var("FASTLY_TRACE_ID").unwrap_or_else(|_| "unknown".to_string());

            println!("Fastly Jason PoP: {}", fastly_pop);
            println!("Fastly Compute Variables:");
            println!("  - FASTLY_CACHE_GENERATION: {}", fastly_cache_generation);
            println!("  - FASTLY_CUSTOMER_ID: {}", fastly_customer_id);
            println!("  - FASTLY_HOSTNAME: {}", fastly_hostname);
            println!("  - FASTLY_POP: {}", fastly_pop);
            println!("  - FASTLY_REGION: {}", fastly_region);
            println!("  - FASTLY_SERVICE_ID: {}", fastly_service_id);
            //println!("  - FASTLY_SERVICE_VERSION: {}", fastly_service_version);
            println!("  - FASTLY_TRACE_ID: {}", fastly_trace_id);

            // Log all response headers
            println!("Response headers from Equativ:");
            for (name, value) in res.get_headers() {
                println!("  {}: {:?}", name, value);
            }

            if res.get_status().is_success() {
                let body = res.take_body_str();
                println!("Backend response body: {}", body);

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
                            println!("Found opid: {}", opid);

                            // Store in opid KV store
                            println!(
                                "Attempting to open KV store: {}",
                                settings.synthetic.opid_store
                            );
                            match KVStore::open(settings.synthetic.opid_store.as_str()) {
                                Ok(Some(store)) => {
                                    println!("Successfully opened KV store");
                                    match store.insert(&synthetic_id, opid.as_bytes()) {
                                        Ok(_) => println!(
                                            "Successfully stored opid {} for synthetic ID: {}",
                                            opid, synthetic_id
                                        ),
                                        Err(e) => {
                                            println!("Error storing opid in KV store: {:?}", e)
                                        }
                                    }
                                }
                                Ok(None) => {
                                    println!(
                                        "KV store returned None: {}",
                                        settings.synthetic.opid_store
                                    );
                                }
                                Err(e) => {
                                    println!(
                                        "Error opening KV store '{}': {:?}",
                                        settings.synthetic.opid_store, e
                                    );
                                }
                            };
                        }
                    }
                }

                // Return the JSON response with CORS headers
                let mut response = Response::from_status(StatusCode::OK)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header(header::CACHE_CONTROL, "no-store, private")
                    .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .with_header(
                        header::ACCESS_CONTROL_EXPOSE_HEADERS,
                        "X-Geo-City, X-Geo-Country, X-Geo-Continent, X-Geo-Coordinates, X-Geo-Metro-Code, X-Geo-Info-Available"
                    )
                    .with_header("x-compress-hint", "on")
                    .with_body(body);

                // Copy geo headers from request to response
                for header_name in &["X-Geo-City", "X-Geo-Country", "X-Geo-Continent", "X-Geo-Coordinates", "X-Geo-Metro-Code", "X-Geo-Info-Available"] {
                    if let Some(value) = req.get_header(*header_name) {
                        response.set_header(*header_name, value);
                    }
                }

                // Attach PoP info to the response
                //response.set_header("X-Debug-Fastly-PoP", &fastly_pop);
                //println!("Added X-Debug-Fastly-PoP: {}", fastly_pop);

                Ok(response)
            } else {
                println!("Backend returned non-success status");
                Ok(Response::from_status(StatusCode::NO_CONTENT)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header("x-compress-hint", "on")
                    .with_body("{}"))
            }
        }
        Err(e) => {
            println!("Error making backend request: {:?}", e);
            Ok(Response::from_status(StatusCode::NO_CONTENT)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header("x-compress-hint", "on")
                .with_body("{}"))
        }
    }
}

/// Handles the prebid test route with detailed error logging
async fn handle_prebid_test(settings: &Settings, mut req: Request) -> Result<Response, Error> {
    println!("Starting prebid test request handling");

    // Check consent status from headers
    let advertising_consent = req.get_header("X-Consent-Advertising")
        .and_then(|h| h.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false);

    // Calculate fresh ID and synthetic ID only if we have advertising consent
    let (fresh_id, synthetic_id) = if advertising_consent {
        let fresh = synthetic::generate_synthetic_id(settings, &req);
        let synth = synthetic::get_or_generate_synthetic_id(settings, &req);
        (fresh, synth)
    } else {
        // Use non-personalized IDs when no consent
        ("non-personalized".to_string(), "non-personalized".to_string())
    };

    println!(
        "Existing POTSI header: {:?}",
        req.get_header(SYNTH_HEADER_POTSI)
    );
    println!("Generated Fresh ID: {}", fresh_id);
    println!("Using POTSI ID: {}", synthetic_id);
    println!("Advertising consent: {}", advertising_consent);

    // Set both IDs as headers
    req.set_header(SYNTH_HEADER_FRESH, &fresh_id);
    req.set_header(SYNTH_HEADER_POTSI, &synthetic_id);
    req.set_header("X-Consent-Advertising", if advertising_consent { "true" } else { "false" });

    println!("Using POTSI ID: {}, Fresh ID: {}", synthetic_id, fresh_id);

    let prebid_req = match PrebidRequest::new(settings, &req) {
        Ok(req) => {
            println!(
                "Successfully created PrebidRequest with synthetic ID: {}",
                req.synthetic_id
            );
            req
        }
        Err(e) => {
            println!("Error creating PrebidRequest: {:?}", e);
            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body_json(&json!({
                    "error": "Failed to create prebid request",
                    "details": format!("{:?}", e)
                }))?);
        }
    };

    println!("Attempting to send bid request to Prebid Server at prebid_backend");

    match prebid_req.send_bid_request(&req).await {
        Ok(mut prebid_response) => {
            println!("Received response from Prebid Server");
            println!("Response status: {}", prebid_response.get_status());

            println!("Response headers:");
            for (name, value) in prebid_response.get_headers() {
                println!("  {}: {:?}", name, value);
            }

            let body = prebid_response.take_body_str();
            println!("Response body: {}", body);

            Ok(Response::from_status(StatusCode::OK)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_header("X-Prebid-Test", "true")
                .with_header("X-Synthetic-ID", &prebid_req.synthetic_id)
                .with_header("X-Consent-Advertising", if advertising_consent { "true" } else { "false" })
                .with_header("x-compress-hint", "on")
                .with_body(body))
        }
        Err(e) => {
            println!("Error sending bid request: {:?}", e);
            println!("Backend name used: prebid_backend");
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
