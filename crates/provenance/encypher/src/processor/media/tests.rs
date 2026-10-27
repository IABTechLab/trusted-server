use std::collections::HashMap;
use std::sync::Mutex;

use edgezero_core::key_value_store::KvPage;
use futures::executor::block_on;
use serde_json::json;
use trusted_server_core::platform::test_support::{
    StubHttpClient, build_services_with_http_client,
};
use trusted_server_core::platform::{KvError, PlatformHttpClient, PlatformKvStore};

use super::*;
use crate::EncypherConfig;

const TOKEN: &str = "cwk_test_worker_token_value";
const POLICY_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PAGE_URL: &str = "https://publisher.example.com/news/story";
const SIGN_URL: &str = "https://api.example.com/api/v1/public/cdn/media/sign";
const SOURCE: &[u8] = b"\x89PNG\r\n\x1a\n-source-pixels";
const SIGNED: &[u8] = b"\x89PNG\r\n\x1a\n-source-pixels-caBX-jumb-c2pa-manifest";
const MANIFEST: &str = "<metadata><c2pa:manifest xmlns:c2pa=\"http://c2pa.org/manifest\">QUJD</c2pa:manifest></metadata>";

/// A runtime KV store held in memory, recording deletes.
#[derive(Default)]
struct MemoryKv {
    entries: Mutex<HashMap<String, Bytes>>,
    deletes: Mutex<Vec<String>>,
}

