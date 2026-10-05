use std::sync::Arc;

use error_stack::Report;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistration;
use crate::settings::{IntegrationConfig, Settings};

const NEXTJS_INTEGRATION_ID: &str = "nextjs";

mod rsc;
mod rsc_placeholders;
mod rsc_stream;
mod script_lexer;
mod script_rewriter;
mod shared;

pub use rsc::rewrite_rsc_scripts_combined;

use rsc_placeholders::NextJsRscPlaceholderRewriter;
use rsc_stream::NextJsRscStreamProcessorFactory;
use script_rewriter::NextJsNextDataRewriter;

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct NextJsIntegrationConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(
        default = "default_rewrite_attributes",
        deserialize_with = "crate::settings::vec_from_seq_or_map"
    )]
    #[validate(length(min = 1))]
    pub rewrite_attributes: Vec<String>,
    #[serde(default = "default_max_combined_payload_bytes")]
    pub max_combined_payload_bytes: usize,
}

impl IntegrationConfig for NextJsIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    false
}

fn default_rewrite_attributes() -> Vec<String> {
    vec!["href".to_owned(), "link".to_owned(), "url".to_owned()]
}

fn default_max_combined_payload_bytes() -> usize {
    10 * 1024 * 1024
}

pub(super) fn configuration_error(message: impl Into<String>) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!(
            "Integration '{NEXTJS_INTEGRATION_ID}' configuration error: {}",
            message.into()
        ),
    })
}

/// Whether the current parser fragment restores or passes through owned Flight.
/// This snapshot remains visible through downstream stages on empty/final text.
pub(crate) fn protects_current_flight_fragment(
    state: &crate::integrations::IntegrationDocumentState,
) -> bool {
    state
        .get::<std::sync::Mutex<rsc_stream::NextJsDocumentState>>(NEXTJS_INTEGRATION_ID)
        .is_some_and(|state| {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .current_fragment_protected
        })
}

/// Register the Next.js integration when enabled.
///
/// # Errors
///
/// Returns an error when the Next.js integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let config = if let Some(config) = build(settings)? {
        log::info!(
            "NextJS integration registered: enabled={}, rewrite_attributes={:?}, max_combined_payload_bytes={}",
            config.enabled,
            config.rewrite_attributes,
            config.max_combined_payload_bytes
        );
        config
    } else {
        log::info!("NextJS integration not registered (disabled or missing config)");
        return Ok(None);
    };
    // Register a structured (Pages Router __NEXT_DATA__) rewriter.
    let structured = Arc::new(NextJsNextDataRewriter::new(config.clone())?);

    // Insert placeholders for App Router RSC payload scripts during the HTML rewrite pass,
    // then substitute them through the bounded output stream processor.
    let placeholders = Arc::new(NextJsRscPlaceholderRewriter::new(config.clone()));

    let gtm_transform = settings
        .integration_config::<crate::integrations::google_tag_manager::GoogleTagManagerConfig>(
            "google_tag_manager",
        )?
        .map(|_| {
            crate::integrations::google_tag_manager::rewrite_gtm_rsc_span as fn(&str) -> String
        });
    let stream_processor = Arc::new(NextJsRscStreamProcessorFactory::new(
        config.clone(),
        gtm_transform,
    ));

    // Pages data must precede Flight capture: it sets the one-shot discriminator
    // that keeps JSON strings from being treated as executable Flight source.
    let builder = IntegrationRegistration::builder(NEXTJS_INTEGRATION_ID)
        .with_script_rewriter(structured)
        .with_script_rewriter(placeholders)
        .with_html_stream_processor(stream_processor);

    Ok(Some(builder.build()))
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<NextJsIntegrationConfig>>, Report<TrustedServerError>> {
    settings
        .integration_config::<NextJsIntegrationConfig>(NEXTJS_INTEGRATION_ID)
        .map(|config| config.map(Arc::new))
}

#[cfg(test)]
mod tests {
    use super::rsc_placeholders::RSC_PAYLOAD_PLACEHOLDER_PREFIX;
    use super::*;
    use crate::html_processor::{HtmlProcessorConfig, create_html_processor};
    use crate::integrations::IntegrationRegistry;
    use crate::streaming_processor::{
        Compression, PipelineConfig, StreamProcessor as _, StreamingPipeline,
    };
    use crate::test_support::tests::create_test_settings;
    use flate2::{read::GzDecoder, write::GzEncoder};
    use serde_json::json;
    use std::io::{Cursor, Read as _, Write as _};

    fn config_from_settings(
        settings: &Settings,
        registry: &IntegrationRegistry,
    ) -> HtmlProcessorConfig {
        HtmlProcessorConfig::from_settings(
            settings,
            registry,
            "origin.example.com",
            "test.example.com",
            "https",
        )
    }

    fn mixed_output(html: &str, chunk_size: usize) -> String {
        mixed_output_with_budget(html, chunk_size, 10 * 1024 * 1024, 16 * 1024 * 1024)
    }

