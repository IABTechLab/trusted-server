//! R-PUBLICATION: official GitHub release, tag and release-statement checks.
//!
//! Requests go only to fixed official REST paths on the fixed API host with
//! an explicit read-only credential, no redirects and bounded bodies. The
//! checks themselves are pure functions over the fetched JSON so they are
//! exercised without network access.
//!
//! Field names were confirmed from the pinned gh 2.97.0 REST models:
//! release `tag_name`/`draft`/`prerelease`/`immutable`/`assets[].name`/
//! `size`/`digest`/`state` in
//! <https://github.com/cli/cli/blob/v2.97.0/pkg/cmd/release/shared/fetch.go>,
//! the tag ref `object.sha` in the same file (`object.type` is from the
//! GitHub git refs REST documentation), and the release attestation `tag`
//! predicate field and release-ref subject in
//! <https://github.com/cli/cli/blob/v2.97.0/pkg/cmd/release/shared/attestation.go>
//! and <https://github.com/cli/cli/blob/v2.97.0/pkg/cmd/release/verify/verify.go>.

use std::collections::BTreeMap;

use error_stack::Report;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderName, HeaderValue, USER_AGENT};
use reqwest::{Client, Method, Request, StatusCode};
use serde_json::{Map, Value};
use url::Url;

use super::artifact::AssetMeasurement;
use super::bounds::{self, AttemptDeadline};
use super::documents::{ReleaseTag, Role, lower_hex, parse_json};
use super::environment::GithubToken;
use super::error::{ReleaseError, report};
use super::evidence::{BuildEvidenceVerified, INDEX_NAME};
use super::gh::{VerificationMode, VerifiedReleaseStatement};
use super::source_policy::BoundedBody;

/// Fixed official repository REST root; only the tag varies below it.
pub(crate) const API_REPOSITORY: &str = "https://api.github.com/repos/IABTechLab/trusted-server"; // allow-domain: api.github.com
/// Certificate SAN of GitHub's release attestation signer, enforced by gh.
const RELEASE_SIGNER: &str = "https://dotcom.releases.github.com"; // allow-domain: dotcom.releases.github.com
/// Name of the external index provenance bundle.
pub(crate) const INDEX_BUNDLE_NAME: &str = "release-index.provenance.sigstore.json";
/// Name of the convenience checksum list.
pub(crate) const CHECKSUMS_NAME: &str = "SHA256SUMS";
const JSON_MEDIA_TYPE: &str = "application/vnd.github+json";
const API_VERSION: &str = "2022-11-28";

fn publication_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    report(ReleaseError::Publication, message)
}

fn rest_request(path: &str, token: &GithubToken) -> Result<Request, Report<ReleaseError>> {
    let url = Url::parse(&format!("{API_REPOSITORY}/{path}"))
        .map_err(|source| report(ReleaseError::Publication, source))?;
    let mut request = Request::new(Method::GET, url);
    let mut authorization = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|_| publication_error("GitHub token is not a valid header value"))?;
    authorization.set_sensitive(true);
    let headers = request.headers_mut();
    headers.insert(ACCEPT, HeaderValue::from_static(JSON_MEDIA_TYPE));
    headers.insert(
        HeaderName::from_static("x-github-api-version"),
        HeaderValue::from_static(API_VERSION),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("trusted-server-cli"));
    headers.insert(AUTHORIZATION, authorization);
    Ok(request)
}

/// Builds the fixed release-by-tag request.
///
/// # Errors
///
/// Returns [`ReleaseError::Publication`] if the request cannot be encoded.
pub(crate) fn release_request(
    tag: &ReleaseTag,
    token: &GithubToken,
) -> Result<Request, Report<ReleaseError>> {
    rest_request(&format!("releases/tags/{}", tag.as_str()), token)
}

/// Builds the fixed lightweight tag ref request.
///
/// # Errors
///
/// Returns [`ReleaseError::Publication`] if the request cannot be encoded.
pub(crate) fn tag_ref_request(
    tag: &ReleaseTag,
    token: &GithubToken,
) -> Result<Request, Report<ReleaseError>> {
    rest_request(&format!("git/ref/tags/{}", tag.as_str()), token)
}

