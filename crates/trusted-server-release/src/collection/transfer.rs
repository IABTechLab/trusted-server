//! Fixed-origin read-only artifact transport; dependency qualification stays separate.
//!
//! Each operation owns an async runtime and uses asynchronous DNS, built-in
//! `WebPKI` roots, no proxies and no automatic redirects. Host DNS configuration
//! remains a bootstrap OS input; this transport does not authenticate that OS or
//! confer workflow, source, tool-acquisition or signing authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::time::{Duration, Instant};

use error_stack::Report;
use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, HeaderValue, LOCATION,
};
use reqwest::{Client, ClientBuilder, Response, StatusCode, Url};
use sha2::{Digest as _, Sha256};

use super::{
    ARCHIVE_LIMIT, ArtifactSource, Download, OptionalBuild, TrustedDependencies, collection_error,
};
use crate::documents::{Role, lower_hex, parse_json};
use crate::error::ReleaseError;
use crate::process::Cancellation;

const REPOSITORY: &str = "IABTechLab/trusted-server";
const REPOSITORY_ID: u64 = 950860906;
const API_ROOT: &str = "https://api.github.com/repos/IABTechLab/trusted-server/"; // allow-domain: api.github.com
// The documented Actions artifact storage suffix is accepted only in a
// Location returned by the exact authenticated API download request.
const STORAGE_SUFFIX: &str = ".blob.core.windows.net";
const RESPONSE_WALL: Duration = Duration::from_secs(120);
const MAX_ATTEMPT: Duration = Duration::from_secs(900);
const CANCEL_POLL: Duration = Duration::from_millis(20);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(100);
const URL_LIMIT: usize = 8192;
const BODY_CHUNK: usize = 64 * 1024;

type Result<T> = core::result::Result<T, Report<ReleaseError>>;

struct Endpoints {
    api: Url,
    storage_suffix: &'static str,
    storage_port: Option<u16>,
    #[cfg(test)]
    addresses: Vec<(&'static str, std::net::SocketAddr)>,
    #[cfg(test)]
    resolver: Option<std::sync::Arc<dyn reqwest::dns::Resolve>>,
}

impl Endpoints {
    fn official() -> Result<Self> {
        Ok(Self {
            api: Url::parse(API_ROOT)
                .map_err(|_| collection_error("compiled artifact API origin is invalid"))?,
            storage_suffix: STORAGE_SUFFIX,
            storage_port: None,
            #[cfg(test)]
            addresses: Vec::new(),
            #[cfg(test)]
            resolver: None,
        })
    }

    fn signed_location(&self, text: &str) -> Result<Url> {
        if text.len() > URL_LIMIT
            || text
                .bytes()
                .any(|byte| !byte.is_ascii_graphic() || byte == b'\\')
        {
            return Err(collection_error("artifact download location is invalid"));
        }
        let url = Url::parse(text)
            .map_err(|_| collection_error("artifact download location is invalid"))?;
        let account = url
            .host_str()
            .and_then(|host| host.strip_suffix(self.storage_suffix));
        if url.scheme() != self.api.scheme()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port() != self.storage_port
            || !account.is_some_and(|account| {
                !account.is_empty()
                    && account
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        {
            return Err(collection_error(
                "artifact download location has an unapproved origin",
            ));
        }
        Ok(url)
    }

    fn client(&self) -> Result<Client> {
        let builder = Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .tls_built_in_webpki_certs(true)
            .hickory_dns(true)
            .https_only(self.api.scheme() == "https")
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .pool_max_idle_per_host(0)
            .connect_timeout(Duration::from_secs(15))
            .user_agent("trusted-server-release");
        self.build_client(builder)
    }

    #[cfg(not(test))]
    fn build_client(&self, builder: ClientBuilder) -> Result<Client> {
        builder
            .build()
            .map_err(|_| collection_error("cannot initialize artifact HTTPS client"))
    }

    #[cfg(test)]
    fn build_client(&self, mut builder: ClientBuilder) -> Result<Client> {
        for (host, address) in &self.addresses {
            builder = builder.resolve(host, *address);
        }
        if let Some(resolver) = &self.resolver {
            builder = builder.dns_resolver2(std::sync::Arc::clone(resolver));
        }
        builder
            .build()
            .map_err(|_| collection_error("cannot initialize artifact HTTPS client"))
    }

    #[cfg(test)]
    fn fixture(api: &str, storage_suffix: &'static str, storage_port: Option<u16>) -> Self {
        Self {
            api: Url::parse(api).expect("should parse fictional API endpoint"),
            storage_suffix,
            storage_port,
            addresses: Vec::new(),
            resolver: None,
        }
    }
}

struct Credential(HeaderValue);

impl Credential {
    fn new(token: &str) -> Result<Self> {
        if token.is_empty()
            || token.len() > URL_LIMIT
            || !token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(collection_error("artifact API credential is invalid"));
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| collection_error("artifact API credential is invalid"))?;
        value.set_sensitive(true);
        Ok(Self(value))
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credential([REDACTED])")
    }
}

struct SelectedArtifact {
    name: String,
    sha256: String,
}

struct Selection {
    repository: String,
    repository_id: u64,
    run_id: u64,
    source_commit: String,
    artifacts: BTreeMap<u64, SelectedArtifact>,
}

impl Selection {
    fn from_dependencies(dependencies: &TrustedDependencies) -> Result<Self> {
        let invocation = &dependencies.invocation;
        if invocation.repository != REPOSITORY || invocation.repository_id != REPOSITORY_ID {
            return Err(collection_error(
                "artifact dependency has another repository identity",
            ));
        }
        let mut artifacts = BTreeMap::new();
        for upload in dependencies
            .required
            .iter()
            .chain(
                dependencies
                    .optional
                    .iter()
                    .filter_map(|optional| match optional {
                        OptionalBuild::Uploaded { upload, .. } => Some(upload),
                        OptionalBuild::NotUploaded(_) => None,
                    }),
            )
        {
            if artifacts
                .insert(
                    upload.artifact_id,
                    SelectedArtifact {
                        name: upload.artifact_name.clone(),
                        sha256: upload.archive_sha256.clone(),
                    },
                )
                .is_some()
            {
                return Err(collection_error("artifact dependency ID is reused"));
            }
        }
        let selection = Self {
            repository: invocation.repository.clone(),
            repository_id: invocation.repository_id,
            run_id: invocation.run_id,
            source_commit: invocation.source_commit.clone(),
            artifacts,
        };
        selection.check()?;
        Ok(selection)
    }

