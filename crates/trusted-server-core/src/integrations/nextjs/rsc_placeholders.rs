use std::sync::{Arc, LazyLock};

use regex::Regex;

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
use super::script_lexer::ScriptLexer;
use super::shared::{
    RSC_PUSH_CALL_PATTERN, RSC_PUSH_CALL_PATTERN_TRIMMED, RSC_RECEIVER_CONTEXT_BYTES,
    find_rsc_push_payload_range, find_trimmed_rsc_push_payload_range,
    receiver_context_is_flight_push,
};
use super::{NEXTJS_INTEGRATION_ID, NextJsIntegrationConfig};

// Observe actual qualified push calls, not bare property references. Unlike
// capture, ownership also covers unsupported channels and whitespace spellings
// because raw fallback must retain their original T lengths.
static QUALIFIED_FLIGHT_PUSH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:self|window)\.__next_f\s*(?:\.\s*push\s*\(|=\s*(?:self|window)\.__next_f\s*\|\|\s*\[\]\s*\)\s*\.\s*push\s*\()")
        .expect("should compile qualified Flight push observation")
});

// The receiver may already have streamed, leaving a verified trimmed head.
static TRIMMED_FLIGHT_PUSH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^__next_f\s*(?:\.\s*push\s*\(|=\s*(?:self|window)\.__next_f\s*\|\|\s*\[\]\s*\)\s*\.\s*push\s*\()")
        .expect("should compile trimmed Flight push observation")
});

// Only completed inert bootstrap controls may be skipped. Opaque channel-2
// strings and incomplete calls must retain the conservative raw fallback.
static INERT_FLIGHT_CONTROL_ARGUMENTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\[\s*(?:0|2\s*,\s*null)\s*\]\s*\)")
        .expect("should compile inert Flight control arguments")
});

