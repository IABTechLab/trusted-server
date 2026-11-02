//! Core with a lookup-only signer, as the asynchronous prepared mode runs
//! it, and with a request its module hooks marked as a preparation
//! snapshot.
//!
//! A lookup-only signer is never given work core keeps or shares: core
//! reads and writes no kept signed representation, so no reader starts or
//! joins a load. A snapshot read gets the origin's answer exactly as it
//! came, with only the signer's facts and a private transport header added.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};

use super::test_support::{Behavior, StubSigner};
use super::*;
use crate::integrations::IntegrationRequestState;
use crate::platform::test_support::{MemoryLoadingCache, noop_services};
use crate::test_support::tests::create_test_settings;

const SOURCE: &[u8] = b"origin-image-bytes";
const PATH: &str = "/media/42";
const FACTS: &str = "x-test-snapshot-facts";
const VARIANT_SOURCE: &[u8] = b"variant-image-data";
const BASE_LINK: &str = "<https://manifests.example.com/base>; rel=\"c2pa-manifest\"";
const VARIANT_LINK: &str = "<https://manifests.example.com/variant>; rel=\"c2pa-manifest\"";

/// What a module's request preparer leaves on a snapshot read.
struct SnapshotMark;

/// [`StubSigner`] as a lookup-only signer that observes marked snapshots.
struct LookupOnly(Arc<StubSigner>);

impl MediaSigner for LookupOnly {
    fn signer_id(&self) -> &'static str {
        self.0.signer_id()
    }

    fn handles_path(&self, path: &str) -> bool {
        self.0.handles_path(path)
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        self.0.handles_media_type(media_type)
    }

    fn max_source_bytes(&self) -> usize {
        self.0.max_source_bytes()
    }

    fn max_large_source_bytes(&self) -> u64 {
        1_000_000
    }

    fn namespace<'a>(&'a self, services: &'a RuntimeServices) -> MediaNamespacing<'a> {
        self.0.namespace(services)
    }

    fn sign<'a>(
        &'a self,
        services: &'a RuntimeServices,
        job: &'a MediaJob<'a>,
    ) -> MediaSigning<'a> {
        self.0.sign(services, job)
    }

    fn lookup_only(&self) -> bool {
        true
    }

    fn lookup_large<'a>(
        &'a self,
        _services: &'a RuntimeServices,
        job: &'a LargeLookupJob<'a>,
    ) -> LargeLookup<'a> {
        Box::pin(async move {
            let (source, link) = match job.reader_query {
                "" => (SOURCE, BASE_LINK),
                "asset=b" => (VARIANT_SOURCE, VARIANT_LINK),
                _ => {
                    return Err(MediaSigningFailure {
                        reason: "not_prepared",
                    });
                }
            };
            Ok(PublishedLarge {
                headers: vec![(header::LINK, HeaderValue::from_static(link))],
                sha256: Sha256::digest(source).into(),
                length: u64::try_from(source.len()).expect("should fit source length"),
            })
        })
    }

    fn observes_snapshot(&self, state: &IntegrationRequestState) -> bool {
        state.get::<SnapshotMark>("testing").is_some()
    }

    fn snapshot_headers(
        &self,
        _state: &IntegrationRequestState,
        facts: &SnapshotFacts,
    ) -> Vec<ResponseHeader> {
        let value = format!(
            "private={};encoding={};host={};link={}",
            facts.private, facts.content_encoding, facts.origin_host, facts.manifest_link
        );
        vec![
            (
                HeaderName::from_static(FACTS),
                HeaderValue::from_str(&value).expect("should be a header value"),
            ),
            (
                HeaderName::from_static("x-test-snapshot-origin-identity"),
                HeaderValue::from_str(&facts.origin_identity_sha256)
                    .expect("should encode the configured-source digest"),
            ),
        ]
    }
}

fn identity() -> VariantIdentity<'static> {
    VariantIdentity {
        reader_url: "https://www.example.com/media/42",
        origin_identity: "https://origin.example.com\0origin.example.com",
        fetched_url: "https://origin.example.com/media/42",
        reader_query: "",
    }
}

fn image_request() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("sec-fetch-dest", HeaderValue::from_static("image"));
    headers
}

fn origin(extra: &[(HeaderName, &str)], body: Vec<u8>) -> Response<EdgeBody> {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ETAG, "\"origin-etag\"")
        .header(header::CACHE_CONTROL, "public, max-age=600");
    for (name, value) in extra {
        builder = builder.header(name.clone(), *value);
    }
    builder
        .body(EdgeBody::from(body))
        .expect("should build an origin answer")
}

fn services_with(signer: Arc<dyn MediaSigner>, cache: &Arc<MemoryLoadingCache>) -> RuntimeServices {
    noop_services()
        .with_media_signer(Some(signer))
        .with_loading_cache(cache.clone())
}

