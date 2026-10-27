use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use futures::StreamExt as _;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};

use super::test_support::{Behavior, MANIFEST_LINK, SIGNED_SUFFIX, StubSigner};
use super::*;
use crate::platform::test_support::{MemoryLoadingCache, noop_services};
use crate::test_support::tests::create_test_settings;

const SOURCE: &[u8] = b"origin-image-bytes";
const PATH: &str = "/media/42";
const FETCHED_URL: &str = "https://origin.example.com/media/42";
const READER_URL: &str = "https://www.example.com/media/42";
const ORIGIN_IDENTITY: &str = "https://origin.example.com\0origin.example.com";
const ORIGIN_ETAG: &str = "\"origin-etag\"";

fn identity() -> VariantIdentity<'static> {
    VariantIdentity {
        reader_url: READER_URL,
        origin_identity: ORIGIN_IDENTITY,
        fetched_url: FETCHED_URL,
        reader_query_free: true,
    }
}

fn signed_bytes() -> Vec<u8> {
    let mut bytes = SOURCE.to_vec();
    bytes.extend_from_slice(SIGNED_SUFFIX);
    bytes
}

fn signed_etag() -> String {
    format!("\"sig-{}\"", hex::encode(Sha256::digest(signed_bytes())))
}

fn request_headers(pairs: &[(HeaderName, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.append(
            name.clone(),
            HeaderValue::from_str(value).expect("should build header"),
        );
    }
    headers
}

/// The headers of a browser asking for an image, which may be answered
/// from a kept copy.
fn image_request(pairs: &[(HeaderName, &str)]) -> HeaderMap {
    let mut headers = request_headers(pairs);
    headers.insert("sec-fetch-dest", HeaderValue::from_static("image"));
    headers
}

fn origin_response(
    status: StatusCode,
    body: EdgeBody,
    extra: &[(HeaderName, &str)],
) -> Response<EdgeBody> {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ETAG, ORIGIN_ETAG)
        .header(header::CACHE_CONTROL, "public, max-age=600");
    for (name, value) in extra {
        builder = builder.header(name.clone(), *value);
    }
    builder.body(body).expect("should build origin response")
}

fn whole_origin() -> Response<EdgeBody> {
    origin_response(StatusCode::OK, EdgeBody::from(SOURCE.to_vec()), &[])
}

fn part_origin() -> Response<EdgeBody> {
    origin_response(
        StatusCode::PARTIAL_CONTENT,
        EdgeBody::from(SOURCE[0..=3].to_vec()),
        &[(header::CONTENT_RANGE, "bytes 0-3/18")],
    )
}

async fn body_bytes(body: EdgeBody) -> Vec<u8> {
    match body {
        EdgeBody::Once(bytes) => bytes.to_vec(),
        EdgeBody::Stream(mut stream) => {
            let mut collected = Vec::new();
            while let Some(chunk) = stream.next().await {
                collected.extend_from_slice(&chunk.expect("should read stream chunk"));
            }
            collected
        }
    }
}

fn header<'a>(response: &'a Response<EdgeBody>, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn claim_shared(
    services: &RuntimeServices,
    method: &Method,
    headers: &HeaderMap,
    shared: bool,
) -> MediaClaim {
    MediaClaim::of_request(
        &create_test_settings(),
        services,
        method,
        PATH,
        headers,
        shared,
    )
    .expect("should claim a media path")
}

fn claim(services: &RuntimeServices, method: &Method, headers: &HeaderMap) -> MediaClaim {
    claim_shared(services, method, headers, true)
}

fn keeping(signer: &Arc<StubSigner>) -> RuntimeServices {
    signer
        .installed_in(noop_services())
        .with_loading_cache(Arc::new(MemoryLoadingCache::default()))
}

fn split(response: Response<EdgeBody>) -> (Response<EdgeBody>, Vec<u8>) {
    let (parts, body) = response.into_parts();
    let bytes = futures::executor::block_on(body_bytes(body));
    (Response::from_parts(parts, EdgeBody::empty()), bytes)
}

/// What `deliver` decides for the origin's `response`, with the facts its
/// own headers give.
fn delivery(
    claim: &MediaClaim,
    services: &RuntimeServices,
    response: Response<EdgeBody>,
) -> Delivery {
    let facts = OriginFacts::of(response.headers());
    futures::executor::block_on(claim.deliver(
        services,
        &identity(),
        Answered::Origin(facts),
        response,
    ))
}

/// Delivers the origin's `response` to a reader whose request is `method`
/// with `headers`, expecting core to send it without a second fetch.
fn deliver(
    signer: &Arc<StubSigner>,
    method: &Method,
    headers: &HeaderMap,
    response: Response<EdgeBody>,
) -> (Response<EdgeBody>, Vec<u8>) {
    let services = signer.installed_in(noop_services());
    let claim = claim(&services, method, headers);
    let Delivery::Send(response) = delivery(&claim, &services, response) else {
        panic!("should send without asking the origin again");
    };
    split(response)
}

