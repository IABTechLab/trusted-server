//! A [`MediaSigner`] for core's tests, which records each job and appends a
//! fixed suffix to the source in place of a manifest.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::{HeaderName, HeaderValue, header};

use super::{
    MediaJob, MediaNamespacing, MediaSigner, MediaSigning, MediaSigningFailure, SignedMedia,
};
use crate::platform::RuntimeServices;

/// What the stub's signing appends to the source.
pub(crate) const SIGNED_SUFFIX: &[u8] = b"|signed-manifest";

/// The header the stub writes on a signed representation.
pub(crate) const MANIFEST_HEADER: &str = "x-test-manifest";

/// The `Link` the stub's [`Behavior::SignWithLink`] writes.
pub(crate) const MANIFEST_LINK: &str = "<https://manifests.example.com/m1>; rel=\"c2pa-manifest\"";

/// How the stub answers a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Behavior {
    /// Returns the source with [`SIGNED_SUFFIX`] appended.
    Sign,
    /// Returns a failure.
    Fail,
    /// Returns signed bytes with a header core owns.
    ReservedHeader,
    /// Signs as [`Behavior::Sign`] and adds a manifest [`MANIFEST_LINK`].
    SignWithLink,
}

/// One job the stub was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedJob {
    pub(crate) reader_url: String,
    pub(crate) content_type: String,
    pub(crate) source: Vec<u8>,
    pub(crate) source_sha256: String,
    pub(crate) source_reference: Option<String>,
    pub(crate) source_origin_host: Option<String>,
    pub(crate) optimizer_output: bool,
}

/// Leaves `/api/` paths alone and signs every `image/` type.
pub(crate) struct StubSigner {
    behavior: Behavior,
    limit: usize,
    jobs: Mutex<Vec<RecordedJob>>,
    namespace: Mutex<Result<String, MediaSigningFailure>>,
}

impl StubSigner {
    pub(crate) fn new(behavior: Behavior, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            behavior,
            limit,
            jobs: Mutex::new(Vec::new()),
            namespace: Mutex::new(Ok("policy-1".to_owned())),
        })
    }

    /// Makes [`MediaSigner::namespace`] answer `namespace` from now on, as a
    /// changed or revoked policy would.
    pub(crate) fn set_namespace(&self, namespace: Result<String, MediaSigningFailure>) {
        *self.namespace.lock().expect("should lock namespace") = namespace;
    }

    pub(crate) fn jobs(&self) -> Vec<RecordedJob> {
        self.jobs.lock().expect("should lock jobs").clone()
    }

    /// `services` carrying this signer, as an adapter hands it in.
    pub(crate) fn installed_in(self: &Arc<Self>, services: RuntimeServices) -> RuntimeServices {
        services.with_media_signer(Some(Arc::clone(self) as Arc<dyn MediaSigner>))
    }
}

impl MediaSigner for StubSigner {
    fn signer_id(&self) -> &'static str {
        "testing.media"
    }

    fn handles_path(&self, path: &str) -> bool {
        !path.starts_with("/api/")
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type.starts_with("image/")
    }

    fn max_source_bytes(&self) -> usize {
        self.limit
    }

    fn namespace<'a>(&'a self, _services: &'a RuntimeServices) -> MediaNamespacing<'a> {
        let namespace = self
            .namespace
            .lock()
            .expect("should lock namespace")
            .clone();
        Box::pin(async move { namespace })
    }

    fn sign<'a>(
        &'a self,
        _services: &'a RuntimeServices,
        job: &'a MediaJob<'a>,
    ) -> MediaSigning<'a> {
        Box::pin(async move {
            self.jobs
                .lock()
                .expect("should lock jobs")
                .push(RecordedJob {
                    reader_url: job.reader_url.to_owned(),
                    content_type: job.content_type.to_owned(),
                    source: job.source.to_vec(),
                    source_sha256: job.source_sha256.to_owned(),
                    source_reference: job.source_reference.map(str::to_owned),
                    source_origin_host: job.source_origin_host.map(str::to_owned),
                    optimizer_output: job.optimizer_output,
                });
            // Signs under the namespace current when it signs, as a real
            // signer reports the policy it used.
            let namespace = self
                .namespace
                .lock()
                .expect("should lock namespace")
                .clone()
                .unwrap_or_default();
            let mut bytes = job.source.to_vec();
            bytes.extend_from_slice(SIGNED_SUFFIX);
            match self.behavior {
                Behavior::Fail => Err(MediaSigningFailure {
                    reason: "stub_failure",
                }),
                Behavior::Sign => Ok(SignedMedia {
                    bytes: Bytes::from(bytes),
                    content_type: HeaderValue::from_static("image/png"),
                    headers: vec![(
                        HeaderName::from_static(MANIFEST_HEADER),
                        HeaderValue::from_static("instance-1"),
                    )],
                    namespace,
                }),
                Behavior::SignWithLink => Ok(SignedMedia {
                    bytes: Bytes::from(bytes),
                    content_type: HeaderValue::from_static("image/png"),
                    headers: vec![(header::LINK, HeaderValue::from_static(MANIFEST_LINK))],
                    namespace,
                }),
                Behavior::ReservedHeader => Ok(SignedMedia {
                    bytes: Bytes::from(bytes),
                    content_type: HeaderValue::from_static("image/png"),
                    headers: vec![(header::ETAG, HeaderValue::from_static("\"forged\""))],
                    namespace,
                }),
            }
        })
    }
}
