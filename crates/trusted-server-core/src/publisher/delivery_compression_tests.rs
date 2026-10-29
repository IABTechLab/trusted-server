#[derive(Clone, Copy)]
enum Finish {
    Inline,
    Replace,
    Unchanged,
}

async fn run_delivery(
    settings: &Arc<Settings>,
    services: &RuntimeServices,
    request: Request<EdgeBody>,
    finalizer: Finalizer,
    capability: DeliveryCompression,
    finish: Finish,
    stream_origin: bool,
) -> Response<EdgeBody> {
    let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());
    orchestrator.register_provider(Arc::new(WinningBidProvider {
        price: STUB_BID_CPM,
    }));
    let orchestrator = Arc::new(orchestrator);
    let registry = match finish {
        Finish::Inline => {
            IntegrationRegistry::new(settings).expect("should create integration registry")
        }
        Finish::Replace => IntegrationRegistry::from_html_stream_processors(vec![Arc::new(
            super::super::TerminalTestFactory,
        )]),
        Finish::Unchanged => IntegrationRegistry::from_html_stream_processors(vec![Arc::new(
            super::super::CountingTerminalFactory(Arc::new(AtomicUsize::new(0))),
        )]),
    };
    let consent = crate::consent::ConsentContext {
        jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
        ..Default::default()
    };
    let mut ec_context = EcContext::new_for_test(None, consent);
    let method = request.method().clone();
    let mut publisher = handle_publisher_request(
        settings,
        services,
        None,
        &mut ec_context,
        AuctionDispatch {
            integration_registry: &registry,
            orchestrator: &orchestrator,
            slots: &[article_slot()],
            registry: None,
        },
        request,
        PublisherAdapterOptions {
            cache_header: EdgeCacheHeader::SurrogateControl,
            delivery_compression: capability,
        },
    )
    .await
    .expect("should proxy delivery request");
    // Reuse the HTTP fixture's response headers, then exercise both live input
    // forms. The fixture's streaming-response helper cannot queue headers.
    if stream_origin && let PublisherResponse::Stream { body, .. } = &mut publisher {
        let bytes = std::mem::replace(body, EdgeBody::empty())
            .into_bytes()
            .expect("should materialize fixture body");
        let chunks = bytes
            .chunks(11)
            .map(bytes::Bytes::copy_from_slice)
            .collect::<Vec<_>>();
        *body = EdgeBody::stream(futures::stream::iter(chunks));
    }
    match finalizer {
        Finalizer::Streaming => publisher_response_into_streaming_response(
            publisher,
            &method,
            Arc::clone(settings),
            &registry,
            orchestrator,
            services.clone(),
        )
        .await
        .expect("should finalize lazy delivery"),
        Finalizer::Buffered => buffer_publisher_response_async(
            publisher,
            &method,
            settings,
            &registry,
            &orchestrator,
            services,
        )
        .await
        .expect("should finalize buffered delivery"),
    }
}

fn reader(encoding: &str) -> Request<EdgeBody> {
    let mut request = navigation_request();
    request.headers_mut().insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_str(encoding).expect("should build reader encoding"),
    );
    request
}

fn assert_offloaded(response: &Response<EdgeBody>) {
    assert_eq!(
        response
            .headers()
            .get("x-compress-hint")
            .and_then(|v| v.to_str().ok()),
        Some("on"),
        "should request platform compression"
    );
    assert!(
        !response.headers().contains_key(header::CONTENT_ENCODING),
        "should emit identity"
    );
    assert!(
        response
            .headers()
            .get_all(header::VARY)
            .iter()
            .any(|v| v.to_str().is_ok_and(|v| v
                .split(',')
                .any(|v| v.trim() == "*" || v.trim().eq_ignore_ascii_case("accept-encoding")))),
        "should retain encoding variation"
    );
    assert!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("private")),
        "should remain private"
    );
}

const ARTICLE: &[u8] = b"<html><head></head><body>Example article</body></html>";

fn queue_encoded(stub: &StubHttpClient, compression: Compression) {
    let encoding = match compression {
        Compression::None => "identity",
        Compression::Gzip => "gzip",
        Compression::Deflate => "deflate",
        Compression::Brotli => "br",
    };
    let origin = encode_complete_body(ARTICLE.to_vec(), compression).expect("should encode origin");
    let length = origin.len().to_string();
    stub.push_response_with_headers(
        200,
        origin,
        vec![
            ("content-type", "text/html"),
            ("content-encoding", encoding),
            ("content-length", &length),
            ("cache-control", "public, max-age=300"),
        ],
    );
}