#[test]
fn signs_the_exact_captured_bytes_and_owns_the_validators() {
    let signer = StubSigner::new(Behavior::Sign, 1024);

    let (response, body) = deliver(&signer, &Method::GET, &HeaderMap::new(), whole_origin());

    assert_eq!(response.status(), StatusCode::OK, "should answer 200");
    assert_eq!(
        body,
        signed_bytes(),
        "should send the signer's bytes unchanged"
    );
    let jobs = signer.jobs();
    assert_eq!(jobs.len(), 1, "should ask the signer once");
    assert_eq!(
        jobs[0].source, SOURCE,
        "should hand the signer the exact origin bytes"
    );
    assert_eq!(jobs[0].source_sha256, hex::encode(Sha256::digest(SOURCE)));
    assert_eq!(
        jobs[0].reader_url, READER_URL,
        "should name the reader's address"
    );
    assert_eq!(
        jobs[0].content_type, "image/png",
        "should pass the origin type"
    );
    assert_eq!(jobs[0].source_reference.as_deref(), Some(FETCHED_URL));
    assert_eq!(
        jobs[0].source_origin_host.as_deref(),
        Some("origin.example.com")
    );
    assert_eq!(header(&response, "etag"), Some(signed_etag().as_str()));
    assert_eq!(
        header(&response, "repr-digest"),
        Some(
            format!(
                "sha-256=:{}:",
                base64::engine::general_purpose::STANDARD.encode(Sha256::digest(signed_bytes()))
            )
            .as_str()
        ),
        "should describe the signed representation's digest"
    );
    assert_eq!(
        header(&response, "content-length"),
        Some(signed_bytes().len().to_string().as_str()),
        "should frame the signed length"
    );
    assert_eq!(
        header(&response, "accept-ranges"),
        Some("none"),
        "a signed representation nothing keeps offers no ranges"
    );
    assert_eq!(header(&response, "x-test-manifest"), Some("instance-1"));
    assert_eq!(
        header(&response, "cache-control"),
        Some("public, max-age=600")
    );
}

#[test]
fn merges_the_signers_link_with_the_origins_links() {
    let signer = StubSigner::new(Behavior::SignWithLink, 1024);
    let origin_link = "<https://www.example.com/style.css>; rel=preload; as=style";

    let (response, _) = deliver(
        &signer,
        &Method::GET,
        &HeaderMap::new(),
        origin_response(
            StatusCode::OK,
            EdgeBody::from(SOURCE.to_vec()),
            &[(header::LINK, origin_link)],
        ),
    );

    let links: Vec<&str> = response
        .headers()
        .get_all(header::LINK)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    assert_eq!(
        links,
        vec![origin_link, MANIFEST_LINK],
        "should keep the origin's link and add the manifest's"
    );
}

#[test]
fn names_the_origin_host_even_where_the_reference_is_withheld() {
    for (fetched_url, host) in [
        (
            "https://origin.example.com/media/42?signature=secret",
            "origin.example.com",
        ),
        (
            "https://user:pass@Origin.Example.com/media/42",
            "origin.example.com",
        ),
        (
            "https://origin.example.com:8443/media/42#part",
            "origin.example.com",
        ),
    ] {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let services = signer.installed_in(noop_services());
        let claim = claim(&services, &Method::GET, &HeaderMap::new());
        let response = whole_origin();
        let facts = OriginFacts::of(response.headers());
        futures::executor::block_on(claim.deliver(
            &services,
            &VariantIdentity {
                fetched_url,
                ..identity()
            },
            Answered::Origin(facts),
            response,
        ));
        let job = &signer.jobs()[0];
        assert_eq!(
            job.source_reference, None,
            "should send no reference for {fetched_url}"
        );
        assert_eq!(
            job.source_origin_host.as_deref(),
            Some(host),
            "should name the host of {fetched_url}"
        );
    }
}

#[test]
fn answers_a_single_range_from_the_signed_bytes() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let total = signed_bytes().len();

    let (response, body) = deliver(
        &signer,
        &Method::GET,
        &request_headers(&[(header::RANGE, "bytes=2-5")]),
        whole_origin(),
    );

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        body,
        signed_bytes()[2..=5],
        "should send the asked bytes of the signed body"
    );
    assert_eq!(
        header(&response, "content-range"),
        Some(format!("bytes 2-5/{total}").as_str())
    );
    assert_eq!(header(&response, "content-length"), Some("4"));
}

#[test]
fn answers_suffix_and_open_ranges_from_the_signed_bytes() {
    let total = signed_bytes().len();
    for (range, first) in [("bytes=-4", total - 4), ("bytes=10-", 10)] {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let (response, body) = deliver(
            &signer,
            &Method::GET,
            &request_headers(&[(header::RANGE, range)]),
            whole_origin(),
        );
        assert_eq!(
            response.status(),
            StatusCode::PARTIAL_CONTENT,
            "{range} should be partial"
        );
        assert_eq!(
            body,
            signed_bytes()[first..],
            "{range} should send the tail"
        );
        assert_eq!(
            header(&response, "content-range"),
            Some(format!("bytes {first}-{}/{total}", total - 1).as_str())
        );
    }
}

#[test]
fn refuses_a_range_past_the_signed_length() {
    let signer = StubSigner::new(Behavior::Sign, 1024);

    let (response, body) = deliver(
        &signer,
        &Method::GET,
        &request_headers(&[(header::RANGE, "bytes=1000-")]),
        whole_origin(),
    );

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert!(body.is_empty(), "should send no body with a 416");
    assert_eq!(
        header(&response, "content-range"),
        Some(format!("bytes */{}", signed_bytes().len()).as_str())
    );
}

#[test]
fn answers_several_ranges_and_malformed_ranges_with_the_whole_signed_body() {
    for range in ["bytes=0-1,4-5", "bytes=5-2", "items=0-1", "bytes=+1-2"] {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let (response, body) = deliver(
            &signer,
            &Method::GET,
            &request_headers(&[(header::RANGE, range)]),
            whole_origin(),
        );
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{range} should be answered whole"
        );
        assert_eq!(body, signed_bytes());
    }
}

#[test]
fn honors_if_range_only_for_the_signed_entity_tag() {
    let signed = signed_etag();
    for (if_range, expected) in [
        (signed.as_str(), StatusCode::PARTIAL_CONTENT),
        (ORIGIN_ETAG, StatusCode::OK),
        ("Tue, 01 Sep 2026 00:00:00 GMT", StatusCode::OK),
    ] {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let (response, _) = deliver(
            &signer,
            &Method::GET,
            &request_headers(&[(header::RANGE, "bytes=0-3"), (header::IF_RANGE, if_range)]),
            whole_origin(),
        );
        assert_eq!(response.status(), expected, "If-Range {if_range}");
    }
}

