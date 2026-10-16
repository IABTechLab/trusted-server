//! Official publication checks, retained asset streaming and the read-only
//! verification report.
//!
//! Publication and signature claims here are synthetic semantic fixtures;
//! they do not qualify the pinned verifier or GitHub signing service.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde_json::json;

use super::fixture::{ASSET, COMMIT, RUN, Signed, TAG, consistent};
use super::{ZERO_DIGEST, context};
use crate::release::artifact::{
    AssetRole, RetainedDownload, VerificationReport, VerifiedReleaseAsset, stream_into,
};
use crate::release::bounds::{self, AttemptDeadline};
use crate::release::documents::ReleaseTag;
use crate::release::environment::{CallerEnvironment, GithubToken};
use crate::release::error::ReleaseError;
use crate::release::evidence::ComparisonOutcome;
use crate::release::github::{API_REPOSITORY, fetch_json, release_request, tag_ref_request};
use crate::release::policy::AcceptedProducerPolicy;
use crate::release::source_policy::plain_http_policy_client_for_tests;

const MIB: usize = 1024 * 1024;

#[test]
fn synthetic_publication_rejects_wrong_repository_and_statement_types() {
    for (path, replacement) in [
        (vec!["predicate", "repository"], json!("fictional/other")),
        (vec!["_type"], json!("https://example.com/Statement/v1")),
        (vec!["predicateType"], json!("https://example.com/release")),
    ] {
        let mut signed = consistent();
        let mut value = &mut signed.publication.statement["verificationResult"]["statement"];
        for key in path {
            value = &mut value[key];
        }
        *value = replacement;
        rejects_publication(&signed, "wrong repository or statement type");
    }
}

#[test]
fn synthetic_publication_rejects_ambiguous_commit_subjects() {
    for replacement in [
        json!({"digest": {"sha1": COMMIT}}),
        json!({"uri": "pkg:github/fictional/other@v1.2.0", "digest": {"sha1": COMMIT}}),
        json!({"uri": format!("pkg:github/IABTechLab/trusted-server@{TAG}"), "digest": {"sha1": COMMIT, "sha256": ZERO_DIGEST}}),
        json!({"uri": format!("pkg:github/IABTechLab/trusted-server@{TAG}"), "name": "", "digest": {"sha1": COMMIT}}),
    ] {
        let mut signed = consistent();
        signed.publication.statement["verificationResult"]["statement"]["subject"][0] = replacement;
        rejects_publication(&signed, "ambiguous commit subject");
    }
}

#[test]
fn synthetic_report_rejects_asset_from_another_publication() {
    let first = consistent();
    let publication = first
        .publish()
        .expect("should verify synthetic publication");
    let executable = retained(ASSET, AssetRole::Executable, &first.documents.executable);
    let mut asset = VerifiedReleaseAsset::from_publication(&publication, executable)
        .expect("should bind synthetic executable");
    let mut other = super::fixture::Graph::new();
    other.executable = b"different synthetic executable".to_vec();
    other.spdx["packages"][0]["checksums"][0]["checksumValue"] =
        json!(crate::release::documents::sha256_hex(&other.executable));
    other.build_evidence["artifact_sha256"] =
        json!(crate::release::documents::sha256_hex(&other.executable));
    let other_publication = other
        .build()
        .sign()
        .publish()
        .expect("should verify second synthetic publication");
    assert!(
        VerificationReport::new(&other_publication, &mut asset).is_err(),
        "should reject cross-publication report"
    );
}

fn rejects_publication(signed: &Signed, why: &str) {
    assert_eq!(
        context(signed.publish()),
        ReleaseError::Publication,
        "should reject: {why}"
    );
}

fn retained(name: &str, role: AssetRole, bytes: &[u8]) -> crate::release::artifact::RetainedAsset {
    let mut download = RetainedDownload::start(name, role).expect("should create retained file");
    download
        .push(bytes)
        .expect("should accept bytes within the limit");
    download.finish().expect("should finish the download")
}

