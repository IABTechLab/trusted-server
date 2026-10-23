//! In-process stand-in for the GitHub releases API; no network.

use std::collections::{BTreeMap, VecDeque};

use super::*;

pub(super) const COMMIT: &str = "1111111111111111111111111111111111111111";
pub(super) const OTHER_COMMIT: &str = "2222222222222222222222222222222222222222";
pub(super) const FIRST_RELEASE_ID: u64 = 99999;
const FIRST_ASSET_ID: u64 = 99999;

/// One recorded call against the fake releases API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Call {
    List,
    Get,
    TagRef,
    Create,
    Upload(String),
    Download(String),
    Publish,
    Statement,
    Delete,
}

impl Call {
    fn mutates(&self) -> bool {
        matches!(
            self,
            Self::Create | Self::Upload(_) | Self::Publish | Self::Delete
        )
    }
}

/// A scripted fault for the next call of one operation.
#[derive(Clone, Copy, Debug)]
pub(super) enum Fault {
    /// The effect is applied but the response is lost.
    Lost,
    /// No effect is applied and the response is lost.
    Unreachable,
    /// The effect is applied and the API reports this status (racing writer).
    Raced(u16),
    /// No effect is applied and the API reports this status.
    Status(u16),
    /// The upload reports success but stores nothing.
    Dropped,
    /// The upload reports success but stores altered bytes.
    Corrupted,
    /// The download returns more bytes than the intended size.
    Oversized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Operation {
    List,
    Get,
    TagRef,
    Create,
    Upload,
    Download,
    Publish,
    Statement,
    Delete,
}

#[derive(Clone, Debug)]
pub(super) struct FakeRelease {
    pub(super) tag: String,
    pub(super) commit: String,
    pub(super) draft: bool,
    pub(super) immutable: Option<bool>,
    pub(super) assets: BTreeMap<String, (u64, Vec<u8>)>,
}

/// A hook run before answering the given 1-based `tag_ref` call.
pub(super) type TagRefHook = (usize, fn(&mut FakeApi));

pub(super) struct FakeApi {
    pub(super) releases: BTreeMap<u64, FakeRelease>,
    pub(super) tags: BTreeMap<String, String>,
    pub(super) calls: Vec<Call>,
    pub(super) immutable_after_publish: Option<bool>,
    pub(super) statement: Option<ReleaseStatement>,
    pub(super) reads_after_publish_unknown: bool,
    /// Runs before answering the given 1-based `tag_ref` call.
    pub(super) tag_ref_hook: Option<TagRefHook>,
    /// Alters the release after a successful publish.
    pub(super) after_publish: Option<fn(&mut FakeRelease)>,
    next_id: u64,
    next_asset_id: u64,
    tag_ref_calls: usize,
    faults: BTreeMap<Operation, VecDeque<Fault>>,
}

impl FakeApi {
    pub(super) fn new() -> Self {
        Self {
            releases: BTreeMap::new(),
            tags: BTreeMap::new(),
            calls: Vec::new(),
            immutable_after_publish: Some(true),
            statement: None,
            reads_after_publish_unknown: false,
            tag_ref_hook: None,
            after_publish: None,
            next_id: FIRST_RELEASE_ID,
            next_asset_id: FIRST_ASSET_ID,
            tag_ref_calls: 0,
            faults: BTreeMap::new(),
        }
    }

    pub(super) fn fault(&mut self, operation: Operation, fault: Fault) {
        self.faults.entry(operation).or_default().push_back(fault);
    }

    fn peek_fault(&self, operation: Operation) -> Option<Fault> {
        self.faults
            .get(&operation)
            .and_then(|queue| queue.front().copied())
    }

    fn next_fault(&mut self, operation: Operation) -> Option<Fault> {
        self.faults.get_mut(&operation)?.pop_front()
    }

    fn asset_id(&mut self) -> u64 {
        let id = self.next_asset_id;
        self.next_asset_id += 1;
        id
    }

    pub(super) fn seed(&mut self, release: FakeRelease) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.releases.insert(id, release);
        id
    }