#[test]
fn tells_a_precondition_naming_signed_bytes_from_one_naming_the_origins() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = signer.installed_in(noop_services());
    let signed = signed_etag();
    for (headers, names_signed) in [
        (
            request_headers(&[
                (header::RANGE, "bytes=0-3"),
                (header::IF_RANGE, signed.as_str()),
            ]),
            true,
        ),
        (
            request_headers(&[(header::IF_MATCH, format!("\"other\", {signed}").as_str())]),
            true,
        ),
        (
            request_headers(&[
                (header::RANGE, "bytes=0-3"),
                (header::IF_RANGE, ORIGIN_ETAG),
            ]),
            false,
        ),
        (
            request_headers(&[(header::IF_NONE_MATCH, signed.as_str())]),
            false,
        ),
        (
            request_headers(&[(header::IF_MATCH, "W/\"sig-00\"")]),
            false,
        ),
    ] {
        assert_eq!(
            claim(&services, &Method::GET, &headers).names_signed_bytes(),
            names_signed,
            "{headers:?}"
        );
    }
}

#[test]
fn answers_if_none_match_against_the_signed_tag_only() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let (response, body) = deliver(
        &signer,
        &Method::GET,
        &request_headers(&[(
            header::IF_NONE_MATCH,
            format!("\"other\", W/{}", signed_etag()).as_str(),
        )]),
        whole_origin(),
    );
    assert_eq!(
        response.status(),
        StatusCode::NOT_MODIFIED,
        "a weak match of the signed tag is a 304"
    );
    assert!(body.is_empty(), "a 304 carries no body");
    assert_eq!(header(&response, "content-length"), None);
    assert_eq!(header(&response, "etag"), Some(signed_etag().as_str()));
    assert_eq!(
        header(&response, "x-test-manifest"),
        Some("instance-1"),
        "a 304 carries the signer's headers"
    );

    let signer = StubSigner::new(Behavior::Sign, 1024);
    let (response, body) = deliver(
        &signer,
        &Method::GET,
        &request_headers(&[(header::IF_NONE_MATCH, ORIGIN_ETAG)]),
        whole_origin(),
    );
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the origin's tag never matches signed bytes"
    );
    assert_eq!(body, signed_bytes());
}

#[test]
fn fails_if_match_that_names_another_representation() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let (response, body) = deliver(
        &signer,
        &Method::GET,
        &request_headers(&[(header::IF_MATCH, ORIGIN_ETAG)]),
        whole_origin(),
    );
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    assert!(body.is_empty());

    let signer = StubSigner::new(Behavior::Sign, 1024);
    let (response, _) = deliver(
        &signer,
        &Method::GET,
        &request_headers(&[(header::IF_MATCH, signed_etag().as_str())]),
        whole_origin(),
    );
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the signed tag satisfies If-Match"
    );
}

#[test]
fn signs_the_whole_behind_a_part_that_can_be_kept_and_ranges_the_signed_bytes() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);
    let headers = image_request(&[(header::RANGE, "bytes=0-3")]);
    let claim = claim(&services, &Method::GET, &headers);

    let Delivery::FetchWhole(fallback) = delivery(&claim, &services, part_origin()) else {
        panic!("should ask for the whole representation of a part it can keep");
    };
    assert!(signer.jobs().is_empty(), "a part is never signed");

    let delivered = futures::executor::block_on(claim.deliver_whole(
        &services,
        &identity(),
        whole_origin(),
        fallback,
    ));
    let (response, body) = split(delivered);
    assert_eq!(
        signer.jobs()[0].source,
        SOURCE,
        "should sign the whole answer's exact bytes"
    );
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        body,
        signed_bytes()[0..=3],
        "should range the signed bytes, not the origin's"
    );
    assert_eq!(header(&response, "etag"), Some(signed_etag().as_str()));
    assert_eq!(
        header(&response, "accept-ranges"),
        Some("bytes"),
        "a kept representation offers ranges"
    );
}

#[test]
fn passes_a_part_through_where_nothing_would_keep_the_signed_whole() {
    let cases: [(RuntimeServices, Response<EdgeBody>, &str); 3] = [
        (
            StubSigner::new(Behavior::Sign, 1024).installed_in(noop_services()),
            part_origin(),
            "no cache keeps values",
        ),
        (
            keeping(&StubSigner::new(Behavior::Sign, 1024)),
            origin_response(
                StatusCode::PARTIAL_CONTENT,
                EdgeBody::from(SOURCE[0..=3].to_vec()),
                &[
                    (header::CONTENT_RANGE, "bytes 0-3/18"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
            ),
            "the origin gave no shared freshness",
        ),
        (
            keeping(&StubSigner::new(Behavior::Sign, 1024)),
            origin_response(
                StatusCode::PARTIAL_CONTENT,
                EdgeBody::from(b"part".to_vec()),
                &[(header::CONTENT_RANGE, "bytes 0-3/50000000")],
            ),
            "the whole is over the limit",
        ),
    ];
    for (services, part, case) in cases {
        let claim = claim(
            &services,
            &Method::GET,
            &image_request(&[(header::RANGE, "bytes=0-3")]),
        );
        let Delivery::Send(response) = delivery(&claim, &services, part) else {
            panic!("{case}: should not read the whole origin for a part");
        };
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{case}");
        assert_eq!(
            header(&response, "etag"),
            Some(ORIGIN_ETAG),
            "{case}: keeps the origin's part"
        );
    }
}

#[test]
fn asks_for_the_signed_tail_the_origin_refused_as_past_its_own_length() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = signer.installed_in(noop_services());
    let tail = signed_bytes().len() - 2;
    let range = format!("bytes={tail}-");
    let headers = request_headers(&[(header::RANGE, range.as_str())]);
    let claim = claim(&services, &Method::GET, &headers);
    let refusal = origin_response(
        StatusCode::RANGE_NOT_SATISFIABLE,
        EdgeBody::empty(),
        &[(header::CONTENT_RANGE, "bytes */18")],
    );

    let Delivery::FetchWhole(fallback) = delivery(&claim, &services, refusal) else {
        panic!("a range past the origin's length may name the signed tail");
    };
    let delivered = futures::executor::block_on(claim.deliver_whole(
        &services,
        &identity(),
        whole_origin(),
        fallback,
    ));
    let (response, body) = split(delivered);
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, signed_bytes()[tail..], "should send the signed tail");
}

