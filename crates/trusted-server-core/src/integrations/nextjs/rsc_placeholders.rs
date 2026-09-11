use std::sync::Arc;

use crate::integrations::{
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
};

use super::rsc::DEFAULT_MAX_COMBINED_PAYLOAD_BYTES;
#[cfg(test)]
pub(super) use super::rsc_stream::RSC_PAYLOAD_PLACEHOLDER_PREFIX;
use super::rsc_stream::{
    CapturedPayload, FragmentCapture, MAX_UNRESOLVED_RSC_PAYLOADS, RscGroupStatus,
    capture_fragment, classify_rsc_group, document_state, rsc_payload_placeholder,
};
use super::shared::{
    RSC_RECEIVER_CONTEXT_BYTES, find_rsc_push_payload_range, find_trimmed_rsc_push_payload_range,
    receiver_context_is_flight_push,
};
use super::{NEXTJS_INTEGRATION_ID, NextJsIntegrationConfig};

pub(super) struct NextJsRscPlaceholderRewriter {
    config: Arc<NextJsIntegrationConfig>,
}

impl NextJsRscPlaceholderRewriter {
    pub(super) fn new(config: Arc<NextJsIntegrationConfig>) -> Self {
        Self { config }
    }

    fn rewrite_complete(
        &self,
        content: &str,
        was_buffered: bool,
        state: &mut super::rsc_stream::NextJsDocumentState,
        limit: usize,
        max_queued_payload_bytes: usize,
    ) -> ScriptRewriteAction {
        if !content.contains("__next_f") {
            return if was_buffered {
                ScriptRewriteAction::replace(content.to_owned())
            } else {
                ScriptRewriteAction::Keep
            };
        }

        let range = if state.rsc_receiver_trimmed {
            find_trimmed_rsc_push_payload_range(content)
        } else {
            find_rsc_push_payload_range(content)
        };
        state.rsc_receiver_trimmed = false;
        let Some((payload_start, payload_end)) = range else {
            return if was_buffered {
                ScriptRewriteAction::replace(content.to_owned())
            } else {
                ScriptRewriteAction::Keep
            };
        };

        if payload_start > payload_end
            || payload_end > content.len()
            || !content.is_char_boundary(payload_start)
            || !content.is_char_boundary(payload_end)
        {
            state.bypass_rsc = true;
            return if was_buffered {
                ScriptRewriteAction::replace(content.to_owned())
            } else {
                ScriptRewriteAction::Keep
            };
        }

        let payload = &content[payload_start..payload_end];
        // `limit` bounds one script here and the downstream unresolved group in
        // `classify_rsc_group`. It deliberately does not bound this queue in
        // aggregate: the processor cannot decrement a resolved group until the
        // whole parser call returns, so a shared `limit` budget would make
        // independent payloads bypass each other purely because they shared a
        // source chunk. The queue is parser-held script text, so it is bounded
        // by the parser's own script-buffer budget instead — the same budget
        // `NextJsNextDataRewriter` buffers against — plus a payload count.
        let exceeds_limit = payload.len() > limit
            || state.captured_payloads.len() >= MAX_UNRESOLVED_RSC_PAYLOADS
            || state
                .captured_payload_bytes
                .checked_add(payload.len())
                .is_none_or(|queued| queued > max_queued_payload_bytes);
        if exceeds_limit {
            state.bypass_rsc = true;
            return if was_buffered {
                ScriptRewriteAction::replace(content.to_owned())
            } else {
                ScriptRewriteAction::Keep
            };
        }

        let placeholder = rsc_payload_placeholder(&state.namespace, state.next_placeholder_index);
        state.next_placeholder_index = state.next_placeholder_index.saturating_add(1);
        state.captured_payload_bytes += payload.len();
        state.captured_payloads.push_back(CapturedPayload {
            placeholder: placeholder.clone(),
            original: payload.to_owned(),
        });

        let mut rewritten = content.to_owned();
        rewritten.replace_range(payload_start..payload_end, &placeholder);
        ScriptRewriteAction::replace(rewritten)
    }