impl MemoryKv {
    fn keys_with(&self, prefix: &str) -> Vec<String> {
        self.entries
            .lock()
            .expect("should lock entries")
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn get(&self, key: &str) -> Option<Bytes> {
        self.entries
            .lock()
            .expect("should lock entries")
            .get(key)
            .cloned()
    }

    fn set(&self, key: &str, value: Bytes) {
        self.entries
            .lock()
            .expect("should lock entries")
            .insert(key.to_owned(), value);
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformKvStore for MemoryKv {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        Ok(self.get(key))
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.set(key, value);
        Ok(())
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        _ttl: Duration,
    ) -> Result<(), KvError> {
        self.set(key, value);
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.entries
            .lock()
            .expect("should lock entries")
            .remove(key);
        self.deletes
            .lock()
            .expect("should lock deletes")
            .push(key.to_owned());
        Ok(())
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Ok(KvPage::default())
    }
}

fn signer() -> Arc<Signer> {
    let config: EncypherConfig = serde_json::from_value(json!({
        "api_base_url": "https://api.example.com",
        "domain": "publisher.example.com",
        "worker_token": TOKEN,
    }))
    .expect("should parse config");
    Arc::new(Signer::new(config, 5_000_000))
}

fn services(http: &Arc<StubHttpClient>, kv: &Arc<MemoryKv>) -> RuntimeServices {
    build_services_with_http_client(Arc::clone(http) as Arc<dyn PlatformHttpClient>)
        .with_kv_store(Arc::clone(kv) as Arc<dyn PlatformKvStore>)
}

/// The signing headers of an embedded answer for `body`.
fn answer(body: &[u8], content_type: &str) -> Vec<(String, String)> {
    [
        ("content-type", content_type.to_owned()),
        ("x-encypher-policy-digest", POLICY_DIGEST.to_owned()),
        ("x-encypher-signed-sha256", sha256_hex(body)),
        ("x-encypher-manifest-instance-id", "xmp:iid:0001".to_owned()),
        ("x-encypher-manifest-disposition", "embedded".to_owned()),
        ("x-encypher-manifest-link-required", "false".to_owned()),
        (
            "x-encypher-verification-url",
            "https://verify.example.com/doc-1".to_owned(),
        ),
        ("x-encypher-document-id", "doc-1".to_owned()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}

/// The signing headers of a remote-manifest answer that needs a `Link`.
fn external_answer(body: &[u8]) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = answer(body, "image/png")
        .into_iter()
        .filter(|(name, _)| {
            name != "x-encypher-manifest-disposition" && name != "x-encypher-manifest-link-required"
        })
        .collect();
    for (name, value) in [
        ("x-encypher-manifest-disposition", "external"),
        ("x-encypher-manifest-link-required", "true"),
        (
            "x-encypher-manifest-url",
            "https://api.example.com/manifests/doc-1",
        ),
        ("x-encypher-manifest-sha256", POLICY_DIGEST),
    ] {
        headers.push((name.to_owned(), value.to_owned()));
    }
    headers
}

fn png_source(sha256: &str) -> MediaSource<'_> {
    MediaSource {
        page_url: PAGE_URL,
        reference: None,
        origin_host: None,
        mime: "image/png",
        media_type: "image/png",
        bytes: SOURCE,
        sha256,
        link_deliverable: false,
        require_embedded_manifest: false,
    }
}

fn sign(
    signer: &Signer,
    services: &RuntimeServices,
    source: &MediaSource<'_>,
) -> Result<(SignedMedia, Reason), Reason> {
    block_on(async {
        let mut work = Deadline::Unbounded;
        let mut calls = 0;
        signer
            .sign_media(services, source, POLICY_DIGEST, &mut work, &mut calls)
            .await
    })
}

#[test]
fn a_document_obeys_its_configured_call_limit_for_distinct_assets() {
    for limit in [3, 12] {
        let http = Arc::new(StubHttpClient::new());
        let images: Vec<Vec<u8>> = (0..=limit)
            .map(|index| {
                let mut bytes = SOURCE.to_vec();
                bytes.push(u8::try_from(index).expect("should fit"));
                bytes
            })
            .collect();
        for image in images.iter().take(limit) {
            let mut signed = image.clone();
            signed.extend_from_slice(b"-c2pa");
            let headers = answer(&signed, "image/png");
            http.push_response_with_headers(200, signed, headers);
        }
        let kv = Arc::new(MemoryKv::default());
        let document: String = images
            .iter()
            .map(|image| {
                format!(
                    "<img src=\"data:image/png;base64,{}\">",
                    STANDARD.encode(image)
                )
            })
            .collect();
        let mut config = signer().config.clone();
        config.max_media_sign_calls_per_document = limit;
        let signer = Signer::new(config, 5_000_000);
        let services = services(&http, &kv);
        let request = page_request();
        let scan = embedded::scan_html(&document, None);
        let output = block_on(async {
            let mut work = Deadline::Unbounded;
            signer
                .sign_embedded(
                    &services,
                    &embedded_job(&request, Surface::Page),
                    &document,
                    &scan,
                    &mut work,
                )
                .await
        })
        .expect("should sign the items within the configured limit");

        assert_eq!(
            http.recorded_request_methods().len(),
            limit,
            "should stop calling at the configured bound"
        );
        for image in images.iter().take(limit) {
            let mut signed = image.clone();
            signed.extend_from_slice(b"-c2pa");
            assert!(
                output.contains(&STANDARD.encode(&signed)),
                "should deliver every signed distinct asset within the limit"
            );
        }
        let last = images.last().expect("should have a last image");
        assert!(
            output.contains(&STANDARD.encode(last)),
            "should leave the deferred item as written"
        );
        assert_eq!(
            kv.keys_with(MEDIA_SKIP_KEY_PREFIX).len(),
            0,
            "a deferred item should not be negatively cached"
        );
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn signs_exact_bytes_once_then_serves_the_verified_cached_answer() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);
    let signer = signer();
    let sha = sha256_hex(SOURCE);

    let (first, reason) = sign(&signer, &services, &png_source(&sha)).expect("should sign");

    assert_eq!(reason, Reason::Signed);
    assert_eq!(first.bytes.as_ref(), SIGNED, "should keep the signed bytes");
    assert_eq!(http.recorded_request_methods(), ["POST"]);
    assert_eq!(http.recorded_request_uris(), [SIGN_URL]);
    let headers = &http.recorded_request_headers()[0];
    let key = header(headers, "idempotency-key").expect("should send a replay key");
    assert!(
        key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "should derive a hex replay key: {key}"
    );
    assert!(
        header(headers, "x-encypher-worker-token").is_some(),
        "should authenticate with the worker token"
    );
    assert!(
        header(headers, "content-type")
            .is_some_and(|value| value.starts_with("multipart/form-data; boundary=")),
        "should send multipart"
    );
    let body = &http.recorded_request_bodies()[0];
    assert!(contains(body, SOURCE), "should send the exact source bytes");
    assert!(
        contains(body, b"\"reference\":null"),
        "should send no reference for media without a safe address"
    );
    assert!(
        contains(body, b"\"origin_host\":null"),
        "should send no origin host for media fetched from no host"
    );
    assert!(
        contains(
            body,
            format!("\"expected_policy_digest\":\"{POLICY_DIGEST}\"").as_bytes()
        ),
        "should send the held policy digest"
    );

    let (second, reason) = sign(&signer, &services, &png_source(&sha)).expect("should sign");
    assert_eq!(reason, Reason::CacheHit);
    assert_eq!(
        second.bytes.as_ref(),
        SIGNED,
        "should replay identical bytes"
    );
    assert_eq!(
        http.recorded_request_methods().len(),
        1,
        "should make no second call"
    );
}

#[test]
fn a_stale_policy_evicts_the_digest_and_caches_a_skip() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(
        409,
        serde_json::to_vec(&json!({
            "success": false,
            "error": {"code": "TRUSTED_SERVER_POLICY_CHANGED", "message": "stale"},
        }))
        .expect("should encode"),
        vec![("content-type", "application/json")],
    );
    let kv = Arc::new(MemoryKv::default());
    let signer = signer();
    kv.set(&signer.policy_key, Bytes::from_static(b"{}"));
    let services = services(&http, &kv);
    let sha = sha256_hex(SOURCE);

    assert_eq!(
        sign(&signer, &services, &png_source(&sha)).expect_err("should not sign"),
        Reason::PolicyChanged
    );
    assert!(
        kv.get(&signer.policy_key).is_none(),
        "should evict the policy"
    );
    assert_eq!(
        kv.keys_with(MEDIA_SKIP_KEY_PREFIX).len(),
        1,
        "should cache a skip"
    );
    assert_eq!(
        sign(&signer, &services, &png_source(&sha)).expect_err("should not sign"),
        Reason::CachedSkip
    );
    assert_eq!(
        http.recorded_request_methods().len(),
        1,
        "should not call again"
    );
}

#[test]
fn a_purged_result_is_asked_for_again_under_a_new_key_on_the_next_view() {
    for (status, code) in [
        (410, "SIGNING_RESULT_PURGED"),
        (409, "SIGNING_IDEMPOTENCY_CONFLICT"),
    ] {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
            status,
            serde_json::to_vec(&json!({"success": false, "error": {"code": code}}))
                .expect("should encode"),
        );
        http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
        let kv = Arc::new(MemoryKv::default());
        let services = services(&http, &kv);
        let signer = signer();
        let sha = sha256_hex(SOURCE);

        let first = sign(&signer, &services, &png_source(&sha)).expect_err("should not sign");
        assert!(
            matches!(first, Reason::ResultPurged | Reason::IdempotencyConflict),
            "{code}: should report {first:?}"
        );
        assert_eq!(
            http.recorded_request_methods().len(),
            1,
            "{code}: should not retry"
        );
        assert!(
            kv.keys_with(MEDIA_SKIP_KEY_PREFIX).is_empty(),
            "{code}: no skip"
        );

        let (_, reason) = sign(&signer, &services, &png_source(&sha)).expect("should sign");
        assert_eq!(reason, Reason::Signed);
        let headers = http.recorded_request_headers();
        assert_ne!(
            header(&headers[0], "idempotency-key"),
            header(&headers[1], "idempotency-key"),
            "{code}: should mint a new replay key"
        );
    }
}