#[test]
fn sends_the_origins_part_without_validators_when_the_whole_fetch_cannot_be_signed() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);
    let claim = claim(
        &services,
        &Method::GET,
        &image_request(&[(header::RANGE, "bytes=0-3")]),
    );

    let mut missing = whole_origin();
    *missing.status_mut() = StatusCode::NOT_FOUND;
    let delivered = futures::executor::block_on(claim.deliver_whole(
        &services,
        &identity(),
        missing,
        part_origin(),
    ));
    let (response, body) = split(delivered);
    assert_eq!(
        response.status(),
        StatusCode::PARTIAL_CONTENT,
        "should keep the origin's part"
    );
    assert_eq!(body, SOURCE[0..=3]);
    assert_eq!(
        header(&response, "etag"),
        None,
        "an unsigned fallback carries no validator to pin it"
    );
    assert_eq!(
        header(&response, "cache-control"),
        Some("private, max-age=60")
    );
    assert!(signer.jobs().is_empty());
}

#[test]
fn answers_head_from_the_signed_whole_with_no_body() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);
    let claim = claim(&services, &Method::HEAD, &image_request(&[]));
    let head = origin_response(
        StatusCode::OK,
        EdgeBody::empty(),
        &[(header::CONTENT_LENGTH, "18")],
    );

    let Delivery::FetchWhole(fallback) = delivery(&claim, &services, head) else {
        panic!("a HEAD of a bounded type that can be kept should ask for the whole body");
    };
    let delivered = futures::executor::block_on(claim.deliver_whole(
        &services,
        &identity(),
        whole_origin(),
        fallback,
    ));
    let (response, body) = split(delivered);
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body.is_empty(), "a HEAD carries no body");
    assert_eq!(
        header(&response, "content-length"),
        Some(signed_bytes().len().to_string().as_str()),
        "a HEAD advertises the signed length"
    );
    assert_eq!(header(&response, "etag"), Some(signed_etag().as_str()));
}

#[test]
fn never_signs_a_response_the_origin_marked_as_one_readers() {
    let private_markers: [(HeaderName, &str); 4] = [
        (header::SET_COOKIE, "session=1"),
        (header::CACHE_CONTROL, "private, max-age=60"),
        (header::VARY, "Accept, Cookie"),
        (header::VARY, "*"),
    ];
    for (name, value) in private_markers {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let mut response = whole_origin();
        response
            .headers_mut()
            .insert(name.clone(), HeaderValue::from_static(value));
        let (response, body) = deliver(&signer, &Method::GET, &HeaderMap::new(), response);
        assert!(
            signer.jobs().is_empty(),
            "{name}: {value} should not reach the signer"
        );
        assert_eq!(
            body, SOURCE,
            "{name}: {value} should go out as the origin sent it"
        );
        assert_eq!(header(&response, "etag"), Some(ORIGIN_ETAG));
    }
}

#[test]
fn reads_the_origins_own_headers_not_the_policy_core_wrote_over_them() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = signer.installed_in(noop_services());
    let claim = claim(&services, &Method::GET, &HeaderMap::new());
    let raw = origin_response(
        StatusCode::OK,
        EdgeBody::empty(),
        &[(header::CACHE_CONTROL, "public, max-age=60, no-transform")],
    );
    let facts = OriginFacts::of(raw.headers());
    let normalized = origin_response(
        StatusCode::OK,
        EdgeBody::from(SOURCE.to_vec()),
        &[(header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
    );

    futures::executor::block_on(claim.deliver(
        &services,
        &identity(),
        Answered::Origin(facts),
        normalized,
    ));
    assert!(
        signer.jobs().is_empty(),
        "the origin's no-transform stands under core's policy"
    );
}

#[test]
fn claims_nothing_without_a_signer_with_authorization_or_off_its_paths() {
    let settings = create_test_settings();
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = signer.installed_in(noop_services());
    let none = |services: &RuntimeServices, method: &Method, path: &str, headers: &HeaderMap| {
        MediaClaim::of_request(&settings, services, method, path, headers, true).is_none()
    };
    assert!(
        none(&noop_services(), &Method::GET, PATH, &HeaderMap::new()),
        "services without a signer claim nothing"
    );
    assert!(
        none(
            &services,
            &Method::GET,
            PATH,
            &request_headers(&[(header::AUTHORIZATION, "Bearer reader-token")])
        ),
        "a request bound to one reader's credentials is never claimed"
    );
    assert!(
        none(&services, &Method::GET, "/api/items", &HeaderMap::new()),
        "a path the signer leaves alone"
    );
    assert!(
        none(&services, &Method::POST, PATH, &HeaderMap::new()),
        "only GET and HEAD are claimed"
    );
}

#[test]
fn tells_a_media_request_by_destination_accept_or_extension() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = signer.installed_in(noop_services());
    let settings = create_test_settings();
    let looks = |path: &str, headers: &HeaderMap| {
        MediaClaim::of_request(&settings, &services, &Method::GET, path, headers, true)
            .expect("should claim")
            .looks_like_media()
    };
    assert!(
        looks(PATH, &image_request(&[])),
        "an image destination is media"
    );
    assert!(!looks(
        "/photo.jpg",
        &request_headers(&[("sec-fetch-dest".parse().expect("should parse"), "script")])
    ));
    assert!(looks(
        PATH,
        &request_headers(&[(header::ACCEPT, "image/avif,image/webp,*/*")])
    ));
    assert!(
        looks("/clips/intro.MP4", &HeaderMap::new()),
        "a media extension is media"
    );
    assert!(
        !looks(PATH, &request_headers(&[(header::ACCEPT, "text/html,*/*")])),
        "a page is not"
    );
}

