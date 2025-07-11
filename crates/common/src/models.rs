//! Data models for ad serving and callbacks.
//!
//! This module defines the structures used for communication with ad servers
//! and tracking callbacks.

use serde::Deserialize;

/// Response from an ad server containing creative details.
///
/// Contains all the information needed to display an ad and track
/// its performance through various callbacks.
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AdResponse {
    /// Network identifier for the ad network.
    pub network_id: String,
    /// Site identifier where the ad will be displayed.
    pub site_id: String,
    /// Page identifier within the site.
    pub page_id: String,
    /// Format identifier for the ad format.
    pub format_id: String,
    /// Advertiser identifier.
    pub advertiser_id: String,
    /// Campaign identifier.
    pub campaign_id: String,
    /// Insertion order identifier.
    pub insertion_id: String,
    /// Creative identifier.
    pub creative_id: String,
    /// URL of the creative asset to display.
    pub creative_url: String,
    /// List of tracking callbacks for various events.
    pub callbacks: Vec<Callback>,
}

/// Tracking callback for ad events.
///
/// Represents a URL that should be called when specific ad events occur,
/// such as impressions, clicks, or viewability milestones.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Callback {
    /// Type of callback (e.g., "impression", "click", "viewable").
    #[serde(rename = "type")]
    pub callback_type: String,
    /// URL to call when the event occurs.
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_callback_deserialization() {
        let json_data = json!({
            "type": "impression",
            "url": "https://example.com/track/impression"
        });

        let callback: Callback = serde_json::from_value(json_data).unwrap();
        assert_eq!(callback.callback_type, "impression");
        assert_eq!(callback.url, "https://example.com/track/impression");
    }

    #[test]
    fn test_callback_type_field_rename() {
        // Test that "type" is correctly renamed to callback_type
        let json_str = r#"{
            "type": "click",
            "url": "https://example.com/track/click"
        }"#;

        let callback: Callback = serde_json::from_str(json_str).unwrap();
        assert_eq!(callback.callback_type, "click");
        assert_eq!(callback.url, "https://example.com/track/click");
    }

    #[test]
    fn test_ad_response_full_deserialization() {
        let json_data = json!({
            "networkId": "12345",
            "siteId": "67890",
            "pageId": "11111",
            "formatId": "22222",
            "advertiserId": "33333",
            "campaignId": "44444",
            "insertionId": "55555",
            "creativeId": "66666",
            "creativeUrl": "https://cdn.example.com/creative/12345.jpg",
            "callbacks": [
                {
                    "type": "impression",
                    "url": "https://track.example.com/impression/12345"
                },
                {
                    "type": "click",
                    "url": "https://track.example.com/click/12345"
                },
                {
                    "type": "viewability",
                    "url": "https://track.example.com/viewability/12345"
                }
            ]
        });

        let ad_response: AdResponse = serde_json::from_value(json_data).unwrap();

        assert_eq!(ad_response.network_id, "12345");
        assert_eq!(ad_response.site_id, "67890");
        assert_eq!(ad_response.page_id, "11111");
        assert_eq!(ad_response.format_id, "22222");
        assert_eq!(ad_response.advertiser_id, "33333");
        assert_eq!(ad_response.campaign_id, "44444");
        assert_eq!(ad_response.insertion_id, "55555");
        assert_eq!(ad_response.creative_id, "66666");
        assert_eq!(
            ad_response.creative_url,
            "https://cdn.example.com/creative/12345.jpg"
        );

        assert_eq!(ad_response.callbacks.len(), 3);
        assert_eq!(ad_response.callbacks[0].callback_type, "impression");
        assert_eq!(
            ad_response.callbacks[0].url,
            "https://track.example.com/impression/12345"
        );
        assert_eq!(ad_response.callbacks[1].callback_type, "click");
        assert_eq!(ad_response.callbacks[2].callback_type, "viewability");
    }

    #[test]
    fn test_ad_response_empty_callbacks() {
        let json_data = json!({
            "networkId": "12345",
            "siteId": "67890",
            "pageId": "11111",
            "formatId": "22222",
            "advertiserId": "33333",
            "campaignId": "44444",
            "insertionId": "55555",
            "creativeId": "66666",
            "creativeUrl": "https://cdn.example.com/creative/12345.jpg",
            "callbacks": []
        });

        let ad_response: AdResponse = serde_json::from_value(json_data).unwrap();
        assert_eq!(ad_response.callbacks.len(), 0);
    }

    #[test]
    fn test_ad_response_missing_field() {
        // Missing required field should fail
        let json_data = json!({
            "networkId": "12345",
            "siteId": "67890",
            // Missing pageId
            "formatId": "22222",
            "advertiserId": "33333",
            "campaignId": "44444",
            "insertionId": "55555",
            "creativeId": "66666",
            "creativeUrl": "https://cdn.example.com/creative/12345.jpg",
            "callbacks": []
        });

        let result: Result<AdResponse, _> = serde_json::from_value(json_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_ad_response_case_sensitivity() {
        // Test camelCase to snake_case conversion
        let json_str = r#"{
            "networkId": "net123",
            "siteId": "site456",
            "pageId": "page789",
            "formatId": "format000",
            "advertiserId": "adv111",
            "campaignId": "camp222",
            "insertionId": "ins333",
            "creativeId": "cre444",
            "creativeUrl": "https://example.com/creative.png",
            "callbacks": []
        }"#;

        let ad_response: AdResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(ad_response.network_id, "net123");
        assert_eq!(ad_response.site_id, "site456");
        assert_eq!(ad_response.page_id, "page789");
        assert_eq!(ad_response.format_id, "format000");
    }

    #[test]
    fn test_callback_missing_field() {
        let json_data = json!({
            "type": "impression"
            // Missing url field
        });

        let result: Result<Callback, _> = serde_json::from_value(json_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_callback_extra_fields() {
        // Extra fields should be ignored
        let json_data = json!({
            "type": "conversion",
            "url": "https://example.com/track/conversion",
            "extra": "ignored",
            "another": 123
        });

        let callback: Callback = serde_json::from_value(json_data).unwrap();
        assert_eq!(callback.callback_type, "conversion");
        assert_eq!(callback.url, "https://example.com/track/conversion");
    }

    #[test]
    fn test_ad_response_debug_format() {
        let callback = Callback {
            callback_type: "test".to_string(),
            url: "https://test.com".to_string(),
        };

        let ad_response = AdResponse {
            network_id: "123".to_string(),
            site_id: "456".to_string(),
            page_id: "789".to_string(),
            format_id: "000".to_string(),
            advertiser_id: "111".to_string(),
            campaign_id: "222".to_string(),
            insertion_id: "333".to_string(),
            creative_id: "444".to_string(),
            creative_url: "https://example.com/ad.jpg".to_string(),
            callbacks: vec![callback],
        };

        let debug_str = format!("{:?}", ad_response);
        assert!(debug_str.contains("AdResponse"));
        assert!(debug_str.contains("network_id"));
        assert!(debug_str.contains("123"));
    }

    #[test]
    fn test_callback_debug_format() {
        let callback = Callback {
            callback_type: "debug_test".to_string(),
            url: "https://debug.test.com".to_string(),
        };

        let debug_str = format!("{:?}", callback);
        assert!(debug_str.contains("Callback"));
        assert!(debug_str.contains("callback_type"));
        assert!(debug_str.contains("debug_test"));
        assert!(debug_str.contains("url"));
        assert!(debug_str.contains("https://debug.test.com"));
    }

    #[test]
    fn test_various_callback_types() {
        let callback_types = vec![
            "impression",
            "click",
            "viewability",
            "conversion",
            "engagement",
            "complete",
            "firstQuartile",
            "midpoint",
            "thirdQuartile",
        ];

        for cb_type in callback_types {
            let json_data = json!({
                "type": cb_type,
                "url": format!("https://example.com/track/{}", cb_type)
            });

            let callback: Callback = serde_json::from_value(json_data).unwrap();
            assert_eq!(callback.callback_type, cb_type);
            assert_eq!(
                callback.url,
                format!("https://example.com/track/{}", cb_type)
            );
        }
    }
}
