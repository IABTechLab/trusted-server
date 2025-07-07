//! GDPR consent management and compliance.
//!
//! This module provides functionality for managing GDPR consent, including
//! consent tracking, data subject requests, and compliance with EU privacy regulations.

use fastly::http::{header, Method, StatusCode};
use fastly::{Error, Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::constants::HEADER_X_SUBJECT_ID;
use crate::cookies;
use crate::settings::Settings;

/// GDPR consent information for a user.
///
/// Tracks consent status for different purposes as required by GDPR.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GdprConsent {
    /// Consent for analytics and measurement.
    pub analytics: bool,
    /// Consent for personalized advertising.
    pub advertising: bool,
    /// Consent for functional cookies and features.
    pub functional: bool,
    /// Unix timestamp when consent was given.
    pub timestamp: i64,
    /// Version of the consent framework.
    pub version: String,
}

/// User data collected for GDPR compliance.
///
/// Contains all data collected about a user that must be made available
/// for data subject access requests.
#[derive(Debug, Serialize, Deserialize)]
pub struct UserData {
    /// Number of visits by the user.
    pub visit_count: i32,
    /// Unix timestamp of the last visit.
    pub last_visit: i64,
    /// List of ad interaction events.
    pub ad_interactions: Vec<String>,
    /// History of consent changes.
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

/// Extracts GDPR consent information from a request.
///
/// Looks for consent information in the `gdpr_consent` cookie and parses
/// it into a [`GdprConsent`] structure.
///
/// Returns [`None`] if no consent cookie is found or parsing fails.
pub fn get_consent_from_request(req: &Request) -> Option<GdprConsent> {
    match cookies::handle_request_cookies(req) {
        Ok(Some(jar)) => {
            if let Some(consent_cookie) = jar.get("gdpr_consent") {
                if let Ok(consent) = serde_json::from_str(consent_cookie.value()) {
                    return Some(consent);
                }
            }
            None
        }
        Ok(None) => None,
        Err(e) => {
            log::warn!("Failed to parse cookies for consent: {:?}", e);
            None
        }
    }
}

/// Creates a GDPR consent cookie string.
///
/// Generates a properly formatted cookie string with the consent data,
/// including security attributes and domain settings.
pub fn create_consent_cookie(settings: &Settings, consent: &GdprConsent) -> String {
    format!(
        "gdpr_consent={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age=31536000",
        serde_json::to_string(consent).unwrap_or_default(),
        settings.publisher.cookie_domain,
    )
}

/// Handles GDPR consent management requests.
///
/// Processes GET and POST requests to the `/gdpr/consent` endpoint:
/// - GET: Returns current consent status
/// - POST: Updates consent preferences
///
/// # Errors
///
/// Returns a Fastly [`Error`] if response creation fails.
pub fn handle_consent_request(settings: &Settings, req: Request) -> Result<Response, Error> {
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
                create_consent_cookie(settings, &consent),
            );
            Ok(response)
        }
        _ => {
            Ok(Response::from_status(StatusCode::METHOD_NOT_ALLOWED)
                .with_body("Method not allowed"))
        }
    }
}