#[test]
fn consistent_publication_verifies_and_reports() {
    let signed = consistent();
    let publication = signed
        .publish()
        .expect("should verify the consistent official publication");
    let executable = retained(ASSET, AssetRole::Executable, &signed.documents.executable);
    assert_eq!(executable.name(), ASSET, "should keep the retained name");
    let mut asset = VerifiedReleaseAsset::from_publication(&publication, executable)
        .expect("should bind the retained executable to verified evidence");
    assert_eq!(asset.name(), ASSET, "should keep the verified asset name");
    assert_eq!(
        publication.build().producer_policy(),
        &AcceptedProducerPolicy::Current,
        "should accept the current producer policy"
    );

    let report = VerificationReport::new(&publication, &mut asset)
        .expect("should summarize the verified asset");

    assert_eq!(report.tag, TAG, "should report the approved tag");
    assert_eq!(report.source_commit, COMMIT, "should report the source");
    assert_eq!(report.asset, ASSET, "should report the asset");
    assert_eq!(
        report.asset_length,
        signed.documents.executable.len() as u64,
        "should report the actual length"
    );
    assert_eq!(report.consumer_policy_version, 1, "should report policy v1");
    assert_eq!(
        report.consumer_policy_sha256, report.producer_policy_sha256,
        "should report the current producer policy"
    );
    assert_eq!(report.run_invocation, RUN, "should report the shared run");
    assert_eq!(
        report.comparison,
        ComparisonOutcome::Matched,
        "should report the informational comparison"
    );
}

#[test]
fn rejects_substituted_retained_executable() {
    let signed = consistent();
    let publication = signed.publish().expect("should verify publication");
    let substituted = retained(
        ASSET,
        AssetRole::Executable,
        b"substituted executable bytes",
    );

    let result = VerifiedReleaseAsset::from_publication(&publication, substituted);

    assert_eq!(
        context(result),
        ReleaseError::Asset,
        "should reject retained bytes that differ from verified evidence"
    );
}

#[test]
fn rejects_unconsumed_or_companion_asset() {
    let signed = consistent();
    let publication = signed.publish().expect("should verify publication");
    let other = retained(
        "ts-x86_64-unknown-linux-gnu",
        AssetRole::Executable,
        &signed.documents.executable,
    );
    assert_eq!(
        context(VerifiedReleaseAsset::from_publication(&publication, other)),
        ReleaseError::Asset,
        "should reject an asset that verification did not consume"
    );

    let companion = retained(ASSET, AssetRole::Companion, &signed.documents.executable);
    assert_eq!(
        context(VerifiedReleaseAsset::from_publication(
            &publication,
            companion
        )),
        ReleaseError::Asset,
        "should reject a non-executable retained role"
    );
}

#[test]
fn rejects_rest_length() {
    let mut signed = consistent();
    signed.publication.release["assets"][0]["size"] = json!(1);

    rejects_publication(&signed, "REST size differs from the signed length");
}

#[test]
fn rejects_rest_digest() {
    let mut signed = consistent();
    signed.publication.release["assets"][3]["digest"] = json!(format!("sha256:{ZERO_DIGEST}"));

    rejects_publication(&signed, "REST digest differs from the signed digest");
}

#[test]
fn rejects_mutable_draft_or_prerelease_release() {
    let edits = [
        ("immutable", json!(false)),
        ("immutable", json!(null)),
        ("draft", json!(true)),
        ("prerelease", json!(true)),
        ("tag_name", json!("v1.2.1")),
    ];
    for (field, value) in edits {
        let mut signed = consistent();
        signed.publication.release[field] = value;

        rejects_publication(&signed, &format!("release `{field}` is not acceptable"));
    }
}

#[test]
fn rejects_annotated_or_retargeted_tag() {
    let mut annotated = consistent();
    annotated.publication.tag_ref["object"]["type"] = json!("tag");
    rejects_publication(&annotated, "annotated tag object");

    let mut retargeted = consistent();
    retargeted.publication.tag_ref["object"]["sha"] =
        json!("89abcdef0123456789abcdef0123456789abcdef");
    rejects_publication(&retargeted, "tag targets another commit");
}

#[test]
fn rejects_extra_missing_and_partial_release_assets() {
    let mut extra = consistent();
    extra.publication.release["assets"]
        .as_array_mut()
        .expect("should hold assets")
        .push(json!({"name": "unlisted.bin", "size": 1, "state": "uploaded"}));
    rejects_publication(&extra, "release carries an unlisted asset");

    let mut missing = consistent();
    missing.publication.release["assets"]
        .as_array_mut()
        .expect("should hold assets")
        .pop();
    rejects_publication(&missing, "release lacks SHA256SUMS");

    let mut partial = consistent();
    partial.publication.release["assets"][1]["state"] = json!("starter");
    rejects_publication(&partial, "release asset is not fully uploaded");
}

