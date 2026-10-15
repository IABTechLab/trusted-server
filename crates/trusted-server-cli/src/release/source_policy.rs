//! Producer-policy bytes from an independently approved source.
//!
//! [`ApprovedSourcePolicy`] can only be built from the fixed official GitHub
//! Contents API endpoint at an approved full commit, or from an explicitly
//! transferred approved-source snapshot. The release index never supplies the
//! ref, host, path or bytes.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use error_stack::Report;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderName, HeaderValue, USER_AGENT};
use reqwest::{Client, ClientBuilder, Method, Request, StatusCode};
use url::Url;

use super::bounds::{self, AttemptDeadline};
use super::documents::ApprovedCommit;
use super::environment::GithubToken;
use super::error::{ReleaseError, report};

/// The only producer-policy endpoint; the approved commit is the sole variable.
pub(crate) const POLICY_CONTENTS_URL: &str = "https://api.github.com/repos/IABTechLab/trusted-server/contents/crates/trusted-server-cli/release-policy.json"; // allow-domain: api.github.com
/// Fixed source-relative directories and file name of the producer policy.
const POLICY_DIRECTORIES: [&str; 2] = ["crates", "trusted-server-cli"];
const POLICY_FILE: &str = "release-policy.json";
/// Raw-content media type for the Contents API.
const RAW_MEDIA_TYPE: &str = "application/vnd.github.raw+json";
/// Reviewed GitHub REST API version header value.
const API_VERSION: &str = "2022-11-28";

/// Producer-policy bytes bound to an independently approved commit.
///
/// Only [`fetch_approved_policy`] and [`read_approved_snapshot`] construct
/// it; neither accepts a caller-chosen URL, path or byte string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovedSourcePolicy {
    commit: ApprovedCommit,
    bytes: Vec<u8>,
}

impl ApprovedSourcePolicy {
    fn bounded(commit: ApprovedCommit, bytes: Vec<u8>) -> Result<Self, Report<ReleaseError>> {
        if bytes.len() as u64 > bounds::POLICY_BYTES {
            return Err(report(
                ReleaseError::PolicyFetch,
                "approved source policy exceeds 1 MiB",
            ));
        }
        Ok(Self { commit, bytes })
    }

    /// Pairs bytes with a commit directly; test fixtures only.
    #[cfg(test)]
    pub(crate) fn for_tests(commit: ApprovedCommit, bytes: Vec<u8>) -> Self {
        Self { commit, bytes }
    }

    /// Returns the approved commit these bytes belong to.
    pub(crate) fn commit(&self) -> &ApprovedCommit {
        &self.commit
    }

