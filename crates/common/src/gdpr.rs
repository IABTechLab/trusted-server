use std::collections::HashMap;

use http::header;
use http::method::Method;
use http::status::StatusCode;

use fastly::{Error, Response};

use serde::{Deserialize, Serialize};

use crate::constants::HEADER_X_SUBJECT_ID;
use crate::cookies;
use crate::http_wrapper::RequestWrapper;
use crate::settings::Settings;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GdprConsent {
    pub analytics: bool,
    pub advertising: bool,
    pub functional: bool,
    pub timestamp: i64,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserData {
    pub visit_count: i32,
    pub last_visit: i64,
    pub ad_interactions: Vec<String>,
    pub consent_history: Vec<GdprConsent>,
}

impl Default for GdprConsent {
    fn default() -> Self {
        Self {
            analytics: false,
            advertising: false,
            functional: false,
            timestamp: chrono::Utc::now().timestamp(),
            version: "1.0".to_string(),
        }
    }
}

impl Default for UserData {
    fn default() -> Self {
        Self {
            visit_count: 0,
            last_visit: chrono::Utc::now().timestamp(),
            ad_interactions: Vec::new(),
            consent_history: Vec::new(),
        }
    }
}

pub fn get_consent_from_request<T: RequestWrapper>(req: &T) -> Option<GdprConsent> {
    if let Some(jar) = cookies::handle_request_cookies(req) {
        if let Some(consent_cookie) = jar.get("gdpr_consent") {
            if let Ok(consent) = serde_json::from_str(consent_cookie.value()) {
                return Some(consent);
            }
        }
    }
    None
}

pub fn create_consent_cookie(consent: &GdprConsent, settings: &Settings) -> String {
    format!(
        "gdpr_consent={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age=31536000",
        serde_json::to_string(consent).unwrap_or_default(),
        settings.server.cookie_domain
    )
}

pub fn handle_consent_request<T: RequestWrapper>(
    settings: &Settings,
    mut req: T,
) -> Result<Response, Error> {
    match *req.get_method() {
        Method::GET => {
            // Return current consent status
            let consent = get_consent_from_request(&req).unwrap_or_default();
            Ok(Response::from_status(StatusCode::OK)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body(serde_json::to_string(&consent)?))
        }
        Method::POST => {
            // Update consent preferences
            let consent: GdprConsent = serde_json::from_slice(req.into_body_bytes().as_slice())?;
            let mut response = Response::from_status(StatusCode::OK)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body(serde_json::to_string(&consent)?);

            response.set_header(
                header::SET_COOKIE,
                create_consent_cookie(&consent, settings),
            );
            Ok(response)
        }
        _ => {
            Ok(Response::from_status(StatusCode::METHOD_NOT_ALLOWED)
                .with_body("Method not allowed"))
        }
    }
}

pub fn handle_data_subject_request<T: RequestWrapper>(
    _settings: &Settings,
    req: T,
) -> Result<Response, Error> {
    match *req.get_method() {
        Method::GET => {
            // Handle data access request
            if let Some(synthetic_id) = req.get_header(HEADER_X_SUBJECT_ID) {
                // Create a HashMap to store all user-related data
                let mut data: HashMap<String, UserData> = HashMap::new();

                // TODO: Implement actual data retrieval from KV store
                // For now, return empty user data
                data.insert(synthetic_id.to_str()?.to_string(), UserData::default());

                Ok(Response::from_status(StatusCode::OK)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_body(serde_json::to_string(&data)?))
            } else {
                Ok(Response::from_status(StatusCode::BAD_REQUEST).with_body("Missing subject ID"))
            }
        }
        Method::DELETE => {
            // Handle right to erasure (right to be forgotten)
            if let Some(_synthetic_id) = req.get_header(HEADER_X_SUBJECT_ID) {
                // TODO: Implement data deletion from KV store
                Ok(Response::from_status(StatusCode::OK)
                    .with_body("Data deletion request processed"))
            } else {
                Ok(Response::from_status(StatusCode::BAD_REQUEST).with_body("Missing subject ID"))
            }
        }
        _ => {
            Ok(Response::from_status(StatusCode::METHOD_NOT_ALLOWED)
                .with_body("Method not allowed"))
        }
    }
}
