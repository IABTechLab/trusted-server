use std::sync::{Arc, Mutex};

use crate::integrations::{
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

use super::shared::find_rsc_push_payload_range;
use super::{NextJsIntegrationConfig, NEXTJS_INTEGRATION_ID};

pub(super) const RSC_PAYLOAD_PLACEHOLDER_PREFIX: &str = "__ts_rsc_payload_";
pub(super) const RSC_PAYLOAD_PLACEHOLDER_SUFFIX: &str = "__";

/// State for RSC placeholder-based rewriting.
///
/// Stores RSC payloads extracted during streaming for later rewriting during post-processing.
/// Only unfragmented RSC scripts are processed during streaming; fragmented scripts are
/// handled by the post-processor which re-parses the final HTML.
#[derive(Default)]
pub(super) struct NextJsRscPostProcessState {
    pub(super) payloads: Vec<String>,
}

impl NextJsRscPostProcessState {
    pub(super) fn take_payloads(&mut self) -> Vec<String> {
        std::mem::take(&mut self.payloads)
    }
}

fn rsc_payload_placeholder(index: usize) -> String {
    format!("{RSC_PAYLOAD_PLACEHOLDER_PREFIX}{index}{RSC_PAYLOAD_PLACEHOLDER_SUFFIX}")
}

pub(super) struct NextJsRscPlaceholderRewriter {
    config: Arc<NextJsIntegrationConfig>,
}

impl NextJsRscPlaceholderRewriter {
    pub(super) fn new(config: Arc<NextJsIntegrationConfig>) -> Self {
        Self { config }
    }
}

impl IntegrationScriptRewriter for NextJsRscPlaceholderRewriter {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn selector(&self) -> &'static str {
        "script"
    }

    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction {
        if !self.config.enabled || self.config.rewrite_attributes.is_empty() {
            return ScriptRewriteAction::keep();
        }

        // Only process complete (unfragmented) scripts during streaming.
        // Fragmented scripts are handled by the post-processor which re-parses the final HTML.
        // This avoids corrupting non-RSC scripts that happen to be fragmented during streaming.
        if !ctx.is_last_in_text_node {
            // Script is fragmented - skip placeholder processing.
            // The post-processor will handle RSC scripts at end-of-document.
            return ScriptRewriteAction::keep();
        }

        // Quick check: skip scripts that can't be RSC payloads
        if !content.contains("__next_f") {
            return ScriptRewriteAction::keep();
        }

        let Some((payload_start, payload_end)) = find_rsc_push_payload_range(content) else {
            // Contains __next_f but doesn't match RSC push pattern - leave unchanged
            return ScriptRewriteAction::keep();
        };

        if payload_start > payload_end
            || payload_end > content.len()
            || !content.is_char_boundary(payload_start)
            || !content.is_char_boundary(payload_end)
        {
            return ScriptRewriteAction::keep();
        }

        // Insert placeholder for this RSC payload and store original for post-processing
        let state = ctx
            .document_state
            .get_or_insert_with(NEXTJS_INTEGRATION_ID, || {
                Mutex::new(NextJsRscPostProcessState::default())
            });
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());

        let placeholder_index = guard.payloads.len();
        let placeholder = rsc_payload_placeholder(placeholder_index);
        guard
            .payloads
            .push(content[payload_start..payload_end].to_string());

        let mut rewritten = content.to_string();
        rewritten.replace_range(payload_start..payload_end, &placeholder);
        ScriptRewriteAction::replace(rewritten)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationDocumentState;

    fn ctx<'a>(
        is_last_in_text_node: bool,
        document_state: &'a IntegrationDocumentState,
    ) -> IntegrationScriptContext<'a> {
        IntegrationScriptContext {
            selector: "script",
            request_host: "proxy.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node,
            document_state,
        }
    }

    fn test_config() -> Arc<NextJsIntegrationConfig> {
        Arc::new(NextJsIntegrationConfig {
            enabled: true,
            rewrite_attributes: vec!["href".into(), "link".into(), "url".into()],
            max_combined_payload_bytes: 10 * 1024 * 1024,
        })
    }

    #[test]
    fn inserts_placeholder_and_records_payload() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());

        let script = r#"self.__next_f.push([1,"https://origin.example.com/page"])"#;
        let action = rewriter.rewrite(script, &ctx(true, &state));

        let ScriptRewriteAction::Replace(rewritten) = action else {
            panic!("Expected placeholder insertion to replace script");
        };
        assert!(
            rewritten.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "Rewritten script should contain placeholder. Got: {rewritten}"
        );

        let stored = state
            .get::<Mutex<NextJsRscPostProcessState>>(NEXTJS_INTEGRATION_ID)
            .expect("should store RSC state");
        let guard = stored.lock().expect("should lock Next.js RSC state");
        assert_eq!(guard.payloads.len(), 1, "Should store exactly one payload");
        assert_eq!(
            guard.payloads[0], "https://origin.example.com/page",
            "Stored payload should match original"
        );
    }

    #[test]
    fn skips_fragmented_scripts_for_post_processor_handling() {
        // Fragmented scripts are not processed during streaming - they're passed through
        // unchanged and handled by the post-processor which re-parses the final HTML.
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());

        let first = "self.__next_f.push([1,\"https://origin.example.com";
        let second = "/page\"])";

        // Intermediate chunk should be kept (not processed)
        let action_first = rewriter.rewrite(first, &ctx(false, &state));
        assert_eq!(
            action_first,
            ScriptRewriteAction::Keep,
            "Intermediate chunk should be kept unchanged"
        );

        // Final chunk should also be kept since it doesn't contain the full RSC pattern
        let action_second = rewriter.rewrite(second, &ctx(true, &state));
        assert_eq!(
            action_second,
            ScriptRewriteAction::Keep,
            "Final chunk of fragmented script should be kept"
        );

        // No payloads should be stored - post-processor will handle this
        assert!(
            state
                .get::<Mutex<NextJsRscPostProcessState>>(NEXTJS_INTEGRATION_ID)
                .is_none(),
            "No RSC state should be created for fragmented scripts"
        );
    }

    #[test]
    fn skips_non_rsc_scripts() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());

        let script = r#"console.log("hello world");"#;
        let action = rewriter.rewrite(script, &ctx(true, &state));

        assert_eq!(
            action,
            ScriptRewriteAction::Keep,
            "Non-RSC scripts should be kept unchanged"
        );
    }
}