    fn check(&self) -> Result<()> {
        if self.repository_id == 0
            || self.run_id == 0
            || !lower_hex(&self.source_commit, 40)
            || self.artifacts.is_empty()
            || self.artifacts.len() > 8
            || self.artifacts.iter().any(|(id, artifact)| {
                *id == 0
                    || !lower_hex(&artifact.sha256, 64)
                    || artifact.name.is_empty()
                    || artifact.name.len() > 128
                    || artifact.name.chars().any(char::is_control)
            })
        {
            return Err(collection_error(
                "artifact selection has invalid bounded identities",
            ));
        }
        Ok(())
    }

    fn selected(&self, repository: &str, id: u64) -> Result<&SelectedArtifact> {
        if repository != self.repository {
            return Err(collection_error("artifact request has another repository"));
        }
        self.artifacts
            .get(&id)
            .ok_or_else(|| collection_error("artifact request ID was not selected"))
    }

    fn metadata(&self, id: u64, raw: &[u8]) -> Result<()> {
        let expected = self.selected(&self.repository, id)?;
        let value = parse_json(raw, Role::Metadata)
            .map_err(|_| collection_error("artifact metadata is not strict bounded JSON"))?;
        if value["id"].as_u64() != Some(id)
            || value["name"].as_str() != Some(expected.name.as_str())
            || value["expired"].as_bool() != Some(false)
            || value["digest"].as_str() != Some(format!("sha256:{}", expected.sha256).as_str())
            || value["workflow_run"]["id"].as_u64() != Some(self.run_id)
            || value["workflow_run"]["head_sha"].as_str() != Some(self.source_commit.as_str())
            || value["workflow_run"]["repository_id"].as_u64() != Some(self.repository_id)
            || value["workflow_run"]["head_repository_id"].as_u64() != Some(self.repository_id)
        {
            return Err(collection_error(
                "artifact REST metadata differs from selected invocation",
            ));
        }
        Ok(())
    }
}

/// Bounded fixed-origin source; it cannot construct trusted workflow dependencies.
pub(crate) struct HttpArtifactSource {
    selection: Selection,
    credential: Credential,
    endpoints: Endpoints,
    deadline: Instant,
    cancellation: Cancellation,
    verified_metadata: BTreeSet<u64>,
}

impl fmt::Debug for HttpArtifactSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpArtifactSource")
            .field("selected_artifacts", &self.selection.artifacts.len())
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

impl HttpArtifactSource {
    /// Binds transport to dependency-selected exact identities and an explicit token.
    ///
    /// Qualified dependency creation remains unavailable. Token possession alone
    /// cannot establish producer job/attempt, source or workflow authority.
    ///
    /// # Errors
    ///
    /// Rejects invalid fixed repository/selection identities, credentials or bounds.
    pub(crate) fn from_dependencies(
        dependencies: &TrustedDependencies,
        token: &str,
        deadline: Instant,
        cancellation: Cancellation,
    ) -> Result<Self> {
        Self::new(
            Selection::from_dependencies(dependencies)?,
            token,
            deadline,
            cancellation,
            Endpoints::official()?,
        )
    }

