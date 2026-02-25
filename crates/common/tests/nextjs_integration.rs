//! Fixture-driven integration tests for Next.js RSC URL rewriting.
//!
//! These tests exercise the full streaming pipeline against realistic HTML
//! fixtures captured from a Next.js App Router application. Each fixture is
//! processed with multiple chunk sizes to exercise both the placeholder path
//! (unfragmented scripts) and the fallback re-parse path (fragmented scripts).

#![allow(clippy::print_stdout)]

use std::io::Cursor;

use trusted_server_common::html_processor::{create_html_processor, HtmlProcessorConfig};
use trusted_server_common::integrations::IntegrationRegistry;
use trusted_server_common::settings::Settings;
use trusted_server_common::streaming_processor::{
    Compression, PipelineConfig, StreamProcessor, StreamingPipeline,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const FIXTURE_SIMPLE: &str =
    include_str!("../src/integrations/nextjs/fixtures/app-router-simple.html");
const FIXTURE_TCHUNK: &str =
    include_str!("../src/integrations/nextjs/fixtures/app-router-tchunk.html");
const FIXTURE_LARGE: &str =
    include_str!("../src/integrations/nextjs/fixtures/app-router-large.html");
const FIXTURE_NON_RSC: &str = include_str!("../src/integrations/nextjs/fixtures/non-rsc-page.html");

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

const ORIGIN_HOST: &str = "origin.example.com";
const PROXY_HOST: &str = "proxy.example.com";
const SCHEME: &str = "https";

/// Small chunk size to maximize script fragmentation and exercise cross-chunk state handling.
/// With 32-64 byte chunks, `lol_html` frequently fragments script text nodes, forcing the
/// fallback re-parse path for RSC placeholder substitution.
const CHUNK_SIZE_SMALL: usize = 32;

/// Medium chunk size - typical for network reads. Balances between fragmentation
/// and realistic streaming behavior.
const CHUNK_SIZE_MEDIUM: usize = 256;

/// Large chunk size - can fit small to medium HTML documents in a single chunk.
/// Tests the placeholder path (unfragmented scripts) vs fallback re-parse path.
const CHUNK_SIZE_LARGE: usize = 8192;

fn create_nextjs_settings() -> Settings {
    let toml = r#"
        [[handlers]]
        path = "^/secure"
        username = "user"
        password = "pass"

        [publisher]
        domain = "test-publisher.com"
        cookie_domain = ".test-publisher.com"
        origin_backend = "publisher_origin"
        origin_url = "https://origin.example.com"
        proxy_secret = "unit-test-proxy-secret"

        [integrations.prebid]
        enabled = false

        [integrations.nextjs]
        enabled = true
        rewrite_attributes = ["href", "link", "url"]

        [synthetic]
        counter_store = "test-counter-store"
        opid_store = "test-opid-store"
        secret_key = "test-secret-key"
        template = "{{client_ip}}:{{user_agent}}"

        [request_signing]
        config_store_id = "test-config-store-id"
        secret_store_id = "test-secret-store-id"

        [[backends]]
        name = "publisher_origin"
        target = "https://origin.example.com"
    "#;
    Settings::from_toml(toml).expect("test settings should parse")
}

fn create_non_rsc_settings() -> Settings {
    let toml = r#"
        [[handlers]]
        path = "^/secure"
        username = "user"
        password = "pass"

        [publisher]
        domain = "test-publisher.com"
        cookie_domain = ".test-publisher.com"
        origin_backend = "publisher_origin"
        origin_url = "https://origin.example.com"
        proxy_secret = "unit-test-proxy-secret"

        [integrations.prebid]
        enabled = false

        [integrations.nextjs]
        enabled = false

        [synthetic]
        counter_store = "test-counter-store"
        opid_store = "test-opid-store"
        secret_key = "test-secret-key"
        template = "{{client_ip}}:{{user_agent}}"

        [request_signing]
        config_store_id = "test-config-store-id"
        secret_store_id = "test-secret-store-id"

        [[backends]]
        name = "publisher_origin"
        target = "https://origin.example.com"
    "#;
    Settings::from_toml(toml).expect("test settings should parse")
}

struct FixtureTestResult {
    output: String,
    intermediate_bytes: usize,
    final_bytes: usize,
}

impl FixtureTestResult {
    fn total_bytes(&self) -> usize {
        self.intermediate_bytes + self.final_bytes
    }

    fn streaming_ratio(&self) -> f64 {
        let total = self.total_bytes();
        if total == 0 {
            0.0
        } else {
            self.intermediate_bytes as f64 / total as f64
        }
    }
}

/// Process a fixture through the full streaming pipeline and return results.
fn run_pipeline_test(fixture: &str, chunk_size: usize, settings: &Settings) -> FixtureTestResult {
    let registry = IntegrationRegistry::new(settings).expect("should create registry");
    let config =
        HtmlProcessorConfig::from_settings(settings, &registry, ORIGIN_HOST, PROXY_HOST, SCHEME);
    let processor = create_html_processor(config);

    let pipeline_config = PipelineConfig {
        input_compression: Compression::None,
        output_compression: Compression::None,
        chunk_size,
    };
    let mut pipeline = StreamingPipeline::new(pipeline_config, processor);
    let mut output = Vec::new();
    pipeline
        .process(Cursor::new(fixture.as_bytes()), &mut output)
        .expect("pipeline should process fixture");

    let output_str = String::from_utf8(output).expect("output should be valid UTF-8");

    // StreamingPipeline doesn't expose per-chunk metrics, so we use a
    // chunk-level processor to measure streaming behavior.
    FixtureTestResult {
        output: output_str,
        intermediate_bytes: 0,
        final_bytes: 0,
    }
}

/// Process a fixture chunk-by-chunk using the raw `StreamProcessor` interface
/// to measure streaming behavior.
fn run_chunked_test(fixture: &str, chunk_size: usize, settings: &Settings) -> FixtureTestResult {
    let registry = IntegrationRegistry::new(settings).expect("should create registry");
    let config =
        HtmlProcessorConfig::from_settings(settings, &registry, ORIGIN_HOST, PROXY_HOST, SCHEME);
    let mut processor = create_html_processor(config);

    let bytes = fixture.as_bytes();
    let chunks: Vec<&[u8]> = bytes.chunks(chunk_size).collect();
    let last_idx = chunks.len().saturating_sub(1);

    let mut intermediate_bytes = 0usize;
    let mut final_bytes = 0usize;
    let mut full_output = Vec::new();

    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i == last_idx;
        let result = processor
            .process_chunk(chunk, is_last)
            .expect("should process chunk");

        if is_last {
            final_bytes = result.len();
        } else {
            intermediate_bytes += result.len();
        }
        full_output.extend_from_slice(&result);
    }

    let output = String::from_utf8(full_output).expect("output should be valid UTF-8");

    FixtureTestResult {
        output,
        intermediate_bytes,
        final_bytes,
    }
}