    /// Replaces the stored asset `name` of every release with a fresh asset ID.
    pub(super) fn reupload_with_new_id(&mut self, name: &str) {
        let fresh = self.asset_id();
        for release in self.releases.values_mut() {
            if let Some(asset) = release.assets.get_mut(name) {
                asset.0 = fresh;
            }
        }
    }

    fn observed(&self, id: u64) -> Option<ObservedRelease> {
        let release = self.releases.get(&id)?;
        Some(ObservedRelease {
            id: ReleaseId(id),
            tag: release.tag.clone(),
            target_commit: release.commit.clone(),
            draft: release.draft,
            immutable: if release.draft {
                Some(false)
            } else {
                release.immutable
            },
            assets: release
                .assets
                .iter()
                .map(|(name, (asset_id, bytes))| ObservedAsset {
                    id: *asset_id,
                    name: name.clone(),
                    size: bytes.len() as u64,
                    sha256: Some(sha256_hex(bytes)),
                })
                .collect(),
        })
    }

    pub(super) fn mutations(&self) -> Vec<Call> {
        self.calls
            .iter()
            .filter(|call| call.mutates())
            .cloned()
            .collect()
    }

    pub(super) fn count(&self, wanted: &Call) -> usize {
        self.calls.iter().filter(|call| *call == wanted).count()
    }

    pub(super) fn position(&self, wanted: &Call) -> usize {
        self.calls
            .iter()
            .position(|call| call == wanted)
            .expect("should record the call")
    }

    /// Applies or withholds an effect according to the scripted fault.
    fn respond<T>(
        &mut self,
        operation: Operation,
        effect: impl FnOnce(&mut Self) -> Result<T, ApiFailure>,
    ) -> Result<T, ApiFailure> {
        match self.next_fault(operation) {
            None | Some(Fault::Dropped | Fault::Corrupted | Fault::Oversized) => effect(self),
            Some(Fault::Lost) => {
                let _ = effect(self);
                Err(ApiFailure::Indeterminate)
            }
            Some(Fault::Raced(status)) => {
                let _ = effect(self);
                Err(ApiFailure::Rejected { status })
            }
            Some(Fault::Unreachable) => Err(ApiFailure::Indeterminate),
            Some(Fault::Status(status)) => Err(ApiFailure::Rejected { status }),
        }
    }
}

impl ReleaseApi for FakeApi {
    fn releases_for_tag(
        &mut self,
        _authority: &ContentsWrite,
        tag: &ReleaseTag,
    ) -> Result<Vec<ObservedRelease>, ApiFailure> {
        self.calls.push(Call::List);
        self.respond(Operation::List, |api| {
            Ok(api
                .releases
                .iter()
                .filter(|(_, release)| release.tag == tag.as_str())
                .filter_map(|(id, _)| api.observed(*id))
                .collect())
        })
    }

    fn release(
        &mut self,
        _authority: &ContentsWrite,
        id: ReleaseId,
    ) -> Result<Option<ObservedRelease>, ApiFailure> {
        self.calls.push(Call::Get);
        let unknown = self.reads_after_publish_unknown
            && self
                .releases
                .get(&id.0)
                .is_some_and(|release| !release.draft);
        self.respond(Operation::Get, |api| {
            if unknown {
                return Err(ApiFailure::Indeterminate);
            }
            Ok(api.observed(id.0))
        })
    }

    fn tag_ref(
        &mut self,
        _authority: &ContentsWrite,
        tag: &ReleaseTag,
    ) -> Result<Option<CommitId>, ApiFailure> {
        self.calls.push(Call::TagRef);
        self.tag_ref_calls += 1;
        if let Some((call, hook)) = self.tag_ref_hook
            && call == self.tag_ref_calls
        {
            hook(self);
        }
        self.respond(Operation::TagRef, |api| {
            Ok(api
                .tags
                .get(tag.as_str())
                .map(|commit| CommitId::parse(commit).expect("should hold a valid fake commit")))
        })
    }