#[tokio::test]
async fn delivery_offload_decodes_origin_without_guest_recompression() {
    for finalizer in [Finalizer::Streaming, Finalizer::Buffered] {
        for stream_origin in [false, true] {
            for finish in [Finish::Inline, Finish::Replace, Finish::Unchanged] {
                for bidding in [false, true] {
                    let settings =
                        Arc::new(crate::publisher::allow_eof_test_settings(if bidding {
                            settings_with_bidder("inline")
                        } else {
                            settings_with_mode("inline")
                        }));
                    let mut expected = None;
                    for compression in [
                        Compression::None,
                        Compression::Gzip,
                        Compression::Deflate,
                        Compression::Brotli,
                    ] {
                        let stub = Arc::new(StubHttpClient::new());
                        let services =
                            services(Arc::clone(&stub), Arc::new(MemoryTemplateCache::default()));
                        if bidding {
                            queue_bid_response(&stub);
                        }
                        queue_encoded(&stub, compression);
                        let response = run_delivery(
                            &settings,
                            &services,
                            reader("br, gzip, deflate"),
                            finalizer,
                            DeliveryCompression::Dynamic,
                            finish,
                            stream_origin,
                        )
                        .await;
                        assert_offloaded(&response);
                        assert_eq!(
                            response.body().is_stream(),
                            matches!(finalizer, Finalizer::Streaming),
                            "should preserve finalizer laziness"
                        );
                        let length = response.headers().get(header::CONTENT_LENGTH).map(|v| {
                            v.to_str()
                                .expect("should read length")
                                .parse::<usize>()
                                .expect("should parse length")
                        });
                        let body = body_of(response).await;
                        if let Some(length) = length {
                            assert_eq!(length, body.len(), "should recompute identity length");
                        }
                        let html = std::str::from_utf8(&body).expect("should emit identity HTML");
                        assert!(html.contains("Example article"), "should preserve article");
                        assert_eq!(
                            html.ends_with("<!--example-finalized-->"),
                            matches!(finish, Finish::Replace),
                            "should retain EOF replacement or fallback"
                        );
                        if bidding {
                            assert!(html.contains("3.50"), "should retain auction tail");
                        }
                        if let Some(expected) = &expected {
                            assert_eq!(
                                &body, expected,
                                "should produce identical HTML for every origin coding"
                            );
                        } else {
                            expected = Some(body);
                        }
                        let headers = stub.recorded_request_headers();
                        assert_eq!(
                            headers
                                .last()
                                .expect("should record origin")
                                .iter()
                                .find(|(name, _)| name.eq_ignore_ascii_case("accept-encoding"))
                                .map(|(_, value)| value.as_str()),
                            Some("gzip, deflate, br"),
                            "should preserve existing origin negotiation"
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn delivery_offload_template_miss_hit_fallback_and_ineligible_reader() {
    for finalizer in [Finalizer::Streaming, Finalizer::Buffered] {
        for fail_assembly in [false, true] {
            let settings = Arc::new(settings_with_mode("esi"));
            let stub = Arc::new(StubHttpClient::new());
            let cache = Arc::new(MemoryTemplateCache::default());
            let assembler = Arc::new(RecordingTemplateAssembler::default());
            assembler.fail.store(fail_assembly, Ordering::Relaxed);
            assembler.require_store_before_call(Arc::clone(&cache.insert_completed));
            let services =
                services_with_assembler(Arc::clone(&stub), Arc::clone(&cache), assembler);
            queue_encoded(&stub, Compression::Gzip);
            for state in ["miss-stored", "hit"] {
                let response = run_delivery(
                    &settings,
                    &services,
                    reader("br, gzip"),
                    finalizer,
                    DeliveryCompression::Dynamic,
                    Finish::Inline,
                    false,
                )
                .await;
                assert_offloaded(&response);
                assert_eq!(
                    response
                        .headers()
                        .get(HEADER_X_TS_TEMPLATE_CACHE)
                        .and_then(|v| v.to_str().ok()),
                    Some(state),
                    "should retain template state"
                );
                let assembly = if state == "hit" {
                    "byte-seam"
                } else if fail_assembly {
                    "byte-seam-fallback"
                } else {
                    "esi-parser"
                };
                assert_eq!(
                    response
                        .headers()
                        .get(HEADER_X_TS_ASSEMBLY)
                        .and_then(|v| v.to_str().ok()),
                    Some(assembly),
                    "should retain assembly and fallback path"
                );
                let html = String::from_utf8(body_of(response).await)
                    .expect("should deliver assembled identity");
                assert!(
                    html.contains("Example article"),
                    "should retain template content"
                );
                assert!(
                    !html.contains(AD_ASSEMBLY_SEAM),
                    "should assemble before delivery"
                );
                let entries = cache
                    .entries
                    .lock()
                    .expect("should inspect stored template");
                let entry = entries.values().next().expect("should store template");
                assert_eq!(
                    entry.metadata.content_encoding, "identity",
                    "should store neutral identity"
                );
                assert!(
                    std::str::from_utf8(&entry.body)
                        .expect("should store HTML")
                        .contains(AD_ASSEMBLY_SEAM),
                    "should store before assembly"
                );
            }
            let mut request = reader("gzip");
            request.headers_mut().insert(
                "sec-purpose",
                HeaderValue::from_static("prefetch;prerender"),
            );
            let response = run_delivery(
                &settings,
                &services,
                request,
                finalizer,
                DeliveryCompression::Dynamic,
                Finish::Inline,
                false,
            )
            .await;
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_X_TS_TEMPLATE_CACHE)
                    .and_then(|v| v.to_str().ok()),
                Some("hit"),
                "should reuse template for ineligible reader"
            );
            assert!(
                !response.headers().contains_key("x-compress-hint"),
                "should use this reader's eligibility"
            );
            assert_eq!(
                response_compression(&response),
                Compression::Gzip,
                "should preserve reader encoding"
            );
            let html = gzip_decode(&body_of(response).await);
            assert!(
                std::str::from_utf8(&html)
                    .expect("should decode template")
                    .contains("Example article"),
                "should assemble ineligible reader"
            );
            assert_eq!(
                stub.recorded_request_uris().len(),
                1,
                "should fetch only on miss"
            );
        }
    }
}

#[tokio::test]
async fn delivery_offload_negotiation_and_capability_are_independent() {
    for capability in [
        DeliveryCompression::Unavailable,
        DeliveryCompression::Dynamic,
    ] {
        for (encoding, supported) in [
            (None, true),
            (Some(""), true),
            (Some("identity"), true),
            (Some("gzip, identity;q=0"), true),
            (Some("br, identity;q=0"), true),
            (Some("deflate, identity;q=0"), false),
            (Some("*;q=0"), false),
            (Some("zstd, identity;q=0"), false),
            (Some("gzip;q=broken"), false),
        ] {
            let settings = Arc::new(settings_with_mode("inline"));
            let stub = Arc::new(StubHttpClient::new());
            let services = services(Arc::clone(&stub), Arc::new(MemoryTemplateCache::default()));
            queue_encoded(&stub, Compression::Deflate);
            let request = encoding.map_or_else(navigation_request, reader);
            let response = run_delivery(
                &settings,
                &services,
                request,
                Finalizer::Buffered,
                capability,
                Finish::Inline,
                false,
            )
            .await;
            let offloaded = capability == DeliveryCompression::Dynamic && supported;
            assert_eq!(
                response.headers().contains_key("x-compress-hint"),
                offloaded,
                "should respect capability and negotiation {encoding:?}"
            );
            assert_eq!(
                response_compression(&response),
                if offloaded {
                    Compression::None
                } else {
                    Compression::Deflate
                },
                "should preserve existing fallback encoding"
            );
            let body = body_of(response).await;
            let body = if offloaded {
                body
            } else {
                let mut decoded = Vec::new();
                flate2::read::ZlibDecoder::new(&body[..])
                    .read_to_end(&mut decoded)
                    .expect("should decode deflate fallback");
                decoded
            };
            assert!(
                std::str::from_utf8(&body)
                    .expect("should emit HTML")
                    .contains("Example article"),
                "should retain body"
            );
        }
    }
}

#[tokio::test]
async fn delivery_offload_excludes_unmodified_and_non_ssat_responses() {
    for (status, content_type, encoding, eligible, method, range) in [
        (200, "text/html", "gzip", false, Method::GET, false),
        (200, "text/css", "gzip", true, Method::GET, false),
        (200, "text/html", "zstd", true, Method::GET, false),
        (302, "text/html", "gzip", true, Method::GET, false),
        (404, "text/html", "gzip", true, Method::GET, false),
        (500, "text/html", "gzip", true, Method::GET, false),
        (200, "text/html", "gzip", true, Method::HEAD, false),
        (204, "text/html", "gzip", true, Method::GET, false),
        (205, "text/html", "gzip", true, Method::GET, false),
        (304, "text/html", "gzip", true, Method::GET, false),
        (206, "text/html", "gzip", true, Method::GET, true),
        (200, "text/html", "gzip", true, Method::GET, true),
    ] {
        let mut settings = settings_with_mode("inline");
        settings.auction.enabled = eligible;
        let settings = Arc::new(settings);
        let stub = Arc::new(StubHttpClient::new());
        let services = services(Arc::clone(&stub), Arc::new(MemoryTemplateCache::default()));
        let bytes = if encoding == "zstd" {
            ARTICLE.to_vec()
        } else {
            gzip_encode(ARTICLE)
        };
        let mut headers = vec![
            ("content-type", content_type),
            ("content-encoding", encoding),
        ];
        if range {
            headers.push(("content-range", "bytes 0-9/100"));
        }
        stub.push_response_with_headers(status, bytes, headers);
        let mut request = reader("br, gzip");
        *request.method_mut() = method.clone();
        let response = run_delivery(
            &settings,
            &services,
            request,
            Finalizer::Streaming,
            DeliveryCompression::Dynamic,
            Finish::Inline,
            false,
        )
        .await;
        assert!(
            !response.headers().contains_key("x-compress-hint"),
            "should not offload {method} {status} {content_type} {encoding}"
        );
        let carries_body = response_carries_body(&method, response.status());
        let compression = response_compression(&response);
        let body = body_of(response).await;
        assert_eq!(
            !body.is_empty(),
            carries_body,
            "should preserve bodyless semantics"
        );
        if carries_body && compression == Compression::Gzip {
            assert!(
                !gzip_decode(&body).is_empty(),
                "should retain in-guest gzip"
            );
        }
    }
}

#[test]
fn delivery_offload_emits_identity_prefix_before_origin_eof_and_auction() {
    let mut params = super::super::html_stream_params(
        "gzip",
        Some(DispatchedAuction::empty_for_test(
            test_auction_request(),
            500,
        )),
    );
    params.output_compression = Some(Compression::None);
    let body = super::super::streaming_finalize_response(
        params,
        super::super::origin_chunk_then_pending(bytes::Bytes::from(gzip_encode(ARTICLE))),
    );
    let first = super::super::first_lazy_body_chunk(body);
    let html = std::str::from_utf8(&first).expect("should emit identity prefix");
    assert!(
        html.contains("Example article"),
        "should emit article before origin EOF"
    );
    assert!(
        !html.contains("var b=JSON.parse("),
        "should not await auction before prefix"
    );
}

#[tokio::test]
async fn delivery_offload_preserves_vary_and_reasserts_terminal_headers() {
    for vary in [
        vec!["Origin", "Accept-Language"],
        vec!["Origin, aCcEpT-EnCoDiNg", "Accept-Language"],
        vec!["*", "Origin"],
    ] {
        let settings = Arc::new(settings_with_mode("inline"));
        let stub = Arc::new(StubHttpClient::new());
        let services = services(Arc::clone(&stub), Arc::new(MemoryTemplateCache::default()));
        let mut headers = vec![
            ("content-type", "text/html"),
            ("cache-control", "private, no-store"),
            ("content-length", "9999"),
        ];
        for value in &vary {
            headers.push(("vary", *value));
        }
        stub.push_response_with_headers(200, ARTICLE.to_vec(), headers);
        let mut response = run_delivery(
            &settings,
            &services,
            reader("gzip"),
            Finalizer::Streaming,
            DeliveryCompression::Dynamic,
            Finish::Inline,
            false,
        )
        .await;
        assert_offloaded(&response);
        let actual = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .map(|v| v.to_str().expect("should read vary"))
            .collect::<Vec<_>>();
        assert_eq!(
            &actual[..vary.len()],
            &vary,
            "should preserve all Vary fields"
        );
        assert_eq!(
            actual.len(),
            vary.len() + usize::from(vary.len() == 2 && vary[0] == "Origin"),
            "should not duplicate wildcard or existing variation"
        );
        assert!(
            response.headers()[header::CACHE_CONTROL]
                .to_str()
                .expect("should read privacy")
                .contains("no-store"),
            "should preserve stronger privacy"
        );
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("9999"));
        response
            .headers_mut()
            .insert("x-compress-hint", HeaderValue::from_static("off"));
        enforce_delivery_compression(&mut response);
        assert_offloaded(&response);
        assert!(
            !response.headers().contains_key(header::CONTENT_LENGTH),
            "should discard late stale framing"
        );
        assert!(
            std::str::from_utf8(&body_of(response).await)
                .expect("should keep identity")
                .contains("Example article"),
            "should keep final body and headers aligned"
        );
    }
}