/// Shared correctness assertions for RSC fixtures.
fn assert_rsc_correctness(result: &FixtureTestResult, fixture_name: &str) {
    // All origin URLs should be rewritten
    assert!(
        result.output.contains(PROXY_HOST),
        "[{fixture_name}] Output should contain proxy host. Got:\n{}",
        &result.output[..result.output.len().min(500)]
    );

    // No RSC placeholder markers should leak
    assert!(
        !result.output.contains("__ts_rsc_payload_"),
        "[{fixture_name}] No RSC placeholder markers should appear in output"
    );

    // HTML structure should be intact
    assert!(
        result.output.contains("<html"),
        "[{fixture_name}] HTML should be structurally intact"
    );
    assert!(
        result.output.contains("</html>"),
        "[{fixture_name}] HTML closing tag should be present"
    );

    // RSC scripts should still be present (even if content is rewritten)
    assert!(
        result.output.contains("__next_f"),
        "[{fixture_name}] RSC scripts should be preserved in output"
    );
}

fn assert_non_rsc_correctness(result: &FixtureTestResult, fixture_name: &str) {
    assert!(
        result.output.contains(PROXY_HOST),
        "[{fixture_name}] Output should contain proxy host"
    );
    assert!(
        result.output.contains("<html"),
        "[{fixture_name}] HTML should be structurally intact"
    );
    // Non-RSC pages should NOT have __next_f scripts
    assert!(
        !result.output.contains("__next_f"),
        "[{fixture_name}] Non-RSC page should not have RSC scripts"
    );
}