    fn new(
        selection: Selection,
        token: &str,
        deadline: Instant,
        cancellation: Cancellation,
        endpoints: Endpoints,
    ) -> Result<Self> {
        selection.check()?;
        if deadline
            .checked_duration_since(Instant::now())
            .is_none_or(|remaining| remaining > MAX_ATTEMPT)
            || cancellation.is_cancelled()
        {
            return Err(collection_error(
                "artifact transport attempt is expired or cancelled",
            ));
        }
        Ok(Self {
            selection,
            credential: Credential::new(token)?,
            endpoints,
            deadline,
            cancellation,
            verified_metadata: BTreeSet::new(),
        })
    }

    fn run<T>(&self, operation: impl Future<Output = Result<T>>) -> Result<T> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(collection_error(
                "artifact transport attempt is expired or cancelled",
            ));
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(collection_error(
                "synchronous artifact transport cannot run inside another async runtime",
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| collection_error("cannot initialize artifact transport runtime"))?;
        let deadline = self.deadline.min(Instant::now() + RESPONSE_WALL);
        let result = runtime.block_on(async {
            tokio::select! {
                biased;
                () = wait_for_cancel(&self.cancellation) => Err(collection_error("artifact transport was cancelled")),
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(collection_error("artifact transport exceeded its deadline")),
                result = operation => result,
            }
        });
        // Asynchronous DNS/HTTP tasks belong to this operation's runtime. No
        // blocking OS resolver is selected, and no pool spans operations.
        runtime.shutdown_timeout(SHUTDOWN_GRACE);
        result
    }

    fn api_url(&self, id: u64, download: bool) -> Result<Url> {
        self.endpoints
            .api
            .join(&format!(
                "actions/artifacts/{id}{}",
                if download { "/zip" } else { "" }
            ))
            .map_err(|_| collection_error("artifact API path is invalid"))
    }

    async fn api_request(&self, id: u64, download: bool) -> Result<Response> {
        let url = self.api_url(id, download)?;
        let response = self
            .endpoints
            .client()?
            .get(url.clone())
            .header(AUTHORIZATION, self.credential.0.clone())
            .header(ACCEPT, "application/vnd.github+json")
            .header(ACCEPT_ENCODING, "identity")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|_| collection_error("artifact API HTTPS request failed"))?;
        if response.url() != &url {
            return Err(collection_error(
                "artifact API changed its fixed request URL",
            ));
        }
        Ok(response)
    }
}

impl ArtifactSource for HttpArtifactSource {
    fn metadata(&mut self, repository: &str, id: u64) -> Result<Vec<u8>> {
        self.selection.selected(repository, id)?;
        self.verified_metadata.remove(&id);
        let raw = self.run(async {
            let response = self.api_request(id, false).await?;
            require_status(&response, StatusCode::OK)?;
            read_response(response, Role::Metadata.limit() as usize, self).await
        })?;
        self.selection.metadata(id, &raw.bytes)?;
        self.verified_metadata.insert(id);
        Ok(raw.bytes)
    }

    fn download(&mut self, repository: &str, id: u64) -> Result<Download> {
        let expected = &self.selection.selected(repository, id)?.sha256;
        if !self.verified_metadata.remove(&id) {
            return Err(collection_error(
                "artifact download lacks checked exact-ID REST metadata",
            ));
        }
        let raw = self.run(async {
            let response = self.api_request(id, true).await?;
            require_status(&response, StatusCode::FOUND)?;
            let mut locations = response.headers().get_all(LOCATION).iter();
            let text = locations
                .next()
                .and_then(|location| location.to_str().ok())
                .ok_or_else(|| {
                    collection_error("artifact API omitted its single download location")
                })?;
            if locations.next().is_some() {
                return Err(collection_error(
                    "artifact API returned duplicate download locations",
                ));
            }
            let location = self.endpoints.signed_location(text)?;
            drop(response);
            let response = self
                .endpoints
                .client()?
                .get(location.clone())
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(|_| collection_error("artifact storage HTTPS request failed"))?;
            if response.url() != &location {
                return Err(collection_error(
                    "artifact storage changed its approved request URL",
                ));
            }
            require_status(&response, StatusCode::OK)?;
            read_response(response, ARCHIVE_LIMIT, self).await
        })?;
        if raw.sha256 != *expected {
            return Err(collection_error(
                "artifact archive differs from the selected upload digest",
            ));
        }
        Ok(Download {
            archive: raw.bytes,
            transfer_warning: false,
        })
    }
}

struct Body {
    bytes: Vec<u8>,
    sha256: String,
}

fn require_status(response: &Response, expected: StatusCode) -> Result<()> {
    if response.status() != expected {
        return Err(collection_error(format!(
            "artifact endpoint returned unexpected HTTP status {}",
            response.status().as_u16()
        )));
    }
    Ok(())
}

async fn read_response(
    mut response: Response,
    limit: usize,
    owner: &HttpArtifactSource,
) -> Result<Body> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
        || response.headers().get_all(CONTENT_ENCODING).iter().count() > 1
        || response
            .headers()
            .get_all(CONTENT_ENCODING)
            .iter()
            .any(|encoding| encoding != "identity")
    {
        return Err(collection_error(
            "artifact response has an oversized, ambiguous or transformed body",
        ));
    }
    let mut bytes = Vec::new();
    let mut hash = Sha256::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| collection_error("artifact response body is truncated or unreadable"))?
    {
        for part in chunk.chunks(BODY_CHUNK) {
            if owner.cancellation.is_cancelled() || Instant::now() >= owner.deadline {
                return Err(collection_error(
                    "artifact streaming attempt expired or was cancelled",
                ));
            }
            append_chunk(&mut bytes, &mut hash, part, limit)?;
        }
    }
    Ok(Body {
        bytes,
        sha256: hex::encode(hash.finalize()),
    })
}