/// Executes a fixed REST request and strictly parses its bounded JSON body.
///
/// The client must not follow redirects (see
/// [`super::source_policy::policy_client`]); any 3xx, non-200 status, final
/// URL change or body over 1 MiB fails.
///
/// # Errors
///
/// Returns [`ReleaseError::Publication`] for transport, status, origin or
/// size failures and [`ReleaseError::Timeout`] after the attempt deadline.
pub(crate) async fn fetch_json(
    client: &Client,
    request: Request,
    token: &GithubToken,
    deadline: AttemptDeadline,
) -> Result<Value, Report<ReleaseError>> {
    let request_url = request.url().clone();
    let fetch = async {
        let mut response = client
            .execute(request)
            .await
            .map_err(|error| publication_error(token.redact(&error.to_string())))?;
        if response.status() != StatusCode::OK || response.url() != &request_url {
            return Err(publication_error(format!(
                "official REST endpoint returned {} (redirects are rejected)",
                response.status()
            )));
        }
        let mut body = BoundedBody::new(bounds::METADATA_BYTES);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| publication_error(token.redact(&error.to_string())))?
        {
            body.push(&chunk).map_err(|error| {
                error
                    .change_context(ReleaseError::Publication)
                    .attach("REST response exceeds 1 MiB")
            })?;
        }
        parse_json(&body.into_bytes(), Role::Metadata)
    };
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.instant()), fetch)
        .await
        .map_err(|_| report(ReleaseError::Timeout, "REST request exceeded the deadline"))?
}

/// Every asset the official release must contain, with exact length and
/// SHA-256.
fn expected_assets(
    build: &BuildEvidenceVerified,
    index_bundle: &AssetMeasurement,
    checksums: &AssetMeasurement,
) -> BTreeMap<String, AssetMeasurement> {
    let index = build.index();
    let mut assets = BTreeMap::new();
    let mut add = |name: &str, size: u64, sha256: &str| {
        assets.insert(name.to_owned(), AssetMeasurement::declared(sha256, size));
    };
    add(
        &index.tsjs_inventory.name,
        index.tsjs_inventory.size,
        &index.tsjs_inventory.sha256,
    );
    for artifact in &index.artifacts {
        add(&artifact.name, artifact.size, &artifact.sha256);
        let companions = &artifact.companions;
        for record in [
            &companions.sbom,
            &companions.provenance,
            &companions.sbom_attestation,
            &companions.build_evidence,
            &companions.build_recipe,
            &companions.rebuild_check,
        ] {
            add(&record.name, record.size, &record.sha256);
        }
    }
    let measured = [
        (INDEX_NAME, build.index_measurement()),
        (INDEX_BUNDLE_NAME, index_bundle),
        (CHECKSUMS_NAME, checksums),
    ];
    for (name, measurement) in measured {
        add(name, measurement.length(), measurement.sha256());
    }
    assets
}

fn is_prerelease(tag: &ReleaseTag) -> bool {
    let version = tag.as_str().split('+').next().unwrap_or_default();
    version.contains('-')
}

/// Checks the release-by-tag REST object against the expected asset set.
///
/// # Errors
///
/// Returns [`ReleaseError::Publication`] unless the release is the published,
/// immutable release of `tag` with a prerelease flag matching the tag and
/// exactly the expected assets, each uploaded with the expected REST size
/// (and digest, when GitHub reports one).
pub(crate) fn check_release(
    release: &Value,
    tag: &ReleaseTag,
    expected: &BTreeMap<String, AssetMeasurement>,
) -> Result<(), Report<ReleaseError>> {
    if release.get("tag_name").and_then(Value::as_str) != Some(tag.as_str()) {
        return Err(publication_error("REST release is for another tag"));
    }
    if release.get("draft") != Some(&Value::Bool(false)) {
        return Err(publication_error("release is a draft, not published"));
    }
    if release.get("prerelease") != Some(&Value::Bool(is_prerelease(tag))) {
        return Err(publication_error(
            "release prerelease flag does not match the tag",
        ));
    }
    if release.get("immutable") != Some(&Value::Bool(true)) {
        return Err(publication_error("release is not immutable"));
    }
    let listed = release_assets(release)?;
    if listed.len() != expected.len() || listed.keys().ne(expected.keys()) {
        return Err(publication_error(
            "release assets are not exactly the signed release asset set",
        ));
    }
    for (name, (size, digest)) in &listed {
        let wanted = expected
            .get(*name)
            .ok_or_else(|| publication_error(format!("unexpected release asset `{name}`")))?;
        if *size != wanted.length() {
            return Err(publication_error(format!(
                "REST size of `{name}` differs from the signed and actual length"
            )));
        }
        if digest.is_some_and(|digest| digest != format!("sha256:{}", wanted.sha256())) {
            return Err(publication_error(format!(
                "REST digest of `{name}` differs from the signed digest"
            )));
        }
    }
    Ok(())
}