#[test]
fn leaves_untransformable_encoded_and_unhandled_bodies_as_the_origin_sent_them() {
    let extras: [(HeaderName, &str); 3] = [
        (header::CACHE_CONTROL, "public, no-transform"),
        (header::CONTENT_ENCODING, "gzip"),
        (header::CONTENT_TYPE, "text/plain"),
    ];
    for (name, value) in extras {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let mut response = whole_origin();
        response
            .headers_mut()
            .insert(name.clone(), HeaderValue::from_static(value));
        let (response, body) = deliver(
            &signer,
            &Method::GET,
            &request_headers(&[(header::RANGE, "bytes=0-1")]),
            response,
        );
        assert!(
            signer.jobs().is_empty(),
            "{name}: {value} should not reach the signer"
        );
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{name}: {value} keeps the origin's status"
        );
        assert_eq!(body, SOURCE);
    }
}

#[test]
fn passes_other_statuses_through_untouched() {
    for status in [
        StatusCode::NOT_FOUND,
        StatusCode::NOT_MODIFIED,
        StatusCode::RANGE_NOT_SATISFIABLE,
    ] {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let response = origin_response(status, EdgeBody::empty(), &[]);
        let (response, _) = deliver(&signer, &Method::GET, &HeaderMap::new(), response);
        assert_eq!(
            response.status(),
            status,
            "a reader that asked no range gets the origin's {status}"
        );
        assert_eq!(header(&response, "etag"), Some(ORIGIN_ETAG));
        assert!(signer.jobs().is_empty());
    }
}

#[test]
fn sends_the_origin_bytes_unsigned_without_validators_when_signing_fails() {
    for behavior in [Behavior::Fail, Behavior::ReservedHeader] {
        let signer = StubSigner::new(behavior, 1024);
        let (response, body) = deliver(
            &signer,
            &Method::GET,
            &request_headers(&[(header::RANGE, "bytes=0-5")]),
            whole_origin(),
        );
        assert_eq!(
            signer.jobs().len(),
            1,
            "{behavior:?} should have asked the signer"
        );
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{behavior:?} keeps the origin's own answer"
        );
        assert_eq!(body, SOURCE);
        assert_eq!(
            header(&response, "etag"),
            None,
            "{behavior:?} drops the origin's validator"
        );
        assert_eq!(
            header(&response, "cache-control"),
            Some("private, max-age=60")
        );
        assert_eq!(header(&response, "x-test-manifest"), None);
        assert_eq!(header(&response, "repr-digest"), None);
    }
}

#[test]
fn captures_a_streamed_body_within_the_limit() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let chunks = SOURCE
        .chunks(5)
        .map(Bytes::copy_from_slice)
        .collect::<Vec<_>>();
    let (response, body) = deliver(
        &signer,
        &Method::GET,
        &HeaderMap::new(),
        origin_response(
            StatusCode::OK,
            EdgeBody::stream(futures::stream::iter(chunks)),
            &[],
        ),
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        signer.jobs()[0].source,
        SOURCE,
        "should join the streamed chunks exactly"
    );
    assert_eq!(body, signed_bytes());
}

#[test]
fn streams_a_body_over_the_limit_unsigned_and_whole() {
    let signer = StubSigner::new(Behavior::Sign, 8);
    let chunks = SOURCE
        .chunks(4)
        .map(Bytes::copy_from_slice)
        .collect::<Vec<_>>();
    let (response, body) = deliver(
        &signer,
        &Method::GET,
        &HeaderMap::new(),
        origin_response(
            StatusCode::OK,
            EdgeBody::stream(futures::stream::iter(chunks)),
            &[],
        ),
    );
    assert!(
        signer.jobs().is_empty(),
        "an oversized body should not reach the signer"
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body, SOURCE,
        "should send every byte read and unread, in order"
    );
    assert_eq!(header(&response, "etag"), Some(ORIGIN_ETAG));
}

#[test]
fn keeps_a_failed_stream_failing_after_the_bytes_read() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = signer.installed_in(noop_services());
    let claim = claim(&services, &Method::GET, &HeaderMap::new());
    let stream = futures::stream::iter(vec![
        Ok(Bytes::from_static(b"first")),
        Err(std::io::Error::other("origin reset")),
    ]);
    let Delivery::Send(delivered) = delivery(
        &claim,
        &services,
        origin_response(StatusCode::OK, EdgeBody::from_stream(stream), &[]),
    ) else {
        panic!("should send the origin's answer");
    };
    let EdgeBody::Stream(mut body) = delivered.into_body() else {
        panic!("should keep a failed body streaming");
    };
    futures::executor::block_on(async {
        let first = body.next().await.expect("should yield the bytes read");
        assert_eq!(
            first.expect("should read the first chunk"),
            Bytes::from_static(b"first")
        );
        assert!(
            body.next()
                .await
                .expect("should yield the failure")
                .is_err(),
            "should hand the reader the origin's failure, never a short body as whole"
        );
    });
    assert!(
        signer.jobs().is_empty(),
        "a body that failed is never signed"
    );
}

/// Signs once through `services` for a reader whose request is `headers`,
/// then gives back what a later image request finds before the origin.
fn sign_then_look_up(
    signer: &Arc<StubSigner>,
    services: &RuntimeServices,
    headers: &HeaderMap,
    shared: bool,
    origin: Response<EdgeBody>,
) -> Option<Response<EdgeBody>> {
    let first = claim_shared(services, &Method::GET, headers, shared);
    let _ = delivery(&first, services, origin);
    assert_eq!(signer.jobs().len(), 1, "should sign the first request");
    let later = claim(services, &Method::GET, &image_request(&[]));
    futures::executor::block_on(later.kept(services, &identity()))
}