    fn mixed_config(group_limit: usize, script_limit: usize) -> HtmlProcessorConfig {
        let mut settings = create_test_settings();
        for (id, config) in [
            (
                "nextjs",
                json!({"enabled": true, "max_combined_payload_bytes": group_limit}),
            ),
            (
                "google_tag_manager",
                json!({"enabled": true, "container_id": "GTM-MIX1"}),
            ),
        ] {
            settings
                .integrations
                .insert_config(id, &config)
                .expect("should configure integration");
        }
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(crate::auction::compile_auction_plan(&settings).expect("should compile plan")),
        )
        .expect("should create registry");
        let mut config = config_from_settings(&settings, &registry);
        config.max_buffered_body_bytes = script_limit;
        config
    }

    fn mixed_output_with_budget(
        html: &str,
        chunk_size: usize,
        group_limit: usize,
        script_limit: usize,
    ) -> String {
        let config = mixed_config(group_limit, script_limit);
        let mut pipeline = StreamingPipeline::new(
            PipelineConfig {
                input_compression: Compression::None,
                output_compression: Compression::None,
                chunk_size,
            },
            create_html_processor(config),
        );
        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should consume complete mixed response");
        String::from_utf8(output).expect("should emit UTF-8")
    }

    fn mixed_output_with_compression(
        html: &str,
        compression: Compression,
        chunk_size: usize,
    ) -> String {
        let input = if compression == Compression::Gzip {
            let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(html.as_bytes())
                .expect("should encode gzip input");
            encoder.finish().expect("should finish gzip input")
        } else {
            html.as_bytes().to_vec()
        };
        let mut pipeline = StreamingPipeline::new(
            PipelineConfig {
                input_compression: compression,
                output_compression: compression,
                chunk_size,
            },
            create_html_processor(mixed_config(100000, 100000)),
        );
        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(&input), &mut output)
            .expect("should consume complete mixed response");
        let mut decoded = Vec::new();
        if compression == Compression::Gzip {
            GzDecoder::new(output.as_slice())
                .read_to_end(&mut decoded)
                .expect("should decode gzip output");
        } else {
            decoded = output;
        }
        String::from_utf8(decoded).expect("should emit UTF-8")
    }

    #[test]
    fn non_code_flight_spellings_preserve_source_and_later_flight_rewrites() {
        let payload = "0:{\"url\":\"https://origin.example.com/page\"}\n";
        let flight = flight_html(&[payload]);
        for prefix in [
            r#"const demo="self.__next_f.push(";"#,
            r#"const demo='self.__next_f.push([1,"1:T7,example"])';"#,
            r#"const demo="escaped\" self.__next_f.push(";"#,
            "// self.__next_f.push(\n",
            "/* self.__next_f.push( */",
            "const demo=/self.__next_f.push()/;",
            "if (ready) { work() } /self.__next_f.push()/;",
            "function demo() { return 1 } /self.__next_f.push()/;",
            "if (ready) { if (nested) { work() } } /self.__next_f.push()/;",
            r#"if (ready) {} else /self.__next_f.push([2,"opaque"])/;"#,
            r#"do /self.__next_f.push([2,"opaque"])/; while (ready);"#,
            "const demo=`self.__next_f.push(`;",
            "const demo=`outer ${`inner self.__next_f.push(`}`;",
            r"const demo=`escaped\` self.__next_f.push( \${self.__next_f.push(}`;",
        ] {
            for split in 0..=prefix.len() {
                let mut processor = create_html_processor(mixed_config(10000, 10000));
                let first = format!("<script>{}", &prefix[..split]);
                let last = format!(
                    "{}const tag='https://www.googletagmanager.com/gtm.js';</script>{flight}",
                    &prefix[split..]
                );
                let mut bytes = processor
                    .process_chunk(first.as_bytes(), false)
                    .expect("should accept non-code prefix");
                bytes.extend(
                    processor
                        .process_chunk(last.as_bytes(), true)
                        .expect("should complete ordinary and Flight scripts"),
                );
                let output = String::from_utf8(bytes).expect("should emit UTF-8");
                assert!(
                    output.contains(&format!("<script>{prefix}const tag='https://www.googletagmanager.com/gtm.js';</script>")),
                    "should conservatively retain GTM beside inert Flight at split {split}: {output}"
                );
                assert!(
                    output.contains(r#"\"url\":\"https://test.example.com/page\""#),
                    "should still rewrite later real Flight at split {split}: {output}"
                );
                assert!(
                    !output.contains("__ts_rsc_"),
                    "should resolve every placeholder"
                );
            }
        }
    }

    #[test]
    fn lexical_filter_captures_only_executable_pushes_in_the_same_script() {
        let fake = r#"const demo='self.__next_f.push([1,"0:{\"url\":\"https://origin.example.com/fake\"}\n"])';"#;
        let push = r#"self.__next_f.push([1,"0:{\"url\":\"https://origin.example.com/page\"}\n"])"#;
        let rewritten_push = push.replace("origin.example.com", "test.example.com");
        for (source, expected) in [
            (
                format!("{fake}{push};/* self.__next_f.push( */"),
                format!("{fake}{rewritten_push};/* self.__next_f.push( */"),
            ),
            (
                format!("if (ready) {{ work() }} /self.__next_f.push()/;{push};"),
                format!("if (ready) {{ work() }} /self.__next_f.push()/;{rewritten_push};"),
            ),
            (
                format!(
                    "const out=`text self.__next_f.push( ${{ {{a: {push}, b: `nested ${{{push}}}`}} }} tail`;"
                ),
                format!(
                    "const out=`text self.__next_f.push( ${{ {{a: {rewritten_push}, b: `nested ${{{rewritten_push}}}`}} }} tail`;"
                ),
            ),
            (
                format!(
                    r#"const re=/["'`/]/;if (ready) /["'`/]/.test(input);const ratio=i++ / 2;const value=obj.return / 2;{push};"#
                ),
                format!(
                    r#"const re=/["'`/]/;if (ready) /["'`/]/.test(input);const ratio=i++ / 2;const value=obj.return / 2;{rewritten_push};"#
                ),
            ),
        ] {
            for split in 0..=source.len() {
                let mut processor = create_html_processor(mixed_config(10000, 10000));
                let first = format!("<script>{}", &source[..split]);
                let last = format!("{}</script>", &source[split..]);
                let mut bytes = processor
                    .process_chunk(first.as_bytes(), false)
                    .expect("should accept script prefix");
                bytes.extend(
                    processor
                        .process_chunk(last.as_bytes(), true)
                        .expect("should complete executable Flight"),
                );
                assert_eq!(
                    String::from_utf8(bytes).expect("should emit UTF-8"),
                    format!("<script>{expected}</script>"),
                    "should rewrite only executable calls at split {split}"
                );
            }
        }
    }

    #[test]
    fn brace_slash_division_keeps_flight_lengths_and_both_rewrites() {
        let data = json!({"url": "https://origin.example.com/page", "tag": "http://www.googletagmanager.com/gtm.js?id=GTM-MIX1", "text": "é😀"});
        let body = data.to_string();
        let payload = format!("1:T{:x},{body}", body.len());
        let push = format!(
            "self.__next_f.push([1,{}])",
            serde_json::to_string(&payload).expect("should encode Flight payload")
        );
        let mut expected = data;
        expected["url"] = json!("https://test.example.com/page");
        expected["tag"] = json!("/integrations/google_tag_manager/gtm.js?id=GTM-MIX1");
        for prefix in [
            "const n = {} / 2;",
            "const n = {nested: {}} / 2;",
            "const n = function() {} / 2;",
            "const n = function named() { return 1 } / 2;",
            "const n = class { method() { return 1 } } / 2;",
            "const n = class extends function() {} {} / 2;",
            "const n = class extends function Base() {} {} / 2;",
            "const n = class extends (function() {}) {} / 2;",
        ] {
            let source = format!("{prefix}{push}");
            for split in (0..=source.len()).filter(|split| source.is_char_boundary(*split)) {
                let mut processor = create_html_processor(mixed_config(10000, 10000));
                let first = format!("<script>{}", &source[..split]);
                let last = format!("{}</script><script>{push}</script>", &source[split..]);
                let mut bytes = processor
                    .process_chunk(first.as_bytes(), false)
                    .expect("should accept division prefix");
                bytes.extend(
                    processor
                        .process_chunk(last.as_bytes(), true)
                        .expect("should complete Flight after division"),
                );
                let output = String::from_utf8(bytes).expect("should emit UTF-8");
                assert!(output.starts_with(&format!("<script>{prefix}")));
                let payloads = flight_payloads(&output);
                assert_eq!(payloads.len(), 2, "should emit each real Flight push once");
                for payload in payloads {
                    assert_mixed_t_model(&payload, &expected);
                }
                assert!(!output.contains("__ts_rsc_"));
            }
            let html = format!("<script>{source}</script><script>{push}</script>");
            for compression in [Compression::None, Compression::Gzip] {
                for chunk_size in [32, 1000, 8192] {
                    let output = mixed_output_with_compression(&html, compression, chunk_size);
                    let payloads = flight_payloads(&output);
                    assert_eq!(
                        payloads.len(),
                        2,
                        "should preserve compressed Flight pushes"
                    );
                    for payload in payloads {
                        assert_mixed_t_model(&payload, &expected);
                    }
                    assert!(!output.contains("__ts_rsc_"));
                }
            }
        }
    }

    #[test]
    fn lexer_false_negatives_preserve_raw_flight_and_later_rewrites() {
        let data = json!({"url": "https://origin.example.com/page", "tag": "http://www.googletagmanager.com/gtm.js?id=GTM-MIX1", "text": "é😀"});
        let body = data.to_string();
        let payload = format!("1:T{:x},{body}", body.len());
        let push = format!(
            "self.__next_f.push([1,{}]);",
            serde_json::to_string(&payload).expect("should encode Flight payload")
        );
        let mut rewritten = data;
        rewritten["url"] = json!("https://test.example.com/page");
        rewritten["tag"] = json!("/integrations/google_tag_manager/gtm.js?id=GTM-MIX1");
        for prefix in [
            "foo: { bar() } /'/.test(s);",
            "switch (k) { case 1: {} /'/.test(s) }",
            "const f = () => {}\n/'/.test(s);",
            "const o = {while: (a) / 2, b: '/'};",
            "for (const m of /'/g.exec(s) || []) {}",
        ] {
            // Exercise both an entirely missed script and a missed call after
            // a captured one. Capturing a safe payload must not unprotect raw
            // Flight elsewhere in the same parser fragment.
            for leading in ["", push.as_str()] {
                let source = format!("{leading}{prefix}{push}");
                let assert_output = |output: &str| {
                    let payloads = flight_payloads(output);
                    let missed = usize::from(!leading.is_empty());
                    assert_eq!(payloads.len(), missed + 2, "should emit each push once");
                    assert_eq!(
                        payloads[missed], payload,
                        "should retain missed Flight bytes and lengths: {prefix}"
                    );
                    if missed != 0 {
                        assert_mixed_t_model(&payloads[0], &rewritten);
                    }
                    assert_mixed_t_model(&payloads[missed + 1], &rewritten);
                    assert!(!output.contains("__ts_rsc_"), "should resolve all captures");
                };
                for split in (0..=source.len()).filter(|split| source.is_char_boundary(*split)) {
                    let mut processor = create_html_processor(mixed_config(10000, 10000));
                    let first = format!("<script>{}", &source[..split]);
                    let last = format!("{}</script><script>{push}</script>", &source[split..]);
                    let mut bytes = processor
                        .process_chunk(first.as_bytes(), false)
                        .expect("should accept ambiguous JavaScript prefix");
                    bytes.extend(
                        processor
                            .process_chunk(last.as_bytes(), true)
                            .expect("should preserve raw and finish later Flight"),
                    );
                    assert_output(&String::from_utf8(bytes).expect("should emit UTF-8"));
                }
                let html = format!("<script>{source}</script><script>{push}</script>");
                for compression in [Compression::None, Compression::Gzip] {
                    for chunk_size in [32, 1000, 8192] {
                        assert_output(&mixed_output_with_compression(
                            &html,
                            compression,
                            chunk_size,
                        ));
                    }
                }
            }
        }
    }

    #[test]
    fn raw_flight_protection_survives_lexer_resynchronization_and_capture() {
        let missed_body = "`http://www.googletagmanager.com/gtm.js`";
        let missed_payload = format!("1:T{:x},{missed_body}", missed_body.len());
        let missed_push = format!(
            "self.__next_f.push([1,{}]);",
            serde_json::to_string(&missed_payload).expect("should encode missed Flight")
        );
        let data = json!({"url": "https://origin.example.com/page", "tag": "http://www.googletagmanager.com/gtm.js?id=GTM-MIX1"});
        let body = data.to_string();
        let push = format!(
            "self.__next_f.push([1,{}]);",
            serde_json::to_string(&format!("1:T{:x},{body}", body.len()))
                .expect("should encode capturable Flight")
        );
        let mut expected = data;
        expected["url"] = json!("https://test.example.com/page");
        expected["tag"] = json!("/integrations/google_tag_manager/gtm.js?id=GTM-MIX1");
        let source = format!("foo: {{ bar() }} /'/.test(s);{missed_push} /'/.test(s);{push}");
        let assert_output = |output: &str, boundary: usize| {
            let payloads = flight_payloads(output);
            assert_eq!(
                payloads.len(),
                3,
                "should retain missed and captured pushes"
            );
            assert_eq!(
                payloads[0], missed_payload,
                "should preserve raw continuation at boundary {boundary}"
            );
            assert_mixed_t_model(&payloads[1], &expected);
            assert_mixed_t_model(&payloads[2], &expected);
            assert!(
                !output.contains("__ts_rsc_"),
                "should resolve safe captures"
            );
        };
        for split in 0..=source.len() {
            let mut processor = create_html_processor(mixed_config(10000, 10000));
            let first = format!("<script>{}", &source[..split]);
            let last = format!("{}</script><script>{push}</script>", &source[split..]);
            let mut bytes = processor
                .process_chunk(first.as_bytes(), false)
                .expect("should accept raw Flight prefix");
            bytes.extend(
                processor
                    .process_chunk(last.as_bytes(), true)
                    .expect("should finish raw continuation and safe captures"),
            );
            assert_output(&String::from_utf8(bytes).expect("should emit UTF-8"), split);
        }
        let html = format!("<script>{source}</script><script>{push}</script>");
        for compression in [Compression::None, Compression::Gzip] {
            for chunk_size in [32, 1000, 8192] {
                assert_output(
                    &mixed_output_with_compression(&html, compression, chunk_size),
                    chunk_size,
                );
            }
        }
        // A qualified head can be observed but not buffered when an earlier
        // unrelated property makes the tentative receiver claim fail.
        let body_start = missed_push.find('`').expect("should locate raw T body");
        let first = format!(
            "<script>var ref=foreign.__next_f;{}",
            &missed_push[..body_start]
        );
        let continuation = format!("{}{push}", &missed_push[body_start..]);
        for split in 0..=continuation.len() {
            let mut processor = create_html_processor(mixed_config(10000, 10000));
            let mut bytes = processor
                .process_chunk(first.as_bytes(), false)
                .expect("should release qualified but unbuffered Flight");
            bytes.extend(
                processor
                    .process_chunk(&continuation.as_bytes()[..split], false)
                    .expect("should preserve unbuffered continuation"),
            );
            let last = format!("{}</script><script>{push}</script>", &continuation[split..]);
            bytes.extend(
                processor
                    .process_chunk(last.as_bytes(), true)
                    .expect("should finish later safe captures"),
            );
            assert_output(&String::from_utf8(bytes).expect("should emit UTF-8"), split);
        }
    }

    #[test]
    fn mixed_next_data_flight_mentions_do_not_claim_json_or_hide_gtm() {
        for text in [
            "self.__next_f",
            "window.__next_f",
            "self.__next_f.push([1,content])",
        ] {
            for chunk in [32, 64, 1000, 8192] {
                let data = json!({"props": {"pageProps": {
                    "text": text, "href": "https://origin.example.com/page",
                    "tag": "http://www.googletagmanager.com/gtm.js", "padding": "_".repeat(10000),
                }}});
                let output = mixed_output(
                    &format!(r#"<script id="__NEXT_DATA__">{data}</script>"#),
                    chunk,
                );
                let script = output
                    .split(r#"<script id="__NEXT_DATA__">"#)
                    .nth(1)
                    .expect("should retain Pages script")
                    .split("</script>")
                    .next()
                    .expect("should close Pages script");
                let mut expected = data;
                expected["props"]["pageProps"]["href"] = json!("https://test.example.com/page");
                expected["props"]["pageProps"]["tag"] =
                    json!("/integrations/google_tag_manager/gtm.js");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(script)
                        .expect("should preserve Pages JSON"),
                    expected,
                    "should not claim quoted Flight mention: {text}, chunk={chunk}"
                );
            }
        }
    }

    #[test]
    fn protected_flight_then_next_data_then_ordinary_gtm_resets_ownership() {
        let payload = "1:Tffff,'http://www.googletagmanager.com/gtm.js'";
        let pages = json!({"text": "self.__next_f.push([1,content])", "href": "https://origin.example.com/page", "tag": "http://www.googletagmanager.com/gtm.js"});
        // Keep the ordinary URL's opening quote and >=6 marker bytes together;
        // standalone GTM deliberately does not hold shorter tentative prefixes.
        let alignment = " ".repeat(23);
        let html = format!(
            r#"<script>self.__next_f.push([1,{}])</script><script id="__NEXT_DATA__">{pages}</script>{alignment}<script>var tag='http://www.googletagmanager.com/gtm.js';</script>"#,
            serde_json::to_string(payload).expect("should encode raw Flight")
        );
        for chunk in [32, 8192] {
            let output = mixed_output_with_budget(&html, chunk, 24, 10000);
            assert_eq!(
                flight_payloads(&output),
                [payload],
                "should preserve raw Flight"
            );
            let source = output
                .split(r#"<script id="__NEXT_DATA__">"#)
                .nth(1)
                .expect("should retain Pages script")
                .split("</script>")
                .next()
                .expect("should close Pages script");
            let mut expected = pages.clone();
            expected["href"] = json!("https://test.example.com/page");
            expected["tag"] = json!("/integrations/google_tag_manager/gtm.js");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(source).expect("should retain JSON"),
                expected
            );
            assert!(
                output.ends_with(
                    "<script>var tag='/integrations/google_tag_manager/gtm.js';</script>"
                ),
                "should rewrite later ordinary GTM at chunk={chunk}: {output}"
            );
        }
    }

    #[test]
    fn push_head_after_tentative_reference_overflow_protects_raw_continuation() {
        let payload = "1:Tffff,'http://www.googletagmanager.com/gtm.js'";
        let html = format!(
            r#"<script>self.__next_f{}.push([1,{}])</script><script>self.__next_f.push([1,"https://origin.example.com/page"])</script>"#,
            " ".repeat(512),
            serde_json::to_string(payload).expect("should encode raw Flight")
        );
        for chunk in [32, 8192] {
            assert_eq!(
                mixed_output_with_budget(&html, chunk, 100, 10000),
                html,
                "should protect a call confirmed after tentative probe overflow and preserve its later raw continuation"
            );
        }
    }

    #[test]
    fn ordinary_next_f_reference_does_not_bypass_later_flight() {
        let body = "http://www.googletagmanager.com/gtm.js";
        let payload = format!("1:T{:x},{body}", body.len());
        for padding in [0, 400] {
            for chunk in [32, 8192] {
                let html = format!(
                    "<script>var ref=self.__next_f;var padding='{}';</script>{}",
                    "_".repeat(padding),
                    flight_html(&[&payload])
                );
                let output = mixed_output_with_budget(&html, chunk, 100, 10000);
                assert_eq!(
                    flight_payloads(&output),
                    [format!(
                        "1:T{:x},/integrations/google_tag_manager/gtm.js",
                        "/integrations/google_tag_manager/gtm.js".len()
                    )],
                    "should not bypass captured Flight because of a prior ordinary reference: padding={padding}, chunk={chunk}"
                );
            }
        }
    }

    #[test]
    fn trailing_next_f_references_preserve_current_and_later_flight() {
        let data = json!({"url": "https://origin.example.com/page", "tag": "http://www.googletagmanager.com/gtm.js?id=GTM-MIX1"});
        let body = data.to_string();
        let payload = format!("1:T{:x},{body}", body.len());
        let push = format!(
            "self.__next_f.push([1,{}])",
            serde_json::to_string(&payload).expect("should encode Flight payload")
        );
        let mut expected = data;
        expected["url"] = json!("https://test.example.com/page");
        expected["tag"] = json!("/integrations/google_tag_manager/gtm.js?id=GTM-MIX1");
        for reference in ["self.__next_f", "window.__next_f", "foreign.__next_f"] {
            let source = format!("{push};console.log({reference});");
            for split in 0..=source.len() {
                let mut processor = create_html_processor(mixed_config(10000, 10000));
                let first = format!("<script>{}", &source[..split]);
                let last = format!("{}</script><script>{push}</script>", &source[split..]);
                let mut bytes = processor
                    .process_chunk(first.as_bytes(), false)
                    .expect("should accept script prefix");
                bytes.extend(
                    processor
                        .process_chunk(last.as_bytes(), true)
                        .expect("should complete Flight with trailing reference"),
                );
                let output = String::from_utf8(bytes).expect("should emit UTF-8");
                let payloads = flight_payloads(&output);
                assert_eq!(payloads.len(), 2, "should emit both Flight pushes once");
                for payload in payloads {
                    assert_mixed_t_model(&payload, &expected);
                }
                assert!(
                    output.contains(&format!(";console.log({reference});</script>")),
                    "should retain trailing reference at split {split}"
                );
                assert!(!output.contains("__ts_rsc_"), "should resolve placeholders");
            }
            let html = format!("<script>{source}</script><script>{push}</script>");
            for compression in [Compression::None, Compression::Gzip] {
                for chunk in [32, 1000, 8192] {
                    let output = mixed_output_with_compression(&html, compression, chunk);
                    let payloads = flight_payloads(&output);
                    assert_eq!(payloads.len(), 2, "should emit both compressed pushes once");
                    for payload in payloads {
                        assert_mixed_t_model(&payload, &expected);
                    }
                }
            }
        }
    }

    #[test]
    fn ordinary_next_f_reference_does_not_hide_gtm() {
        let script = "var ref=self.__next_f;var tag='http://www.googletagmanager.com/gtm.js';";
        for chunk in [32, 64, 1000, 8192] {
            let output = mixed_output(&format!("<script>{script}</script>"), chunk);
            assert_eq!(
                output,
                "<script>var ref=self.__next_f;var tag='/integrations/google_tag_manager/gtm.js';</script>",
                "should not protect a non-push reference, chunk={chunk}"
            );
        }
    }

    #[test]
    fn mixed_next_data_preserves_json_and_both_rewrites() {
        for chunk in [32, 64, 1000, 8192] {
            for marker in ["_", "__", "__n", "__ne", "__nex", "__next", "__next_"] {
                for (url, routed) in [
                    (
                        "https://www.googletagmanager.com/gtm.js?id=GTM-MIX1",
                        "/integrations/google_tag_manager/gtm.js?id=GTM-MIX1",
                    ),
                    (
                        "http://www.google-analytics.com/g/collect?v=2",
                        "/integrations/google_tag_manager/g/collect?v=2",
                    ),
                ] {
                    for late in [false, true] {
                        let padding = json!(format!("{}{marker}", "_".repeat(10000)));
                        let source = if late {
                            format!(
                                r#"{{"padding":{padding},"href":"https://origin.example.com/page","tag":"{url}"}}"#
                            )
                        } else {
                            format!(
                                r#"{{"tag":"{url}","padding":{padding},"href":"https://origin.example.com/page"}}"#
                            )
                        };
                        let html = format!(
                            r#"<html><body><script id="__NEXT_DATA__">{source}</script><p>suffix</p></body></html>"#
                        );
                        let output = mixed_output(&html, chunk);
                        let script = output
                            .split(r#"<script id="__NEXT_DATA__">"#)
                            .nth(1)
                            .expect("should retain script")
                            .split("</script>")
                            .next()
                            .expect("should close script");
                        let expected = json!({"padding": format!("{}{marker}", "_".repeat(10000)), "href": "https://test.example.com/page", "tag": routed});
                        assert_eq!(
                            serde_json::from_str::<serde_json::Value>(script)
                                .expect("should preserve valid JSON"),
                            expected,
                            "chunk={chunk}, marker={marker}, late={late}"
                        );
                        assert!(output.ends_with("</body></html>"), "should finish response");
                    }
                }
            }
        }
    }

    fn flight_payloads(output: &str) -> Vec<String> {
        let mut payloads = Vec::new();
        for part in output.split("<script>").skip(1) {
            let mut script = part.split("</script>").next().expect("should close script");
            while let Some((start, end)) = shared::find_rsc_push_payload_range(script) {
                payloads.push(
                    serde_json::from_str::<String>(&script[start - 1..=end])
                        .expect("should retain valid push string JSON"),
                );
                script = &script[end + 1..];
            }
        }
        payloads
    }

    fn assert_mixed_t_model(payload: &str, expected: &serde_json::Value) {
        let (header, body) = payload.split_once(',').expect("should retain T record");
        assert_eq!(
            usize::from_str_radix(&header[3..], 16).expect("should retain hex length"),
            body.len(),
            "should count decoded transformed T bytes"
        );
        assert_eq!(
            &serde_json::from_str::<serde_json::Value>(body).expect("should retain model JSON"),
            expected
        );
    }

    #[test]
    fn normal_flight_bootstrap_controls_allow_later_payload_rewriting() {
        let controls = "(self.__next_f=self.__next_f||[]).push([0]);self.__next_f.push([2,null])";
        let data = json!({"href": "https://origin.example.com/page", "tag": "http://www.googletagmanager.com/gtm.js", "ga": "//www.google-analytics.com/g/collect?v=2", "text": "é😀"});
        let body = data.to_string();
        let push = format!(
            "self.__next_f.push([1,{}])",
            serde_json::to_string(&format!("1:T{:x},{body}", body.len()))
                .expect("should encode Flight")
        );
        let mut expected = data;
        expected["href"] = json!("https://test.example.com/page");
        expected["tag"] = json!("/integrations/google_tag_manager/gtm.js");
        expected["ga"] = json!("/integrations/google_tag_manager/g/collect?v=2");
        for together in [false, true] {
            let scripts = if together {
                format!("<script>{controls};{push};self.__next_f.push([2,null])</script>")
            } else {
                format!("<script>{controls}</script><script>{push}</script>")
            };
            let html = format!("<html><body>{scripts}<p>suffix</p></body></html>");
            for compression in [Compression::None, Compression::Gzip] {
                for chunk_size in [32, 64, 1000, 8192] {
                    let output = mixed_output_with_compression(&html, compression, chunk_size);
                    assert!(
                        output.contains(controls),
                        "should preserve inert bootstrap controls"
                    );
                    let payloads = flight_payloads(&output);
                    assert_eq!(payloads.len(), 1, "should emit payload exactly once");
                    assert_mixed_t_model(&payloads[0], &expected);
                    assert!(!output.contains("__ts_rsc_"));
                    assert!(output.ends_with("<p>suffix</p></body></html>"));
                }
            }
        }
    }

    #[test]
    fn same_delimiter_escaped_flight_queries_preserve_data_and_t_lengths() {
        let data = json!({"tag": "http://www.googletagmanager.com/gtm.js?q=\"x\"&v=é", "href": "https://origin.example.com/page", "url": "http://origin.example.com/other"});
        let body = data.to_string();
        let payload = format!("1:T{:x},{body}", body.len());
        let mut expected = data;
        expected["tag"] = json!("/integrations/google_tag_manager/gtm.js?q=\"x\"&v=é");
        expected["href"] = json!("https://test.example.com/page");
        expected["url"] = json!("https://test.example.com/other");
        for chunk in [32, 64, 1000, 8192] {
            let output = mixed_output(&flight_html(&[&payload]), chunk);
            let payloads = flight_payloads(&output);
            assert_mixed_t_model(&payloads[0], &expected);
            let body = r"'http://www.googletagmanager.com/gtm.js?q=\'x\''";
            let payload = format!("1:T{:x},{body}", body.len());
            let output = mixed_output(&flight_html(&[&payload]), chunk);
            let payloads = flight_payloads(&output);
            let (header, transformed) = payloads[0].split_once(',').expect("should retain T");
            assert_eq!(
                usize::from_str_radix(&header[3..], 16).expect("should parse hex"),
                transformed.len()
            );
            assert_eq!(
                transformed,
                r"'/integrations/google_tag_manager/gtm.js?q=\'x\''"
            );
        }
    }

    #[test]
    fn sticky_false_google_prefix_preserves_raw_script_metacharacters() {
        let fragments = [
            "<script>var probe='google",
            "X';",
            "if (a && b < c) { var u='https://example.com/?a=1&b=2'; }",
            "tail();</script>",
        ];
        let mut processor = create_html_processor(mixed_config(10000, 10000));
        let mut bytes = Vec::new();
        for (index, fragment) in fragments.iter().enumerate() {
            bytes.extend(
                processor
                    .process_chunk(fragment.as_bytes(), index == fragments.len() - 1)
                    .expect("should consume raw script fragment"),
            );
        }
        assert_eq!(
            String::from_utf8(bytes).expect("should emit UTF-8"),
            fragments.concat(),
            "holding/restoration must preserve raw JS source, not HTML-escape it"
        );
    }

    #[test]
    fn ordinary_gtm_before_captured_flight_is_chunk_invariant() {
        let ordinary = "var tag='http://www.googletagmanager.com/gtm.js';";
        let expected_ordinary = "var tag='/integrations/google_tag_manager/gtm.js';";
        let data = json!({"url": "https://origin.example.com/page", "tag": "http://www.googletagmanager.com/gtm.js?id=GTM-MIX1", "text": "é😀"});
        let body = data.to_string();
        let mut expected = data;
        expected["url"] = json!("https://test.example.com/page");
        expected["tag"] = json!("/integrations/google_tag_manager/gtm.js?id=GTM-MIX1");
        for payload in [format!("0:{body}\n"), format!("1:T{:x},{body}", body.len())] {
            let push = format!(
                "self.__next_f.push([1,{}])",
                serde_json::to_string(&payload).expect("should encode Flight payload")
            );
            let html = format!("<script>{ordinary}{push}</script>");
            let whole = mixed_output(&html, 8192);
            assert!(
                whole.starts_with(&format!("<script>{expected_ordinary}")),
                "should rewrite ordinary source before Flight"
            );
            let payloads = flight_payloads(&whole);
            assert_eq!(payloads.len(), 1, "should emit one captured push");
            if payload.starts_with("1:T") {
                assert_mixed_t_model(&payloads[0], &expected);
            } else {
                assert_eq!(payloads[0], format!("0:{expected}\n"));
            }
            for split in (0..=push.len()).filter(|split| push.is_char_boundary(*split)) {
                let mut processor = create_html_processor(mixed_config(10000, 10000));
                let fragments = [
                    format!("<script>{ordinary}"),
                    push[..split].to_owned(),
                    format!("{}</script>", &push[split..]),
                ];
                let mut bytes = Vec::new();
                for (index, fragment) in fragments.iter().enumerate() {
                    bytes.extend(
                        processor
                            .process_chunk(fragment.as_bytes(), index == fragments.len() - 1)
                            .expect("should complete ordinary and captured Flight source"),
                    );
                }
                assert_eq!(
                    String::from_utf8(bytes).expect("should emit UTF-8"),
                    whole,
                    "should preserve both rewrites at Flight split {split}"
                );
            }
            for compression in [Compression::None, Compression::Gzip] {
                for chunk in [128, 1000, 8192] {
                    assert_eq!(
                        mixed_output_with_compression(&html, compression, chunk),
                        whole,
                        "should preserve both rewrites for {compression:?} at chunk {chunk}"
                    );
                }
            }
        }
    }

    #[test]
    fn ordinary_gtm_before_flight_fallback_preserves_raw_payloads() {
        let ordinary = "var tag='http://www.googletagmanager.com/gtm.js';";
        let payload = "1:Tffff,'http://www.googletagmanager.com/gtm.js?id=GTM-MIX1'";
        let encoded = serde_json::to_string(payload).expect("should encode raw Flight payload");
        for (channel, limit, captured) in [(1, 10000, true), (1, 24, false), (2, 10000, false)] {
            let push = format!("self.__next_f.push([{channel},{encoded}])");
            let html = format!("<script>{ordinary}{push}</script>");
            let whole = mixed_output_with_budget(&html, 8192, limit, 10000);
            let expected_ordinary = if captured {
                ordinary.replace(
                    "http://www.googletagmanager.com",
                    "/integrations/google_tag_manager",
                )
            } else {
                ordinary.to_owned()
            };
            assert_eq!(
                whole,
                format!("<script>{expected_ordinary}{push}</script>"),
                "should retain original fallback payload bytes"
            );
            for split in 0..=push.len() {
                let mut processor = create_html_processor(mixed_config(limit, 10000));
                let fragments = [
                    format!("<script>{ordinary}"),
                    push[..split].to_owned(),
                    push[split..].to_owned(),
                    "</script>".to_owned(),
                ];
                let mut bytes = Vec::new();
                for (index, fragment) in fragments.iter().enumerate() {
                    bytes.extend(
                        processor
                            .process_chunk(fragment.as_bytes(), index == fragments.len() - 1)
                            .expect("should restore raw Flight without changing payload lengths"),
                    );
                }
                assert_eq!(
                    String::from_utf8(bytes).expect("should emit UTF-8"),
                    whole,
                    "should preserve fallback at channel {channel}, limit {limit}, split {split}"
                );
            }
        }
    }

    #[test]
    fn mixed_flight_first_party_urls_preserve_t_lengths() {
        for scheme in ["https:", "http:", ""] {
            for host_path in [
                "www.googletagmanager.com/gtm.js",
                "www.googletagmanager.com/gtag/js?id=G-MIX1",
                "www.google-analytics.com/g/collect",
                "analytics.google.com/collect?v=2",
            ] {
                for quoted in [false, true] {
                    let url = format!("{scheme}//{host_path}");
                    let body = if quoted { format!("\"{url}\"") } else { url };
                    let payload = format!("1:T{:x},{body}", body.len());
                    let encoded = serde_json::to_string(&payload).expect("should encode payload");
                    let html = format!(
                        "<html><body><script>self.__next_f.push([1,{encoded}])</script><p>suffix</p></body></html>"
                    );
                    for chunk in [32, 64, 1000, 8192] {
                        let output = mixed_output(&html, chunk);
                        let payloads = flight_payloads(&output);
                        assert_eq!(payloads.len(), 1);
                        let (header, body) =
                            payloads[0].split_once(',').expect("should retain T record");
                        assert!(
                            body.contains("/integrations/google_tag_manager/"),
                            "should rewrite captured GTM-only flight: {body}"
                        );
                        assert_eq!(
                            usize::from_str_radix(&header[3..], 16)
                                .expect("should keep hex length"),
                            body.len(),
                            "should count transformed decoded bytes"
                        );
                    }
                }
            }
        }
        let payload = "0:{\"url\":\"https://www.googletagmanager.com/gtm.js\",\"href\":\"https://origin.example.com/page\"}\n";
        let encoded = serde_json::to_string(&payload).expect("should encode payload");
        let output = mixed_output(
            &format!("<script>self.__next_f.push([1,{encoded}])</script>"),
            32,
        );
        let payloads = flight_payloads(&output);
        assert!(payloads[0].contains("test.example.com/page"));
        assert!(
            payloads[0].contains("/integrations/google_tag_manager/gtm.js"),
            "should rewrite queryless escaped quote URL"
        );
    }

    #[test]
    fn mixed_flight_batch_rewrites_every_payload() {
        let bodies = [
            "http://www.googletagmanager.com/gtm.js",
            "//www.google-analytics.com/collect?v=2",
        ];
        let pushes: Vec<String> = bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                let payload = format!("{index}:T{:x},{body}", body.len());
                format!(
                    "self.__next_f.push([1,{}])",
                    serde_json::to_string(&payload).expect("should encode payload")
                )
            })
            .collect();
        let output = mixed_output(&format!("<script>{}</script>", pushes.join(";")), 8192);
        let script = output
            .split("<script>")
            .nth(1)
            .expect("should retain script")
            .split("</script>")
            .next()
            .expect("should close script");
        for push in script.split(';') {
            let json = push
                .strip_prefix("self.__next_f.push(")
                .expect("should retain push")
                .strip_suffix(')')
                .expect("should close push");
            let value: serde_json::Value =
                serde_json::from_str(json).expect("should preserve push JSON");
            let (header, body) = value[1]
                .as_str()
                .expect("should retain payload")
                .split_once(',')
                .expect("should retain T header");
            assert!(
                body.contains("/integrations/google_tag_manager/"),
                "should rewrite every captured push"
            );
            assert_eq!(
                usize::from_str_radix(&header[3..], 16).expect("should parse hex"),
                body.len(),
                "should recount every payload"
            );
        }
    }

    #[test]
    fn mixed_next_data_prefix_tails_and_false_google_preserve_payload() {
        for chunk in [32, 64, 1000, 8192] {
            for prefix in [
                "_", "__", "__n", "__ne", "__nex", "__next", "__next_", "google",
            ] {
                for alignment in [0, 1, 13, 1023, 1024] {
                    let data = json!({"padding": format!("{}{prefix}", "_".repeat(12000)), "href": "https://origin.example.com/page"});
                    let start = format!(
                        "<html><body>{}<script id=\"__NEXT_DATA__\">",
                        " ".repeat(alignment)
                    );
                    let serialized = data.to_string();
                    let padding_start = serialized
                        .find("\"padding\":\"")
                        .expect("should locate padding")
                        + "\"padding\":\"".len();
                    let boundary = start.len() + padding_start + 12000 + prefix.len();
                    let padding = (chunk - boundary % chunk) % chunk;
                    let mut data = data;
                    data["padding"] = json!(format!("{}{prefix}", "_".repeat(12000 + padding)));
                    let output =
                        mixed_output(&format!("{start}{data}</script></body></html>"), chunk);
                    let script = output
                        .split("<script id=\"__NEXT_DATA__\">")
                        .nth(1)
                        .expect("should retain script")
                        .split("</script>")
                        .next()
                        .expect("should close script");
                    data["href"] = json!("https://test.example.com/page");
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(script)
                            .expect("should keep valid JSON"),
                        data,
                        "prefix={prefix}, chunk={chunk}, alignment={alignment}"
                    );
                }
            }
        }
    }

    #[test]
    fn mixed_next_data_identity_and_gzip_chunk_sweep() {
        let data = json!({"href": "https://origin.example.com/page", "tag": "https://www.google-analytics.com/collect?v=2", "padding": "_".repeat(12000)});
        let html = format!(
            "<html><body><script id=\"__NEXT_DATA__\">{data}</script><p>suffix</p></body></html>"
        );
        for compression in [Compression::None, Compression::Gzip] {
            for chunk_size in [32, 64, 1000, 8192] {
                let output = mixed_output_with_compression(&html, compression, chunk_size);
                let script = output
                    .split("<script id=\"__NEXT_DATA__\">")
                    .nth(1)
                    .expect("should keep script")
                    .split("</script>")
                    .next()
                    .expect("should close script");
                let mut expected = data.clone();
                expected["href"] = json!("https://test.example.com/page");
                expected["tag"] = json!("/integrations/google_tag_manager/collect?v=2");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(script)
                        .expect("should retain valid JSON"),
                    expected
                );
                assert!(output.ends_with("</body></html>"));
            }
        }
    }

    #[test]
    fn mixed_registry_isolates_interleaved_html_documents() {
        let config = mixed_config(10000, 10000);
        let mut a = create_html_processor(config.clone());
        let mut b = create_html_processor(config);
        let mut output_a = a
            .process_chunk(b"<script>var a='http://www.google", false)
            .expect("should accept document A prefix");
        let mut output_b = b
            .process_chunk(b"<script>var b='https://www.google", false)
            .expect("should accept document B prefix");
        output_a.extend(
            a.process_chunk(b"tagmanager.com/gtm.js';</script>", true)
                .expect("should finish document A"),
        );
        output_b.extend(
            b.process_chunk(b"-analytics.com/collect?v=2';</script>", true)
                .expect("should finish document B"),
        );
        assert_eq!(
            String::from_utf8(output_a).expect("should emit UTF-8"),
            "<script>var a='/integrations/google_tag_manager/gtm.js';</script>"
        );
        assert_eq!(
            String::from_utf8(output_b).expect("should emit UTF-8"),
            "<script>var b='/integrations/google_tag_manager/collect?v=2';</script>"
        );
    }

    #[test]
    fn structured_and_probe_composition_at_every_identifier_prefix_tail() {
        for prefix in ["_", "__", "__n", "__ne", "__nex", "__next", "__next_"] {
            let mut processor = create_html_processor(mixed_config(100000, 100000));
            let first = format!("<script id=\"__NEXT_DATA__\">{{\"padding\":\"{prefix}");
            let second = "\",\"href\":\"https://origin.example.com/page\"}</script>";
            let mut bytes = processor
                .process_chunk(first.as_bytes(), false)
                .expect("should accept explicit prefix tail");
            bytes.extend(
                processor
                    .process_chunk(second.as_bytes(), true)
                    .expect("should finish suppressed text"),
            );
            let html = String::from_utf8(bytes).expect("should emit UTF-8");
            let script = html
                .split("<script id=\"__NEXT_DATA__\">")
                .nth(1)
                .expect("should keep script")
                .split("</script>")
                .next()
                .expect("should close script");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(script).expect("should keep valid JSON"),
                json!({"padding": prefix, "href": "https://test.example.com/page"})
            );
        }
    }

    #[test]
    fn mixed_existing_app_fixture_completes_with_both_enabled() {
        let fixture = include_str!("../../html_processor.test.html");
        let data = json!({"href": "https://origin.example.com/fixture-proof", "tag": "http://www.googletagmanager.com/gtm.js", "ga": "//www.google-analytics.com/g/collect?v=2"});
        let body = data.to_string();
        let push = format!(
            "<script>self.__next_f.push([1,{}])</script>",
            serde_json::to_string(&format!("f:T{:x},{body}", body.len()))
                .expect("should encode fixture proof")
        );
        let (before, after) = fixture
            .rsplit_once("</body>")
            .expect("should retain structural body close");
        let fixture_copy = format!("{before}{push}</body>{after}");
        assert!(
            fixture_copy.contains(
                "(self.__next_f=self.__next_f||[]).push([0]);self.__next_f.push([2,null])"
            ),
            "should retain real bootstrap sequence"
        );
        let mut expected = data;
        expected["href"] = json!("https://test.example.com/fixture-proof");
        expected["tag"] = json!("/integrations/google_tag_manager/gtm.js");
        expected["ga"] = json!("/integrations/google_tag_manager/g/collect?v=2");
        for chunk in [32, 8192] {
            let output = mixed_output(&fixture_copy, chunk);
            let payloads = flight_payloads(&output);
            let proof = payloads
                .iter()
                .find(|payload| payload.starts_with("f:T"))
                .expect("should retain designated Flight proof");
            assert_mixed_t_model(proof, &expected);
            assert!(!output.contains("__ts_rsc_"));
            assert!(
                output.contains("googletagmanager.com/ns.html"),
                "should retain unsupported iframe routing"
            );
            assert!(output.trim_end().ends_with("</html>"));
        }
    }

    fn flight_html(payloads: &[&str]) -> String {
        let scripts: String = payloads
            .iter()
            .map(|payload| {
                format!(
                    "<script>self.__next_f.push([1,{}])</script>",
                    serde_json::to_string(payload).expect("should encode Flight")
                )
            })
            .collect();
        format!("<html><body>{scripts}<p>suffix</p></body></html>")
    }

    #[test]
    fn mixed_flight_cross_script_lengths_and_split_hostname_safety() {
        let body = r#"{"tag":"http://www.googletagmanager.com/gtm.js","url":"https://origin.example.com/page","text":"é😀"}"#;
        let payload = format!("1:T{:x},{body}", body.len());
        for split in [
            (payload.find("http:").expect("should locate URL") - 1),
            payload
                .find("tagmanager")
                .expect("should locate hostname split"),
        ] {
            for chunk in [32, 64, 1000, 8192] {
                let output =
                    mixed_output(&flight_html(&[&payload[..split], &payload[split..]]), chunk);
                let combined = flight_payloads(&output).concat();
                let (header, rewritten) = combined.split_once(',').expect("should retain T header");
                assert_eq!(
                    usize::from_str_radix(&header[3..], 16).expect("should parse length"),
                    rewritten.len()
                );
                let data: serde_json::Value =
                    serde_json::from_str(rewritten).expect("should preserve model JSON");
                if split == (payload.find("http:").expect("should locate URL") - 1) {
                    assert_eq!(data["tag"], "/integrations/google_tag_manager/gtm.js");
                } else {
                    assert_eq!(
                        data["tag"], "http://www.googletagmanager.com/gtm.js",
                        "should safely preserve URL split across actual payload scripts"
                    );
                }
                assert_eq!(data["url"], "https://test.example.com/page");
                assert_eq!(data["text"], "é😀");
            }
        }
        let escaped = body.replace('/', r"\/");
        let payload = format!("1:T{:x},{escaped}", escaped.len());
        let output = mixed_output(&flight_html(&[&payload]), 32);
        let payloads = flight_payloads(&output);
        let (header, body) = payloads[0].split_once(',').expect("should retain T");
        assert_eq!(
            usize::from_str_radix(&header[3..], 16).expect("should parse length"),
            body.len()
        );
        let data: serde_json::Value =
            serde_json::from_str(body).expect("should preserve escaped model JSON");
        assert_eq!(data["tag"], "/integrations/google_tag_manager/gtm.js");
    }

    #[test]
    fn mixed_flight_fallback_preserves_original_payloads_and_later_scripts() {
        let url = "'http://www.googletagmanager.com/gtm.js?id=GTM-MIX1'";
        let incomplete = format!("1:Tffff,{url}");
        let oversized = format!("1:T{:x},{url}", url.len());
        for (payloads, group_limit, script_limit) in [
            (vec![incomplete.as_str()], 10000, 10000),
            (vec!["1:T", url], 10000, 10000),
            (vec![oversized.as_str()], 24, 10000),
            (vec![oversized.as_str()], 10000, 10),
        ] {
            for chunk in [32, 64, 1000, 8192] {
                let html = flight_html(&payloads).replace("<p>suffix</p>", "<script>var ordinary='http://www.googletagmanager.com/gtm.js';</script><p>suffix</p>");
                let output = mixed_output_with_budget(&html, chunk, group_limit, script_limit);
                assert_eq!(
                    flight_payloads(&output),
                    payloads,
                    "should preserve fallback payload bytes: group={group_limit}, script={script_limit}, chunk={chunk}"
                );
                if script_limit > 10 {
                    assert!(
                        output.contains("var ordinary='/integrations/google_tag_manager/gtm.js';"),
                        "should rewrite ordinary script after Flight fallback"
                    );
                }
                assert!(output.ends_with("</body></html>"));
            }
        }
    }

    #[test]
    fn mixed_flight_response_completes() {
        let payload = r#"0:{\"href\":\"https://origin.example.com/page\",\"url\":\"https://www.googletagmanager.com/gtm.js?id=GTM-MIX1\"}\n"#;
        let html = format!(
            r#"<html><body><script>self.__next_f.push([1,"{payload}"])</script><p>suffix</p></body></html>"#
        );
        let output = mixed_output(&html, 8192);
        assert!(
            output.contains("test.example.com/page"),
            "should rewrite origin"
        );
        assert!(!output.contains("__ts_rsc_"), "should resolve placeholders");
        assert!(output.ends_with("</body></html>"), "should finish response");
    }

    #[test]
    fn html_processor_rewrites_nextjs_script_when_enabled() {
        let html = r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
                {"props":{"pageProps":{"primary":{"href":"https://origin.example.com/reviews"},"secondary":{"href":"http://origin.example.com/sign-in"},"fallbackHref":"http://origin.example.com/legacy","protoRelative":"//origin.example.com/assets/logo.png"}}}
            </script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");
        let processed = String::from_utf8_lossy(&output);

        // Note: URLs may have padding characters for length preservation
        assert!(
            processed.contains("test.example.com") && processed.contains("/reviews"),
            "should rewrite https Next.js href values to test.example.com"
        );
        assert!(
            processed.contains("test.example.com") && processed.contains("/sign-in"),
            "should rewrite http Next.js href values to test.example.com"
        );
        assert!(
            processed.contains(r#""fallbackHref":"http://origin.example.com/legacy""#),
            "should leave other fields untouched"
        );
        assert!(
            processed.contains(r#""protoRelative":"//origin.example.com/assets/logo.png""#),
            "should not rewrite non-href keys"
        );
        assert!(
            !processed.contains("\"href\":\"https://origin.example.com/reviews\""),
            "should remove origin https href"
        );
        assert!(
            !processed.contains("\"href\":\"http://origin.example.com/sign-in\""),
            "should remove origin http href"
        );
    }

    #[test]
    fn html_processor_rewrites_next_data_fixture_like_payload() {
        let html = r#"<html><body>
            <script id="__NEXT_DATA__" type="application/json">
                {
                    "props": {
                        "pageProps": {
                            "siteProductionDomain": "origin.example.com",
                            "siteBaseUrl": "https://origin.example.com",
                            "navigation": {
                                "href": "https://origin.example.com:8443/reviews",
                                "link": "//origin.example.com:9443/assets/logo.png"
                            },
                            "article": {
                                "url": "https://origin.example.com/news"
                            },
                            "canonicalUrl": "https://origin.example.com/should-stay",
                            "metadata": {
                                "ogUrl": "https://origin.example.com/should-stay-too"
                            }
                        }
                    }
                }
            </script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": [
                        "href",
                        "link",
                        "siteBaseUrl",
                        "siteProductionDomain",
                        "url"
                    ],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");
        let processed = String::from_utf8_lossy(&output);

        assert!(
            processed.contains(r#""siteProductionDomain": "test.example.com""#),
            "should rewrite siteProductionDomain in __NEXT_DATA__. Output: {processed}"
        );
        assert!(
            processed.contains(r#""siteBaseUrl": "https://test.example.com""#),
            "should rewrite siteBaseUrl in __NEXT_DATA__. Output: {processed}"
        );
        assert!(
            processed.contains(r#""href": "https://test.example.com:8443/reviews""#),
            "should preserve explicit ports while rewriting href in __NEXT_DATA__. Output: {processed}"
        );
        assert!(
            processed.contains(r#""link": "//test.example.com:9443/assets/logo.png""#),
            "should preserve explicit ports while rewriting protocol-relative links in __NEXT_DATA__. Output: {processed}"
        );
        assert!(
            processed.contains(r#""url": "https://test.example.com/news""#),
            "should rewrite url fields in __NEXT_DATA__. Output: {processed}"
        );
        assert!(
            processed.contains(r#""canonicalUrl": "https://origin.example.com/should-stay""#),
            "should leave non-configured fields untouched in __NEXT_DATA__. Output: {processed}"
        );
        assert!(
            processed.contains(r#""ogUrl": "https://origin.example.com/should-stay-too""#),
            "should leave nested non-configured fields untouched in __NEXT_DATA__. Output: {processed}"
        );
        assert!(
            !processed.contains(r#""siteProductionDomain": "origin.example.com""#),
            "should not leave the origin host in rewritten __NEXT_DATA__ fields. Output: {processed}"
        );
    }

    #[test]
    fn html_processor_rewrites_rsc_stream_payload_with_length_preservation() {
        // RSC payloads (self.__next_f.push) are rewritten via post-processing.
        // The streaming phase skips RSC push scripts, and the HTML post-processor handles them
        // at end-of-document to correctly handle cross-script T-chunks.
        let html = r#"<html><body>
            <script>self.__next_f.push([1,"prefix {\"inner\":\"value\"} \\\"href\\\":\\\"http://origin.example.com/dashboard\\\", \\\"link\\\":\\\"https://origin.example.com/api-test\\\" suffix"])</script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");

        let final_html = String::from_utf8_lossy(&output);

        // RSC payloads should be rewritten via end-of-document post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC stream payloads should be rewritten to proxy host via post-processing. Output: {final_html}"
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {final_html}"
        );
    }

    #[test]
    fn html_processor_rewrites_rsc_stream_payload_with_chunked_input() {
        // RSC payloads are rewritten via post-processing, even with chunked streaming input
        let html = r#"<html><body>
<script>self.__next_f.push([1,'{"href":"https://origin.example.com/app","url":"http://origin.example.com/api"}'])</script>
        </body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 32,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");

        let final_html = String::from_utf8_lossy(&output);

        // RSC payloads should be rewritten via end-of-document post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC stream payloads should be rewritten to proxy host with chunked input. Output: {final_html}"
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {final_html}"
        );
    }

    #[test]
    fn html_processor_respects_max_combined_payload_bytes() {
        // When the combined payload size exceeds `max_combined_payload_bytes` and the document
        // contains cross-script T-chunks, we skip post-processing to avoid breaking hydration.
        let html = r#"<html><body>
<script>self.__next_f.push([1,"other:data\n1a:T40,partial content"])</script>
<script>self.__next_f.push([1," with https://origin.example.com/page goes here"])</script>
</body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                    "max_combined_payload_bytes": 1,
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");

        let final_html = String::from_utf8_lossy(&output);

        assert!(
            final_html.contains("https://origin.example.com/page"),
            "Origin URL should remain when rewrite is skipped due to size limit. Output: {final_html}"
        );
        assert!(
            !final_html.contains("test.example.com"),
            "Proxy host should not be introduced when rewrite is skipped. Output: {final_html}"
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {final_html}"
        );
    }

    #[test]
    fn register_respects_enabled_flag() {
        let settings = create_test_settings();
        let registration = register(&settings).expect("should evaluate registration");

        assert!(
            registration.is_none(),
            "should skip registration when integration is disabled"
        );
    }

    #[test]
    fn html_processor_rewrites_rsc_payloads_with_length_preservation() {
        // RSC payloads (self.__next_f.push) are rewritten via post-processing.
        // This allows navigation to stay on proxy while correctly handling cross-script T-chunks.

        let html = r#"<html><body>
<script>self.__next_f.push([1,'458:{"ID":879000,"title":"Makes","url":"https://origin.example.com/makes","children":"$45a"}\n442:["$443"]'])</script>
</body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["url"],
                }),
            )
            .expect("should update nextjs config");

        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");
        let final_html = String::from_utf8_lossy(&output);

        // RSC payloads should be rewritten via post-processing
        assert!(
            final_html.contains("test.example.com"),
            "RSC payload URLs should be rewritten to proxy host. Output: {final_html}"
        );

        // Verify the RSC payload structure is preserved
        assert!(
            final_html.contains(r#""ID":879000"#),
            "RSC payload ID should be preserved"
        );
        assert!(
            final_html.contains(r#""title":"Makes""#),
            "RSC payload title should be preserved"
        );
        assert!(
            final_html.contains(r#""children":"$45a""#),
            "RSC payload children reference should be preserved"
        );

        // Verify \n separators are preserved (crucial for RSC parsing)
        assert!(
            final_html.contains(r"\n442:"),
            "RSC record separator \\n should be preserved. Output: {final_html}"
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "RSC placeholder markers should not appear in final HTML. Output: {final_html}"
        );
    }

    #[test]
    fn html_processor_preserves_non_rsc_scripts_with_chunked_streaming() {
        // Regression test: ensure non-RSC scripts are preserved when streamed alongside RSC scripts.
        // With small chunk sizes, scripts get fragmented and the buffering logic must correctly
        // handle non-RSC scripts without corrupting them.
        let html = r#"<html><body>
<script>console.log("hello world");</script>
<script>self.__next_f.push([1,'{"url":"https://origin.example.com/page"}'])</script>
<script>window.analytics = { track: function(e) { console.log(e); } };</script>
</body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        // Use small chunk size to force fragmentation
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 16,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");
        let final_html = String::from_utf8_lossy(&output);

        // Non-RSC scripts should be preserved
        assert!(
            final_html.contains(r#"console.log("hello world");"#),
            "First non-RSC script should be preserved intact. Output: {final_html}"
        );
        assert!(
            final_html.contains("window.analytics"),
            "Third non-RSC script should be preserved. Output: {final_html}"
        );
        assert!(
            final_html.contains("track: function(e)"),
            "Third non-RSC script content should be intact. Output: {final_html}"
        );

        // RSC scripts should be rewritten
        assert!(
            final_html.contains("test.example.com"),
            "RSC URL should be rewritten. Output: {final_html}"
        );
        assert!(
            !final_html.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "No placeholders should remain. Output: {final_html}"
        );
    }

    /// Regression test: with a small chunk size, `lol_html` fragments the
    /// `__NEXT_DATA__` text node across chunks. The rewriter must accumulate
    /// fragments and produce correct output.
    #[test]
    fn small_chunk_next_data_rewrite_survives_fragmentation() {
        // Build a __NEXT_DATA__ payload large enough to cross a 32-byte chunk boundary.
        let html = r#"<html><body><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"href":"https://origin.example.com/reviews","title":"Hello World"}}}</script></body></html>"#;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);

        // Use a very small chunk size to force fragmentation.
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 32,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should process with small chunks");

        let processed = String::from_utf8_lossy(&output);
        assert!(
            processed.contains("test.example.com") && processed.contains("/reviews"),
            "should rewrite fragmented __NEXT_DATA__ href. Got: {processed}"
        );
        assert!(
            !processed.contains("origin.example.com/reviews"),
            "should not contain original origin href. Got: {processed}"
        );
        assert!(
            processed.contains("Hello World"),
            "should preserve non-URL content. Got: {processed}"
        );
    }

    /// Regression test: a fragmented `self.__next_f.push([1, "…"])` RSC script
    /// must still have its origin URLs rewritten through the streaming pipeline.
    #[test]
    fn small_chunk_rsc_push_survives_fragmentation() {
        // Build an RSC push script whose payload contains multiple origin URLs.
        // With chunk_size = 128, this script's text node will be fragmented at
        // chunk boundaries by the streaming input.
        let html = format!(
            r#"<html><body><script>self.__next_f.push([1,"1:{{\"link\":\"https://origin.example.com/a\",\"img\":\"https://origin.example.com/img.png\",\"nested\":{{\"url\":\"https://origin.example.com/deep/path?q=1\",\"extra\":\"{}\"}}}}"])</script></body></html>"#,
            "x".repeat(400), // pad to guarantee chunk-boundary fragmentation
        );

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);

        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 128,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should process fragmented RSC push");
        let processed = String::from_utf8_lossy(&output);

        assert!(
            !processed.contains("origin.example.com"),
            "no origin host should leak through post-processor fallback. Got: {processed}"
        );
        assert!(
            processed.contains("test.example.com/a")
                && processed.contains("test.example.com/img.png")
                && processed.contains("test.example.com/deep/path?q=1"),
            "all origin URLs must be rewritten to proxy host. Got: {processed}"
        );
        assert!(
            !processed.contains(RSC_PAYLOAD_PLACEHOLDER_PREFIX),
            "no placeholder should leak to output. Got: {processed}"
        );
        // Structural integrity: the push call envelope must still be present
        // and the JS string literal must be properly terminated.
        assert!(
            processed.contains(r#"self.__next_f.push([1,""#),
            "push call must survive. Got: {processed}"
        );
        assert!(
            processed.contains(r#""])</script>"#),
            "push call must close properly \u{2014} `\"])` followed by </script>. Got: {processed}"
        );
    }

    /// Build the production HTML processor for the Next.js integration and run
    /// `html` through it at `chunk_size`, returning the streamed output.
    fn stream_nextjs_html(html: &str, chunk_size: usize) -> String {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let processor = create_html_processor(config_from_settings(&settings, &registry));
        let mut pipeline = StreamingPipeline::new(
            PipelineConfig {
                input_compression: Compression::None,
                output_compression: Compression::None,
                chunk_size,
            },
            processor,
        );
        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should stream HTML");
        String::from_utf8(output).expect("should emit UTF-8 HTML")
    }

    /// Regression test: only `self.`/`window.` own `__next_f`. A publisher script
    /// that happens to hold a property of that name must stream through byte for
    /// byte, at any fragmentation.
    #[test]
    fn foreign_next_f_receivers_stream_through_unchanged() {
        for receiver in [
            "myAnalytics",
            "foo.bar",
            "window.myapp",
            "a__next_f_store",
            "myself",
        ] {
            let script =
                format!(r#"{receiver}.__next_f.push([1,"https://origin.example.com/track"])"#);
            let html = format!("<html><body><script>{script}</script></body></html>");
            for chunk_size in [8, 32, 8192] {
                let processed = stream_nextjs_html(&html, chunk_size);
                assert_eq!(
                    processed, html,
                    "`{receiver}.__next_f` is not a Flight receiver and must not be rewritten at chunk size {chunk_size}"
                );
            }
        }
    }

    /// Regression test: a genuine receiver must still be rewritten when the
    /// stream splits it from its `__next_f` identifier.
    #[test]
    fn split_flight_receivers_are_still_rewritten() {
        for receiver in ["self", "window"] {
            let html = format!(
                r#"<html><body><script>{receiver}.__next_f.push([1,"{{\"url\":\"https://origin.example.com/page\"}}"])</script></body></html>"#
            );
            for chunk_size in [8, 32, 8192] {
                let processed = stream_nextjs_html(&html, chunk_size);
                assert!(
                    processed.contains("test.example.com/page")
                        && !processed.contains("origin.example.com/page"),
                    "`{receiver}.__next_f` should be rewritten at chunk size {chunk_size}. Got: {processed}"
                );
            }
        }
    }

    /// Regression test: an escape whose body straddles a multi-byte character
    /// must not panic the T-chunk escape scanner.
    #[test]
    fn malformed_escapes_at_character_boundaries_do_not_panic() {
        for payload in [
            r#"1:T9,\x4ézzzzzzzz"#,
            r#"1:T9,\u12😀zzzzzzzz"#,
            r#"1:T9,\ud83d\u12😀zzzz"#,
        ] {
            let html = format!(
                r#"<html><body><script>self.__next_f.push([1,"{payload}"])</script></body></html>"#
            );
            let processed = stream_nextjs_html(&html, 8192);
            assert!(
                processed.contains(payload),
                "malformed escape payload should stream through unchanged. Got: {processed}"
            );
        }
    }

    /// Regression test: two independent payloads that each fit the configured
    /// limit must both be rewritten, whether they arrive in one source chunk or
    /// separate ones. The limit bounds one script and one unresolved group, not
    /// every placeholder queued during a single parser call.
    #[test]
    fn independent_payloads_do_not_share_the_group_limit() {
        let first = r#"{\"url\":\"https://origin.example.com/first\"}"#;
        let second = r#"{\"url\":\"https://origin.example.com/second\"}"#;
        let html = format!(
            "<html><body><script>self.__next_f.push([1,\"{first}\"])</script>\
             <script>self.__next_f.push([1,\"{second}\"])</script></body></html>"
        );

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "nextjs",
                &json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                    // Fits either script alone, not both payloads together.
                    "max_combined_payload_bytes": 80,
                }),
            )
            .expect("should update nextjs config");
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let processor = create_html_processor(config_from_settings(&settings, &registry));
        let mut pipeline = StreamingPipeline::new(
            PipelineConfig {
                input_compression: Compression::None,
                output_compression: Compression::None,
                chunk_size: 8192,
            },
            processor,
        );
        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should stream HTML");
        let processed = String::from_utf8(output).expect("should emit UTF-8 HTML");

        assert!(
            processed.contains("test.example.com/first")
                && processed.contains("test.example.com/second"),
            "both independent payloads should be rewritten in one parser call. Got: {processed}"
        );
        assert!(
            !processed.contains("origin.example.com"),
            "no origin host should survive. Got: {processed}"
        );
    }

    /// Regression test: Next.js always emits a `self.`/`window.` receiver. A bare
    /// `__next_f.push(...)` has no receiver to verify, and the boundary check
    /// cannot reject it because nothing precedes the identifier, so only the
    /// qualified-receiver requirement keeps it from being claimed.
    #[test]
    fn unqualified_next_f_push_streams_through_unchanged() {
        let html = concat!(
            r#"<html><body><script>__next_f.push([1,"#,
            r#""{\"url\":\"https://origin.example.com/page\"}"])</script></body></html>"#
        );
        for chunk_size in [8, 32, 8192] {
            let processed = stream_nextjs_html(html, chunk_size);
            assert_eq!(
                processed, html,
                "an unqualified `__next_f` receiver should stream through unchanged at chunk size {chunk_size}"
            );
        }
    }
}