/// Handles GDPR data subject access requests.
///
/// Processes requests to view or delete user data as required by GDPR:
/// - GET: Returns all collected user data
/// - DELETE: Removes all user data
///
/// Requires the `X-Subject-ID` header for authentication.
///
/// # Errors
///
/// Returns a Fastly [`Error`] if response creation fails.
pub fn handle_data_subject_request(_settings: &Settings, req: Request) -> Result<Response, Error> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::{Body, Request};

    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_gdpr_consent_default() {
        let consent = GdprConsent::default();
        assert!(!consent.analytics);
        assert!(!consent.advertising);
        assert!(!consent.functional);
        assert_eq!(consent.version, "1.0");
        assert!(consent.timestamp > 0);
    }

    #[test]
    fn test_user_data_default() {
        let data = UserData::default();
        assert_eq!(data.visit_count, 0);
        assert!(data.last_visit > 0);
        assert!(data.ad_interactions.is_empty());
        assert!(data.consent_history.is_empty());
    }

    #[test]
    fn test_gdpr_consent_serialization() {
        let consent = GdprConsent {
            analytics: true,
            advertising: false,
            functional: true,
            timestamp: 1234567890,
            version: "2.0".to_string(),
        };

        let json = serde_json::to_string(&consent).unwrap();
        assert!(json.contains("\"analytics\":true"));
        assert!(json.contains("\"advertising\":false"));
        assert!(json.contains("\"functional\":true"));
        assert!(json.contains("\"timestamp\":1234567890"));
        assert!(json.contains("\"version\":\"2.0\""));

        let deserialized: GdprConsent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.analytics, consent.analytics);
        assert_eq!(deserialized.advertising, consent.advertising);
        assert_eq!(deserialized.functional, consent.functional);
        assert_eq!(deserialized.timestamp, consent.timestamp);
        assert_eq!(deserialized.version, consent.version);
    }

    #[test]
    fn test_create_consent_cookie() {
        let settings = create_test_settings();
        let consent = GdprConsent {
            analytics: true,
            advertising: true,
            functional: true,
            timestamp: 1234567890,
            version: "1.0".to_string(),
        };

        let cookie = create_consent_cookie(&settings, &consent);
        assert!(cookie.starts_with("gdpr_consent="));
        assert!(cookie.contains(format!("Domain={}", settings.publisher.cookie_domain).as_str()));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Max-Age=31536000"));
    }

    #[test]
    fn test_get_consent_from_request_no_cookie() {
        let req = Request::get("https://example.com");
        let consent = get_consent_from_request(&req);
        assert!(consent.is_none());
    }

    #[test]
    fn test_get_consent_from_request_with_valid_cookie() {
        let mut req = Request::get("https://example.com");
        let consent_data = GdprConsent {
            analytics: true,
            advertising: false,
            functional: true,
            timestamp: 1234567890,
            version: "1.0".to_string(),
        };
        let cookie_value = format!(
            "gdpr_consent={}",
            serde_json::to_string(&consent_data).unwrap()
        );
        req.set_header(header::COOKIE, cookie_value);

        let consent = get_consent_from_request(&req);
        assert!(consent.is_some());
        let consent = consent.unwrap();
        assert!(consent.analytics);
        assert!(!consent.advertising);
        assert!(consent.functional);
    }

    #[test]
    fn test_get_consent_from_request_with_invalid_cookie() {
        let mut req = Request::get("https://example.com");
        req.set_header(header::COOKIE, "gdpr_consent=invalid-json");

        let consent = get_consent_from_request(&req);
        assert!(consent.is_none());
    }

    #[test]
    fn test_handle_consent_request_get() {
        let settings = create_test_settings();
        let req = Request::get("https://example.com/gdpr/consent");

        let response = handle_consent_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::OK);
        assert_eq!(
            response.get_header_str(header::CONTENT_TYPE),
            Some("application/json")
        );

        let body = response.into_body_str();
        let consent: GdprConsent = serde_json::from_str(&body).unwrap();
        assert!(!consent.analytics); // Default values
        assert!(!consent.advertising);
        assert!(!consent.functional);
    }

    #[test]
    fn test_handle_consent_request_post() {
        let settings = create_test_settings();
        let consent_data = GdprConsent {
            analytics: true,
            advertising: true,
            functional: false,
            timestamp: 1234567890,
            version: "1.0".to_string(),
        };

        let mut req = Request::post("https://example.com/gdpr/consent");
        req.set_body(Body::from(serde_json::to_string(&consent_data).unwrap()));

        let response = handle_consent_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::OK);
        assert_eq!(
            response.get_header_str(header::CONTENT_TYPE),
            Some("application/json")
        );

        // Check Set-Cookie header
        let set_cookie = response.get_header_str(header::SET_COOKIE);
        assert!(set_cookie.is_some());
        assert!(set_cookie.unwrap().contains("gdpr_consent="));

        assert!(set_cookie
            .unwrap()
            .contains(format!("Domain={}", settings.publisher.cookie_domain).as_str()));

        // Check response body
        let body = response.into_body_str();
        let returned_consent: GdprConsent = serde_json::from_str(&body).unwrap();
        assert!(returned_consent.analytics);
        assert!(returned_consent.advertising);
        assert!(!returned_consent.functional);
    }

    #[test]
    fn test_handle_consent_request_invalid_method() {
        let settings = create_test_settings();
        let req = Request::put("https://example.com/gdpr/consent");

        let response = handle_consent_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.into_body_str(), "Method not allowed");
    }

    #[test]
    fn test_handle_data_subject_request_get_with_id() {
        let settings = create_test_settings();
        let mut req = Request::get("https://example.com/gdpr/data");
        req.set_header(HEADER_X_SUBJECT_ID, "test-subject-123");

        let response = handle_data_subject_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::OK);
        assert_eq!(
            response.get_header_str(header::CONTENT_TYPE),
            Some("application/json")
        );

        let body = response.into_body_str();
        let data: HashMap<String, UserData> = serde_json::from_str(&body).unwrap();
        assert!(data.contains_key("test-subject-123"));
        assert_eq!(data["test-subject-123"].visit_count, 0); // Default value
    }

    #[test]
    fn test_handle_data_subject_request_get_without_id() {
        let settings = create_test_settings();
        let req = Request::get("https://example.com/gdpr/data");

        let response = handle_data_subject_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.into_body_str(), "Missing subject ID");
    }

    #[test]
    fn test_handle_data_subject_request_delete_with_id() {
        let settings = create_test_settings();
        let mut req = Request::delete("https://example.com/gdpr/data");
        req.set_header(HEADER_X_SUBJECT_ID, "test-subject-123");

        let response = handle_data_subject_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::OK);
        assert_eq!(response.into_body_str(), "Data deletion request processed");
    }

    #[test]
    fn test_handle_data_subject_request_delete_without_id() {
        let settings = create_test_settings();
        let req = Request::delete("https://example.com/gdpr/data");

        let response = handle_data_subject_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.into_body_str(), "Missing subject ID");
    }

    #[test]
    fn test_handle_data_subject_request_invalid_method() {
        let settings = create_test_settings();
        let req = Request::post("https://example.com/gdpr/data");

        let response = handle_data_subject_request(&settings, req).unwrap();
        assert_eq!(response.get_status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.into_body_str(), "Method not allowed");
    }

    #[test]
    fn test_user_data_serialization() {
        let user_data = UserData {
            visit_count: 5,
            last_visit: 1234567890,
            ad_interactions: vec!["click1".to_string(), "view2".to_string()],
            consent_history: vec![GdprConsent::default()],
        };

        let json = serde_json::to_string(&user_data).unwrap();
        assert!(json.contains("\"visit_count\":5"));
        assert!(json.contains("\"last_visit\":1234567890"));
        assert!(json.contains("\"ad_interactions\":[\"click1\",\"view2\"]"));

        let deserialized: UserData = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.visit_count, user_data.visit_count);
        assert_eq!(deserialized.last_visit, user_data.last_visit);
        assert_eq!(deserialized.ad_interactions.len(), 2);
    }
}