#[test]
fn answers_a_later_range_from_the_kept_signed_representation_before_the_origin() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);

    let kept = sign_then_look_up(
        &signer,
        &services,
        &image_request(&[]),
        true,
        whole_origin(),
    )
    .expect("should keep a fresh signed representation");
    let age = header(&kept, "age")
        .and_then(|age| age.parse::<u64>().ok())
        .expect("a kept copy carries its age");
    assert!(
        age <= 1,
        "a copy kept just now has almost no age, got {age}"
    );

    let ranged = image_request(&[(header::RANGE, "bytes=1-4")]);
    let claim = claim(&services, &Method::GET, &ranged);
    let Delivery::Send(response) =
        futures::executor::block_on(claim.deliver(&services, &identity(), Answered::Kept, kept))
    else {
        panic!("a kept representation is answered at once");
    };
    let (response, body) = split(response);
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, signed_bytes()[1..=4]);
    assert_eq!(header(&response, "etag"), Some(signed_etag().as_str()));
    assert_eq!(header(&response, "x-test-manifest"), Some("instance-1"));
    assert_eq!(signer.jobs().len(), 1, "the kept copy is not signed again");
}

#[test]
fn keeps_and_serves_nothing_for_a_request_whose_answer_could_be_one_readers() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);
    assert!(
        sign_then_look_up(
            &signer,
            &services,
            &image_request(&[]),
            false,
            whole_origin()
        )
        .is_none(),
        "an answer to a cookie-bearing request is never kept for other readers"
    );

    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);
    assert!(
        sign_then_look_up(
            &signer,
            &services,
            &image_request(&[]),
            true,
            whole_origin()
        )
        .is_some()
    );
    let cookie_bearing = claim_shared(&services, &Method::GET, &image_request(&[]), false);
    assert!(
        futures::executor::block_on(cookie_bearing.kept(&services, &identity())).is_none(),
        "a cookie-bearing request is never answered from a copy kept for others"
    );
    let revalidating = claim(
        &services,
        &Method::GET,
        &image_request(&[(header::CACHE_CONTROL, "no-cache")]),
    );
    assert!(
        futures::executor::block_on(revalidating.kept(&services, &identity())).is_none(),
        "a request asking for revalidation is not answered from a kept copy"
    );
    let not_media = claim(&services, &Method::GET, &HeaderMap::new());
    assert!(
        futures::executor::block_on(not_media.kept(&services, &identity())).is_none(),
        "a request that does not look like media is not looked up"
    );
}

#[test]
fn serves_nothing_kept_once_the_signers_namespace_changes_or_fails() {
    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);
    assert!(
        sign_then_look_up(
            &signer,
            &services,
            &image_request(&[]),
            true,
            whole_origin()
        )
        .is_some()
    );
    let claim = claim(&services, &Method::GET, &image_request(&[]));

    signer.set_namespace(Ok("policy-2".to_owned()));
    assert!(
        futures::executor::block_on(claim.kept(&services, &identity())).is_none(),
        "a changed policy finds nothing kept under the old one"
    );

    signer.set_namespace(Err(MediaSigningFailure { reason: "revoked" }));
    assert!(
        futures::executor::block_on(claim.kept(&services, &identity())).is_none(),
        "a revoked signer serves nothing kept"
    );
}

#[test]
fn keeps_nothing_the_origin_did_not_make_shareably_fresh() {
    for cache_control in ["no-cache", "public", "public, max-age=0"] {
        let signer = StubSigner::new(Behavior::Sign, 1024);
        let services = keeping(&signer);
        let mut origin = whole_origin();
        origin.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
        assert!(
            sign_then_look_up(&signer, &services, &image_request(&[]), true, origin).is_none(),
            "`{cache_control}` should keep no signed copy"
        );
    }

    let signer = StubSigner::new(Behavior::Sign, 1024);
    let services = keeping(&signer);
    let mut origin = whole_origin();
    origin
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));
    assert!(
        sign_then_look_up(&signer, &services, &image_request(&[]), true, origin).is_none(),
        "an answer that varies on the reader's Accept keeps no single signed copy"
    );
}

#[test]
fn a_kept_copy_ages_and_is_refused_when_its_body_is_not_the_one_signed() {
    let mut headers = HeaderMap::new();
    let digest = Sha256::digest(signed_bytes());
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&signed_entity_tag(digest.as_slice())).expect("should build tag"),
    );
    let body = Bytes::from(signed_bytes());
    let encoded = encode_kept(&KeptRecord {
        kept_at: 1_000,
        age: 5,
        headers: &headers,
        body: &body,
    });

    let kept = decode_kept(&encoded, 1_030).expect("should read an intact copy");
    assert_eq!(
        header(&kept, "age"),
        Some("35"),
        "should add the time kept to the origin's age"
    );

    let mut altered = encoded.to_vec();
    let last = altered.len() - 1;
    altered[last] ^= 0x01;
    assert!(
        decode_kept(&Bytes::from(altered), 1_030).is_none(),
        "a body that no longer hashes to its tag is never served"
    );
}

#[test]
fn strips_every_header_that_shortens_the_origin_answer() {
    let mut headers = request_headers(&[
        (header::RANGE, "bytes=0-1"),
        (header::IF_RANGE, ORIGIN_ETAG),
        (header::IF_NONE_MATCH, ORIGIN_ETAG),
        (header::IF_MODIFIED_SINCE, "Tue, 01 Sep 2026 00:00:00 GMT"),
        (header::IF_MATCH, ORIGIN_ETAG),
        (header::IF_UNMODIFIED_SINCE, "Tue, 01 Sep 2026 00:00:00 GMT"),
        (header::ACCEPT, "image/avif"),
    ]);
    strip_representation_preconditions(&mut headers);
    assert_eq!(headers.len(), 1, "should keep only the unrelated header");
    assert!(headers.contains_key(header::ACCEPT));
}