#[test]
fn a_tampered_cached_answer_is_refused_and_evicted() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);
    let signer = signer();
    let sha = sha256_hex(SOURCE);
    sign(&signer, &services, &png_source(&sha)).expect("should sign");
    let key = kv.keys_with(MEDIA_KEY_PREFIX)[0].clone();
    let mut entry: serde_json::Value =
        serde_json::from_slice(&kv.get(&key).expect("should hold the answer")).expect("JSON");
    entry["bytes"] = json!(STANDARD.encode(b"\x89PNG\r\n\x1a\nother-c2pa"));
    kv.set(&key, Bytes::from(serde_json::to_vec(&entry).expect("JSON")));

    assert_eq!(
        sign(&signer, &services, &png_source(&sha)).expect_err("should refuse"),
        Reason::CachedEntryInvalid
    );
    assert!(kv.get(&key).is_none(), "should evict the tampered entry");
}

#[test]
fn unsigned_or_mismatched_answers_are_never_used_and_are_negatively_cached() {
    let other_policy = "b".repeat(64);
    let cases = [
        (
            SOURCE.to_vec(),
            answer(SOURCE, "image/png"),
            Reason::UnsignedResponse,
        ),
        (
            b"\x89PNG\r\n\x1a\nswapped-c2pa".to_vec(),
            answer(SIGNED, "image/png"),
            Reason::SignedDigestMismatch,
        ),
        (
            SIGNED.to_vec(),
            answer(SIGNED, "image/jpeg"),
            Reason::SignedTypeMismatch,
        ),
        (
            SIGNED.to_vec(),
            answer(SIGNED, "image/png")
                .into_iter()
                .map(|(name, value)| {
                    if name == "x-encypher-policy-digest" {
                        (name, other_policy.clone())
                    } else {
                        (name, value)
                    }
                })
                .collect(),
            Reason::PolicyDigestMismatch,
        ),
        (
            SIGNED.to_vec(),
            external_answer(SIGNED),
            Reason::ManifestUndeliverable,
        ),
        (
            SIGNED.to_vec(),
            answer(SIGNED, "image/png")
                .into_iter()
                .map(|(name, value)| {
                    if name == "x-encypher-verification-url" {
                        (
                            name,
                            "http://localhost:9000/api/v1/verify/cdnmedia_dev".to_owned(),
                        )
                    } else {
                        (name, value)
                    }
                })
                .collect(),
            Reason::InvalidSignedResponse,
        ),
    ];
    for (body, headers, expected) in cases {
        let http = Arc::new(StubHttpClient::new());
        http.push_response_with_headers(200, body, headers);
        let kv = Arc::new(MemoryKv::default());
        let services = services(&http, &kv);
        let sha = sha256_hex(SOURCE);

        assert_eq!(
            sign(&signer(), &services, &png_source(&sha)).expect_err("should not sign"),
            expected
        );
        assert!(
            kv.keys_with(MEDIA_KEY_PREFIX).is_empty(),
            "{expected:?}: should cache no answer"
        );
        assert_eq!(
            kv.keys_with(MEDIA_SKIP_KEY_PREFIX).len(),
            1,
            "{expected:?}: should cache the failure"
        );
    }
}