// ===========================================================================
// Tests: App Router Simple (unfragmented RSC scripts)
// ===========================================================================

#[test]
fn app_router_simple_pipeline_large_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_SIMPLE, CHUNK_SIZE_LARGE, &settings);
    assert_rsc_correctness(&result, "simple/8192");
}

#[test]
fn app_router_simple_pipeline_medium_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_SIMPLE, 64, &settings);
    assert_rsc_correctness(&result, "simple/64");
}

#[test]
fn app_router_simple_pipeline_small_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_SIMPLE, CHUNK_SIZE_SMALL, &settings);
    assert_rsc_correctness(&result, "simple/32");
}

#[test]
fn app_router_simple_streaming_behavior() {
    let settings = create_nextjs_settings();

    // Large chunks: RSC scripts fit in single lol_html text nodes → placeholder path
    let large = run_chunked_test(FIXTURE_SIMPLE, CHUNK_SIZE_LARGE, &settings);
    assert_rsc_correctness(&large, "simple/streaming/8192");

    // Small chunks: scripts get fragmented → fallback re-parse path
    let small = run_chunked_test(FIXTURE_SIMPLE, CHUNK_SIZE_SMALL, &settings);
    assert_rsc_correctness(&small, "simple/streaming/32");

    println!(
        "app-router-simple streaming ratios: large={:.1}%, small={:.1}%",
        large.streaming_ratio() * 100.0,
        small.streaming_ratio() * 100.0
    );
}

// ===========================================================================
// Tests: App Router T-chunks (escaped HTML in RSC payloads)
// ===========================================================================

#[test]
fn app_router_tchunk_pipeline_large_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_TCHUNK, CHUNK_SIZE_LARGE, &settings);
    assert_rsc_correctness(&result, "tchunk/8192");
}

#[test]
fn app_router_tchunk_pipeline_small_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_TCHUNK, CHUNK_SIZE_SMALL, &settings);
    assert_rsc_correctness(&result, "tchunk/32");
}

#[test]
fn app_router_tchunk_streaming_behavior() {
    let settings = create_nextjs_settings();

    let large = run_chunked_test(FIXTURE_TCHUNK, CHUNK_SIZE_LARGE, &settings);
    assert_rsc_correctness(&large, "tchunk/streaming/8192");

    let small = run_chunked_test(FIXTURE_TCHUNK, CHUNK_SIZE_SMALL, &settings);
    assert_rsc_correctness(&small, "tchunk/streaming/32");

    println!(
        "app-router-tchunk streaming ratios: large={:.1}%, small={:.1}%",
        large.streaming_ratio() * 100.0,
        small.streaming_ratio() * 100.0
    );
}

// ===========================================================================
// Tests: App Router Large (multiple RSC scripts, potential cross-script)
// ===========================================================================

#[test]
fn app_router_large_pipeline_large_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_LARGE, CHUNK_SIZE_LARGE, &settings);
    assert_rsc_correctness(&result, "large/8192");
}

#[test]
fn app_router_large_pipeline_medium_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_LARGE, 64, &settings);
    assert_rsc_correctness(&result, "large/64");
}

#[test]
fn app_router_large_pipeline_small_chunks() {
    let settings = create_nextjs_settings();
    let result = run_pipeline_test(FIXTURE_LARGE, CHUNK_SIZE_SMALL, &settings);
    assert_rsc_correctness(&result, "large/32");
}

#[test]
fn app_router_large_streaming_behavior() {
    let settings = create_nextjs_settings();

    let large = run_chunked_test(FIXTURE_LARGE, CHUNK_SIZE_LARGE, &settings);
    assert_rsc_correctness(&large, "large/streaming/8192");

    let small = run_chunked_test(FIXTURE_LARGE, CHUNK_SIZE_SMALL, &settings);
    assert_rsc_correctness(&small, "large/streaming/32");

    println!(
        "app-router-large streaming ratios: large={:.1}%, small={:.1}%",
        large.streaming_ratio() * 100.0,
        small.streaming_ratio() * 100.0
    );
}

// ===========================================================================
// Tests: Non-RSC page (no __next_f scripts)
// ===========================================================================