fn claim(services: &RuntimeServices) -> MediaClaim {
    MediaClaim::of_request(
        &create_test_settings(),
        services,
        &Method::GET,
        PATH,
        &image_request(),
        true,
    )
    .expect("should claim a media path")
}

fn deliver(
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

fn sent(delivery: Delivery) -> (Response<EdgeBody>, Bytes) {
    let Delivery::Send(response) = delivery else {
        panic!("should send a response");
    };
    let (parts, body) = response.into_parts();
    let bytes = futures::executor::block_on(body.into_bytes_bounded(1024 * 1024))
        .expect("should read the body");
    (Response::from_parts(parts, EdgeBody::empty()), bytes)
}

#[test]
fn a_lookup_only_signer_never_reads_or_keeps_a_shared_signed_copy() {
    // The same deployment with an ordinary signer keeps what it signs.
    let ordinary = StubSigner::new(Behavior::Sign, 1024);
    let ordinary_cache = Arc::new(MemoryLoadingCache::default());
    let ordinary_services = services_with(ordinary, &ordinary_cache);
    let _ = sent(deliver(
        &claim(&ordinary_services),
        &ordinary_services,
        origin(&[], SOURCE.to_vec()),
    ));
    assert!(
        !ordinary_cache.calls().is_empty(),
        "an ordinary signer's result is kept for other readers"
    );

    let stub = StubSigner::new(Behavior::Sign, 1024);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services_with(Arc::new(LookupOnly(Arc::clone(&stub))), &cache);
    let claim = claim(&services);
    assert!(
        futures::executor::block_on(claim.kept(&services, &identity())).is_none(),
        "nothing kept is looked up"
    );
    let (response, body) = sent(deliver(&claim, &services, origin(&[], SOURCE.to_vec())));
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        body.starts_with(SOURCE),
        "should deliver what the signer looked up for these bytes"
    );
    assert_eq!(stub.jobs().len(), 1, "the signer is asked once, to look up");
    assert!(
        cache.calls().is_empty(),
        "no load is started, joined or kept: {:?}",
        cache.calls()
    );
}

#[test]
fn a_snapshot_read_gets_the_origin_answer_as_it_came_with_the_signer_s_facts() {
    let stub = StubSigner::new(Behavior::Sign, 1024);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services_with(Arc::new(LookupOnly(Arc::clone(&stub))), &cache);
    let mut request = Request::builder()
        .uri(PATH)
        .body(EdgeBody::empty())
        .expect("should build");
    IntegrationRequestState::observe(&mut request, "testing", SnapshotMark);
    let snapshot = claim(&services).observing(&IntegrationRequestState::of(&request));

    let (response, body) = sent(deliver(
        &snapshot,
        &services,
        origin(&[(header::SET_COOKIE, "session=reader")], SOURCE.to_vec()),
    ));

    assert_eq!(body.as_ref(), SOURCE, "the origin's exact bytes");
    assert!(stub.jobs().is_empty(), "nothing is signed or looked up");
    assert!(cache.calls().is_empty(), "nothing is kept");
    assert_eq!(
        response
            .headers()
            .get(FACTS)
            .and_then(|value| value.to_str().ok()),
        Some("private=true;encoding=;host=origin.example.com;link=false"),
        "the origin's own privacy is reported, read before core changed anything"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store"),
        "the snapshot's own transport is private, apart from the origin's facts"
    );
    assert_eq!(
        response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok()),
        Some("\"origin-etag\""),
        "the origin's validators pass unchanged"
    );

    // An ordinary reader's request on the same deployment is no snapshot.
    let reader = Request::builder()
        .uri(PATH)
        .body(EdgeBody::empty())
        .expect("should build");
    let ordinary = claim(&services).observing(&IntegrationRequestState::of(&reader));
    let (response, _) = sent(deliver(&ordinary, &services, origin(&[], SOURCE.to_vec())));
    assert!(response.headers().get(FACTS).is_none());
    assert_eq!(
        stub.jobs().len(),
        1,
        "a reader's answer goes to the signer's lookup"
    );
}