fn embedded_job<'a>(request: &'a PageRequest, surface: Surface) -> EmbeddedJob<'a> {
    EmbeddedJob {
        request,
        surface,
        policy_digest: POLICY_DIGEST,
    }
}

fn page_request() -> PageRequest {
    PageRequest {
        method: Method::GET,
        page_url: PAGE_URL.to_owned(),
        removed_prefix: String::new(),
        response_status: 200,
        origin_response_is_private: false,
    }
}

fn sign_document(services: &RuntimeServices, document: &str, surface: Surface) -> Option<String> {
    sign_document_with_policies(services, document, surface, None)
}

fn sign_document_with_policies(
    services: &RuntimeServices,
    document: &str,
    surface: Surface,
    policies: Option<&[String]>,
) -> Option<String> {
    let request = page_request();
    let scan = match surface {
        Surface::Page => embedded::scan_html(document, policies),
        Surface::Stylesheet => embedded::scan_stylesheet(document),
    };
    block_on(async {
        let mut work = Deadline::Unbounded;
        signer()
            .sign_embedded(
                services,
                &embedded_job(&request, surface),
                document,
                &scan,
                &mut work,
            )
            .await
    })
}

#[test]
fn embedded_data_uris_are_replaced_by_their_signed_bytes_once_per_source() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    let kv = Arc::new(MemoryKv::default());
    let source = format!("data:image/png;base64,{}", STANDARD.encode(SOURCE));
    let signed = format!("data:image/png;base64,{}", STANDARD.encode(SIGNED));
    let document = format!(
        "<p>x</p><img src=\"{source}\" alt=a><img srcset=\"/a.png 1x, {source} 2x\" loading=lazy>"
    );

    let output = sign_document(&services(&http, &kv), &document, Surface::Page)
        .expect("should sign the page's media");

    assert_eq!(
        output,
        format!(
            "<p>x</p><img src=\"{signed}\" alt=a><img srcset=\"/a.png 1x, {signed} 2x\" loading=lazy>"
        ),
        "should replace each URI and change nothing else"
    );
    assert_eq!(
        http.recorded_request_methods(),
        ["POST"],
        "should sign one source once"
    );
}

#[test]
fn a_failed_embedded_signature_leaves_the_uri_as_written() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response(
        422,
        b"{\"error\":{\"code\":\"UNSUPPORTED_MEDIA_TYPE\"}}".to_vec(),
    );
    let kv = Arc::new(MemoryKv::default());
    let document = format!(
        "<img src=\"data:image/png;base64,{}\">",
        STANDARD.encode(SOURCE)
    );

    assert_eq!(
        sign_document(&services(&http, &kv), &document, Surface::Page),
        None,
        "should leave the page as written"
    );
}

#[test]
fn stylesheet_media_is_signed_in_place() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    let kv = Arc::new(MemoryKv::default());
    let document = format!(
        ".hero{{background:url('data:image/png;base64,{}') no-repeat}}",
        STANDARD.encode(SOURCE)
    );

    let output = sign_document(&services(&http, &kv), &document, Surface::Stylesheet)
        .expect("should sign the stylesheet's media");

    assert_eq!(
        output,
        format!(
            ".hero{{background:url('data:image/png;base64,{}') no-repeat}}",
            STANDARD.encode(SIGNED)
        )
    );
}