    fn rewrite_claimed_fragment(
        &self,
        content: &str,
        is_last: bool,
        state: &mut super::rsc_stream::NextJsDocumentState,
        limit: usize,
        max_queued_payload_bytes: usize,
    ) -> ScriptRewriteAction {
        match capture_fragment(&mut state.rsc_script, content, is_last, limit) {
            FragmentCapture::CompleteBorrowed(complete) => {
                self.rewrite_complete(complete, false, state, limit, max_queued_payload_bytes)
            }
            FragmentCapture::CompleteOwned(complete) => {
                self.rewrite_complete(&complete, true, state, limit, max_queued_payload_bytes)
            }
            FragmentCapture::Suppress => ScriptRewriteAction::RemoveNode,
            FragmentCapture::Restore(restored) => {
                state.rsc_receiver_trimmed = false;
                state.bypass_rsc = true;
                ScriptRewriteAction::replace(restored)
            }
            FragmentCapture::PassThrough => {
                if is_last && content.len() > limit && content.contains("__next_f") {
                    let unsafe_continuation = find_rsc_push_payload_range(content)
                        .map(|(start, end)| {
                            matches!(
                                classify_rsc_group(&[&content[start..end]], limit),
                                RscGroupStatus::NeedMore | RscGroupStatus::Invalid
                            )
                        })
                        .unwrap_or(true);
                    state.bypass_rsc |= unsafe_continuation
                        || state.captured_payload_bytes > 0
                        || !state.captured_payloads.is_empty();
                } else if !is_last && content.len() > limit {
                    state.bypass_rsc = true;
                }
                ScriptRewriteAction::Keep
            }
        }
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

        let state = document_state(ctx.document_state);
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.bypass_rsc {
            // The downstream processor can enter bypass after the parser has
            // suppressed part of this script. Restore it before passing through
            // the next fragment so an output limit cannot truncate JavaScript.
            let mut restored = match std::mem::take(&mut state.rsc_script) {
                super::rsc_stream::FragmentState::Buffering(buffer) => buffer,
                _ => String::new(),
            };
            restored.push_str(&std::mem::take(&mut state.rsc_probe));
            state.rsc_receiver_context.clear();
            state.rsc_receiver_trimmed = false;
            if !restored.is_empty() {
                restored.push_str(content);
                return ScriptRewriteAction::replace(restored);
            }
            return ScriptRewriteAction::keep();
        }
        let limit = if self.config.max_combined_payload_bytes == 0 {
            DEFAULT_MAX_COMBINED_PAYLOAD_BYTES
        } else {
            self.config.max_combined_payload_bytes
        };
        if !matches!(state.rsc_script, super::rsc_stream::FragmentState::Idle) {
            return self.rewrite_claimed_fragment(
                content,
                ctx.is_last_in_text_node,
                &mut state,
                limit,
                ctx.max_buffered_script_bytes,
            );
        }

        if state.rsc_probe.is_empty() && !content.contains("__next_f") {
            if ctx.is_last_in_text_node {
                state.rsc_receiver_context.clear();
                return ScriptRewriteAction::Keep;
            }
            let probe_length = longest_identifier_prefix(content.as_bytes());
            let ready_length = content.len() - probe_length;
            state.rsc_probe.push_str(&content[ready_length..]);
            remember_released(&mut state.rsc_receiver_context, &content[..ready_length]);
            return if probe_length == 0 {
                ScriptRewriteAction::Keep
            } else if ready_length == 0 {
                ScriptRewriteAction::RemoveNode
            } else {
                ScriptRewriteAction::replace(&content[..ready_length])
            };
        }

        let prior_probe = std::mem::take(&mut state.rsc_probe);
        let mut combined = prior_probe.clone();
        combined.push_str(content);
        if !combined.contains("__next_f") {
            if ctx.is_last_in_text_node {
                state.rsc_receiver_context.clear();
                return if prior_probe.is_empty() {
                    ScriptRewriteAction::Keep
                } else {
                    ScriptRewriteAction::replace(combined)
                };
            }
            let probe_length = longest_identifier_prefix(combined.as_bytes());
            let ready_length = combined.len() - probe_length;
            state.rsc_probe.push_str(&combined[ready_length..]);
            remember_released(&mut state.rsc_receiver_context, &combined[..ready_length]);
            if prior_probe.is_empty() && probe_length == 0 {
                return ScriptRewriteAction::Keep;
            }
            return if ready_length == 0 {
                ScriptRewriteAction::RemoveNode
            } else {
                ScriptRewriteAction::replace(&combined[..ready_length])
            };
        }

        let identifier_start = combined
            .find("__next_f")
            .expect("should find the identifier that selected this branch");
        let mut context = state.rsc_receiver_context.clone();
        context.push_str(&combined[..identifier_start]);
        if !receiver_context_is_flight_push(&context) {
            // Some other object owns a `__next_f` property. Release the text
            // unchanged rather than claiming an unrelated publisher script.
            if ctx.is_last_in_text_node {
                state.rsc_receiver_context.clear();
            } else {
                remember_released(&mut state.rsc_receiver_context, &combined);
            }
            return if prior_probe.is_empty() {
                ScriptRewriteAction::Keep
            } else {
                ScriptRewriteAction::replace(combined)
            };
        }

        // A receiver that survives inside `combined` keeps the claim qualified;
        // one that already streamed leaves a trimmed claim whose receiver this
        // verified context stands in for.
        state.rsc_receiver_trimmed =
            !receiver_context_is_flight_push(&combined[..identifier_start]);
        state.rsc_receiver_context.clear();
        let claimed_start = if state.rsc_receiver_trimmed {
            identifier_start
        } else {
            0
        };
        let prefix = &combined[..claimed_start];
        let claimed = &combined[claimed_start..];
        let action = self.rewrite_claimed_fragment(
            claimed,
            ctx.is_last_in_text_node,
            &mut state,
            limit,
            ctx.max_buffered_script_bytes,
        );
        match action {
            ScriptRewriteAction::RemoveNode if prefix.is_empty() => ScriptRewriteAction::RemoveNode,
            ScriptRewriteAction::RemoveNode => ScriptRewriteAction::replace(prefix),
            ScriptRewriteAction::Replace(rewritten) => {
                ScriptRewriteAction::replace(format!("{prefix}{rewritten}"))
            }
            ScriptRewriteAction::Keep if prior_probe.is_empty() => ScriptRewriteAction::Keep,
            ScriptRewriteAction::Keep => ScriptRewriteAction::replace(combined),
        }
    }
}

