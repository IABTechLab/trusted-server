//! GAM (Google Ad Manager) Interceptor Integration
//!
//! This integration forces Prebid creatives to render when GAM doesn't have
//! matching line items configured. It's a client-side only integration that
//! works by intercepting GPT's `slotRenderEnded` event.
//!
//! # Configuration
//!
//! ```toml
//! [integrations.gam]
//! enabled = true
//! bidders = ["mocktioneer"]  # Only intercept these bidders, empty = all
//! force_render = false       # Force render even if GAM has a line item
//! ```
//!
//! # Environment Variables
//!
//! ```bash
//! TRUSTED_SERVER__INTEGRATIONS__GAM__ENABLED=true
//! TRUSTED_SERVER__INTEGRATIONS__GAM__BIDDERS="mocktioneer,appnexus"
//! TRUSTED_SERVER__INTEGRATIONS__GAM__FORCE_RENDER=false
//! ```

use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::settings::IntegrationConfig;

use super::{IntegrationHeadInjector, IntegrationHtmlContext, IntegrationRegistration};

const GAM_INTEGRATION_ID: &str = "gam";

/// GAM interceptor configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize, Validate)]
pub struct GamIntegrationConfig {
    /// Enable the GAM interceptor. Defaults to false.
    #[serde(default)]
    pub enabled: bool,

    /// Only intercept bids from these bidders. Empty = all bidders.
    #[serde(default, deserialize_with = "crate::settings::vec_from_seq_or_map")]
    pub bidders: Vec<String>,

    /// Force render Prebid creative even if GAM returned a line item.
    #[serde(default)]
    pub force_render: bool,
}

impl IntegrationConfig for GamIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Generate the JavaScript config script tag for GAM integration.
/// Sets window.tsGamConfig which is picked up by the GAM integration on init.
#[must_use]
pub fn gam_config_script_tag(config: &GamIntegrationConfig) -> String {
    let bidders_json = if config.bidders.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            config
                .bidders
                .iter()
                .map(|b| format!("\"{}\"", b))
                .collect::<Vec<_>>()
                .join(",")
        )
    };

    format!(
        r#"<script>window.tsGamConfig={{enabled:true,bidders:{},forceRender:{}}};</script>"#,
        bidders_json, config.force_render
    )
}

pub struct GamIntegration {
    config: GamIntegrationConfig,
}

impl GamIntegration {
    #[must_use]
    pub fn new(config: GamIntegrationConfig) -> Self {
        Self { config }
    }
}

impl IntegrationHeadInjector for GamIntegration {
    fn integration_id(&self) -> &'static str {
        GAM_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        vec![gam_config_script_tag(&self.config)]
    }
}

/// Register the GAM integration if enabled.
#[must_use]
pub fn register(settings: &crate::settings::Settings) -> Option<IntegrationRegistration> {
    use std::sync::Arc;

    let config: GamIntegrationConfig =
        settings.integrations.get_typed(GAM_INTEGRATION_ID).ok()??;

    log::info!(
        "GAM integration enabled: bidders={:?}, force_render={}",
        config.bidders,
        config.force_render
    );

    let integration = Arc::new(GamIntegration::new(config));

    Some(
        IntegrationRegistration::builder(GAM_INTEGRATION_ID)
            .with_head_injector(integration)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gam_config_script_tag_with_bidders() {
        let config = GamIntegrationConfig {
            enabled: true,
            bidders: vec!["mocktioneer".to_string(), "appnexus".to_string()],
            force_render: false,
        };
        let tag = gam_config_script_tag(&config);
        assert!(tag.contains("window.tsGamConfig="));
        assert!(tag.contains("enabled:true"));
        assert!(tag.contains(r#"bidders:["mocktioneer","appnexus"]"#));
        assert!(tag.contains("forceRender:false"));
    }

    #[test]
    fn gam_config_script_tag_empty_bidders() {
        let config = GamIntegrationConfig {
            enabled: true,
            bidders: vec![],
            force_render: true,
        };
        let tag = gam_config_script_tag(&config);
        assert!(tag.contains("bidders:[]"));
        assert!(tag.contains("forceRender:true"));
    }

    #[test]
    fn gam_config_disabled_by_default() {
        let config = GamIntegrationConfig::default();
        assert!(!config.enabled);
        assert!(config.bidders.is_empty());
        assert!(!config.force_render);
    }
}