type ListedAssets<'a> = BTreeMap<&'a str, (u64, Option<&'a str>)>;

fn release_assets(release: &Value) -> Result<ListedAssets<'_>, Report<ReleaseError>> {
    let assets = release
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| publication_error("REST release has no asset list"))?;
    let mut listed = BTreeMap::new();
    for asset in assets {
        let name = asset.get("name").and_then(Value::as_str);
        let size = asset.get("size").and_then(Value::as_u64);
        let (Some(name), Some(size)) = (name, size) else {
            return Err(publication_error("REST asset lacks a name or size"));
        };
        if asset.get("state").and_then(Value::as_str) != Some("uploaded") {
            return Err(publication_error(format!(
                "release asset `{name}` is not fully uploaded"
            )));
        }
        let digest = asset.get("digest").and_then(Value::as_str);
        if listed.insert(name, (size, digest)).is_some() {
            return Err(publication_error(format!(
                "release lists asset `{name}` twice"
            )));
        }
    }
    Ok(listed)
}

/// Checks that the tag is lightweight and targets the approved commit.
///
/// # Errors
///
/// Returns [`ReleaseError::Publication`] for another ref, an annotated tag
/// object or another commit.
pub(crate) fn check_tag_ref(
    tag_ref: &Value,
    tag: &ReleaseTag,
    commit: &str,
) -> Result<(), Report<ReleaseError>> {
    if tag_ref.get("ref").and_then(Value::as_str) != Some(&format!("refs/tags/{}", tag.as_str())) {
        return Err(publication_error("REST ref is not the release tag"));
    }
    let object = tag_ref.get("object");
    if object
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        != Some("commit")
    {
        return Err(publication_error(
            "release tag must be lightweight; annotated tag objects are rejected",
        ));
    }
    if object
        .and_then(|object| object.get("sha"))
        .and_then(Value::as_str)
        != Some(commit)
    {
        return Err(publication_error(
            "release tag does not target the approved commit",
        ));
    }
    Ok(())
}

/// Checks the verified GitHub release attestation (`gh release verify
/// --format json`) binds the tag, approved commit and every asset digest.
///
/// # Errors
///
/// Returns [`ReleaseError::Publication`] for another signer, tag or commit,
/// or signed asset names/digests that differ from the expected set.
pub(crate) fn check_release_statement(
    verified: &Value,
    tag: &ReleaseTag,
    commit: &str,
    expected: &BTreeMap<String, AssetMeasurement>,
) -> Result<(), Report<ReleaseError>> {
    let result = verified.get("verificationResult");
    let field = |path: &[&str]| {
        path.iter()
            .try_fold(result?, |value, key| value.get(key))
            .and_then(Value::as_str)
    };
    if field(&["mediaType"])
        != Some("application/vnd.dev.sigstore.verificationresult+json;version=0.1")
        || field(&["statement", "_type"]) != Some("https://in-toto.io/Statement/v1")
        || field(&["statement", "predicateType"])
            != Some("https://in-toto.io/attestation/release/v0.1")
    {
        return Err(publication_error(
            "unsupported verified release statement type",
        ));
    }
    let purl = format!("pkg:github/{}@{}", super::policy::REPOSITORY, tag.as_str());
    for (key, wanted) in [
        ("repository", super::policy::REPOSITORY),
        ("repositoryId", super::policy::REPOSITORY_ID),
        ("ownerId", super::policy::REPOSITORY_OWNER_ID),
        ("purl", purl.as_str()),
    ] {
        if field(&["statement", "predicate", key]) != Some(wanted) {
            return Err(publication_error(format!(
                "signed release `{key}` differs from the official repository"
            )));
        }
    }
    if field(&["signature", "certificate", "subjectAlternativeName"]) != Some(RELEASE_SIGNER) {
        return Err(publication_error("release statement has another signer"));
    }
    if field(&["statement", "predicate", "tag"]) != Some(tag.as_str()) {
        return Err(publication_error("release statement names another tag"));
    }
    let subjects = result
        .and_then(|result| result.get("statement"))
        .and_then(|statement| statement.get("subject"))
        .and_then(Value::as_array)
        .ok_or_else(|| publication_error("release statement has no subjects"))?;
    let (named, commits) = release_subjects(subjects, &purl)?;
    if commits != [commit] {
        return Err(publication_error(
            "release statement does not bind exactly the approved commit",
        ));
    }
    let wanted: BTreeMap<&str, &str> = expected
        .iter()
        .map(|(name, measurement)| (name.as_str(), measurement.sha256()))
        .collect();
    if named != wanted {
        return Err(publication_error(
            "signed release asset names or digests differ from the verified assets",
        ));
    }
    Ok(())
}