    fn create_draft(
        &mut self,
        _authority: &ContentsWrite,
        tag: &ReleaseTag,
        commit: &CommitId,
    ) -> Result<ReleaseId, ApiFailure> {
        self.calls.push(Call::Create);
        let release = FakeRelease {
            tag: tag.as_str().to_owned(),
            commit: commit.as_str().to_owned(),
            draft: true,
            immutable: None,
            assets: BTreeMap::new(),
        };
        self.respond(Operation::Create, |api| Ok(ReleaseId(api.seed(release))))
    }

    fn upload_asset(
        &mut self,
        _authority: &ContentsWrite,
        id: ReleaseId,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), ApiFailure> {
        self.calls.push(Call::Upload(name.to_owned()));
        let fault = self.peek_fault(Operation::Upload);
        self.respond(Operation::Upload, |api| {
            let asset_id = api.asset_id();
            let release = api
                .releases
                .get_mut(&id.0)
                .ok_or(ApiFailure::Rejected { status: 404 })?;
            if !release.draft || release.assets.contains_key(name) {
                return Err(ApiFailure::Rejected { status: 422 });
            }
            match fault {
                Some(Fault::Dropped) => {}
                Some(Fault::Corrupted) => {
                    let mut altered = bytes.to_vec();
                    altered.push(b'!');
                    release.assets.insert(name.to_owned(), (asset_id, altered));
                }
                _ => {
                    release
                        .assets
                        .insert(name.to_owned(), (asset_id, bytes.to_vec()));
                }
            }
            Ok(())
        })
    }

    fn download_asset(
        &mut self,
        _authority: &ContentsWrite,
        id: ReleaseId,
        name: &str,
        limit: u64,
    ) -> Result<Vec<u8>, ApiFailure> {
        self.calls.push(Call::Download(name.to_owned()));
        let oversized = matches!(self.peek_fault(Operation::Download), Some(Fault::Oversized));
        self.respond(Operation::Download, |api| {
            let (_, stored) = api
                .releases
                .get(&id.0)
                .and_then(|release| release.assets.get(name))
                .ok_or(ApiFailure::Rejected { status: 404 })?;
            let mut bytes = stored.clone();
            if oversized {
                bytes.extend_from_slice(b"oversized");
            }
            let bounded = bytes
                .len()
                .min(usize::try_from(limit + 1).unwrap_or(usize::MAX));
            bytes.truncate(bounded);
            Ok(bytes)
        })
    }

    fn publish_draft(
        &mut self,
        _authority: &ContentsWrite,
        id: ReleaseId,
    ) -> Result<(), ApiFailure> {
        self.calls.push(Call::Publish);
        let immutable = self.immutable_after_publish;
        let after_publish = self.after_publish;
        self.respond(Operation::Publish, |api| {
            let release = api
                .releases
                .get_mut(&id.0)
                .ok_or(ApiFailure::Rejected { status: 404 })?;
            if !release.draft {
                return Err(ApiFailure::Rejected { status: 422 });
            }
            release.draft = false;
            release.immutable = immutable;
            let (tag, commit) = (release.tag.clone(), release.commit.clone());
            if let Some(alter) = after_publish {
                alter(release);
            }
            api.tags.entry(tag).or_insert(commit);
            Ok(())
        })
    }

    fn release_statement(
        &mut self,
        _authority: &ContentsWrite,
        tag: &ReleaseTag,
    ) -> Result<ReleaseStatement, ApiFailure> {
        self.calls.push(Call::Statement);
        let configured = self.statement.clone();
        self.respond(Operation::Statement, |api| {
            if let Some(statement) = configured {
                return Ok(statement);
            }
            let (_, release) = api
                .releases
                .iter()
                .find(|(_, release)| release.tag == tag.as_str() && !release.draft)
                .ok_or(ApiFailure::Rejected { status: 404 })?;
            Ok(ReleaseStatement {
                tag: release.tag.clone(),
                commit: release.commit.clone(),
                subjects: release
                    .assets
                    .iter()
                    .map(|(name, (_, bytes))| StatementSubject {
                        name: name.clone(),
                        sha256: sha256_hex(bytes),
                    })
                    .collect(),
            })
        })
    }