#[test]
fn rejects_release_statement_edges() {
    let mut digest = consistent();
    digest.publication.statement["verificationResult"]["statement"]["subject"][2]["digest"]["sha256"] =
        json!(ZERO_DIGEST);
    rejects_publication(&digest, "signed release asset digest differs");

    let mut name = consistent();
    name.publication.statement["verificationResult"]["statement"]["subject"][2]["name"] =
        json!("renamed.bin");
    rejects_publication(&name, "signed release asset name differs");

    let mut tag = consistent();
    tag.publication.statement["verificationResult"]["statement"]["predicate"]["tag"] =
        json!("v1.2.1");
    rejects_publication(&tag, "release statement names another tag");

    let mut commit = consistent();
    commit.publication.statement["verificationResult"]["statement"]["subject"][0]["digest"]["sha1"] =
        json!("89abcdef0123456789abcdef0123456789abcdef");
    rejects_publication(&commit, "release statement binds another commit");

    let mut signer = consistent();
    signer.publication.statement["verificationResult"]["signature"]["certificate"]["subjectAlternativeName"] =
        json!("https://releases.example.com");
    rejects_publication(&signer, "release statement has another signer");
}

#[test]
fn rejects_altered_index_bundle_or_checksums() {
    let mut bundle = consistent();
    bundle.publication.index_bundle.push(b' ');
    rejects_publication(
        &bundle,
        "index bundle bytes differ from REST and signed membership",
    );

    let mut checksums = consistent();
    checksums.publication.checksums.push(b' ');
    rejects_publication(&checksums, "checksum list differs from signed membership");
}

#[test]
fn oversized_stream_is_aborted_mid_download() {
    let mut download =
        RetainedDownload::start(ASSET, AssetRole::Spdx).expect("should create retained file");
    let chunk = vec![b'x'; MIB];
    for _ in 0..16 {
        download
            .push(&chunk)
            .expect("should accept bytes up to 16 MiB");
    }

    let result = download.push(b"y");

    assert_eq!(
        context(result),
        ReleaseError::Asset,
        "should abort the transfer at the first byte past the SPDX limit"
    );
}

#[test]
fn role_limits_match_r_bounds() {
    assert_eq!(
        AssetRole::Executable.limit(),
        bounds::EXECUTABLE_BYTES,
        "512 MiB"
    );
    assert_eq!(
        AssetRole::Companion.limit(),
        bounds::COMPANION_BYTES,
        "32 MiB"
    );
    assert_eq!(AssetRole::Spdx.limit(), bounds::SPDX_BYTES, "16 MiB");
    assert_eq!(AssetRole::Metadata.limit(), bounds::METADATA_BYTES, "1 MiB");
}

/// Serves one chunked response of `total` bytes, ignoring client aborts.
fn serve_chunked(total: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind loopback");
    let address = listener.local_addr().expect("should have address");
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        let chunk = vec![b'z'; 64 * 1024];
        let mut sent = 0;
        while sent < total {
            let header = format!("{:x}\r\n", chunk.len());
            let written = stream
                .write_all(header.as_bytes())
                .and_then(|()| stream.write_all(&chunk))
                .and_then(|()| stream.write_all(b"\r\n"));
            if written.is_err() {
                return;
            }
            sent += chunk.len();
        }
        let _ = stream.write_all(b"0\r\n\r\n");
    });
    format!("http://{address}/asset")
}

#[tokio::test]
async fn streamed_download_aborts_past_role_limit() {
    let url = serve_chunked(4 * MIB);
    let client = plain_http_policy_client_for_tests().expect("should build client");
    let response = client.get(url).send().await.expect("should connect");
    let download =
        RetainedDownload::start("SHA256SUMS", AssetRole::Metadata).expect("should create file");
    let deadline = AttemptDeadline::at(Instant::now() + Duration::from_secs(30));

    let result = stream_into(response, download, deadline).await;

    assert_eq!(
        context(result),
        ReleaseError::Asset,
        "should abort a chunked body once it passes the 1 MiB metadata limit"
    );
}

