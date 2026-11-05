//! APS renderer wire contract and handler-owned targeting metadata.
use crate::auction::types::BidRenderer;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;
use serde_json::json;
/// APS creative delivery mode interpreted by the APS renderer handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApsTagType {
    /// Load the creative URL in a nested iframe.
    Iframe,
    /// Fetch creative HTML in the nested renderer frame.
    Script,
}

/// Version-one APS renderer payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApsRendererV1 {
    /// Renderer contract version.
    pub version: u8,
    /// Account used to initialize the runner.
    pub account_id: String,
    /// Selected bid identifier, also supplied as trusted targeting metadata.
    pub bid_id: String,
    /// Optional creative identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creative_id: Option<String>,
    /// Creative delivery mode.
    pub tag_type: ApsTagType,
    /// Creative URL consumed by the runner.
    pub creative_url: String,
    /// Base64-encoded exact one-bid response.
    pub aax_response: String,
    /// Creative width.
    pub width: u32,
    /// Creative height.
    pub height: u32,
}
impl From<ApsRendererV1> for BidRenderer {
    fn from(renderer: ApsRendererV1) -> Self {
        let mut payload = vec![
            ("version".to_string(), json!(renderer.version)),
            ("accountId".to_string(), json!(renderer.account_id)),
            ("bidId".to_string(), json!(renderer.bid_id)),
        ];
        if let Some(id) = renderer.creative_id {
            payload.push(("creativeId".to_string(), json!(id)));
        }
        payload.extend([
            ("tagType".to_string(), json!(renderer.tag_type)),
            ("creativeUrl".to_string(), json!(renderer.creative_url)),
            ("aaxResponse".to_string(), json!(renderer.aax_response)),
            ("width".to_string(), json!(renderer.width)),
            ("height".to_string(), json!(renderer.height)),
        ]);
        BidRenderer::new("aps", payload, Some(renderer.bid_id))
    }
}

/// APS-owned interpretation used by its behavior tests.
#[cfg(test)]
pub(crate) trait ApsRendererDescriptor {
    fn as_aps(&self) -> Option<ApsRendererV1>;
}
#[cfg(test)]
impl ApsRendererDescriptor for BidRenderer {
    fn as_aps(&self) -> Option<ApsRendererV1> {
        if self.renderer_type != "aps" {
            return None;
        }
        serde_json::from_value(Value::Object(self.payload.iter().cloned().collect())).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_retains_exact_tagged_aps_bytes_without_targeting_metadata() {
        let descriptor = BidRenderer::from(ApsRendererV1 {
            version: 1,
            account_id: "example-account".to_string(),
            bid_id: "selected-bid".to_string(),
            creative_id: Some("selected-creative".to_string()),
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "encoded".to_string(),
            width: 300,
            height: 250,
        });
        let wire = serde_json::to_string(&descriptor).expect("should serialize APS descriptor");
        assert_eq!(
            wire,
            r#"{"type":"aps","version":1,"accountId":"example-account","bidId":"selected-bid","creativeId":"selected-creative","tagType":"iframe","creativeUrl":"https://creative.example/render","aaxResponse":"encoded","width":300,"height":250}"#
        );
        assert_eq!(descriptor.targeting_bid_id.as_deref(), Some("selected-bid"));
        let decoded: BidRenderer =
            serde_json::from_str(&wire).expect("should deserialize opaque descriptor");
        assert_eq!(
            serde_json::to_string(&decoded).expect("should serialize opaque descriptor"),
            wire
        );
        assert!(
            decoded.targeting_bid_id.is_none(),
            "wire input must not assert trusted targeting metadata"
        );
    }
}