#[test]
fn non_rsc_page_pipeline() {
    let settings = create_non_rsc_settings();
    let result = run_pipeline_test(FIXTURE_NON_RSC, 64, &settings);
    assert_non_rsc_correctness(&result, "non-rsc/pipeline");

    // No origin URLs should remain (only checking href/src attributes)
    assert!(
        !result.output.contains("origin.example.com"),
        "[non-rsc] No origin URLs should remain in output"
    );
}

#[test]
fn non_rsc_page_streams_incrementally() {
    // Without Next.js enabled, non-RSC pages should stream fully
    let settings = create_non_rsc_settings();
    let result = run_chunked_test(FIXTURE_NON_RSC, 64, &settings);
    assert_non_rsc_correctness(&result, "non-rsc/streaming");

    assert!(
        result.intermediate_bytes > 0,
        "Non-RSC pages should stream incrementally (got 0 intermediate bytes). \
         Final bytes: {}",
        result.final_bytes
    );

    println!(
        "non-rsc streaming ratio: {:.1}%",
        result.streaming_ratio() * 100.0
    );
}

#[test]
fn non_rsc_page_streams_with_nextjs_enabled() {
    // Even with Next.js enabled, non-RSC pages with unfragmented scripts should
    // stream because the lazy accumulation fix only triggers for RSC content.
    let settings = create_nextjs_settings();

    // Use a chunk size that produces multiple chunks for the ~1KB fixture,
    // but is large enough that the small analytics scripts (~30 bytes each)
    // won't be fragmented by lol_html.
    let result = run_chunked_test(FIXTURE_NON_RSC, CHUNK_SIZE_MEDIUM, &settings);
    assert_non_rsc_correctness(&result, "non-rsc/nextjs-enabled/256");

    assert!(
        result.intermediate_bytes > 0,
        "Non-RSC pages should stream even when Next.js is enabled \
         (got 0 intermediate bytes). Final bytes: {}",
        result.final_bytes
    );

    println!(
        "non-rsc with nextjs enabled streaming ratio: {:.1}%",
        result.streaming_ratio() * 100.0
    );
}

// ===========================================================================
// Tests: URL rewriting completeness across fixtures
// ===========================================================================

#[test]
fn all_fixtures_rewrite_html_attribute_urls() {
    let settings = create_nextjs_settings();

    for (name, fixture) in [
        ("simple", FIXTURE_SIMPLE),
        ("tchunk", FIXTURE_TCHUNK),
        ("large", FIXTURE_LARGE),
    ] {
        let result = run_pipeline_test(fixture, 8192, &settings);

        // href attributes should be rewritten
        assert!(
            !result.output.contains("href=\"https://origin.example.com"),
            "[{name}] href attributes should be rewritten to proxy host"
        );

        // src attributes should be rewritten
        assert!(
            !result.output.contains("src=\"https://origin.example.com"),
            "[{name}] src attributes should be rewritten to proxy host"
        );
    }
}

// ===========================================================================
// Tests: Real Next.js output (captured from the example app)
// ===========================================================================
// These fixtures are actual HTML responses from a Next.js 15 App Router app,
// not hand-crafted. They exercise the full complexity of real RSC payloads.

const REAL_HOME: &str = include_str!("../src/integrations/nextjs/fixtures/real-nextjs-home.html");
const REAL_ABOUT: &str = include_str!("../src/integrations/nextjs/fixtures/real-nextjs-about.html");
const REAL_BLOG: &str = include_str!("../src/integrations/nextjs/fixtures/real-nextjs-blog.html");

#[test]
fn real_nextjs_home_pipeline() {
    let settings = create_nextjs_settings();
    for chunk_size in [32, 64, 256, 8192] {
        let result = run_pipeline_test(REAL_HOME, chunk_size, &settings);

        assert!(
            result.output.contains(PROXY_HOST),
            "[real-home/chunk={chunk_size}] Output should contain proxy host"
        );
        assert!(
            !result.output.contains("__ts_rsc_payload_"),
            "[real-home/chunk={chunk_size}] No placeholder markers should leak"
        );
        assert!(
            result.output.contains("<html"),
            "[real-home/chunk={chunk_size}] HTML should be intact"
        );
        // RSC payloads should have origin URLs rewritten
        assert!(
            !result.output.contains("href=\"https://origin.example.com"),
            "[real-home/chunk={chunk_size}] href attributes should be rewritten"
        );
    }
}