#[test]
fn inline_svg_is_signed_as_its_exact_bytes_and_kept_only_with_the_writer_s_carrier_alone() {
    let root = "<svg xmlns=\"http://www.w3.org/2000/svg\">";
    let body = "<style>.a{fill:currentColor}</style><use href=\"#i\"/></svg>";
    let svg = format!("{root}{body}");
    let document = format!("<p>x</p>{svg}<p>y</p>");
    let signed_svg = format!("{root}{MANIFEST}{body}");
    let restyled = signed_svg.replace("currentColor", "#000");
    for (answer_svg, expected) in [
        (
            signed_svg.clone(),
            Some(format!("<p>x</p>{signed_svg}<p>y</p>")),
        ),
        (restyled, None),
    ] {
        let http = Arc::new(StubHttpClient::new());
        http.push_response_with_headers(
            200,
            answer_svg.clone().into_bytes(),
            answer(answer_svg.as_bytes(), "image/svg+xml"),
        );
        let kv = Arc::new(MemoryKv::default());

        let output = sign_document(&services(&http, &kv), &document, Surface::Page);

        assert_eq!(
            output, expected,
            "should keep only a pure manifest insertion"
        );
        assert!(
            contains(&http.recorded_request_bodies()[0], svg.as_bytes()),
            "should send the element's exact bytes"
        );
    }
}

#[test]
fn inline_svg_the_browser_reads_differently_is_signed_as_its_parsed_tree() {
    // The browser adds no namespace and keeps the first `viewBox`; what is
    // hashed, sent, compared and placed is that tree as standalone XML.
    let original = "<svg viewbox=\"0 0 2 2\" viewBox=\"0 0 1 1\"><rect/></svg>";
    let serialized = crate::svg_equivalence::signable_svg(original, 1024 * 1024)
        .expect("should have an equivalent standalone document")
        .into_owned();
    assert_ne!(serialized, original, "should not sign the bytes as written");
    let document = roxmltree::Document::parse(&serialized).expect("should be standalone XML");
    assert_eq!(
        document.root_element().attribute("viewBox"),
        Some("0 0 2 2"),
        "should keep the browser's first viewBox"
    );
    // The writer's carrier goes right after the root start tag, whose
    // attribute values escape any `>`.
    let (root, body) =
        serialized.split_at(serialized.find('>').expect("should close the root") + 1);
    let signed_svg = format!("{root}{MANIFEST}{body}");
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(
        200,
        signed_svg.clone().into_bytes(),
        answer(signed_svg.as_bytes(), "image/svg+xml"),
    );
    let kv = Arc::new(MemoryKv::default());

    let output = sign_document(
        &services(&http, &kv),
        &format!("<p>x</p>{original}<p>y</p>"),
        Surface::Page,
    );

    assert_eq!(
        output,
        Some(format!("<p>x</p>{signed_svg}<p>y</p>")),
        "should place the signed serialization where the element was"
    );
    let sent = &http.recorded_request_bodies()[0];
    assert!(
        contains(sent, serialized.as_bytes()),
        "should send the serialized tree"
    );
    assert!(
        !contains(sent, original.as_bytes()),
        "should not send bytes the browser reads differently"
    );
}

#[test]
fn unsigned_self_closing_svgs_never_add_whole_asset_signing_calls() {
    let http = Arc::new(StubHttpClient::new());
    let kv = Arc::new(MemoryKv::default());
    assert_eq!(
        sign_document(&services(&http, &kv), "<svg/>", Surface::Page),
        None
    );
    assert!(http.recorded_request_methods().is_empty());

    let source = format!("data:image/png;base64,{}", STANDARD.encode(SOURCE));
    let signed = format!("data:image/png;base64,{}", STANDARD.encode(SIGNED));
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    assert_eq!(
        sign_document(
            &services(&http, &kv),
            &format!("<svg href=\"{source}\"/>"),
            Surface::Page,
        ),
        Some(format!("<svg href=\"{signed}\"/>"))
    );
    assert_eq!(http.recorded_request_methods(), ["POST"]);
}

#[test]
fn original_svg_provenance_makes_no_child_or_whole_asset_signing_calls() {
    let source = format!("data:image/png;base64,{}", STANDARD.encode(SOURCE));
    for provenance in [
        MANIFEST,
        "<metadata dcterms:provenance=\"https://example.com/m\"/>",
        "<metadata><dcterms:provenance>https://example.com/m</dcterms:provenance></metadata>",
        "<metadata provenance=\"https://example.com/m\"/>",
        "<metadata xmlns:proof=\"https://c2pa.org/manifest\"/>",
    ] {
        let http = Arc::new(StubHttpClient::new());
        let kv = Arc::new(MemoryKv::default());
        let document = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"{source}\"/>\
             <style>.a{{fill:url('{source}')}}</style>{provenance}</svg>"
        );
        assert_eq!(
            sign_document_with_policies(&services(&http, &kv), &document, Surface::Page, Some(&[]),),
            None,
            "must preserve the entire original range for {provenance}"
        );
        assert!(http.recorded_request_methods().is_empty(), "{provenance}");
    }
}