fn append_chunk(bytes: &mut Vec<u8>, hash: &mut Sha256, chunk: &[u8], limit: usize) -> Result<()> {
    if bytes.len() > limit || chunk.len() > limit.saturating_sub(bytes.len()) {
        return Err(collection_error(
            "artifact response exceeds its streaming byte bound",
        ));
    }
    let required = bytes.len() + chunk.len();
    if required > bytes.capacity() {
        let capacity = bytes
            .capacity()
            .saturating_mul(2)
            .max(BODY_CHUNK)
            .min(limit)
            .max(required);
        bytes
            .try_reserve_exact(capacity - bytes.len())
            .map_err(|_| collection_error("cannot allocate bounded artifact response"))?;
    }
    bytes.extend_from_slice(chunk);
    hash.update(chunk);
    Ok(())
}

async fn wait_for_cancel(cancellation: &Cancellation) {
    while !cancellation.is_cancelled() {
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

#[cfg(test)]
mod ordinary;
#[cfg(test)]
mod tests;

/// Explicit ordinary-CI request; this test-only data cannot mint dependencies.
#[cfg(test)]
pub(crate) struct OrdinaryArtifactSelection {
    /// Run whose REST head identity is independently supplied by the CI context.
    pub(crate) run_id: u64,
    /// REST run head, distinct from a PR's checked-out merge source.
    pub(crate) run_head_commit: String,
    /// Exact ID returned by the selected upload step.
    pub(crate) artifact_id: u64,
    /// Exact artifact name selected by that upload step.
    pub(crate) artifact_name: String,
    /// Original ZIP digest returned by that upload step.
    pub(crate) archive_sha256: String,
}

/// Raw corroborated ordinary-CI bytes, without workflow or signing authority.
#[cfg(test)]
pub(crate) struct OrdinaryArtifactTransfer {
    metadata: Vec<u8>,
    archive: Vec<u8>,
}

#[cfg(test)]
impl OrdinaryArtifactTransfer {
    /// Returns exact original REST body bytes rather than a reserialization.
    pub(crate) fn metadata(&self) -> &[u8] {
        &self.metadata
    }
    /// Checks flat ZIP members under the unchanged collector archive contract.
    ///
    /// # Errors
    ///
    /// Rejects unsafe, undeclared, truncated or oversized ZIP members.
    pub(crate) fn members(
        &self,
        expected: &BTreeMap<String, u64>,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        super::archive::read(&self.archive, expected)
    }
}

/// Exercises the actual fixed-origin backend with unqualified CI selections.
///
/// # Errors
///
/// Rejects changed identity, transport, metadata or archive-digest bindings. This
/// returns diagnostic bytes only and cannot construct [`TrustedDependencies`].
#[cfg(test)]
pub(crate) fn retrieve_ordinary_ci(
    selection: OrdinaryArtifactSelection,
    token: &str,
    deadline: Instant,
    cancellation: Cancellation,
) -> Result<OrdinaryArtifactTransfer> {
    let id = selection.artifact_id;
    let mut source = HttpArtifactSource::new(
        Selection {
            repository: REPOSITORY.to_owned(),
            repository_id: REPOSITORY_ID,
            run_id: selection.run_id,
            source_commit: selection.run_head_commit,
            artifacts: BTreeMap::from([(
                id,
                SelectedArtifact {
                    name: selection.artifact_name,
                    sha256: selection.archive_sha256,
                },
            )]),
        },
        token,
        deadline,
        cancellation,
        Endpoints::official()?,
    )?;
    let metadata = source.metadata(REPOSITORY, id)?;
    let archive = source.download(REPOSITORY, id)?.archive;
    Ok(OrdinaryArtifactTransfer { metadata, archive })
}
