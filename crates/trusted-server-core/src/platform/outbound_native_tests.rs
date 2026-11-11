//! Runs the TS collection policy against the actual `EdgeZero` native provider.

use edgezero_core::body::Body;
use edgezero_core::http::HeaderValue;
use futures::{FutureExt as _, StreamExt as _};
use http::Request;
use std::time::Duration;
use tokio::task::LocalSet;

use crate::test_support::native_outbound::{
    Reply, client, closed, gzip_hello, observed, origin, request,
};

use super::*;

async fn ready(request: PlatformPendingRequest) -> PlatformPendingRequest {
    let mut pending = request
        .downcast::<Pending>()
        .expect("should retain a managed exchange");
    tokio::time::timeout(Duration::from_secs(3), async {
        while !pending.pending.is_ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("should publish a complete native response");
    let backend = pending.backend_name.clone();
    PlatformPendingRequest::new(pending).with_backend_name(backend)
}

#[tokio::test(flavor = "current_thread")]
async fn collection_cutoff_bounds_headers_and_full_body_not_just_response_head() {
    LocalSet::new()
        .run_until(async {
            for reply in [Reply::HeldHeaders, Reply::HeldBody, Reply::TrickleBody] {
                let client = client();
                let mut origin = origin(reply).await;
                let pending = client
                    .send_async(request(&origin.url, "slow"))
                    .await
                    .expect("should schedule the request");
                observed(&mut origin).await;
                let start = Instant::now();
                let result = client
                    .select_until(vec![pending], start + Duration::from_millis(80))
                    .await
                    .expect("should enforce the collection cutoff");
                assert!(
                    result.is_none(),
                    "should not treat a response head as a complete buffered response"
                );
                assert!(
                    start.elapsed() < Duration::from_secs(1),
                    "should not wait for the 30-second transport budget"
                );
                closed(&mut origin).await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn completion_wakeup_preserves_a_healthy_sibling_before_the_cutoff() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut slow = origin(Reply::HeldBody).await;
            let slow_request = client
                .send_async(request(&slow.url, "slow"))
                .await
                .expect("should schedule the slow request");
            observed(&mut slow).await;
            let mut fast = origin(Reply::Complete).await;
            let fast_request = client
                .send_async(request(&fast.url, "fast"))
                .await
                .expect("should schedule the fast request");
            let selection = tokio::time::timeout(
                Duration::from_secs(3),
                client.select_until(
                    vec![slow_request, fast_request],
                    Instant::now() + Duration::from_secs(30),
                ),
            )
            .await
            .expect("should wake on completion rather than only the deadline")
            .expect("should select a healthy sibling")
            .expect("should retain the completed sibling");
            assert_eq!(
                selection
                    .ready
                    .expect("should receive the healthy response")
                    .backend_name
                    .as_deref(),
                Some("fast"),
                "should preserve backend correlation"
            );
            assert_eq!(
                selection.remaining.len(),
                1,
                "should retain unfinished siblings before cutoff"
            );
            drop(selection.remaining);
            observed(&mut fast).await;
            closed(&mut slow).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn expired_snapshot_retains_complete_successes_and_failure_but_cancels_unfinished_work() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut slow = origin(Reply::HeldHeaders).await;
            let slow_request = client
                .send_async(request(&slow.url, "slow"))
                .await
                .expect("should schedule unfinished work");
            observed(&mut slow).await;
            let mut first = origin(Reply::Complete).await;
            let first_request = ready(
                client
                    .send_async(request(&first.url, "first"))
                    .await
                    .expect("should start first response"),
            )
            .await;
            let mut second = origin(Reply::Complete).await;
            let second_request = ready(
                client
                    .send_async(request(&second.url, "second"))
                    .await
                    .expect("should start second response"),
            )
            .await;
            let mut failed = origin(Reply::Failed).await;
            let failed_request = ready(
                client
                    .send_async(request(&failed.url, "failed"))
                    .await
                    .expect("should schedule the failing request"),
            )
            .await;
            let deadline = Instant::now();
            let mut requests = vec![slow_request, first_request, second_request, failed_request];
            let mut successful = Vec::new();
            let mut errors = Vec::new();
            while let Some(selection) = client
                .select_until(requests, deadline)
                .await
                .expect("should drain only the completion snapshot")
            {
                match selection.ready {
                    Ok(response) => {
                        successful.push(response.backend_name.expect("should retain backend name"))
                    }
                    Err(_) => errors.push(
                        selection
                            .failed_backend_name
                            .expect("should attribute the failure"),
                    ),
                }
                requests = selection.remaining;
                if requests.is_empty() {
                    break;
                }
            }
            successful.sort();
            assert_eq!(
                successful,
                ["first", "second"],
                "should retain both complete buffered siblings after expiry"
            );
            assert_eq!(
                errors,
                ["failed"],
                "should retain a completed error without poisoning healthy siblings"
            );
            closed(&mut slow).await;
            observed(&mut first).await;
            observed(&mut second).await;
            observed(&mut failed).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_a_collection_borrow_cancels_owned_native_work() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut origin = origin(Reply::HeldBody).await;
            let pending = client
                .send_async(request(&origin.url, "canceled"))
                .await
                .expect("should schedule native work");
            observed(&mut origin).await;
            let mut collection = Box::pin(
                client.select_until(vec![pending], Instant::now() + Duration::from_secs(30)),
            );
            assert!(
                collection.as_mut().now_or_never().is_none(),
                "should retain an unfinished collection"
            );
            drop(collection);
            closed(&mut origin).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_mapping_preserves_host_cookies_and_uses_cache_metadata_not_wire_tags() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut origin = origin(Reply::Complete).await;
            let mut outbound = request(
                &format!("{}?dup=one&dup=two+space&symbol=%2f", origin.url),
                "origin",
            )
            .with_shared_cache("ts-template ts-page-fixture");
            outbound.request.headers_mut().insert(
                header::HOST,
                HeaderValue::from_static("virtual.example.com"),
            );
            let response = client
                .send(outbound)
                .await
                .expect("should send through the single native provider");
            let head = observed(&mut origin).await.to_ascii_lowercase();
            assert!(
                head.starts_with("get /?dup=one&dup=two+space&symbol=%2f http/1.1\r\n"),
                "should retain query ordering, duplicates and percent encoding"
            );
            assert!(
                head.contains("host: virtual.example.com\r\n"),
                "should carry the checked authority override"
            );
            assert!(
                !head.contains("surrogate-key:"),
                "should not send cache override metadata as an origin header"
            );
            assert_eq!(
                response
                    .response
                    .headers()
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .count(),
                2,
                "should retain duplicate Set-Cookie fields"
            );
            assert!(
                !response.response.headers().contains_key("x-private-hop"),
                "should use the shared head finalizer"
            );
            assert_eq!(
                response.response.body().as_bytes(),
                Some(b"hello".as_slice()),
                "should retain the exact body"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_response_limit_is_enforced_before_ts_parsing() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut origin = origin(Reply::Complete).await;
            let pending = client
                .send_async(request(&origin.url, "oversized").with_response_limit(4))
                .await
                .expect("should schedule the bounded exchange");
            let selection = client
                .select_until(vec![pending], Instant::now() + Duration::from_secs(3))
                .await
                .expect("should observe the exchange")
                .expect("should retain the failure");
            assert!(
                selection.ready.is_err(),
                "should reject a five-byte response under a four-byte cap"
            );
            assert_eq!(
                selection.failed_backend_name.as_deref(),
                Some("oversized"),
                "should retain the failing provider identity"
            );
            observed(&mut origin).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fully_completed_unobserved_response_survives_its_transport_deadline() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut origin = origin(Reply::Complete).await;
            let deadline = client
                .time
                .clock()
                .deadline_after(Duration::from_millis(100));
            let pending = client
                .send_async(
                    request(&origin.url, "late-collection")
                        .with_deadline(deadline, Duration::from_millis(100)),
                )
                .await
                .expect("should schedule bounded native work");
            observed(&mut origin).await;
            closed(&mut origin).await;
            client
                .time
                .wait_until(deadline)
                .await
                .expect("should reach the paired deadline");
            let selection = client
                .select_until(vec![pending], deadline.instant())
                .await
                .expect("should inspect the non-driving snapshot")
                .expect("should retain a fully completed response");
            assert_eq!(
                selection
                    .ready
                    .expect("should not replace completed work with a late timeout")
                    .response
                    .body()
                    .as_bytes(),
                Some(b"hello".as_slice()),
                "should retain buffered bytes after delayed collection"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn plain_send_does_not_follow_a_redirect() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut origin = origin(Reply::Redirect).await;
            let response = client
                .send(request(&origin.url, "redirect"))
                .await
                .expect("should return the original redirect");
            assert_eq!(
                response.response.status().as_u16(),
                302,
                "should leave redirect policy to the application"
            );
            assert_eq!(
                response.response.headers()[header::LOCATION],
                "/followed",
                "should retain redirect metadata"
            );
            observed(&mut origin).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn head_content_length_is_metadata_not_a_response_body_allocation() {
    LocalSet::new()
        .run_until(async {
            let client = client();
            let mut origin = origin(Reply::Complete).await;
            let raw = Request::builder()
                .method("HEAD")
                .uri(&origin.url)
                .body(Body::empty())
                .expect("should construct a HEAD request");
            let response = client
                .send(PlatformHttpRequest::new(raw, "head").with_response_limit(4))
                .await
                .expect("should allow five-byte metadata without reading a body");
            assert_eq!(
                response.response.headers()[header::CONTENT_LENGTH],
                "5",
                "should retain valid HEAD metadata"
            );
            assert_eq!(
                response.response.body().as_bytes(),
                Some(b"".as_slice()),
                "should retain a bodyless response"
            );
            observed(&mut origin).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cache_intents_keep_the_typed_policy_on_every_target() {
    let mut client = client();
    for target in [
        AuctionTargetId::Fastly,
        AuctionTargetId::Axum,
        AuctionTargetId::Cloudflare,
        AuctionTargetId::Spin,
    ] {
        client.target = target;
        let tagged = client
            .prepare(
                request("http://origin.example.com/", "origin")
                    .with_shared_cache("ts-page ts-template"),
            )
            .expect("should map shared cache intent");
        assert!(
            matches!(tagged.cache_policy_setting(), OutboundCachePolicy::PlatformDefaultWithSurrogateKey(value) if value.as_bytes() == b"ts-page ts-template"),
            "should retain cache keys as metadata rather than wire fields"
        );
        let invalid = client
            .prepare(request("http://origin.example.com/", "origin").with_shared_cache("bad\nkey"))
            .expect("should map an invalid purge tag to bypass");
        assert_eq!(
            invalid.cache_policy_setting(),
            &OutboundCachePolicy::Bypass,
            "should not cache an unpurgeable object"
        );
        let bypass = client
            .prepare(request("http://origin.example.com/", "origin"))
            .expect("should retain explicit bypass");
        assert_eq!(
            bypass.cache_policy_setting(),
            &OutboundCachePolicy::Bypass,
            "should retain personalized bypass"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn collection_capabilities_and_invalid_handle_correlation_remain_explicit() {
    let mut client = client();
    for target in [
        AuctionTargetId::Fastly,
        AuctionTargetId::Axum,
        AuctionTargetId::Cloudflare,
        AuctionTargetId::Spin,
    ] {
        client.target = target;
        assert_eq!(
            client.supports_concurrent_fanout(),
            matches!(target, AuctionTargetId::Fastly | AuctionTargetId::Axum),
            "should retain each target's auction contract"
        );
        assert_eq!(
            client.supports_streaming_responses(),
            matches!(target, AuctionTargetId::Fastly | AuctionTargetId::Axum),
            "should expose native SDK streaming while CF and Spin stay buffered-only"
        );
        assert_eq!(
            client.supports_pending_streaming_responses(),
            target == AuctionTargetId::Fastly,
            "should reserve direct pending streams for Fastly"
        );
        if !client.supports_concurrent_fanout() {
            let error = client
                .select(vec![
                    PlatformPendingRequest::new(1_u32),
                    PlatformPendingRequest::new(2_u32),
                ])
                .await
                .expect_err("should reject unsupported fanout before inspecting handles");
            assert!(
                format!("{error:?}").contains("multi-provider"),
                "should report the preserved single-provider restriction"
            );
        }
    }
    client.target = AuctionTargetId::Fastly;
    let error = client
        .select(vec![
            PlatformPendingRequest::new(42_u32).with_backend_name("origin-a"),
        ])
        .await
        .expect_err("should reject another client's pending type");
    assert!(
        format!("{error:?}").contains("origin-a"),
        "should retain invalid-handle backend correlation"
    );
    let streamed = PlatformPendingRequest::new(42_u32)
        .with_response_handling(true, edgezero_core::http::Method::GET);
    let error = client
        .select(vec![streamed])
        .await
        .expect_err("should require direct wait for a pending stream");
    assert!(
        format!("{error:?}").contains("use wait"),
        "should reject stream-marked fanout before downcast"
    );
}

#[test]
fn image_metadata_maps_the_closed_transformation_set_without_sdk_types_in_callers() {
    let params = crate::platform::PlatformImageOptimizerParams {
        format: Some("JPEGXL".to_owned()),
        quality: Some(88),
        resize_filter: Some("nearest".to_owned()),
        width: Some(200),
        height: Some(100),
        crop: Some(crate::platform::PlatformImageOptimizerCrop {
            width: 16,
            height: 9,
            mode: None,
            offset_x: Some(25),
            offset_y: Some(75),
        }),
    };
    let output = image_options(
        PlatformImageOptimizerOptions::new("us-west-2", params)
            .with_preserve_query_string_on_origin_request(true),
    )
    .expect("should translate checked TS metadata");
    assert_eq!(
        output.region_setting(),
        Some(&OutboundImageOptimizerRegion::UsWest),
        "should retain region aliases"
    );
    assert_eq!(
        output.format_setting(),
        Some(OutboundImageFormat::JpegXl),
        "should retain format aliases"
    );
    assert_eq!(output.quality_setting(), Some(88), "should retain quality");
    assert_eq!(
        output.resize_filter_setting(),
        Some(OutboundResizeFilter::Nearest),
        "should retain the resize filter"
    );
    assert_eq!(output.width_setting(), Some(200), "should retain width");
    assert_eq!(output.height_setting(), Some(100), "should retain height");
    assert_eq!(
        output.crop_setting(),
        Some((16, 9, OutboundCropFocus::Percent { x: 25, y: 75 })),
        "should retain paired crop offsets"
    );
    assert!(
        output.preserve_origin_query_setting(),
        "should retain explicit query permission"
    );
}

#[test]
fn invalid_image_metadata_is_rejected_before_transport() {
    for params in [
        crate::platform::PlatformImageOptimizerParams {
            quality: Some(101),
            ..Default::default()
        },
        crate::platform::PlatformImageOptimizerParams {
            width: Some(0),
            ..Default::default()
        },
        crate::platform::PlatformImageOptimizerParams {
            crop: Some(crate::platform::PlatformImageOptimizerCrop {
                width: 16,
                height: 9,
                mode: None,
                offset_x: Some(25),
                offset_y: None,
            }),
            ..Default::default()
        },
    ] {
        assert!(
            image_options(PlatformImageOptimizerOptions::new("us_east", params)).is_err(),
            "should reject unsupported or incoherent transformations"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn original_content_coding_and_entity_bytes_survive_fastly_and_standalone_native_policy() {
    LocalSet::new()
        .run_until(async {
            let mut client = client();
            for target in [AuctionTargetId::Axum, AuctionTargetId::Fastly] {
                client.target = target;
                let mut origin = origin(Reply::Gzip).await;
                let response = client
                    .send(request(&origin.url, "coded"))
                    .await
                    .expect("should retain original coded bytes");
                assert_eq!(
                    response.response.headers()[header::CONTENT_ENCODING],
                    "gzip",
                    "should preserve the origin coding header"
                );
                let expected = gzip_hello();
                assert_eq!(
                    response.response.body().as_bytes(),
                    Some(expected.as_slice()),
                    "should not silently switch the transport to decoded payloads"
                );
                assert_eq!(
                    response.response.headers()[header::CONTENT_LENGTH]
                        .to_str()
                        .expect("should read coded length"),
                    expected.len().to_string(),
                    "should retain the original representation length"
                );
                observed(&mut origin).await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn default_streaming_does_not_inherit_a_buffered_allocation_cap() {
    let mut client = client();
    client.target = AuctionTargetId::Fastly;
    let raw = client
        .prepare(request("https://asset.example.com/image", "asset").with_stream_response())
        .expect("should retain an uncapped streaming policy")
        .into_parts();
    assert_eq!(
        raw.response_mode,
        edgezero_core::outbound::ResponseMode::Streamed,
        "should preserve streamed delivery"
    );
    assert_eq!(
        raw.max_encoded_response_bytes, None,
        "should not apply the buffered10MiB cap to streamed entities"
    );
    assert_eq!(
        raw.max_decoded_response_bytes, None,
        "should not invent an additional stream cap"
    );
    let explicit = client
        .prepare(
            request("https://asset.example.com/image", "asset")
                .with_stream_response()
                .with_response_limit(4),
        )
        .expect("should retain explicit caller policy")
        .into_parts();
    assert_eq!(
        explicit.max_encoded_response_bytes,
        Some(4),
        "should retain an explicit stream ceiling"
    );
    assert_eq!(
        explicit.response_mode,
        edgezero_core::outbound::ResponseMode::Streamed,
        "should not replace a stream with a buffer"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tagged_image_requests_fail_exact_default_preflight_without_losing_cache_metadata() {
    let mut client = client();
    client.target = AuctionTargetId::Fastly;
    let make = || {
        let mut request = request("https://asset.example.com/image", "image");
        request.cache_intent = PlatformCacheIntent::Default;
        request.with_image_optimizer(PlatformImageOptimizerOptions::new(
            "us-east-1",
            crate::platform::PlatformImageOptimizerParams::default(),
        ))
    };
    let plain = client
        .prepare(make())
        .expect("should translate a default image request");
    assert_eq!(
        plain.cache_policy_setting(),
        &OutboundCachePolicy::PlatformDefault,
        "should preserve the exact default policy"
    );
    edgezero_core::outbound::validate_for_dispatch(
        &plain,
        edgezero_core::outbound::OutboundSendMode::Send,
    )
    .expect("should admit ordinary default image metadata");
    let tagged = client
        .prepare(make().with_shared_cache("ts-image"))
        .expect("should translate tags as typed metadata");
    assert!(
        edgezero_core::outbound::validate_for_dispatch(
            &tagged,
            edgezero_core::outbound::OutboundSendMode::Send
        )
        .is_err(),
        "should reject tags combined with image optimization before I/O"
    );
    assert!(
        matches!(
            tagged.cache_policy_setting(),
            OutboundCachePolicy::PlatformDefaultWithSurrogateKey(_)
        ),
        "should not flatten tagged-default into ordinary default"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn buffered_begin_cannot_launder_a_response_owned_body_into_upload_bytes() {
    let client = client();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&reads);
    let stream = futures::stream::iter([bytes::Bytes::from_static(b"fictional-private-body")])
        .inspect(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
    let owned = edgezero_core::outbound::OutboundScopeOwner::response_body()
        .attach(edgezero_core::http::Response::new(Body::stream(stream)))
        .expect("should attach the original response owner");
    let request = Request::builder()
        .method("POST")
        .uri("https://origin.example.com/")
        .body(owned.into_body())
        .expect("should construct the invalid upload");
    let error = client
        .send_async(PlatformHttpRequest::new(request, "invalid-body"))
        .await
        .expect_err("should reject response-owned uploads before materialization");
    assert!(
        matches!(
            error.downcast_ref::<edgezero_core::EdgeError>(),
            Some(edgezero_core::EdgeError::BadRequest { .. })
        ),
        "should retain the SDK's typed rejection"
    );
    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "should not consume guarded response data before SDK validation"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn absent_timeout_and_deadline_remain_unset_for_the_sdk() {
    let mut client = client();
    for target in [
        AuctionTargetId::Fastly,
        AuctionTargetId::Axum,
        AuctionTargetId::Cloudflare,
        AuctionTargetId::Spin,
    ] {
        client.target = target;
        let mapped = client
            .prepare(request("http://origin.example.com/", "origin"))
            .expect("should map the request")
            .into_parts();
        assert!(mapped.timeout.is_none(), "should inherit the SDK timeout");
        assert!(mapped.deadline.is_none(), "should not invent a deadline");
        let prepared = client
            .prepare_pending(request("http://origin.example.com/", "origin"))
            .await
            .expect("should prepare the buffered upload")
            .into_parts();
        assert!(
            prepared.timeout.is_none(),
            "should leave the SDK default implicit"
        );
        assert!(
            prepared.deadline.is_none(),
            "should not anchor a TS default timer"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_deadline_is_not_restarted_by_begin_preparation() {
    let client = client();
    let deadline = client
        .time
        .clock()
        .deadline_after(Duration::from_millis(300));
    let prepared = client
        .prepare_pending(
            request("http://origin.example.com/", "origin")
                .with_deadline(deadline, Duration::from_secs(2)),
        )
        .await
        .expect("should retain the tighter explicit deadline")
        .into_parts();
    assert_eq!(
        prepared
            .deadline
            .expect("should retain the explicit deadline")
            .instant(),
        deadline.instant(),
        "should retain the original absolute deadline"
    );
    assert_eq!(
        prepared.timeout,
        Some(Duration::from_secs(2)),
        "should retain the caller timeout"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_timeout_bounds_upload_preparation_without_an_independent_timer() {
    let client = client();
    let source = futures::stream::pending::<Result<bytes::Bytes, edgezero_core::EdgeError>>();
    let raw = Request::builder()
        .method("POST")
        .uri("http://origin.example.com/")
        .body(Body::from_stream(source))
        .expect("should construct a pending upload");
    let start = Instant::now();
    let error = client
        .prepare_pending(
            PlatformHttpRequest::new(raw, "origin").with_timeout(Duration::from_millis(50)),
        )
        .await
        .expect_err("should enforce the caller's explicit budget during preparation");
    assert!(
        matches!(error.current_context(), PlatformError::DeadlineExceeded),
        "should retain the explicit application deadline result"
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "should not wait for the SDK default"
    );
}