#[test]
fn snapshot_origin_digest_tracks_configured_source_not_transport_host_or_validators() {
    let stub = StubSigner::new(Behavior::Sign, 1024);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services_with(Arc::new(LookupOnly(Arc::clone(&stub))), &cache);
    let mut request = Request::builder()
        .uri(PATH)
        .body(EdgeBody::empty())
        .expect("should build a snapshot request");
    IntegrationRequestState::observe(&mut request, "testing", SnapshotMark);
    let snapshot = claim(&services).observing(&IntegrationRequestState::of(&request));
    let mut digests = Vec::new();
    for configured in [
        "https://origin.example.com/old/\0origin.example.com",
        "https://origin.example.com/new/\0origin.example.com",
        "https://origin.example.com/new/\0virtual.example.com",
        "https://origin.example.com/new/\0/assets/\0virtual.example.com",
    ] {
        let identity = VariantIdentity {
            origin_identity: configured,
            ..identity()
        };
        let response = origin(&[(header::CONTENT_LENGTH, "18")], SOURCE.to_vec());
        let facts = OriginFacts::of(response.headers());
        let delivery = futures::executor::block_on(snapshot.deliver(
            &services,
            &identity,
            Answered::Origin(facts),
            response,
        ));
        let (response, body) = sent(delivery);
        assert_eq!(body.as_ref(), SOURCE);
        assert_eq!(
            response
                .headers()
                .get(header::ETAG)
                .expect("should retain the origin validator"),
            "\"origin-etag\""
        );
        let digest = response
            .headers()
            .get("x-test-snapshot-origin-identity")
            .expect("should expose configured-source digest to the signer")
            .to_str()
            .expect("should be lowercase hex");
        assert_eq!(digest, hex::encode(Sha256::digest(configured.as_bytes())));
        assert_eq!(digest.len(), 64);
        assert!(!digests.iter().any(|previous| previous == digest));
        digests.push(digest.to_owned());
    }
    assert!(stub.jobs().is_empty());
    assert!(cache.calls().is_empty());
}

#[test]
fn a_lookup_only_signer_needs_no_timer_or_streaming_to_look_up_a_large_version() {
    let stub = StubSigner::new(Behavior::Sign, 4);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services_with(Arc::new(LookupOnly(Arc::clone(&stub))), &cache);
    assert!(services.timer().is_none(), "the services carry no timer");

    let delivery = deliver(
        &claim(&services),
        &services,
        origin(&[(header::CONTENT_LENGTH, "18")], SOURCE.to_vec()),
    );

    assert!(
        matches!(delivery, Delivery::Large(_)),
        "a large version is admitted for a lookup by its strong validator"
    );
    assert!(
        stub.jobs().is_empty(),
        "nothing is signed while it is admitted"
    );
    assert!(cache.calls().is_empty(), "no load is started or joined");
}

#[test]
fn large_reader_delivery_preserves_the_query_selected_resource_and_its_own_provenance() {
    assert_eq!(SOURCE.len(), VARIANT_SOURCE.len());
    assert_ne!(SOURCE, VARIANT_SOURCE);
    let stub = StubSigner::new(Behavior::Sign, 4);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services_with(Arc::new(LookupOnly(Arc::clone(&stub))), &cache);
    let claim = claim(&services);
    for (query, source, expected_link) in [
        ("", SOURCE, Some(BASE_LINK)),
        ("asset=b", VARIANT_SOURCE, Some(VARIANT_LINK)),
        ("unknown=b", VARIANT_SOURCE, None),
        ("asset=b&asset=b", VARIANT_SOURCE, None),
    ] {
        let fetched_url = format!("https://origin.example.com/media/42?{query}");
        let identity = VariantIdentity {
            reader_query: query,
            fetched_url: &fetched_url,
            ..identity()
        };
        let response = origin(&[(header::CONTENT_LENGTH, "18")], source.to_vec());
        let facts = OriginFacts::of(response.headers());
        let delivery = futures::executor::block_on(claim.deliver(
            &services,
            &identity,
            Answered::Origin(facts),
            response,
        ));
        let Delivery::Large(answer) = delivery else {
            panic!("should admit this source for large lookup");
        };
        let url = url::Url::parse(&fetched_url).expect("should parse source URL");
        let headers = HeaderMap::new();
        let response = futures::executor::block_on(claim.deliver_large(
            &services,
            &identity,
            PinnedOrigin {
                url: &url,
                headers: &headers,
                backend_name: "origin",
                auth: None,
                unsigned_headers: &[],
                response_header_rewrite: None,
            },
            answer,
        ));
        let (response, body) = sent(Delivery::Send(response));
        assert_eq!(
            body.as_ref(),
            source,
            "{query}: should preserve origin bytes"
        );
        assert_eq!(
            response
                .headers()
                .get(header::LINK)
                .and_then(|value| value.to_str().ok()),
            expected_link,
            "{query}: should select only this query resource's manifest"
        );
        if expected_link.is_some() {
            let digest = format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(source)));
            assert_eq!(
                response
                    .headers()
                    .get("repr-digest")
                    .and_then(|value| value.to_str().ok()),
                Some(digest.as_str()),
                "{query}: should carry this resource's own digest"
            );
        } else {
            assert!(
                response.headers().get("repr-digest").is_none(),
                "{query}: should not carry another resource's digest"
            );
        }
    }
    assert!(stub.jobs().is_empty(), "no source is signed during lookup");
    assert!(
        cache.calls().is_empty(),
        "no signed load is started or joined"
    );
}