/// A manifest the origin already links its bytes to.
const ORIGIN_MANIFEST_LINK: &str = "<https://origin.example.com/own.c2pa>; rel=\"c2pa-manifest\"";

/// `response` with the origin's own manifest `Link`.
fn linked(mut response: Response<EdgeBody>) -> Response<EdgeBody> {
    response
        .headers_mut()
        .append(header::LINK, HeaderValue::from_static(ORIGIN_MANIFEST_LINK));
    response
}

fn links(response: &Response<EdgeBody>) -> Vec<String> {
    response
        .headers()
        .get_all(header::LINK)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .collect()
}

#[test]
fn keeps_an_origin_manifest_link_and_its_bytes_for_external_and_embeddable_signers() {
    // `SignWithLink` stands for an external-only signer, `Sign` for one
    // that embeds a manifest and changes the bytes.
    for behavior in [Behavior::SignWithLink, Behavior::Sign] {
        let signer = StubSigner::new(behavior, 1024);
        let (response, body) = deliver(
            &signer,
            &Method::GET,
            &image_request(&[]),
            linked(whole_origin()),
        );
        assert!(
            signer.jobs().is_empty(),
            "{behavior:?}: should never sign bytes that name a manifest of their own"
        );
        assert_eq!(body, SOURCE, "{behavior:?}: should send the origin's bytes");
        assert_eq!(
            header(&response, "etag"),
            Some(ORIGIN_ETAG),
            "{behavior:?}: should keep the origin's validator"
        );
        assert_eq!(
            links(&response),
            vec![ORIGIN_MANIFEST_LINK.to_owned()],
            "{behavior:?}: should keep only the origin's manifest link"
        );
        assert_eq!(header(&response, "x-test-manifest"), None);
        assert_eq!(header(&response, "repr-digest"), None);
    }
}

#[test]
fn never_signs_a_whole_fetch_where_an_answer_names_an_origin_manifest() {
    let signer = StubSigner::new(Behavior::SignWithLink, 1024);
    let services = keeping(&signer);
    let first_claim = claim(
        &services,
        &Method::GET,
        &image_request(&[(header::RANGE, "bytes=0-3")]),
    );
    let Delivery::Send(response) = delivery(&first_claim, &services, linked(part_origin())) else {
        panic!("should never ask for the whole behind a part that names an origin manifest");
    };
    let (response, body) = split(response);
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, SOURCE[0..=3]);
    assert_eq!(links(&response), vec![ORIGIN_MANIFEST_LINK.to_owned()]);
    assert!(signer.jobs().is_empty());

    let tail = signed_bytes().len() - 2;
    let range = format!("bytes={tail}-");
    for (refusal_linked, whole_linked) in [(false, true), (true, false)] {
        let signer = StubSigner::new(Behavior::SignWithLink, 1024);
        let services = signer.installed_in(noop_services());
        let claim = claim(
            &services,
            &Method::GET,
            &request_headers(&[(header::RANGE, range.as_str())]),
        );
        let refusal = origin_response(
            StatusCode::RANGE_NOT_SATISFIABLE,
            EdgeBody::empty(),
            &[(header::CONTENT_RANGE, "bytes */18")],
        );
        let refusal = if refusal_linked {
            linked(refusal)
        } else {
            refusal
        };
        let Delivery::FetchWhole(fallback) = delivery(&claim, &services, refusal) else {
            panic!("a range past the origin's length may name the signed tail");
        };
        let whole = if whole_linked {
            linked(whole_origin())
        } else {
            whole_origin()
        };
        let delivered = futures::executor::block_on(claim.deliver_whole(
            &services,
            &identity(),
            whole,
            fallback,
        ));
        let (response, body) = split(delivered);
        let case = format!("refusal_linked={refusal_linked} whole_linked={whole_linked}");
        assert!(signer.jobs().is_empty(), "{case}: should sign nothing");
        assert_eq!(
            response.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "{case}: should send the origin's own answer"
        );
        assert!(body.is_empty(), "{case}");
        assert_eq!(header(&response, "etag"), Some(ORIGIN_ETAG), "{case}");
        let expected = if refusal_linked {
            vec![ORIGIN_MANIFEST_LINK.to_owned()]
        } else {
            Vec::new()
        };
        assert_eq!(
            links(&response),
            expected,
            "{case}: should add no manifest link"
        );
    }
}

/// One ISO BMFF box of `kind` around `body`, with a 32-bit size.
fn bmff_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(body.len() + 8).expect("should fit a 32-bit size");
    let mut bytes = size.to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(body);
    bytes
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// A C2PA verifiable segment info `emsg`, as a packager writes it.
fn c2pa_emsg() -> Vec<u8> {
    bmff_box(b"emsg", b"\0\0\0\0urn:c2pa:verifiable-segment-info\0fseg\0")
}

