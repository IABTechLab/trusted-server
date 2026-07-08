use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use trusted_server_core::html_processor::{
    create_html_processor, BidInjectionMode, HtmlPostProcessingMode, HtmlProcessorConfig,
};
use trusted_server_core::integrations::IntegrationDocumentState;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::streaming_processor::StreamProcessor as _;

fn make_config() -> HtmlProcessorConfig {
    HtmlProcessorConfig {
        origin_host: "origin.bench.example.com".to_string(),
        request_host: "proxy.bench.example.com".to_string(),
        request_scheme: "https".to_string(),
        integrations: IntegrationRegistry::default(),
        ad_slots_script: None,
        ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
        max_buffered_body_bytes: 16 * 1024 * 1024,
        bid_injection_mode: BidInjectionMode::DirectState,
        post_processing_mode: HtmlPostProcessingMode::Enabled,
        document_state: IntegrationDocumentState::default(),
    }
}

fn make_html(size_kb: usize) -> Vec<u8> {
    let link_block = r#"<a href="https://origin.bench.example.com/page">Link</a>
<img src="https://origin.bench.example.com/img.png">
<div data-ad-unit="/test/banner"><a href="https://origin.bench.example.com/ad">Ad</a></div>
"#;

    let body_content = link_block.repeat((size_kb * 1024) / link_block.len() + 1);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Benchmark Page</title>
</head>
<body>
{body_content}
</body>
</html>"#
    )
    .into_bytes()
}

fn bench_html_processor(c: &mut Criterion) {
    let mut group = c.benchmark_group("html_processor");

    for size_kb in [10usize, 100] {
        let html = make_html(size_kb);

        group.bench_with_input(
            BenchmarkId::new("process_chunk", format!("{size_kb}kb")),
            &html,
            |b, html| {
                b.iter(|| {
                    let config = make_config();
                    let mut processor = create_html_processor(config);
                    processor
                        .process_chunk(black_box(html.as_slice()), true)
                        .expect("should process HTML")
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_html_processor);
criterion_main!(benches);
