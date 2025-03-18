use fastly::http::{header, Method, StatusCode};
use fastly::KVStore;
use fastly::{Error, Request, Response};
use log::LevelFilter::Info;
use serde_json::json;
use std::env;

mod constants;
mod cookies;
use constants::*;
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

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    let _settings = Settings::new();
    futures::executor::block_on(async {
        println!(
            "FASTLY_SERVICE_VERSION: {}",
            std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
        );

        match (req.get_method(), req.get_path()) {
            (&Method::GET, "/") => handle_main_page(req),
            (&Method::GET, "/ad-creative") => handle_ad_request(req),
            (&Method::GET, "/prebid-test") => handle_prebid_test(req).await,
            _ => Ok(Response::from_status(StatusCode::NOT_FOUND)
                .with_body("Not Found")
                .with_header(header::CONTENT_TYPE, "text/plain")),
        }
    })
}

fn handle_main_page(req: Request) -> Result<Response, Error> {
    println!(
        "Testing constants - BACKEND2: {}, SYNTH_ID_COUNTER_STORE: {}",
        BACKEND2, SYNTH_ID_COUNTER_STORE
    );

    log_fastly::init_simple("mylogs", Info);

    // Calculate fresh ID first using the synthetic module
    let fresh_id = synthetic::generate_synthetic_id(&req);

    // Check for existing POTSI ID in this specific order:
    // 1. X-Synthetic-Potsi header
    // 2. Cookie
    // 3. Fall back to fresh ID
    let synthetic_id = synthetic::get_or_generate_synthetic_id(&req);

    println!(
        "Existing POTSI header: {:?}",
        req.get_header("X-Synthetic-Potsi")
    );
    println!("Generated Fresh ID: {}", fresh_id);
    println!("Using POTSI ID: {}", synthetic_id);

    // Create response with the main page HTML
    let mut response = Response::from_status(StatusCode::OK)
        .with_body(HTML_TEMPLATE)
        .with_header(header::CONTENT_TYPE, "text/html")
        .with_header("X-Synthetic-Fresh", &fresh_id) // Fresh ID always changes
        .with_header("X-Synthetic-Potsi", &synthetic_id); // POTSI ID remains stable

    // Always set the cookie with the synthetic ID
    response.set_header(
        header::SET_COOKIE,
        cookies::create_synthetic_cookie(&synthetic_id),
    );

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

fn handle_ad_request(req: Request) -> Result<Response, Error> {
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

    // Generate synthetic ID
    let synthetic_id = generate_synthetic_id(&req);

    // Increment visit counter in KV store
    println!("Opening KV store: {}", SYNTH_ID_COUNTER_STORE);
    let store = match KVStore::open(SYNTH_ID_COUNTER_STORE) {
        Ok(Some(store)) => store,
        Ok(None) => {
            println!("KV store not found");
            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR));
        }
        Err(e) => {
            println!("Error opening KV store: {:?}", e);
            return Ok(Response::from_status(StatusCode::INTERNAL_SERVER_ERROR));
        }
    };

    println!("Fetching current count for synthetic ID: {}", synthetic_id);
    let current_count: i32 = store
        .lookup(&synthetic_id)
        .and_then(|mut val| {
            // Convert LookupResponse to bytes first
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

    match store.insert(&synthetic_id, new_count.to_string().as_bytes()) {
        Ok(_) => println!("Successfully updated count in KV store"),
        Err(e) => println!("Error updating KV store: {:?}", e),
    }

    println!("Synthetic ID {} visit count: {}", synthetic_id, new_count);

    // Construct URL with synthetic ID
    let ad_server_url = format!(
        "https://adapi-srv-eu.smartadserver.com/ac?pgid=2040327&fmtid=137675&synthetic_id={}",
        synthetic_id
    );

    println!("Sending request to backend: {}", ad_server_url);

    // Add header logging here
    let req = Request::get(ad_server_url);
    println!("Request headers to Equativ:");
    for (name, value) in req.get_headers() {
        println!("  {}: {:?}", name, value);
    }

    match req.send(BACKEND2) {
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
                            println!("Attempting to open KV store: {}", SYNTH_ID_OPID_STORE);
                            match KVStore::open(SYNTH_ID_OPID_STORE) {
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
                                    println!("KV store returned None: {}", SYNTH_ID_OPID_STORE);
                                }
                                Err(e) => {
                                    println!(
                                        "Error opening KV store '{}': {:?}",
                                        SYNTH_ID_OPID_STORE, e
                                    );
                                }
                            };
                        }
                    }
                }

                // Return the JSON response with CORS headers
                let response = Response::from_status(StatusCode::OK)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header(header::CACHE_CONTROL, "no-store, private")
                    .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .with_body(body);

                // Attach PoP info to the response
                //response.set_header("X-Debug-Fastly-PoP", &fastly_pop);
                //println!("Added X-Debug-Fastly-PoP: {}", fastly_pop);

                Ok(response)
            } else {
                println!("Backend returned non-success status");
                Ok(Response::from_status(StatusCode::NO_CONTENT)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_body("{}"))
            }
        }
        Err(e) => {
            println!("Error making backend request: {:?}", e);
            Ok(Response::from_status(StatusCode::NO_CONTENT)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body("{}"))
        }
    }
}

/// Handles the prebid test route with detailed error logging
async fn handle_prebid_test(mut req: Request) -> Result<Response, Error> {
    println!("Starting prebid test request handling");

    // Calculate fresh ID
    let fresh_id = synthetic::generate_synthetic_id(&req);

    // Check for existing POTSI ID in same order as handle_main_page
    let synthetic_id = synthetic::get_or_generate_synthetic_id(&req);

    println!(
        "Existing POTSI header: {:?}",
        req.get_header("X-Synthetic-Potsi")
    );
    println!("Generated Fresh ID: {}", fresh_id);
    println!("Using POTSI ID: {}", synthetic_id);

    // Set both IDs as headers
    req.set_header("X-Synthetic-Fresh", &fresh_id);
    req.set_header("X-Synthetic-Potsi", &synthetic_id);

    println!("Using POTSI ID: {}, Fresh ID: {}", synthetic_id, fresh_id);

    let prebid_req = match PrebidRequest::new(&req) {
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
        // Pass the original request
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