#[test]
fn signing_an_outside_image_cannot_change_original_svg_provenance_ranges() {
    let source = format!("data:image/png;base64,{}", STANDARD.encode(SOURCE));
    let signed = format!("data:image/png;base64,{}", STANDARD.encode(SIGNED));
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\">{MANIFEST}\
         <image href=\"{source}\"/><g style=\"fill:url({source})\"/>\
         <style>.a{{fill:url('{source}')}}</style></svg>"
    );
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    let kv = Arc::new(MemoryKv::default());
    let document = format!("<img src=\"{source}\">{svg}<img src=\"{source}\">");
    assert_eq!(
        sign_document_with_policies(&services(&http, &kv), &document, Surface::Page, Some(&[]),),
        Some(format!("<img src=\"{signed}\">{svg}<img src=\"{signed}\">")),
        "the rescan after outside edits must still preserve every SVG byte"
    );
    assert_eq!(http.recorded_request_methods(), ["POST"]);
}

#[test]
fn an_unsigned_svg_still_signs_its_attribute_css_and_then_the_whole_asset() {
    let source = format!("data:image/png;base64,{}", STANDARD.encode(SOURCE));
    let signed = format!("data:image/png;base64,{}", STANDARD.encode(SIGNED));
    let root = "<svg xmlns=\"http://www.w3.org/2000/svg\">";
    let body = format!(
        "<title>Signed with C2PA</title><image href=\"{source}\"/>\
         <g style=\"fill:url({source})\"/><style>.a{{fill:url('{source}')}}</style></svg>"
    );
    let source_svg = format!("{root}{body}");
    let child_signed_svg = source_svg.replace(&source, &signed);
    let signed_svg = format!("{root}{MANIFEST}{}", body.replace(&source, &signed));
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    http.push_response_with_headers(
        200,
        signed_svg.as_bytes().to_vec(),
        answer(signed_svg.as_bytes(), "image/svg+xml"),
    );
    let kv = Arc::new(MemoryKv::default());
    assert_eq!(
        sign_document_with_policies(&services(&http, &kv), &source_svg, Surface::Page, Some(&[]),),
        Some(signed_svg)
    );
    assert_eq!(http.recorded_request_methods(), ["POST", "POST"]);
    let requests = http.recorded_request_bodies();
    assert!(contains(&requests[0], SOURCE));
    assert!(contains(&requests[1], child_signed_svg.as_bytes()));
    assert!(!contains(&requests[1], source_svg.as_bytes()));
}

#[test]
fn inline_svg_that_breaks_out_is_reported_without_a_call() {
    let http = Arc::new(StubHttpClient::new());
    let kv = Arc::new(MemoryKv::default());

    let output = sign_document(
        &services(&http, &kv),
        "<svg viewBox=\"0 0 1 1\"><p>x</p></svg>",
        Surface::Page,
    );

    assert_eq!(output, None);
    assert!(
        http.recorded_request_methods().is_empty(),
        "should not call"
    );
}

fn job<'a>(
    reader_url: &'a str,
    source: &'a Bytes,
    sha256: &'a str,
    reference: Option<&'a str>,
    origin_host: Option<&'a str>,
) -> MediaJob<'a> {
    MediaJob {
        reader_url,
        content_type: "image/png",
        source,
        source_sha256: sha256,
        source_reference: reference,
        source_origin_host: origin_host,
        optimizer_output: false,
    }
}

fn deliver(
    services: &RuntimeServices,
    job: &MediaJob<'_>,
) -> Result<DeliveredMedia, MediaSigningFailure> {
    let media = EncypherMedia::new(signer());
    block_on(media.sign(services, job))
}

const ORIGIN: Option<&str> = Some("origin.example.com");