type ReleaseSubjects<'a> = (BTreeMap<&'a str, &'a str>, Vec<&'a str>);

fn release_subjects<'a>(
    subjects: &'a [Value],
    purl: &str,
) -> Result<ReleaseSubjects<'a>, Report<ReleaseError>> {
    let mut named = BTreeMap::new();
    let mut commits = Vec::new();
    for subject in subjects {
        let digest = subject.get("digest").and_then(Value::as_object);
        let name = subject
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if name.is_empty() {
            if subject.as_object().map(Map::len) != Some(2)
                || subject.get("uri").and_then(Value::as_str) != Some(purl)
                || digest.map(Map::len) != Some(1)
            {
                return Err(publication_error(
                    "release commit subject must have exactly the official URI and SHA-1",
                ));
            }
            let sha1 = digest
                .and_then(|digest| digest.get("sha1"))
                .and_then(Value::as_str);
            commits.push(sha1.unwrap_or_default());
            continue;
        }
        if subject.as_object().map(Map::len) != Some(2) {
            return Err(publication_error(
                "release asset subject must have exactly name and digest",
            ));
        }
        let sha256 = digest
            .filter(|digest| digest.len() == 1)
            .and_then(|digest| digest.get("sha256"))
            .and_then(Value::as_str)
            .filter(|sha256| lower_hex(sha256, 64))
            .ok_or_else(|| publication_error("signed release asset lacks one SHA-256"))?;
        if named.insert(name, sha256).is_some() {
            return Err(publication_error(format!(
                "release statement names `{name}` twice"
            )));
        }
    }
    Ok((named, commits))
}

/// Fetched official publication data and release-wide asset measurements.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PublicationInputs<'a> {
    pub(crate) release: &'a Value,
    pub(crate) tag_ref: &'a Value,
    pub(crate) release_statement: &'a VerifiedReleaseStatement,
    pub(crate) index_bundle: &'a AssetMeasurement,
    pub(crate) checksums: &'a AssetMeasurement,
}

/// Build evidence that is also the official immutable publication.
///
/// Only [`verify_publication`] constructs it.
#[derive(Debug)]
pub(crate) struct PublicationVerified {
    build: BuildEvidenceVerified,
}

impl PublicationVerified {
    /// Returns the verified build evidence.
    pub(crate) fn build(&self) -> &BuildEvidenceVerified {
        &self.build
    }
}

/// Verifies the official immutable publication of verified build evidence.
///
/// # Errors
///
/// Returns [`ReleaseError::Publication`] if the tag is annotated or targets
/// another commit, the release is not the immutable published release with
/// exactly the expected assets and REST lengths, or the release statement
/// does not bind every asset name and digest.
pub(crate) fn verify_publication(
    build: BuildEvidenceVerified,
    inputs: &PublicationInputs<'_>,
) -> Result<PublicationVerified, Report<ReleaseError>> {
    if build.mode() != VerificationMode::Online {
        return Err(publication_error(
            "offline build evidence cannot become official publication evidence",
        ));
    }
    let tag = build.release().tag().clone();
    let commit = build.release().commit().as_str().to_owned();
    check_tag_ref(inputs.tag_ref, &tag, &commit)?;
    let expected = expected_assets(&build, inputs.index_bundle, inputs.checksums);
    check_release(inputs.release, &tag, &expected)?;
    if inputs.index_bundle != build.index_bundle() {
        return Err(publication_error(
            "published index bundle differs from the verified bundle",
        ));
    }
    let statement = inputs.release_statement.for_release(build.release())?;
    check_release_statement(statement, &tag, &commit, &expected)?;
    Ok(PublicationVerified { build })
}