// Collapsing whitespace runs bounds either supported call head well below this
// size, while keeping the receiver's preceding boundary for qualification.
const FLIGHT_QUALIFIER_TAIL_BYTES: usize = 96;

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
        let code = state.rsc_lexical_start.clone().mask(content);
        if !code.contains("__next_f") {
            return if was_buffered {
                ScriptRewriteAction::replace(content.to_owned())
            } else {
                ScriptRewriteAction::Keep
            };
        }

        // Discover all ranges and validate the queue budget before committing any
        // capture. A malformed or unrecognized remainder stays original/protected.
        let mut ranges = Vec::new();
        let mut cursor = 0;
        let mut unclaimed_raw_flight = false;
        let mut trimmed = std::mem::take(&mut state.rsc_receiver_trimmed);
        let mut queued_bytes = state.captured_payload_bytes;
        loop {
            let remaining = &content[cursor..];
            let remaining_code = &code[cursor..];
            let head = if trimmed {
                TRIMMED_FLIGHT_PUSH_PATTERN.find(remaining_code)
            } else {
                first_qualified_flight_push(remaining_code)
            };
            // Property references are ordinary source, including after a push.
            // Only another qualified call can require capture or raw fallback.
            let Some(head) = head else {
                break;
            };
            if let Some(arguments) = INERT_FLIGHT_CONTROL_ARGUMENTS.find(&remaining[head.end()..]) {
                unclaimed_raw_flight |=
                    contains_qualified_flight(&content[cursor..cursor + head.start()]);
                cursor += head.end() + arguments.end();
                trimmed = false;
                continue;
            }
            let range = executable_payload_range(remaining, remaining_code, trimmed);
            let call = if trimmed {
                RSC_PUSH_CALL_PATTERN_TRIMMED.find(remaining_code)
            } else {
                RSC_PUSH_CALL_PATTERN.find(remaining_code)
            };
            trimmed = false;
            let Some((start, end)) = range.filter(|(start, _)| {
                call.is_some_and(|call| {
                    call.end() == *start
                        && !contains_qualified_flight(&remaining_code[..call.start()])
                })
            }) else {
                // A raw qualified push cannot safely continue a pending T group
                // or be skipped by later captures. A bare property reference is
                // ordinary script text and must not disable later Flight capture.
                state.bypass_rsc |= state.flight_node_owned;
                return if was_buffered {
                    ScriptRewriteAction::replace(content.to_owned())
                } else {
                    ScriptRewriteAction::Keep
                };
            };
            let start = cursor + start;
            let end = cursor + end;
            let length = end.saturating_sub(start);
            // Group bytes and queued parser-held bytes have distinct limits:
            // independent groups parsed in one call must not consume each other's
            // group allowance before the output processor can release them.
            let total = queued_bytes.checked_add(length);
            if start > end
                || end >= content.len()
                || !content.is_char_boundary(start)
                || !content.is_char_boundary(end)
                || length > limit
                || state.captured_payloads.len() + ranges.len() >= MAX_UNRESOLVED_RSC_PAYLOADS
                || total.is_none_or(|bytes| bytes > max_queued_payload_bytes)
            {
                state.bypass_rsc = true;
                return if was_buffered {
                    ScriptRewriteAction::replace(content.to_owned())
                } else {
                    ScriptRewriteAction::Keep
                };
            }
            queued_bytes = total.expect("should have validated queue size");
            unclaimed_raw_flight |=
                contains_qualified_flight(&content[cursor..cursor + head.start()]);
            ranges.push((start, end));
            cursor = end + 1;
        }
        if ranges.is_empty() {
            return if was_buffered {
                ScriptRewriteAction::replace(content.to_owned())
            } else {
                ScriptRewriteAction::Keep
            };
        }
        let raw_flight_remaining = state.raw_flight_unclaimed
            || unclaimed_raw_flight
            || contains_qualified_flight(&content[cursor..]);
        let mut rewritten = String::with_capacity(content.len());
        cursor = 0;
        for (start, end) in ranges {
            let placeholder =
                rsc_payload_placeholder(&state.namespace, state.next_placeholder_index);
            state.next_placeholder_index = state.next_placeholder_index.saturating_add(1);
            state.captured_payloads.push_back(CapturedPayload {
                placeholder: placeholder.clone(),
                original: content[start..end].to_owned(),
            });
            rewritten.push_str(&content[cursor..start]);
            rewritten.push_str(&placeholder);
            cursor = end;
        }
        state.captured_payload_bytes = queued_bytes;
        rewritten.push_str(&content[cursor..]);
        // Captured payloads and recognized bootstrap controls are safe, but
        // raw push spellings outside them may be executable lexer misses.
        // Preserve those bytes even when another call was captured successfully.
        state.current_fragment_protected = raw_flight_remaining;
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
            FragmentCapture::Suppress => {
                // Captured bytes are withheld, so ordinary preceding source may
                // still receive GTM rewriting. An earlier unclaimed raw head can
                // leave its continuation in a released prefix, however, and that
                // source must stay protected even during this later capture.
                state.current_fragment_protected = state.raw_flight_unclaimed;
                ScriptRewriteAction::RemoveNode
            }
            FragmentCapture::Restore(restored) => {
                state.rsc_receiver_trimmed = false;
                state.bypass_rsc |= state.flight_node_owned;
                ScriptRewriteAction::replace(restored)
            }
            FragmentCapture::PassThrough => {
                let trimmed = state.rsc_receiver_trimmed;
                state.rsc_receiver_trimmed = false;
                if !state.flight_node_owned {
                    return ScriptRewriteAction::Keep;
                }
                if is_last && content.len() > limit && content.contains("__next_f") {
                    let code = state.rsc_lexical_start.clone().mask(content);
                    let mut remaining = content;
                    let mut remaining_code = code.as_str();
                    let mut receiver_trimmed = trimmed;
                    let unsafe_continuation = loop {
                        let range =
                            executable_payload_range(remaining, remaining_code, receiver_trimmed);
                        let Some((start, end)) = range else {
                            break true;
                        };
                        if matches!(
                            classify_rsc_group(&[&remaining[start..end]], limit),
                            RscGroupStatus::NeedMore | RscGroupStatus::Invalid
                        ) {
                            break true;
                        }
                        // Skip the entire string so push-like text inside a
                        // payload is not classified as another call. Only the
                        // first receiver can have streamed in an earlier fragment.
                        remaining = &remaining[end + 1..];
                        remaining_code = &remaining_code[end + 1..];
                        receiver_trimmed = false;
                        if !contains_qualified_flight(remaining_code) {
                            break false;
                        }
                    };
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
    fn rewrite_fragment(
        &self,
        content: &str,
        code: &str,
        lexical_start: ScriptLexer,
        ctx: &IntegrationScriptContext<'_>,
        state: &mut super::rsc_stream::NextJsDocumentState,
    ) -> ScriptRewriteAction {
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
                state,
                limit,
                ctx.max_buffered_script_bytes,
            );
        }

        if state.rsc_probe.is_empty() && !code.contains("__next_f") {
            if ctx.is_last_in_text_node {
                state.rsc_receiver_context.clear();
                return ScriptRewriteAction::Keep;
            }
            let probe_length = longest_identifier_prefix(code.as_bytes());
            let ready_length = content.len() - probe_length;
            state.rsc_probe.push_str(&content[ready_length..]);
            remember_released(&mut state.rsc_receiver_context, &code[..ready_length]);
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
        let combined_code = format!("{prior_probe}{code}");
        if !combined_code.contains("__next_f") {
            if ctx.is_last_in_text_node {
                state.rsc_receiver_context.clear();
                return if prior_probe.is_empty() {
                    ScriptRewriteAction::Keep
                } else {
                    ScriptRewriteAction::replace(combined)
                };
            }
            let probe_length = longest_identifier_prefix(combined_code.as_bytes());
            let ready_length = combined.len() - probe_length;
            state.rsc_probe.push_str(&combined[ready_length..]);
            remember_released(
                &mut state.rsc_receiver_context,
                &combined_code[..ready_length],
            );
            if prior_probe.is_empty() && probe_length == 0 {
                return ScriptRewriteAction::Keep;
            }
            return if ready_length == 0 {
                ScriptRewriteAction::RemoveNode
            } else {
                ScriptRewriteAction::replace(&combined[..ready_length])
            };
        }

        let identifier_start = combined_code
            .find("__next_f")
            .expect("should find the identifier that selected this branch");
        let mut context = state.rsc_receiver_context.clone();
        context.push_str(&combined_code[..identifier_start]);
        if !receiver_context_is_flight_push(&context) {
            // Some other object owns a `__next_f` property. Release the text
            // unchanged rather than claiming an unrelated publisher script.
            if ctx.is_last_in_text_node {
                state.rsc_receiver_context.clear();
            } else {
                remember_released(&mut state.rsc_receiver_context, &combined_code);
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
            !receiver_context_is_flight_push(&combined_code[..identifier_start]);
        state.rsc_receiver_context.clear();
        let claimed_start = if state.rsc_receiver_trimmed {
            identifier_start
        } else {
            0
        };
        let prefix = &combined[..claimed_start];
        let claimed = &combined[claimed_start..];
        state.rsc_lexical_start = lexical_start;
        state.rsc_lexical_start.mask(&combined[..claimed_start]);
        let action = self.rewrite_claimed_fragment(
            claimed,
            ctx.is_last_in_text_node,
            state,
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

impl IntegrationScriptRewriter for NextJsRscPlaceholderRewriter {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn selector(&self) -> &'static str {
        "script"
    }

    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction {
        let shared = document_state(ctx.document_state);
        let mut guard = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = &mut *guard;
        if std::mem::take(&mut state.next_data_fragment) {
            // Pages data is not executable Flight, even when a JSON string
            // contains a complete push spelling. Clear the downstream snapshot
            // as well so a prior protected script cannot suppress Pages GTM.
            state.current_fragment_protected = false;
            state.flight_node_owned = false;
            state.flight_qualifier_tail.clear();
            state.raw_flight_seen = false;
            state.raw_flight_unclaimed = false;
            state.raw_qualifier_tail.clear();
            state.flight_lexical = ScriptLexer::default();
            return ScriptRewriteAction::Keep;
        }
        if !self.config.enabled || self.config.rewrite_attributes.is_empty() {
            state.current_fragment_protected = false;
            return ScriptRewriteAction::Keep;
        }
        // A bounded raw observer is a safety guard, not a capture parser. Inert
        // spellings may conservatively miss GTM rewriting in this script, but a
        // lexical false negative must never let GTM corrupt raw Flight lengths.
        let raw_head_end = observe_flight_qualifier(
            content,
            &mut state.raw_qualifier_tail,
            &mut state.raw_flight_seen,
        );
        let previously_owned = state.flight_node_owned;
        let lexical_start = state.flight_lexical.clone();
        let code = state.flight_lexical.mask(content);
        let code_head_end = if state.flight_lexical.is_opaque() {
            // At the lexical nesting limit, retain the existing conservative raw
            // qualification. Ordinary scripts without a push do not bypass Flight.
            let head_end = observe_flight_qualifier(
                content,
                &mut state.flight_qualifier_tail,
                &mut state.flight_node_owned,
            );
            state.bypass_rsc |= state.flight_node_owned;
            head_end
        } else {
            observe_flight_qualifier(
                &code,
                &mut state.flight_qualifier_tail,
                &mut state.flight_node_owned,
            )
        };
        // An unclaimed raw head may precede this fragment's capture buffer.
        // Compare first-head offsets: a later lexical claim in the same fragment
        // does not prove an earlier raw head safe, nor does a later fragment.
        state.raw_flight_unclaimed |= !previously_owned
            && raw_head_end
                .is_some_and(|raw_end| code_head_end.is_none_or(|code_end| raw_end < code_end));
        // A call head can complete after a tentative property probe overflowed.
        // Once it proves Flight, preserve its entire raw continuation/group.
        if state.flight_node_owned
            && matches!(
                state.rsc_script,
                super::rsc_stream::FragmentState::BypassUntilLast
            )
        {
            state.bypass_rsc = true;
        }
        state.current_fragment_protected = state.flight_node_owned || state.raw_flight_seen;
        let action = self.rewrite_fragment(content, &code, lexical_start, ctx, state);
        // Ownership alone does not prove buffering: an earlier unrelated
        // property can cause the fragment to stream without a capture claim.
        // Never let a later capture unprotect that raw source's continuation.
        if !ctx.is_last_in_text_node
            && state.raw_flight_seen
            && !matches!(
                state.rsc_script,
                super::rsc_stream::FragmentState::Buffering(_)
            )
        {
            state.raw_flight_unclaimed = true;
        }
        if ctx.is_last_in_text_node {
            state.flight_node_owned = false;
            state.flight_qualifier_tail.clear();
            state.raw_flight_seen = false;
            state.raw_flight_unclaimed = false;
            state.raw_qualifier_tail.clear();
            state.rsc_probe.clear();
            state.rsc_receiver_context.clear();
            state.rsc_receiver_trimmed = false;
            state.flight_lexical = ScriptLexer::default();
            state.rsc_lexical_start = ScriptLexer::default();
        }
        action
    }
}

/// Match a call head only in executable source, then read its original string.
fn executable_payload_range(content: &str, code: &str, trimmed: bool) -> Option<(usize, usize)> {
    if trimmed {
        RSC_PUSH_CALL_PATTERN_TRIMMED.find(code)?;
        return find_trimmed_rsc_push_payload_range(content);
    }
    let call = RSC_PUSH_CALL_PATTERN.find(code)?;
    let identifier = call.start() + call.as_str().find("__next_f")?;
    if !receiver_context_is_flight_push(&code[..identifier]) {
        return None;
    }
    let (start, end) = find_rsc_push_payload_range(&content[call.start()..])?;
    Some((call.start() + start, call.start() + end))
}

fn contains_qualified_flight(source: &str) -> bool {
    first_qualified_flight_push(source).is_some()
}

fn first_qualified_flight_push(source: &str) -> Option<regex::Match<'_>> {
    QUALIFIED_FLIGHT_PUSH_PATTERN
        .find_iter(source)
        .find(|call| {
            let identifier = call.start()
                + call
                    .as_str()
                    .find("__next_f")
                    .expect("should find identifier in qualified push pattern");
            receiver_context_is_flight_push(&source[..identifier])
        })
}

/// Observe a bounded whitespace-normalized call head without buffering script
/// source, including initializer heads split after document bypass. Return its
/// original fragment byte-end so raw and masked observations can be compared.
fn observe_flight_qualifier(content: &str, tail: &mut String, seen: &mut bool) -> Option<usize> {
    if *seen {
        return None;
    }
    for (offset, character) in content.char_indices() {
        let character = if character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' && tail.ends_with(' ') {
            continue;
        }
        tail.push(character);
        if tail.len() > FLIGHT_QUALIFIER_TAIL_BYTES {
            let start = tail.len() - FLIGHT_QUALIFIER_TAIL_BYTES;
            let start = (start..tail.len())
                .find(|index| tail.is_char_boundary(*index))
                .expect("should find bounded qualifier character boundary");
            tail.drain(..start);
        }
        if character == '(' && contains_qualified_flight(tail) {
            *seen = true;
            tail.clear();
            return Some(offset + 1);
        }
    }
    None
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
    fn quoted_flight_spellings_do_not_claim_ownership() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
        rewriter.rewrite(r#"const demo="self.__next_f.push(";"#, &ctx(false, &state));
        let shared = document_state(&state);
        let state = shared.lock().expect("should lock state");
        assert!(!state.flight_node_owned, "quoted text must not own Flight");
        assert!(
            state.raw_flight_seen,
            "raw spelling should trigger only protection"
        );
        assert!(
            state.current_fragment_protected,
            "should protect possible raw Flight"
        );
        assert!(
            !state.bypass_rsc,
            "inert source must not bypass later scripts"
        );
    }

    #[test]
    fn raw_qualifier_observation_is_bounded_and_survives_every_split() {
        for head in [
            "self.__next_f . push (",
            "window.__next_f . push (",
            "(self.__next_f = self.__next_f || []).push (",
            "(window.__next_f = window.__next_f || []).push (",
        ] {
            let head = head.replace(' ', &" \n".repeat(64));
            for split in 0..=head.len() {
                let mut tail = String::new();
                let mut seen = false;
                let _ = observe_flight_qualifier(&"é😀".repeat(1000), &mut tail, &mut seen);
                assert!(
                    tail.len() <= FLIGHT_QUALIFIER_TAIL_BYTES,
                    "should bound raw tail"
                );
                assert!(!seen, "ordinary source must not protect Flight");
                let _ = observe_flight_qualifier(&head[..split], &mut tail, &mut seen);
                let _ = observe_flight_qualifier(&head[split..], &mut tail, &mut seen);
                assert!(
                    seen,
                    "should recognize normalized raw head at split {split}"
                );
                assert!(
                    tail.is_empty(),
                    "should release tail once qualification succeeds"
                );
            }
        }
    }

    #[test]
    fn unrecognized_qualified_prefix_makes_the_whole_batch_raw_and_protected() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
        let script = r#"self.__next_f.push([2,"1:T40,'http://www.googletagmanager.com/gtm.js'"]);self.__next_f.push([1,"later"])"#;
        assert_eq!(
            rewriter.rewrite(script, &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should not partially hide a batch with uncaptured qualified payloads"
        );
        let shared = document_state(&state);
        let state = shared.lock().expect("should lock state");
        assert!(state.captured_payloads.is_empty());
        assert!(state.current_fragment_protected);
    }

    #[test]
    fn inert_controls_do_not_make_unsafe_batches_partially_capturable() {
        for unsafe_call in [
            r#"self.__next_f.push([2,"1:T40,'http://www.googletagmanager.com/gtm.js'"])"#,
            "self.__next_f.push([0,1])",
            "self.__next_f.push([2,null",
            "self.__next_f.push([0]",
        ] {
            for unsafe_first in [false, true] {
                let state = IntegrationDocumentState::default();
                let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
                let control = "self.__next_f.push([0]);self.__next_f.push([2,null])";
                let supported = r#"self.__next_f.push([1,"later"])"#;
                let script = if unsafe_first {
                    format!("{control};{unsafe_call};{supported}")
                } else {
                    format!("{control};{supported};{unsafe_call}")
                };
                assert_eq!(
                    rewriter.rewrite(&script, &ctx(true, &state)),
                    ScriptRewriteAction::Keep,
                    "should preserve unsafe batch: {script}"
                );
                let shared = document_state(&state);
                let state = shared.lock().expect("should lock state");
                assert!(state.captured_payloads.is_empty());
                assert!(state.current_fragment_protected);
                assert!(state.bypass_rsc);
            }
        }
    }

    #[test]
    fn inert_controls_are_preserved_without_document_bypass() {
        for script in [
            "(self.__next_f=self.__next_f||[]).push([0]);self.__next_f.push([2,null])",
            "window.__next_f . push ( [ 0 ] );window.__next_f.push( [ 2 , null ] )",
        ] {
            let state = IntegrationDocumentState::default();
            let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
            assert_eq!(
                rewriter.rewrite(script, &ctx(true, &state)),
                ScriptRewriteAction::Keep
            );
            let shared = document_state(&state);
            let state = shared.lock().expect("should lock state");
            assert!(state.captured_payloads.is_empty());
            assert!(state.current_fragment_protected);
            assert!(!state.bypass_rsc);
        }
    }

    #[test]
    fn batched_capture_is_atomic_at_byte_and_count_limits() {
        let payload = "x".repeat(40);
        let push = format!(r#"self.__next_f.push([1,"{payload}"])"#);
        for (count, budget, captured) in
            [(2, 80, 2), (2, 79, 0), (256, 20000, 256), (257, 20000, 0)]
        {
            let state = IntegrationDocumentState::default();
            let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
            let script = vec![push.as_str(); count].join(";");
            let action = rewriter.rewrite(
                &script,
                &IntegrationScriptContext {
                    max_buffered_script_bytes: budget,
                    ..ctx(true, &state)
                },
            );
            let shared = document_state(&state);
            let state = shared.lock().expect("should lock state");
            assert_eq!(
                state.captured_payloads.len(),
                captured,
                "should commit every push or none"
            );
            assert_eq!(state.captured_payload_bytes, captured * payload.len());
            assert_eq!(
                matches!(action, ScriptRewriteAction::Replace(_)),
                captured > 0
            );
            assert_eq!(
                state.current_fragment_protected,
                captured == 0,
                "should protect rejected raw batch"
            );
        }
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
        let script = format!("{push};self.__next_f.push([1,\"unfinished");
        assert_eq!(
            rewriter.rewrite(&script, &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should leave unsafe remainder original"
        );
        assert!(
            document_state(&state)
                .lock()
                .expect("should lock state")
                .captured_payloads
                .is_empty()
        );
    }

    #[test]
    fn bypass_ownership_recognizes_every_split_and_resets_on_empty_final() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({"enabled": true, "container_id": "GTM-MIX1"}),
            )
            .expect("should enable GTM");
        let registry = crate::integrations::IntegrationRegistry::with_plan(
            &settings,
            Arc::new(crate::auction::compile_auction_plan(&settings).expect("should compile plan")),
        )
        .expect("should create registry");
        let gtm = registry
            .script_rewriters()
            .pop()
            .expect("should register GTM");
        let rewriter = NextJsRscPlaceholderRewriter::new(test_config());
        let spaced_initializer = format!(
            r#"(self.__next_f{}={}window.__next_f{}||{}[]{}).{}push{}([1,"1:T40,'http://www.googletagmanager.com/gtm.js'"])"#,
            " ".repeat(256),
            " ".repeat(256),
            " ".repeat(256),
            " ".repeat(256),
            " ".repeat(256),
            " ".repeat(256),
            " ".repeat(256)
        );
        for script in [
            r#"self.__next_f.push([1,"1:T40,'http://www.googletagmanager.com/gtm.js'"])"#,
            r#"window.__next_f.push([1,"1:T40,'http://www.googletagmanager.com/gtm.js'"])"#,
            r#"(self.__next_f=self.__next_f||[]).push([1,"1:T40,'http://www.googletagmanager.com/gtm.js'"])"#,
            r#"(window.__next_f=window.__next_f||[]).push([1,"1:T40,'http://www.googletagmanager.com/gtm.js'"])"#,
            &spaced_initializer,
        ] {
            for split in 1..script.len() {
                let state = IntegrationDocumentState::default();
                document_state(&state)
                    .lock()
                    .expect("should lock state")
                    .bypass_rsc = true;
                for fragment in [&script[..split], &script[split..], ""] {
                    let final_fragment = fragment.is_empty();
                    assert_eq!(
                        rewriter.rewrite(fragment, &ctx(final_fragment, &state)),
                        ScriptRewriteAction::Keep
                    );
                    assert_eq!(
                        IntegrationScriptRewriter::rewrite(
                            &*gtm,
                            fragment,
                            &ctx(final_fragment, &state)
                        ),
                        ScriptRewriteAction::Keep,
                        "should preserve raw Flight at split {split}"
                    );
                    assert!(
                        document_state(&state)
                            .lock()
                            .expect("should lock state")
                            .flight_qualifier_tail
                            .len()
                            <= FLIGHT_QUALIFIER_TAIL_BYTES,
                        "should bound fallback qualification observation"
                    );
                }
                let ordinary = "var ordinary='http://www.googletagmanager.com/gtm.js';";
                assert_eq!(
                    rewriter.rewrite(ordinary, &ctx(true, &state)),
                    ScriptRewriteAction::Keep
                );
                assert!(
                    matches!(IntegrationScriptRewriter::rewrite(&*gtm, ordinary, &ctx(true, &state)), ScriptRewriteAction::Replace(ref text) if text.contains("/integrations/google_tag_manager/gtm.js"))
                );
            }
        }
    }

    #[test]
    fn raw_flight_fallback_is_protected_but_later_ordinary_gtm_is_not() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(Arc::new(NextJsIntegrationConfig {
            max_combined_payload_bytes: 24,
            ..(*test_config()).clone()
        }));
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({"enabled": true, "container_id": "GTM-MIX1"}),
            )
            .expect("should configure GTM");
        let registry = crate::integrations::IntegrationRegistry::with_plan(
            &settings,
            Arc::new(crate::auction::compile_auction_plan(&settings).expect("should compile plan")),
        )
        .expect("should create registry");
        let gtm = registry
            .script_rewriters()
            .pop()
            .expect("should register script hook");
        let raw = r#"self.__next_f.push([1,"1:T2,'http://www.googletagmanager.com/gtm.js?id=GTM-MIX1'"] )"#;
        assert_eq!(
            rewriter.rewrite(raw, &ctx(true, &state)),
            ScriptRewriteAction::Keep
        );
        assert_eq!(
            IntegrationScriptRewriter::rewrite(&*gtm, raw, &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should not change original T bytes on overflow"
        );
        for part in [
            "self.__n",
            "ext_f.push([1,\"",
            "1:T2,'http://www.googletagmanager.com/gtm.js?id=GTM-MIX1'\"])",
        ] {
            let last = part.ends_with(")");
            assert_eq!(
                rewriter.rewrite(part, &ctx(last, &state)),
                ScriptRewriteAction::Keep
            );
            assert_eq!(
                IntegrationScriptRewriter::rewrite(&*gtm, part, &ctx(last, &state)),
                ScriptRewriteAction::Keep,
                "should recognize and protect split qualified flight after bypass"
            );
        }
        let prefix = "var before='google";
        assert_eq!(
            rewriter.rewrite(prefix, &ctx(false, &state)),
            ScriptRewriteAction::Keep
        );
        assert_eq!(
            IntegrationScriptRewriter::rewrite(&*gtm, prefix, &ctx(false, &state)),
            ScriptRewriteAction::RemoveNode
        );
        let flight = r#"';self.__next_f.push([1,"1:T2,'http://www.googletagmanager.com/gtm.js'"])"#;
        assert_eq!(
            rewriter.rewrite(flight, &ctx(false, &state)),
            ScriptRewriteAction::Keep
        );
        assert_eq!(
            IntegrationScriptRewriter::rewrite(&*gtm, flight, &ctx(false, &state)),
            ScriptRewriteAction::Replace(format!("{prefix}{flight}")),
            "should drain GTM-held prefix unchanged before protected raw text"
        );
        assert_eq!(
            rewriter.rewrite("", &ctx(true, &state)),
            ScriptRewriteAction::Keep
        );
        assert_eq!(
            IntegrationScriptRewriter::rewrite(&*gtm, "", &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should preserve empty final protection"
        );
        let ordinary = "var url='http://www.googletagmanager.com/gtm.js';";
        assert_eq!(
            rewriter.rewrite(ordinary, &ctx(true, &state)),
            ScriptRewriteAction::Keep
        );
        assert!(
            matches!(IntegrationScriptRewriter::rewrite(&*gtm, ordinary, &ctx(true, &state)), ScriptRewriteAction::Replace(ref value) if value.contains("/integrations/google_tag_manager/gtm.js")),
            "should not leak flight ownership to ordinary scripts"
        );
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
            rewriter.rewrite("self.__next_f.push(", &ctx(false, &state)),
            ScriptRewriteAction::RemoveNode,
            "should initially suppress the script prefix",
        );
        assert_eq!(
            rewriter.rewrite("-payload-overflow", &ctx(false, &state)),
            ScriptRewriteAction::Replace("self.__next_f.push(-payload-overflow".to_owned()),
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
    fn oversized_batched_pushes_classify_later_payloads() {
        for trimmed in [false, true] {
            for (header, should_bypass) in [("T64", true), ("T50", false), ("T", true)] {
                let state = IntegrationDocumentState::default();
                let rewriter =
                    NextJsRscPlaceholderRewriter::new(Arc::new(NextJsIntegrationConfig {
                        max_combined_payload_bytes: 100,
                        ..(*test_config()).clone()
                    }));
                let receiver = if trimmed {
                    assert_eq!(
                        rewriter.rewrite("self.", &ctx(false, &state)),
                        ScriptRewriteAction::Keep,
                        "should release the qualified receiver"
                    );
                    ""
                } else {
                    "self."
                };
                let script = format!(
                    r#"{receiver}__next_f.push([1,"1:T3,abc"]);self.__next_f.push([1,"2:{header},{}"])"#,
                    "x".repeat(80)
                );

                assert_eq!(
                    rewriter.rewrite(&script, &ctx(true, &state)),
                    ScriptRewriteAction::Keep,
                    "should preserve the oversized batch"
                );
                assert_eq!(
                    document_state(&state)
                        .lock()
                        .expect("should lock document state")
                        .bypass_rsc,
                    should_bypass,
                    "should classify the second payload with header {header}, trimmed={trimmed}"
                );
                let later = r#"self.__next_f.push([1,"1:T3,abc"]);"#;
                let action = rewriter.rewrite(later, &ctx(true, &state));
                assert_eq!(
                    matches!(action, ScriptRewriteAction::Keep),
                    should_bypass,
                    "should bypass later scripts only when the batch is unsafe"
                );
            }
        }
    }

    #[test]
    fn oversized_trimmed_complete_claim_keeps_later_scripts_capturable() {
        let state = IntegrationDocumentState::default();
        let rewriter = NextJsRscPlaceholderRewriter::new(Arc::new(NextJsIntegrationConfig {
            max_combined_payload_bytes: 100,
            ..(*test_config()).clone()
        }));
        assert_eq!(
            rewriter.rewrite("self.", &ctx(false, &state)),
            ScriptRewriteAction::Keep,
            "should release and remember the qualified receiver"
        );
        let script = format!(r#"__next_f.push([1,"1:T50,{}"])"#, "x".repeat(80));

        assert_eq!(
            rewriter.rewrite(&script, &ctx(true, &state)),
            ScriptRewriteAction::Keep,
            "should pass through the oversized complete script"
        );

        let shared = document_state(&state);
        {
            let guard = shared.lock().expect("should lock document state");
            assert!(
                !guard.bypass_rsc,
                "should limit fallback to the oversized script"
            );
            assert!(
                !guard.rsc_receiver_trimmed,
                "should clear the completed claim's receiver flag"
            );
        }
        let later = r#"self.__next_f.push([1,"1:T3,abc"])"#;
        assert!(
            matches!(
                rewriter.rewrite(later, &ctx(true, &state)),
                ScriptRewriteAction::Replace(_)
            ),
            "should capture the next qualified script"
        );
    }

    #[test]
    fn oversized_payload_with_trailing_reference_keeps_later_capture() {
        for trimmed in [false, true] {
            for reference in ["self.__next_f", "window.__next_f", "foreign.__next_f"] {
                let state = IntegrationDocumentState::default();
                let rewriter =
                    NextJsRscPlaceholderRewriter::new(Arc::new(NextJsIntegrationConfig {
                        max_combined_payload_bytes: 100,
                        ..(*test_config()).clone()
                    }));
                let receiver = if trimmed {
                    assert_eq!(
                        rewriter.rewrite("self.", &ctx(false, &state)),
                        ScriptRewriteAction::Keep,
                        "should release the qualified receiver"
                    );
                    ""
                } else {
                    "self."
                };
                let script = format!(
                    r#"{receiver}__next_f.push([1,"1:T50,{}"]);console.log({reference});"#,
                    "x".repeat(80)
                );
                assert_eq!(
                    rewriter.rewrite(&script, &ctx(true, &state)),
                    ScriptRewriteAction::Keep,
                    "should retain oversized script"
                );
                assert!(
                    !document_state(&state)
                        .lock()
                        .expect("should lock document state")
                        .bypass_rsc,
                    "should not bypass a complete group with a trailing reference"
                );
                assert!(
                    matches!(
                        rewriter
                            .rewrite(r#"self.__next_f.push([1,"1:T3,abc"])"#, &ctx(true, &state)),
                        ScriptRewriteAction::Replace(_)
                    ),
                    "should still capture the next script"
                );
            }
        }
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