#[test]
fn delivered_media_outside_the_claim_with_other_bytes_or_no_origin_host_is_refused() {
    let http = Arc::new(StubHttpClient::new());
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);
    let source = Bytes::from_static(SOURCE);
    let sha = sha256_hex(SOURCE);
    let reader = "https://publisher.example.com/a.png";

    let outside = deliver(
        &services,
        &job("https://cdn.example.org/a.png", &source, &sha, None, ORIGIN),
    )
    .expect_err("should refuse another host");
    assert_eq!(outside.reason, "outside_claim");
    let other = "0".repeat(64);
    let mismatch = deliver(&services, &job(reader, &source, &other, None, ORIGIN))
        .expect_err("should refuse other bytes");
    assert_eq!(mismatch.reason, "source_digest_mismatch");
    for host in [
        None,
        Some(""),
        Some("origin.example.com:8443"),
        Some("user@origin.example.com"),
        Some("origin.example.com/images"),
        Some("Origin.example.com"),
    ] {
        let refused = deliver(&services, &job(reader, &source, &sha, None, host))
            .expect_err("should refuse a missing or malformed origin host");
        assert_eq!(refused.reason, "invalid_origin_host", "{host:?}");
    }
    assert!(
        http.recorded_request_methods().is_empty(),
        "should not call"
    );
}

#[test]
fn delivered_media_carries_a_link_for_a_remote_manifest_and_only_a_safe_reference() {
    for (reference, origin_host, expected_reference, expected_origin) in [
        (
            Some("https://publisher.example.com/images/a.png"),
            "publisher.example.com",
            "\"reference\":\"https://publisher.example.com/images/a.png\"",
            "\"origin_host\":\"publisher.example.com\"",
        ),
        (
            Some("https://origin.example.com/images/a.png"),
            "origin.example.com",
            "\"reference\":null",
            "\"origin_host\":\"origin.example.com\"",
        ),
        // A fetched address with a query is no reference, but its host is
        // still the origin's.
        (
            None,
            "origin.example.com",
            "\"reference\":null",
            "\"origin_host\":\"origin.example.com\"",
        ),
    ] {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
            200,
            serde_json::to_vec(&json!({"policy_digest": POLICY_DIGEST})).expect("JSON"),
        );
        http.push_response_with_headers(200, SOURCE.to_vec(), external_answer(SOURCE));
        let kv = Arc::new(MemoryKv::default());
        let services = services(&http, &kv);
        let source = Bytes::from_static(SOURCE);
        let sha = sha256_hex(SOURCE);

        let delivered = deliver(
            &services,
            &job(
                "https://publisher.example.com/images/a.png",
                &source,
                &sha,
                reference,
                Some(origin_host),
            ),
        )
        .expect("should sign");

        assert_eq!(
            delivered.bytes.as_ref(),
            SOURCE,
            "should deliver the answer's bytes"
        );
        assert_eq!(delivered.content_type, "image/png");
        assert_eq!(
            delivered.namespace,
            format!(
                "{NAMESPACE_VERSION}:{POLICY_DIGEST}:{}",
                signer().cache_context
            ),
            "should name the policy it was signed under for core's kept copy"
        );
        let named = |name: &str| {
            delivered
                .headers
                .iter()
                .find(|(header, _)| header.as_str() == name)
                .map(|(_, value)| value.to_str().expect("text").to_owned())
        };
        assert_eq!(
            named("link").as_deref(),
            Some("<https://api.example.com/manifests/doc-1>; rel=\"c2pa-manifest\""),
            "should name the remote manifest on every response"
        );
        assert_eq!(named("x-encypher-document-id").as_deref(), Some("doc-1"));
        let body = &http.recorded_request_bodies()[1];
        assert!(
            contains(body, expected_reference.as_bytes()),
            "should send {expected_reference}"
        );
        assert!(
            contains(body, expected_origin.as_bytes()),
            "should send {expected_origin}"
        );
        assert!(
            contains(body, b"\"http_link_header\":true"),
            "should say a Link can be delivered"
        );
    }
}

#[test]
fn an_optimizer_answer_is_sent_requiring_its_manifest_and_bound_by_its_bytes() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response(
        200,
        serde_json::to_vec(&json!({"policy_digest": POLICY_DIGEST})).expect("JSON"),
    );
    http.push_response_with_headers(200, SOURCE.to_vec(), external_answer(SOURCE));
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);
    let source = Bytes::from_static(SOURCE);
    let sha = sha256_hex(SOURCE);
    let optimized = MediaJob {
        optimizer_output: true,
        ..job(
            "https://publisher.example.com/images/a.png",
            &source,
            &sha,
            Some("https://publisher.example.com/images/a.png"),
            Some("publisher.example.com"),
        )
    };

    deliver(&services, &optimized).expect("should sign");

    let body = &http.recorded_request_bodies()[1];
    assert!(
        contains(body, b"\"require_embedded_manifest\":true"),
        "should require the optimizer's embedded manifest"
    );
    assert!(
        contains(body, b"\"reference\":null"),
        "should not name the address the optimizer read as the source"
    );
    assert!(
        contains(body, b"\"operations\":[]"),
        "should report no operation of the edge's own"
    );
}