/// Bytes to withhold so a `__next_f` identifier split across text fragments can
/// still be recognized once the next fragment arrives.
fn longest_identifier_prefix(bytes: &[u8]) -> usize {
    let identifier = b"__next_f";
    let maximum = bytes.len().min(identifier.len().saturating_sub(1));
    (1..=maximum)
        .rev()
        .find(|length| bytes.ends_with(&identifier[..*length]))
        .unwrap_or(0)
}

/// Retain the tail of released script text so a receiver that streams before its
/// `__next_f` identifier is recognized can still be verified.
fn remember_released(context: &mut String, released: &str) {
    context.push_str(released);
    if context.len() > RSC_RECEIVER_CONTEXT_BYTES {
        let start = context.len() - RSC_RECEIVER_CONTEXT_BYTES;
        // Character boundaries only matter for the ASCII receiver spellings, so a
        // split multi-byte character can be dropped along with the excess.
        let start = (start..context.len())
            .find(|index| context.is_char_boundary(*index))
            .unwrap_or(context.len());
        context.drain(..start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationDocumentState;
    use crate::integrations::nextjs::rsc_stream::NextJsDocumentState;

    fn ctx(
        is_last_in_text_node: bool,
        document_state: &IntegrationDocumentState,
    ) -> IntegrationScriptContext<'_> {
        IntegrationScriptContext {
            selector: "script",
            request_host: "proxy.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node,
            max_buffered_script_bytes: 16 * 1024 * 1024,
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
            .get::<std::sync::Mutex<NextJsDocumentState>>(NEXTJS_INTEGRATION_ID)
            .expect("should store RSC state");
        let guard = stored.lock().expect("should lock Next.js RSC state");
        assert_eq!(
            guard.captured_payloads.len(),
            1,
            "Should store exactly one payload"
        );
        assert_eq!(
            guard
                .captured_payloads
                .front()
                .expect("should contain captured payload")
                .original,
            "https://origin.example.com/page",
            "Stored payload should match original"
        );
    }

    #[test]
    fn captures_fragmented_scripts_as_one_namespaced_placeholder() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());

        let first = "self.__next_f.push([1,\"https://origin.example.com";
        let second = "/page\"])";

        let action_first = rewriter.rewrite(first, &ctx(false, &state));
        assert_eq!(
            action_first,
            ScriptRewriteAction::RemoveNode,
            "should suppress a bounded intermediate fragment"
        );

        let action_second = rewriter.rewrite(second, &ctx(true, &state));
        assert!(
            matches!(action_second, ScriptRewriteAction::Replace(ref value) if value.contains("__ts_rsc_")),
            "should emit one request-namespaced placeholder",
        );
    }

    #[test]
    fn captures_initializer_push_at_every_fragment_boundary() {
        let script = r#"(self.__next_f=self.__next_f||[]).push([1,"1:T3,ab"])"#;
        for split in 1..script.len() {
            let state = IntegrationDocumentState::default();
            let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
            let _ = rewriter.rewrite(&script[..split], &ctx(false, &state));
            let _ = rewriter.rewrite(&script[split..], &ctx(true, &state));
            let shared = document_state(&state);
            let guard = shared.lock().expect("should lock document state");
            assert_eq!(
                guard.captured_payloads.len(),
                1,
                "should capture initializer split at byte {split}"
            );
            assert_eq!(
                guard.captured_payloads[0].original, "1:T3,ab",
                "should capture complete payload"
            );
        }
    }

    #[test]
    fn overflowing_fragmented_rsc_restores_prefix_and_bypasses_later_rsc() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(Arc::new(NextJsIntegrationConfig {
            max_combined_payload_bytes: 24,
            ..(*test_config()).clone()
        }));

        assert_eq!(
            rewriter.rewrite("self.__next_f", &ctx(false, &state)),
            ScriptRewriteAction::RemoveNode,
            "should initially suppress the script prefix",
        );
        assert_eq!(
            rewriter.rewrite("-payload-overflow", &ctx(false, &state)),
            ScriptRewriteAction::Replace("self.__next_f-payload-overflow".to_owned()),
            "should restore suppressed text before overflow",
        );
        assert_eq!(
            rewriter.rewrite("tail", &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should pass through until the text node ends",
        );

        let later = r#"self.__next_f.push([1,"later"])"#;
        assert_eq!(
            rewriter.rewrite(later, &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should keep later RSC scripts unchanged after unsafe overflow",
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

    #[test]
    fn oversized_partial_header_bypasses_later_continuation() {
        for suffix in ["1", "1:", "1:T", "1:T2"] {
            let state = IntegrationDocumentState::default();
            let rewriter = NextJsRscPlaceholderRewriter::new(Arc::new(NextJsIntegrationConfig {
                max_combined_payload_bytes: 100,
                ..(*test_config()).clone()
            }));
            let script = format!("self.__next_f.push([1,\"{}{suffix}\"])", "x".repeat(100),);
            assert_eq!(
                rewriter.rewrite(&script, &ctx(true, &state)),
                ScriptRewriteAction::Keep,
                "should pass through an oversized script",
            );
            let continuation = r#"self.__next_f.push([1,"a,https://origin.example.com/path"] )"#;
            assert_eq!(
                rewriter.rewrite(continuation, &ctx(true, &state)),
                ScriptRewriteAction::Keep,
                "should preserve later payloads after oversized header prefix {suffix}",
            );
        }
    }

    #[test]
    fn oversized_continuation_bypasses_an_unresolved_group() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(Arc::new(NextJsIntegrationConfig {
            max_combined_payload_bytes: 100,
            ..(*test_config()).clone()
        }));
        let first = r#"self.__next_f.push([1,"1:T200,start"])"#;
        assert!(
            matches!(
                rewriter.rewrite(first, &ctx(true, &state)),
                ScriptRewriteAction::Replace(_)
            ),
            "should capture incomplete group"
        );
        let oversized = format!("self.__next_f.push([1,\"{}\"])", "x".repeat(101));
        assert_eq!(
            rewriter.rewrite(&oversized, &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should pass through oversized continuation"
        );
        let later = r#"self.__next_f.push([1,"https://origin.example.com/path/"])"#;
        assert_eq!(
            rewriter.rewrite(later, &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should preserve later continuation of bypassed group"
        );
    }

    /// Every fragmentation of a genuine qualified push must still be captured.
    /// The receiver can be split from its identifier at any byte, and the
    /// verified receiver context is what lets the claim proceed.
    #[test]
    fn captures_qualified_push_at_every_fragment_boundary() {
        for script in [
            r#"self.__next_f.push([1,"1:T3,ab"])"#,
            r#"window.__next_f.push([1,"1:T3,ab"])"#,
            r#";self.__next_f.push([1,"1:T3,ab"])"#,
        ] {
            for split in 1..script.len() {
                let state = IntegrationDocumentState::default();
                let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
                let _ = rewriter.rewrite(&script[..split], &ctx(false, &state));
                let _ = rewriter.rewrite(&script[split..], &ctx(true, &state));

                let shared = document_state(&state);
                let guard = shared.lock().expect("should lock document state");
                assert_eq!(
                    guard.captured_payloads.len(),
                    1,
                    "should capture `{script}` split at byte {split}"
                );
                assert_eq!(
                    guard.captured_payloads[0].original, "1:T3,ab",
                    "should capture the complete payload of `{script}` split at byte {split}"
                );
            }
        }
    }

    /// No fragmentation may turn an unrelated publisher script into Flight data.
    /// The receiver context has to reject these at every split, including the
    /// splits that leave a bare `__next_f` at the start of the claim.
    #[test]
    fn never_captures_foreign_receiver_at_any_fragment_boundary() {
        for script in [
            r#"myself.__next_f.push([1,"1:T3,ab"])"#,
            r#"myAnalytics.__next_f.push([1,"1:T3,ab"])"#,
            r#"foo.bar.__next_f.push([1,"1:T3,ab"])"#,
            r#"window.myapp.__next_f.push([1,"1:T3,ab"])"#,
            r#"a__next_f.push([1,"1:T3,ab"])"#,
            // Nothing precedes the identifier, so the receiver context is empty
            // rather than disqualifying: only the qualified-receiver rule rejects it.
            r#"__next_f.push([1,"1:T3,ab"])"#,
            r#"(myself.__next_f=self.__next_f||[]).push([1,"1:T3,ab"])"#,
        ] {
            for split in 1..script.len() {
                let state = IntegrationDocumentState::default();
                let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
                let first = rewriter.rewrite(&script[..split], &ctx(false, &state));
                let second = rewriter.rewrite(&script[split..], &ctx(true, &state));

                let shared = document_state(&state);
                let guard = shared.lock().expect("should lock document state");
                assert!(
                    guard.captured_payloads.is_empty(),
                    "should not claim `{script}` split at byte {split}"
                );
                drop(guard);

                // The bytes must also survive unchanged across both fragments.
                let mut emitted = String::new();
                for (action, source) in [(first, &script[..split]), (second, &script[split..])] {
                    match action {
                        ScriptRewriteAction::Keep => emitted.push_str(source),
                        ScriptRewriteAction::Replace(value) => emitted.push_str(&value),
                        ScriptRewriteAction::RemoveNode => {}
                    }
                }
                assert_eq!(
                    emitted, script,
                    "should stream `{script}` through unchanged when split at byte {split}"
                );
            }
        }
    }

    /// The captured-payload queue holds parser-held script text, so it must stay
    /// bounded even though the group limit no longer gates it. Payloads that each
    /// fit the group limit still fall back once their total exceeds the parser's
    /// script-buffer budget.
    #[test]
    fn queued_payloads_are_bounded_by_the_script_buffer_budget() {
        // Quote-free so the JS string literal is not terminated early; this test
        // is about the queue budget, not about rewriting.
        let payload = "a".repeat(40);
        let script = format!(r#"self.__next_f.push([1,"{payload}"])"#);
        // Room for one payload, not two.
        let budget = payload.len() + payload.len() / 2;

        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
        let context = IntegrationScriptContext {
            max_buffered_script_bytes: budget,
            ..ctx(true, &state)
        };

        let first = rewriter.rewrite(&script, &context);
        assert!(
            matches!(first, ScriptRewriteAction::Replace(ref value) if value.contains("__ts_rsc_")),
            "the first payload should fit the queue budget"
        );

        let second = rewriter.rewrite(&script, &context);
        assert_eq!(
            second,
            ScriptRewriteAction::Keep,
            "the payload crossing the queue budget should stream through unchanged"
        );

        let shared = document_state(&state);
        let guard = shared.lock().expect("should lock document state");
        assert_eq!(
            guard.captured_payloads.len(),
            1,
            "should not queue a payload past the script-buffer budget"
        );
        assert!(
            guard.bypass_rsc,
            "crossing the queue budget should bypass the rest of the document"
        );
    }
}
