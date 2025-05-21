use fastly::http::{header, Method};
use fastly::{Error, Request, Response};
use serde_json::json;

use crate::constants::{
    HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER, HEADER_X_FORWARDED_FOR,
};
use crate::http_wrapper::RequestWrapper;
use crate::settings::Settings;
use crate::synthetic::generate_synthetic_id;

/// Represents a request to the Prebid Server with all necessary parameters
pub struct PrebidRequest {
    /// Synthetic ID used for user identification across requests
    pub synthetic_id: String,
    /// Domain for the ad request
    pub domain: String,
    /// List of banner sizes as (width, height) tuples
    pub banner_sizes: Vec<(u32, u32)>,
    /// Client's IP address for geo-targeting and fraud prevention
    pub client_ip: String,
    /// Origin header for CORS and tracking
    pub origin: String,
}

impl PrebidRequest {
    /// Creates a new PrebidRequest from an incoming Fastly request
    ///
    /// # Arguments
    /// * `req` - The incoming Fastly request
    ///
    /// # Returns
    /// * `Result<Self, Error>` - New PrebidRequest or error
    pub fn new<T: RequestWrapper>(settings: &Settings, req: &T) -> Result<Self, Error> {
        // Get the Trusted Server ID from header (which we just set in handle_prebid_test)
        let synthetic_id = req
            .get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| generate_synthetic_id(settings, req));

        // Get the original client IP from Fastly headers
        let client_ip = req
            .get_client_ip_addr()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| {
                req.get_header(HEADER_X_FORWARDED_FOR)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
                    .split(',') // X-Forwarded-For can be a comma-separated list
                    .next() // Get the first (original) IP
                    .unwrap_or("")
                    .to_string()
            });

        // Try to get domain from Referer or Origin headers, fallback to default
        let domain = req
            .get_header(header::REFERER)
            .and_then(|h| h.to_str().ok())
            .and_then(|r| url::Url::parse(r).ok())
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .or_else(|| {
                req.get_header(header::ORIGIN)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|o| url::Url::parse(o).ok())
                    .and_then(|u| u.host_str().map(|h| h.to_string()))
            })
            .unwrap_or_else(|| "auburndao.com".to_string());

        println!("Detected domain: {}", domain);

        // Create origin with owned String
        let origin = req
            .get_header(header::ORIGIN)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("https://{}", domain));

        Ok(Self {
            synthetic_id,
            domain,
            banner_sizes: vec![(728, 90)], // TODO: Make this configurable
            client_ip,
            origin,
        })
    }

    /// Sends bid request to Prebid Server
    ///
    /// Makes an HTTP POST request to PBS with all necessary headers and body
    /// Uses the stored synthetic ID for user identification
    ///
    /// # Returns
    /// * `Result<Response, Error>` - Prebid Server response or error
    pub async fn send_bid_request(
        &self,
        settings: &Settings,
        incoming_req: &Request,
    ) -> Result<Response, Error> {
        let mut req = Request::new(Method::POST, settings.prebid.server_url.to_owned());

        // Get and store the POTSI ID value from the incoming request
        let id: String = incoming_req
            .get_header(HEADER_SYNTHETIC_TRUSTED_SERVER)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.synthetic_id.clone());

        println!("Found Truted Server ID from incoming request: {}", id);

        // Construct the OpenRTB2 bid request
        let prebid_body = json!({
            "id": id,
            "imp": [{
                "id": "imp1",
                "banner": {
                    "format": self.banner_sizes.iter().map(|(w, h)| {
                        json!({ "w": w, "h": h })
                    }).collect::<Vec<_>>()
                },
                "bidfloor": 0.01,
                "bidfloorcur": "USD",
                "ext": {
                    "prebid": {
                        "bidder": {
                            "smartadserver": {
                                "siteId": 686105,
                                "networkId": 5280,
                                "pageId": 2040327,
                                "formatId": 137675,
                                "target": "testing=prebid",
                                "domain": &self.domain
                            }
                        }
                    }
                }
            }],
            "site": { "page": format!("https://{}", self.domain) },
            "user": {
                "id": "5280",
                "ext": {
                    "eids": [
                        {
                            "source": &self.domain,
                            "uids": [{
                                "id": self.synthetic_id,
                                "atype": 1,
                                "ext": {
                                    "type": "fresh"
                                }
                            }],
                        },
                        {
                            "source": &self.domain,
                            "uids": [{
                                "id": &id,
                                "atype": 1,
                                "ext": {
                                    "type": "potsi" // TODO: remove reference to potsi
                                }
                            }]
                        }
                    ]
                }
            },
            "test": 1,
            "debug": 1,
            "tmax": 1000,
            "at": 1
        });

        req.set_header(header::CONTENT_TYPE, "application/json");
        req.set_header("X-Forwarded-For", &self.client_ip);
        req.set_header(header::ORIGIN, &self.origin);
        req.set_header(HEADER_SYNTHETIC_FRESH, &self.synthetic_id);
        req.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &id);

        println!(
            "Sending prebid request with Fresh ID: {} and Trusted Server ID: {}",
            self.synthetic_id, id
        );

        req.set_body_json(&prebid_body)?;

        let resp = req.send("prebid_backend")?;
        Ok(resp)
    }
}