#[test]
fn a_result_is_never_reused_for_the_same_bytes_from_another_origin() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);
    let signer = signer();
    let sha = sha256_hex(SOURCE);

    for host in ["origin-a.example.com", "origin-b.example.com"] {
        let source = MediaSource {
            origin_host: Some(host),
            ..png_source(&sha)
        };
        let (_, reason) = sign(&signer, &services, &source).expect("should sign");
        assert_eq!(reason, Reason::Signed, "{host}: should ask the endpoint");
    }
    assert_eq!(
        http.recorded_request_methods().len(),
        2,
        "should bind each result to its origin"
    );
    let headers = http.recorded_request_headers();
    assert_ne!(
        header(&headers[0], "idempotency-key"),
        header(&headers[1], "idempotency-key"),
        "should replay neither under the other's key"
    );
}

#[test]
fn media_is_recognized_by_its_type_on_every_path_but_excluded_prefixes() {
    let media = EncypherMedia::new(signer());
    for path in [
        "/images/a.png",
        "/video/clip.MOV",
        "/audio/a.mp3",
        "/img/123",
        "/media",
    ] {
        assert!(media.handles_path(path), "should consider {path}");
    }
    for path in [
        "/api/v1/a.png",
        "/wp-admin/x.jpg",
        "/.well-known/a.png",
        "/feed/",
    ] {
        assert!(!media.handles_path(path), "should exclude {path}");
    }
    for media_type in [
        "image/webp",
        "image/svg+xml",
        "video/mp4",
        "video/webm",
        "audio/mpeg",
        "font/woff2",
    ] {
        assert!(
            media.handles_media_type(media_type),
            "should sign {media_type}"
        );
    }
    for media_type in [
        "image/",
        "text/css",
        "text/html",
        "application/pdf",
        "video/iso.segment",
    ] {
        assert!(
            !media.handles_media_type(media_type),
            "should not sign {media_type}"
        );
    }
    assert_eq!(media.max_source_bytes(), 5_000_000, "should bound captures");
}

#[test]
fn the_kept_copy_namespace_fails_closed_on_revocation_however_warm_the_caches() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response(
        200,
        serde_json::to_vec(&json!({"policy_digest": POLICY_DIGEST})).expect("JSON"),
    );
    http.push_response(403, b"{}".to_vec());
    let kv = Arc::new(MemoryKv::default());
    let runtime = services(&http, &kv);
    let media = EncypherMedia::new(signer());
    // The article path's policy cache holds a fresh digest throughout.
    let signer = signer();
    kv.set(
        &signer.policy_key,
        Bytes::from(
            serde_json::to_vec(&json!({
                "kind": "digest",
                "policy_digest": POLICY_DIGEST,
                "expires_at": unix_now() + 300,
            }))
            .expect("JSON"),
        ),
    );

    let namespace = block_on(media.namespace(&runtime)).expect("should answer a namespace");
    assert!(
        namespace.contains(POLICY_DIGEST),
        "should bind the policy digest: {namespace}"
    );
    assert!(
        !namespace.contains(TOKEN),
        "should never carry the credential"
    );
    let failure = block_on(media.namespace(&runtime))
        .expect_err("should refuse a revoked credential with the cached digest still fresh");
    assert_eq!(failure.reason, "forbidden");
}

#[test]
fn a_warm_signed_result_is_not_delivered_under_a_revoked_credential() {
    let http = Arc::new(StubHttpClient::new());
    http.push_response(
        200,
        serde_json::to_vec(&json!({"policy_digest": POLICY_DIGEST})).expect("JSON"),
    );
    http.push_response_with_headers(200, SIGNED.to_vec(), answer(SIGNED, "image/png"));
    http.push_response(403, b"{}".to_vec());
    let kv = Arc::new(MemoryKv::default());
    let runtime = services(&http, &kv);
    let source = Bytes::from_static(SOURCE);
    let sha = sha256_hex(SOURCE);
    let job = job(
        "https://publisher.example.com/images/a.png",
        &source,
        &sha,
        None,
        ORIGIN,
    );
    deliver(&runtime, &job).expect("should sign and cache the result");
    assert!(
        !kv.keys_with(MEDIA_KEY_PREFIX).is_empty(),
        "should hold the signed result"
    );

    let refused = deliver(&runtime, &job).expect_err("should refuse the revoked credential");
    assert_eq!(refused.reason, "forbidden");
    assert_eq!(
        http.recorded_request_methods(),
        ["GET", "POST", "GET"],
        "should neither sign again nor deliver the cached result"
    );
}
