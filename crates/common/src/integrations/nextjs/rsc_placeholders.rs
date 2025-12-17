use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use crate::integrations::{
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

use super::shared::find_rsc_push_payload_range;
use super::{NextJsIntegrationConfig, NEXTJS_INTEGRATION_ID};

pub(super) const RSC_PAYLOAD_PLACEHOLDER_PREFIX: &str = "__ts_rsc_payload_";
pub(super) const RSC_PAYLOAD_PLACEHOLDER_SUFFIX: &str = "__";

#[derive(Default)]
pub(super) struct NextJsRscPostProcessState {
    pub(super) payloads: Vec<String>,
    buffer: String,
    buffering: bool,
}

impl NextJsRscPostProcessState {
    fn buffer_chunk(&mut self, chunk: &str) {
        if !self.buffering {
            self.buffering = true;
            self.buffer.clear();
        }
        self.buffer.push_str(chunk);
    }

    /// Returns the complete script content, either borrowed from input or owned from buffer.
    fn take_script_or_borrow<'a>(&mut self, chunk: &'a str) -> Cow<'a, str> {
        if self.buffering {
            self.buffer.push_str(chunk);
            self.buffering = false;
            Cow::Owned(std::mem::take(&mut self.buffer))
        } else {
            Cow::Borrowed(chunk)
        }
    }

    pub(super) fn take_payloads(&mut self) -> Vec<String> {
        self.buffer.clear();
        self.buffering = false;
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

        if !ctx.is_last_in_text_node {
            if let Some(existing) = ctx
                .document_state
                .get::<Mutex<NextJsRscPostProcessState>>(NEXTJS_INTEGRATION_ID)
            {
                let mut guard = existing.lock().unwrap_or_else(|e| e.into_inner());
                if guard.buffering {
                    guard.buffer_chunk(content);
                    return ScriptRewriteAction::remove_node();
                }
            }

            let trimmed = content.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                // Avoid interfering with other inline JSON scripts (e.g. `__NEXT_DATA__`, JSON-LD).
                return ScriptRewriteAction::keep();
            }

            let state = ctx
                .document_state
                .get_or_insert_with(NEXTJS_INTEGRATION_ID, || {
                    Mutex::new(NextJsRscPostProcessState::default())
                });
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            guard.buffer_chunk(content);
            return ScriptRewriteAction::remove_node();
        }

        if !content.contains("__next_f")
            && ctx
                .document_state
                .get::<Mutex<NextJsRscPostProcessState>>(NEXTJS_INTEGRATION_ID)
                .is_none()
        {
            return ScriptRewriteAction::keep();
        }

        let state = ctx
            .document_state
            .get_or_insert_with(NEXTJS_INTEGRATION_ID, || {
                Mutex::new(NextJsRscPostProcessState::default())
            });
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let script = guard.take_script_or_borrow(content);
        let was_buffered = matches!(script, Cow::Owned(_));

        if !script.contains("__next_f") {
            if was_buffered {
                return ScriptRewriteAction::replace(script.into_owned());
            }
            return ScriptRewriteAction::keep();
        }

        let Some((payload_start, payload_end)) = find_rsc_push_payload_range(&script) else {
            if was_buffered {
                return ScriptRewriteAction::replace(script.into_owned());
            }
            return ScriptRewriteAction::keep();
        };

        if payload_start > payload_end
            || payload_end > script.len()
            || !script.is_char_boundary(payload_start)
            || !script.is_char_boundary(payload_end)
        {
            if was_buffered {
                return ScriptRewriteAction::replace(script.into_owned());
            }
            return ScriptRewriteAction::keep();
        }

        let placeholder_index = guard.payloads.len();
        let placeholder = rsc_payload_placeholder(placeholder_index);
        guard
            .payloads
            .push(script[payload_start..payload_end].to_string());

        let mut rewritten = script.into_owned();
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
    fn buffers_fragmented_scripts_and_emits_single_replacement() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());

        let first = "self.__next_f.push([1,\"https://origin.example.com";
        let second = "/page\"])";

        let action_first = rewriter.rewrite(first, &ctx(false, &state));
        assert_eq!(
            action_first,
            ScriptRewriteAction::RemoveNode,
            "Intermediate chunk should be removed"
        );

        let action_second = rewriter.rewrite(second, &ctx(true, &state));
        let ScriptRewriteAction::Replace(rewritten) = action_second else {
            panic!("Final chunk should be replaced with combined output");
        };

        assert!(
            rewritten.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "Combined output should include placeholder. Got: {rewritten}"
        );
        assert!(
            rewritten.contains("self.__next_f.push"),
            "Combined output should keep the push call. Got: {rewritten}"
        );
    }
}