    fn delete_mutable_release(
        &mut self,
        _authority: &ContentsWrite,
        _cleanup: &CleanupAuthority,
        id: ReleaseId,
    ) -> Result<(), ApiFailure> {
        self.calls.push(Call::Delete);
        self.respond(Operation::Delete, |api| {
            let release = api
                .releases
                .get(&id.0)
                .ok_or(ApiFailure::Rejected { status: 404 })?;
            if release.immutable == Some(true) {
                return Err(ApiFailure::Rejected { status: 422 });
            }
            api.releases.remove(&id.0);
            Ok(())
        })
    }
}

pub(super) fn tag() -> ReleaseTag {
    ReleaseTag::parse("v1.2.3").expect("should parse example tag")
}

pub(super) fn commit(value: &str) -> CommitId {
    CommitId::parse(value).expect("should parse example commit")
}

pub(super) fn assets() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("example-release-index.json", b"{\"example\":true}"),
        ("example-server.wasm", b"\0asm\x01\0\0\0example"),
        ("ts-example-target", b"example executable bytes"),
    ]
}

pub(super) fn intended_with(approval: SourceApproval) -> IntendedRelease {
    let assets = assets()
        .into_iter()
        .map(|(name, bytes)| IntendedAsset::new(name, bytes.to_vec()).expect("should accept asset"))
        .collect::<Vec<_>>();
    IntendedRelease::new(tag(), commit(COMMIT), approval, assets)
        .expect("should accept intended release")
}

pub(super) fn intended() -> IntendedRelease {
    intended_with(SourceApproval::Main)
}

pub(super) fn correct_statement() -> ReleaseStatement {
    ReleaseStatement {
        tag: "v1.2.3".to_owned(),
        commit: COMMIT.to_owned(),
        subjects: assets()
            .into_iter()
            .map(|(name, bytes)| StatementSubject {
                name: name.to_owned(),
                sha256: sha256_hex(bytes),
            })
            .collect(),
    }
}

pub(super) fn role(cleanup: CleanupGrant) -> PublicationRole {
    PublicationRole::from_job(&[JobPermission::ContentsWrite], cleanup)
        .expect("should accept contents-write-only role")
}

pub(super) fn draft_with(commit_value: &str, present: &[(&str, &[u8])]) -> FakeRelease {
    FakeRelease {
        tag: "v1.2.3".to_owned(),
        commit: commit_value.to_owned(),
        draft: true,
        immutable: None,
        assets: present
            .iter()
            .zip(FIRST_ASSET_ID + 1000..)
            .map(|((name, bytes), asset_id)| ((*name).to_owned(), (asset_id, bytes.to_vec())))
            .collect(),
    }
}

pub(super) fn published(immutable: Option<bool>) -> FakeRelease {
    let mut release = draft_with(COMMIT, &assets());
    release.draft = false;
    release.immutable = immutable;
    release
}

pub(super) fn run(
    api: &mut FakeApi,
    cleanup: CleanupGrant,
) -> Result<PublishedRelease, PublicationFailure> {
    publish(api, &role(cleanup), &intended())
}

pub(super) fn failure(result: Result<PublishedRelease, PublicationFailure>) -> PublicationFailure {
    match result {
        Ok(_) => panic!("should fail publication"),
        Err(failure) => failure,
    }
}

pub(super) fn assert_message(failure: &PublicationFailure, needle: &str) {
    let rendered = format!("{:?}", failure.report());
    assert!(
        matches!(
            failure.report().current_context(),
            ReleaseError::Publication
        ),
        "should report a publication error"
    );
    assert!(
        rendered.contains(needle),
        "should mention `{needle}` in {rendered}"
    );
}

pub(super) fn incident(failure: &PublicationFailure) -> &Incident {
    failure.incident().expect("should raise a release incident")
}