    /// Returns the exact approved-source policy bytes.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The root of an independently transferred source snapshot for an explicit
/// approved commit.
///
/// The root directory is opened once at construction and held, so later
/// reads resolve relative to that descriptor rather than a re-resolved path.
/// Offline checks establish build proof only, never official deployment.
#[derive(Debug)]
pub(crate) struct ApprovedSourceRoot {
    commit: ApprovedCommit,
    directory: OwnedFd,
}

impl ApprovedSourceRoot {
    /// Opens an absolute snapshot root directory bound to its approved commit.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::PolicyFetch`] unless `root` is an absolute
    /// directory that can be opened.
    pub(crate) fn new(commit: ApprovedCommit, root: &Path) -> Result<Self, Report<ReleaseError>> {
        if !root.is_absolute() {
            return Err(report(
                ReleaseError::PolicyFetch,
                "approved source root must be absolute",
            ));
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)
            .map_err(|source| {
                report(
                    ReleaseError::PolicyFetch,
                    format!("cannot open approved source root: {source}"),
                )
            })?;
        Ok(Self {
            commit,
            directory: OwnedFd::from(directory),
        })
    }
}

fn open_at(directory: &OwnedFd, name: &str, flags: libc::c_int) -> io::Result<OwnedFd> {
    let name = CString::new(name).map_err(io::Error::other)?;
    // SAFETY: `name` is a NUL-terminated string that outlives the call and
    // `directory` is an open descriptor borrowed for its duration. No
    // `O_CREAT` flag is passed, so no mode argument is read.
    let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` just returned this new descriptor; nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

/// Reads `crates/trusted-server-cli/release-policy.json` under an approved
/// snapshot root.
///
/// Every component is opened with `openat` relative to the previously held
/// descriptor using `O_NOFOLLOW` (directories also `O_DIRECTORY`), so no
/// symlink is ever followed and no path is re-resolved; the file is then
/// checked with `fstat` on its own descriptor.
///
/// # Errors
///
/// Returns [`ReleaseError::PolicyFetch`] for a symlinked or missing
/// component, a non-regular or oversize policy file, or a read failure.
pub(crate) fn read_approved_snapshot(
    root: &ApprovedSourceRoot,
) -> Result<ApprovedSourcePolicy, Report<ReleaseError>> {
    let mut current = root
        .directory
        .try_clone()
        .map_err(|source| report(ReleaseError::PolicyFetch, source))?;
    for name in POLICY_DIRECTORIES {
        current = open_at(
            &current,
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
        .map_err(|source| {
            report(
                ReleaseError::PolicyFetch,
                format!("approved source component `{name}` must be a real directory: {source}"),
            )
        })?;
    }
    let file = File::from(
        open_at(
            &current,
            POLICY_FILE,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
        .map_err(|source| {
            report(
                ReleaseError::PolicyFetch,
                format!("cannot open approved source policy: {source}"),
            )
        })?,
    );
    let held = file
        .metadata()
        .map_err(|source| report(ReleaseError::PolicyFetch, source))?;
    if !held.is_file() || held.len() > bounds::POLICY_BYTES {
        return Err(report(
            ReleaseError::PolicyFetch,
            "approved source policy must be a regular file of at most 1 MiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take(bounds::POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| report(ReleaseError::PolicyFetch, source))?;
    ApprovedSourcePolicy::bounded(root.commit.clone(), bytes)
}

/// Builds the fixed read-only Contents API request.
///
/// # Errors
///
/// Returns [`ReleaseError::PolicyFetch`] if the request cannot be encoded.
pub(crate) fn policy_request(
    commit: &ApprovedCommit,
    token: &GithubToken,
) -> Result<Request, Report<ReleaseError>> {
    let url = Url::parse(&format!("{POLICY_CONTENTS_URL}?ref={}", commit.as_str()))
        .map_err(|source| report(ReleaseError::PolicyFetch, source))?;
    let mut request = Request::new(Method::GET, url);
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {}", token.expose())).map_err(|_| {
            report(
                ReleaseError::PolicyFetch,
                "GitHub token is not a valid header value",
            )
        })?;
    authorization.set_sensitive(true);
    let headers = request.headers_mut();
    headers.insert(ACCEPT, HeaderValue::from_static(RAW_MEDIA_TYPE));
    headers.insert(
        HeaderName::from_static("x-github-api-version"),
        HeaderValue::from_static(API_VERSION),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("trusted-server-cli"));
    headers.insert(AUTHORIZATION, authorization);
    Ok(request)
}

/// Builds the HTTPS client used for policy acquisition.
///
/// Redirects are never followed, so a credential is never forwarded to a
/// different origin or path; any 3xx is then rejected by
/// [`check_policy_response`].
///
/// # Errors
///
/// Returns [`ReleaseError::PolicyFetch`] if the client cannot be built.
pub(crate) fn policy_client() -> Result<Client, Report<ReleaseError>> {
    no_redirect_builder()
        .https_only(true)
        .build()
        .map_err(|source| report(ReleaseError::PolicyFetch, source))
}

/// The shared policy-client configuration: no redirect is ever followed.
fn no_redirect_builder() -> ClientBuilder {
    crate::tls::install_crypto_provider();
    Client::builder().redirect(reqwest::redirect::Policy::none())
}

/// Builds the production client configuration without `https_only`, so a
/// local plain-HTTP server can prove redirects are returned, not followed.
#[cfg(test)]
pub(crate) fn plain_http_policy_client_for_tests() -> Result<Client, Report<ReleaseError>> {
    no_redirect_builder()
        .build()
        .map_err(|source| report(ReleaseError::PolicyFetch, source))
}

/// Validates response status, final URL and declared length.
///
/// # Errors
///
/// Returns [`ReleaseError::PolicyFetch`] for redirects, non-200 statuses, a
/// final URL differing from the request, or a declared length over 1 MiB.
pub(crate) fn check_policy_response(
    request_url: &Url,
    status: StatusCode,
    final_url: &Url,
    content_length: Option<u64>,
) -> Result<(), Report<ReleaseError>> {
    if status.is_redirection() {
        return Err(report(
            ReleaseError::PolicyFetch,
            format!("policy endpoint redirected ({status}); redirects are rejected"),
        ));
    }
    if status != StatusCode::OK {
        return Err(report(
            ReleaseError::PolicyFetch,
            format!("policy endpoint returned {status}"),
        ));
    }
    if final_url != request_url {
        return Err(report(
            ReleaseError::PolicyFetch,
            "policy response origin or path differs from the fixed endpoint",
        ));
    }
    if content_length.is_some_and(|length| length > bounds::POLICY_BYTES) {
        return Err(report(
            ReleaseError::PolicyFetch,
            "policy response declares more than 1 MiB",
        ));
    }
    Ok(())
}

/// Accumulates a streamed response body, rejecting it once the total would
/// exceed a fixed byte limit.
#[derive(Debug)]
pub(crate) struct BoundedBody {
    bytes: Vec<u8>,
    limit: u64,
}

impl BoundedBody {
    /// Starts an empty body that accepts at most `limit` bytes in total.
    pub(crate) fn new(limit: u64) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    /// Appends one chunk.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::PolicyFetch`] once the limit would be exceeded.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<(), Report<ReleaseError>> {
        if chunk.len() as u64 > self.limit.saturating_sub(self.bytes.len() as u64) {
            return Err(report(
                ReleaseError::PolicyFetch,
                "policy response exceeds 1 MiB",
            ));
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    /// Returns the accumulated bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Fetches producer-policy bytes from the fixed endpoint at an approved commit.
///
/// # Errors
///
/// Returns [`ReleaseError::Timeout`] when the attempt deadline elapses and
/// [`ReleaseError::PolicyFetch`] for transport, status, redirect, origin or
/// size failures. Diagnostics never include the credential.
pub(crate) async fn fetch_approved_policy(
    client: &Client,
    commit: &ApprovedCommit,
    token: &GithubToken,
    deadline: AttemptDeadline,
) -> Result<ApprovedSourcePolicy, Report<ReleaseError>> {
    // Check first: an elapsed deadline must not start any network activity.
    if std::time::Instant::now() >= deadline.instant() {
        return Err(report(
            ReleaseError::Timeout,
            "policy acquisition deadline already elapsed",
        ));
    }
    let request = policy_request(commit, token)?;
    let request_url = request.url().clone();
    let fetch =
        async {
            let mut response = client.execute(request).await.map_err(|error| {
                report(ReleaseError::PolicyFetch, token.redact(&error.to_string()))
            })?;
            check_policy_response(
                &request_url,
                response.status(),
                response.url(),
                response.content_length(),
            )?;
            let mut body = BoundedBody::new(bounds::POLICY_BYTES);
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                report(ReleaseError::PolicyFetch, token.redact(&error.to_string()))
            })? {
                body.push(&chunk)?;
            }
            ApprovedSourcePolicy::bounded(commit.clone(), body.into_bytes())
        };
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.instant()), fetch)
        .await
        .map_err(|_| {
            report(
                ReleaseError::Timeout,
                "policy acquisition exceeded the deadline",
            )
        })?
}