#[test]
fn real_nextjs_about_pipeline() {
    let settings = create_nextjs_settings();
    for chunk_size in [32, 64, 256, 8192] {
        let result = run_pipeline_test(REAL_ABOUT, chunk_size, &settings);

        assert!(
            result.output.contains(PROXY_HOST),
            "[real-about/chunk={chunk_size}] Output should contain proxy host"
        );
        assert!(
            !result.output.contains("__ts_rsc_payload_"),
            "[real-about/chunk={chunk_size}] No placeholder markers should leak"
        );
        assert!(
            !result.output.contains("href=\"https://origin.example.com"),
            "[real-about/chunk={chunk_size}] href attributes should be rewritten"
        );
    }
}

#[test]
fn real_nextjs_blog_pipeline() {
    let settings = create_nextjs_settings();
    for chunk_size in [32, 64, 256, 8192] {
        let result = run_pipeline_test(REAL_BLOG, chunk_size, &settings);

        assert!(
            result.output.contains(PROXY_HOST),
            "[real-blog/chunk={chunk_size}] Output should contain proxy host"
        );
        assert!(
            !result.output.contains("__ts_rsc_payload_"),
            "[real-blog/chunk={chunk_size}] No placeholder markers should leak"
        );
        assert!(
            !result.output.contains("href=\"https://origin.example.com"),
            "[real-blog/chunk={chunk_size}] href attributes should be rewritten"
        );
    }
}

#[test]
fn real_nextjs_rsc_payloads_rewritten() {
    // Verify that origin URLs inside RSC Flight payloads (not just HTML attributes)
    // are rewritten. This is the critical test — RSC payloads contain URLs as JSON
    // strings inside JavaScript, which lol_html can't reach via attribute rewriting.
    let settings = create_nextjs_settings();

    for (name, fixture) in [
        ("real-home", REAL_HOME),
        ("real-about", REAL_ABOUT),
        ("real-blog", REAL_BLOG),
    ] {
        for chunk_size in [32, 64, 256, 8192] {
            let result = run_pipeline_test(fixture, chunk_size, &settings);

            // Count remaining origin URLs in RSC script content
            let rsc_origin_count = result
                .output
                .match_indices("origin.example.com")
                .filter(|(pos, _)| {
                    // Only count occurrences inside <script> content (RSC payloads)
                    let before = &result.output[..*pos];
                    let last_script_open = before.rfind("<script");
                    let last_script_close = before.rfind("</script>");
                    match (last_script_open, last_script_close) {
                        (Some(open), Some(close)) => open > close, // inside a script
                        (Some(_), None) => true,                   // inside first script
                        _ => false,
                    }
                })
                .count();

            println!(
                "[{name}/chunk={chunk_size}] RSC payload origin URLs remaining: {rsc_origin_count}"
            );

            // RSC payloads should be rewritten (origin URLs replaced with proxy URLs)
            assert_eq!(
                rsc_origin_count, 0,
                "[{name}/chunk={chunk_size}] All origin URLs in RSC payloads should be rewritten \
                 to proxy host. Found {rsc_origin_count} remaining."
            );
        }
    }
}

#[test]
fn real_nextjs_streaming_behavior() {
    let settings = create_nextjs_settings();

    for (name, fixture) in [
        ("real-home", REAL_HOME),
        ("real-about", REAL_ABOUT),
        ("real-blog", REAL_BLOG),
    ] {
        // Small chunks to see streaming behavior
        let result = run_chunked_test(fixture, 64, &settings);

        println!(
            "[{name}] streaming: {:.1}% ({} intermediate, {} final)",
            result.streaming_ratio() * 100.0,
            result.intermediate_bytes,
            result.final_bytes
        );

        // Correctness should hold regardless of chunk size
        assert!(
            result.output.contains(PROXY_HOST),
            "[{name}] Output should contain proxy host with 64-byte chunks"
        );
        assert!(
            result.output.contains("<html"),
            "[{name}] HTML should be intact with 64-byte chunks"
        );
    }
}