#[test]
fn tells_the_parts_of_a_fragmented_stream_from_a_progressive_file() {
    let init = concat(&[
        bmff_box(b"ftyp", b"cmfc\0\0\0\0iso6cmfc"),
        bmff_box(
            b"moov",
            &concat(&[
                bmff_box(b"mvhd", &[0; 100]),
                bmff_box(b"trak", &bmff_box(b"tkhd", &[0; 84])),
                bmff_box(b"mvex", &bmff_box(b"trex", &[0; 24])),
                bmff_box(b"pssh", &[0; 32]),
            ]),
        ),
    ]);
    let fragment = || {
        vec![
            bmff_box(b"sidx", &[0; 32]),
            bmff_box(b"moof", &bmff_box(b"mfhd", &[0; 8])),
            bmff_box(b"mdat", b"coded-samples"),
        ]
    };
    let segment = concat(&[vec![bmff_box(b"styp", b"msdh\0\0\0\0msdhmsix")], fragment()].concat());
    let segment_with_emsg = concat(
        &[
            vec![bmff_box(b"styp", b"msdh\0\0\0\0msdhmsix"), c2pa_emsg()],
            fragment(),
        ]
        .concat(),
    );
    let chunk_without_styp = concat(&fragment());
    let progressive = concat(&[
        bmff_box(b"ftyp", b"isom\0\0\0\0isomavc1"),
        bmff_box(b"moov", &bmff_box(b"trak", &[0; 16])),
        bmff_box(b"mdat", b"coded-samples"),
    ]);
    let fragmented_file = concat(&[
        bmff_box(b"ftyp", b"iso6\0\0\0\0iso6dash"),
        bmff_box(b"moov", &bmff_box(b"mvex", &[0; 8])),
        bmff_box(b"moof", &[0; 8]),
        bmff_box(b"mdat", b"coded-samples"),
    ]);
    let mut large_size = init.clone();
    // The same init with its `ftyp` given a 64-bit size.
    let ftyp_body = b"cmfc\0\0\0\0iso6cmfc";
    let mut large_ftyp = 1_u32.to_be_bytes().to_vec();
    large_ftyp.extend_from_slice(b"ftyp");
    large_ftyp.extend_from_slice(&(16 + ftyp_body.len() as u64).to_be_bytes());
    large_ftyp.extend_from_slice(ftyp_body);
    large_size.splice(0..8 + ftyp_body.len(), large_ftyp);
    let mut truncated = segment.clone();
    truncated.truncate(segment.len() - 3);
    let mut zero_sized_last = segment.clone();
    let mdat_at = zero_sized_last.len() - (8 + b"coded-samples".len());
    zero_sized_last[mdat_at..mdat_at + 4].copy_from_slice(&0_u32.to_be_bytes());
    let too_many = bmff_box(b"free", &[]).repeat(MAX_BMFF_BOXES + 1);

    for (case, bytes, expected) in [
        ("Shaka-style init", &init, true),
        ("init with a 64-bit ftyp size", &large_size, true),
        ("media segment", &segment, true),
        ("media segment with a C2PA emsg", &segment_with_emsg, true),
        ("CMAF chunk without styp", &chunk_without_styp, true),
        ("segment whose mdat runs to the end", &zero_sized_last, true),
        ("progressive file", &progressive, false),
        ("fragmented file with its media", &fragmented_file, false),
        ("truncated segment", &truncated, false),
        ("too many boxes", &too_many, false),
        ("an image", &SOURCE.to_vec(), false),
        ("nothing", &Vec::new(), false),
    ] {
        assert_eq!(
            is_fragmented_stream_part(bytes),
            expected,
            "{case}: should be told correctly"
        );
    }
}

#[test]
fn origin_freshness_keeps_browser_and_shared_age_and_processing_independent() {
    let receipt_seconds = 1_700_000_000;
    let receipt = UNIX_EPOCH + Duration::from_secs(receipt_seconds);
    for (control, age, date_age, elapsed, browser, shared) in [
        (
            "public, max-age=60, s-maxage=3600",
            Some("0"),
            None,
            0,
            60,
            3600,
        ),
        (
            "public, max-age=60, s-maxage=3600",
            Some("5"),
            Some(120),
            0,
            0,
            3480,
        ),
        (
            "public, max-age=60, s-maxage=3600",
            None,
            Some(120),
            0,
            0,
            3480,
        ),
        (
            "public, max-age=60, s-maxage=3600",
            Some("200"),
            Some(120),
            0,
            0,
            3400,
        ),
        (
            "public, max-age=90, s-maxage=180",
            Some("10"),
            None,
            40,
            40,
            130,
        ),
        (
            "public, max-age=90, s-maxage=180",
            Some("10"),
            None,
            90,
            0,
            80,
        ),
        (
            "public, max-age=90, s-maxage=10",
            Some("15"),
            None,
            0,
            75,
            0,
        ),
        ("public, max-age=90", Some("70"), None, 0, 20, 20),
        ("public, max-age=90", Some("100"), None, 0, 0, 0),
        ("public, s-maxage=90", None, None, 0, 0, 90),
        ("public, max-age=bad, s-maxage=90", None, None, 0, 0, 90),
        ("public, max-age=60, s-maxage=bad", None, None, 0, 60, 0),
        ("public, max-age=60, s-maxage", None, None, 0, 60, 0),
        ("public, max-age=60, max-age=90", None, None, 0, 0, 0),
        ("public, max-age=60", Some("bad"), None, 0, 0, 0),
        ("no-cache, max-age=90", Some("0"), None, 0, 0, 0),
        ("public", Some("0"), None, 0, 0, 0),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_str(control).expect("control"),
        );
        if let Some(age) = age {
            headers.insert(header::AGE, HeaderValue::from_str(age).expect("age"));
        }
        if let Some(date_age) = date_age {
            let date = httpdate::fmt_http_date(
                std::time::UNIX_EPOCH + Duration::from_secs(receipt_seconds - date_age),
            );
            headers.insert(header::DATE, HeaderValue::from_str(&date).expect("date"));
        }
        assert_eq!(
            OriginFacts::of_at(&headers, receipt).remaining_freshness(Duration::from_secs(elapsed)),
            OriginFreshness {
                browser: Duration::from_secs(browser),
                shared: Duration::from_secs(shared),
            },
            "{control}, Age {age:?}, apparent age {date_age:?}, processing {elapsed}",
        );
    }
}

#[test]
fn invalid_origin_date_cannot_gain_freshness_when_rebased() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=3600"),
    );
    headers.insert(header::DATE, HeaderValue::from_static("not a date"));
    assert_eq!(
        OriginFacts::of_at(&headers, UNIX_EPOCH).remaining_freshness(Duration::ZERO),
        OriginFreshness::default(),
    );
}