#[tokio::test]
async fn streamed_download_within_limit_is_measured() {
    let url = serve_chunked(256 * 1024);
    let client = plain_http_policy_client_for_tests().expect("should build client");
    let response = client.get(url).send().await.expect("should connect");
    let download =
        RetainedDownload::start("SHA256SUMS", AssetRole::Metadata).expect("should create file");
    let deadline = AttemptDeadline::at(Instant::now() + Duration::from_secs(30));

    let mut asset = stream_into(response, download, deadline)
        .await
        .expect("should retain a body within the limit");

    assert_eq!(
        asset.measurement().length(),
        256 * 1024,
        "should count bytes"
    );
    let bytes = asset
        .document_bytes()
        .expect("should reread retained bytes");
    assert_eq!(bytes, vec![b'z'; 256 * 1024], "should retain exact bytes");
}

#[test]
fn rest_requests_use_fixed_official_paths_and_explicit_token() {
    let tag = ReleaseTag::parse(TAG).expect("should accept fictional tag");
    let environment = CallerEnvironment::from_pairs([("GH_TOKEN", "fictional-read-only-token")]);
    let token = GithubToken::select(&environment).expect("should select explicit token");

    let release = release_request(&tag, &token).expect("should build release request");
    let tag_ref = tag_ref_request(&tag, &token).expect("should build ref request");

    assert_eq!(
        release.url().as_str(),
        format!("{API_REPOSITORY}/releases/tags/v1.2.0"),
        "should target the fixed release-by-tag path"
    );
    assert_eq!(
        tag_ref.url().as_str(),
        format!("{API_REPOSITORY}/git/ref/tags/v1.2.0"),
        "should target the fixed tag ref path"
    );
    for request in [&release, &tag_ref] {
        assert_eq!(
            request.url().host_str(),
            Some("api.github.com"),
            "should use the fixed API host"
        );
        assert_eq!(
            request
                .headers()
                .get(ACCEPT)
                .map(reqwest::header::HeaderValue::as_bytes),
            Some(&b"application/vnd.github+json"[..]),
            "should request JSON"
        );
        let authorization = request
            .headers()
            .get(AUTHORIZATION)
            .expect("should carry the explicit token");
        assert!(
            authorization.is_sensitive(),
            "should mark the token sensitive"
        );
    }
}

/// Serves one fixed raw HTTP response.
fn serve_once(response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind loopback");
    let address = listener.local_addr().expect("should have address");
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(&response);
    });
    format!("http://{address}/repos/release")
}

async fn fetch_loopback(response: Vec<u8>) -> Result<serde_json::Value, ReleaseError> {
    let url = serve_once(response);
    let client = plain_http_policy_client_for_tests().expect("should build client");
    let environment = CallerEnvironment::from_pairs([("GH_TOKEN", "fictional-read-only-token")]);
    let token = GithubToken::select(&environment).expect("should select explicit token");
    let request = client.get(url).build().expect("should build request");
    let deadline = AttemptDeadline::at(Instant::now() + Duration::from_secs(30));
    fetch_json(&client, request, &token, deadline)
        .await
        .map_err(|report| *report.current_context())
}

fn http(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

#[tokio::test]
async fn fetch_json_parses_bounded_strict_json() {
    let value = fetch_loopback(http("200 OK", "", br#"{"immutable":true}"#))
        .await
        .expect("should parse a bounded JSON body");

    assert_eq!(
        value,
        json!({"immutable": true}),
        "should return parsed JSON"
    );
}

#[tokio::test]
async fn fetch_json_rejects_redirect_oversize_and_duplicates() {
    let redirect = http(
        "302 Found",
        "Location: https://example.com/elsewhere\r\n",
        b"",
    );
    assert_eq!(
        fetch_loopback(redirect).await.expect_err("should reject"),
        ReleaseError::Publication,
        "should reject a redirect instead of following it"
    );

    let oversize = http("200 OK", "", &vec![b' '; MIB + 1]);
    assert_eq!(
        fetch_loopback(oversize).await.expect_err("should reject"),
        ReleaseError::Publication,
        "should reject a body over 1 MiB"
    );

    let duplicate = http("200 OK", "", br#"{"immutable":false,"immutable":true}"#);
    assert_eq!(
        fetch_loopback(duplicate).await.expect_err("should reject"),
        ReleaseError::Document,
        "should reject duplicate keys in REST JSON"
    );
}
