//! Filesystem ownership of quota-charged objects. All mutations hold `Store::objects`.
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write};
use std::path::Path;

use redb::{ReadableDatabase as _, ReadableTable as _};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{
    Artifact, ArtifactFile, PLANS, PROBE_REFERENCES, RETIRED, SOURCES, SourceRecord, Store,
    StoreError, TEXT_OPERATIONS, encode, storage,
};
use trusted_server_provenance_encypher::preparation::protocol::is_digest;

struct Measure {
    bytes: u64,
    hash: Sha256,
}

impl Default for Measure {
    fn default() -> Self {
        Self {
            bytes: 0,
            hash: Sha256::new(),
        }
    }
}

impl Write for Measure {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("artifact size overflow"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hash_file(path: &Path) -> Result<(u64, String), StoreError> {
    let mut input = File::open(path).map_err(storage)?;
    let mut measure = Measure::default();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(storage)?;
        if read == 0 {
            break;
        }
        measure.write_all(&buffer[..read]).map_err(storage)?;
    }
    Ok((measure.bytes, crate::hex_lower(&measure.hash.finalize())))
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(storage)
}

impl Store {
    fn object_path(&self, kind: &str, digest: &str) -> Result<std::path::PathBuf, StoreError> {
        if !is_digest(digest) || !matches!(kind, "source" | "probe" | "artifact" | "text") {
            return Err(StoreError::Storage("invalid object identity".to_owned()));
        }
        let (directory, suffix) = match kind {
            "source" => ("sources", ""),
            "probe" => ("probes", ".wasm"),
            "text" => ("texts", ".json"),
            _ => ("artifacts", ".json"),
        };
        Ok(self.root.join(directory).join(format!("{digest}{suffix}")))
    }

    /// Keeps digest-addressed source or probe bytes, with a durable hard reservation.
    ///
    /// # Errors
    /// Refuses invalid bytes, quota exhaustion or filesystem failure.
    pub fn keep_blob(&self, kind: &str, digest: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        self.keep_blob_locked(kind, digest, bytes)
    }

    fn keep_blob_locked(&self, kind: &str, digest: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.object_path(kind, digest)?;
        if crate::hex_lower(&Sha256::digest(bytes)) != digest {
            return Err(StoreError::Storage("object digest mismatch".to_owned()));
        }
        self.write_object(kind, digest, &path, bytes.len() as u64, |file| {
            file.write_all(bytes).map_err(storage)
        })
    }

    /// Keeps public source bytes and their owner reference under one retirement guard.
    ///
    /// # Errors
    /// A retired owner, invalid source, full quota or storage failure.
    pub fn keep_source(
        &self,
        digest: &str,
        mime: &str,
        bytes: &[u8],
        identity: &str,
    ) -> Result<(), StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        if self.retirement(identity)?.is_some() {
            return Err(storage("source owner retired"));
        }
        self.reserve_identity(identity)?;
        self.keep_blob_locked("source", digest, bytes)?;
        self.record_source_locked(digest, mime, true, identity)
    }

    /// Reads and checks a retained source or probe object without releasing its charge on error.
    ///
    /// # Errors
    /// Missing/corrupt files and invalid identities refuse explicitly.
    pub fn blob(&self, kind: &str, digest: &str) -> Result<Vec<u8>, StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        let path = self.object_path(kind, digest)?;
        let bytes = std::fs::read(path).map_err(storage)?;
        if crate::hex_lower(&Sha256::digest(&bytes)) != digest {
            return Err(StoreError::Storage(
                "retained object digest mismatch".to_owned(),
            ));
        }
        Ok(bytes)
    }

    fn write_object(
        &self,
        kind: &str,
        identity: &str,
        path: &Path,
        bytes: u64,
        write: impl FnOnce(&mut File) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let owner = format!("{kind}:{identity}");
        if !self.reserve(&owner, bytes)? {
            return Err(StoreError::Quota);
        }
        let parent = path
            .parent()
            .ok_or_else(|| storage("missing object directory"))?;
        std::fs::create_dir_all(parent).map_err(storage)?;
        // A second writer never creates a second copy of an already valid object.
        if matches!(kind, "source" | "probe")
            && path.exists()
            && hash_file(path)? == (bytes, identity.to_owned())
        {
            return Ok(());
        }
        // A replacement must charge both the old file and its temporary replacement.
        let temporary_owner = format!("temp:replace:{kind}:{identity}");
        let replacement = path.exists();
        if replacement && !self.reserve(&temporary_owner, bytes)? {
            return Err(StoreError::Quota);
        }
        let pending = path.with_extension("pending");
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&pending)
                .map_err(storage)?;
            write(&mut file)?;
            file.sync_all().map_err(storage)?;
            std::fs::rename(&pending, path).map_err(storage)?;
            sync_directory(parent)
        })();
        if result.is_err() && pending.exists() {
            // Do not release accounting until removal is confirmed.
            std::fs::remove_file(&pending).map_err(storage)?;
            sync_directory(parent)?;
        }
        if replacement {
            self.release(&temporary_owner)?;
        }
        result
    }

    pub(super) fn write_artifact_file(
        &self,
        identity: &str,
        artifact: &Artifact,
    ) -> Result<ArtifactFile, StoreError> {
        self.write_json_file("artifact", identity, artifact, artifact.acknowledged)
    }

    pub(super) fn write_text_file(
        &self,
        identity: &str,
        request: &trusted_server_provenance_encypher::preparation::pipeline::TextSignRequest,
    ) -> Result<ArtifactFile, StoreError> {
        self.write_json_file("text", identity, request, false)
    }

    fn write_json_file<T: Serialize>(
        &self,
        kind: &str,
        identity: &str,
        value: &T,
        acknowledged: bool,
    ) -> Result<ArtifactFile, StoreError> {
        let mut measure = Measure::default();
        serde_json::to_writer(&mut measure, value).map_err(storage)?;
        let record = ArtifactFile {
            bytes: measure.bytes,
            sha256: crate::hex_lower(&measure.hash.finalize()),
            acknowledged,
        };
        let path = self.object_path(kind, identity)?;
        if path.exists() {
            let (bytes, digest) = hash_file(&path)?;
            if bytes != record.bytes || digest != record.sha256 {
                return Err(storage("immutable artifact conflict"));
            }
            if !self.reserve(
                &format!("{kind}:{identity}"),
                record.bytes + encode(&record)?.len() as u64,
            )? {
                return Err(StoreError::Quota);
            }
            return Ok(record);
        }
        self.write_object(
            kind,
            identity,
            &path,
            record.bytes + encode(&record)?.len() as u64,
            |file| serde_json::to_writer(file, value).map_err(storage),
        )?;
        Ok(record)
    }

    pub(super) fn read_artifact_file(
        &self,
        identity: &str,
        record: &ArtifactFile,
    ) -> Result<Artifact, StoreError> {
        let path = self.object_path("artifact", identity)?;
        if hash_file(&path)? != (record.bytes, record.sha256.clone()) {
            return Err(storage("retained artifact digest mismatch"));
        }
        let mut artifact: Artifact =
            serde_json::from_reader(File::open(path).map_err(storage)?).map_err(storage)?;
        if artifact.envelope.identity != identity {
            return Err(storage("retained artifact identity mismatch"));
        }
        artifact.acknowledged = record.acknowledged;
        Ok(artifact)
    }

    /// Removes crash-only temporary files before releasing their ownership records.
    /// Retained objects, including probes written by the earlier unmetered cache,
    /// are measured and charged before the worker can report ready.
    ///
    /// # Errors
    /// Any unreadable directory, unsafe entry, deletion failure or existing quota excess blocks startup.
    pub fn recover_storage(&self) -> Result<(), StoreError> {
        let _guard = self.objects.lock().map_err(storage)?;
        // A committed retirement remains authoritative if file cleanup was interrupted.
        for (identity, _) in self.read_all::<super::Retirement>(RETIRED)? {
            self.remove_artifact_file(&identity)?;
            self.remove_text_file(&identity)?;
            self.reserve_identity(&identity)?;
        }
        // No task survives exclusive-owner restart. References without a
        // persisted plan/result never reached finalize and can be reacquired.
        let tx = self.db.begin_write().map_err(storage)?;
        {
            let mut sources = tx.open_table(SOURCES).map_err(storage)?;
            let mut changed = Vec::new();
            for entry in sources.iter().map_err(storage)? {
                let (sha, value) = entry.map_err(storage)?;
                let mut source: SourceRecord = super::decode(value.value())?;
                let mut kept = Vec::new();
                for identity in &source.references {
                    if self.has_reconstruction(identity)? {
                        kept.push(identity.clone());
                    }
                }
                source.references = kept;
                source.public &= !source.references.is_empty();
                changed.push((sha.value().to_owned(), source));
            }
            for (sha, source) in changed {
                sources
                    .insert(sha.as_str(), encode(&source)?.as_slice())
                    .map_err(storage)?;
            }
        }
        tx.commit().map_err(storage)?;
        self.remove_unreferenced_sources()?;
        let mut planned_sources = HashSet::new();
        {
            let tx = self.db.begin_read().map_err(storage)?;
            let plans = tx.open_table(PLANS).map_err(storage)?;
            for entry in plans.iter().map_err(storage)? {
                let (_, value) = entry.map_err(storage)?;
                let plan: trusted_server_provenance_encypher::preparation::pipeline::PlannedMedia =
                    super::decode(value.value())?;
                planned_sources.insert(plan.request.source.sha256);
            }
        }
        let directories = [
            ("sources", "source", ""),
            ("probes", "probe", ".wasm"),
            ("artifacts", "artifact", ".json"),
            ("texts", "text", ".json"),
            ("large", "temp", ""),
        ];
        // Remove owned crash-only objects before releasing their durable reservations.
        for (directory, kind, _) in directories {
            let dir = self.root.join(directory);
            std::fs::create_dir_all(&dir).map_err(storage)?;
            let metadata = std::fs::symlink_metadata(&dir).map_err(storage)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(storage("unsafe object directory"));
            }
            for entry in std::fs::read_dir(&dir).map_err(storage)? {
                let entry = entry.map_err(storage)?;
                if !entry.file_type().map_err(storage)?.is_file() {
                    return Err(storage("unsafe object entry"));
                }
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| storage("invalid object filename"))?;
                if directory == "large" {
                    if self.reservation(&format!("temp:large:{name}"))?.is_none()
                        && self.reservation(&format!("temp:{name}"))?.is_none()
                    {
                        return Err(storage("temporary source has no durable owner"));
                    }
                    std::fs::remove_file(entry.path()).map_err(storage)?;
                } else if name.ends_with(".pending") || name.contains(".tmp-") {
                    let digest = name
                        .split('.')
                        .next()
                        .ok_or_else(|| storage("invalid temporary object"))?;
                    if !is_digest(digest)
                        || self.reservation(&format!("{kind}:{digest}"))?.is_none()
                    {
                        return Err(storage("temporary object has no durable owner"));
                    }
                    std::fs::remove_file(entry.path()).map_err(storage)?;
                } else if directory == "sources"
                    && is_digest(&name)
                    && self.source(&name)?.is_none()
                    && !planned_sources.contains(&name)
                {
                    std::fs::remove_file(entry.path()).map_err(storage)?;
                    sync_directory(&dir)?;
                    self.release(&format!("source:{name}"))?;
                } else if directory == "texts" {
                    let identity = name
                        .strip_suffix(".json")
                        .ok_or_else(|| storage("invalid text filename"))?;
                    if !is_digest(identity) {
                        return Err(storage("invalid text filename"));
                    }
                    // Without the metadata commit, begin_text never returned and
                    // this owner could not have sent the signing request.
                    if self
                        .read_value::<ArtifactFile>(TEXT_OPERATIONS, identity)?
                        .is_none()
                    {
                        std::fs::remove_file(entry.path()).map_err(storage)?;
                        sync_directory(&dir)?;
                        self.release(&format!("text:{identity}"))?;
                    }
                }
            }
            sync_directory(&dir)?;
        }
        self.reconcile_reservations()?;
        // Historical unmetered probes and every retained filesystem object are charged.
        for (directory, kind, suffix) in directories {
            if directory == "large" {
                continue;
            }
            for entry in std::fs::read_dir(self.root.join(directory)).map_err(storage)? {
                let entry = entry.map_err(storage)?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| storage("invalid object filename"))?;
                let digest = name
                    .strip_suffix(suffix)
                    .ok_or_else(|| storage("unknown object filename"))?;
                if !is_digest(digest) {
                    return Err(storage("unknown object filename"));
                }
                if !self.reserve(
                    &format!("{kind}:{digest}"),
                    entry.metadata().map_err(storage)?.len(),
                )? {
                    return Err(StoreError::Quota);
                }
            }
        }
        for (kind, table) in [
            ("plan", PLANS),
            ("source-record", SOURCES),
            ("probe-record", PROBE_REFERENCES),
            ("inventory", super::INVENTORY),
            ("outcome", super::OUTCOMES),
        ] {
            for (identity, value) in self.read_all::<serde_json::Value>(table)? {
                if kind == "plan" {
                    self.reserve_identity(&identity)?;
                }
                let bytes = encode(&value)?.len() as u64
                    + if kind == "outcome" {
                        identity.len() as u64
                    } else {
                        0
                    };
                if !self.reserve(&format!("{kind}:{identity}"), bytes)? {
                    return Err(StoreError::Quota);
                }
            }
        }
        // A crash may follow fsync+rename but precede the metadata transaction.
        // Adopt that exact object instead of rebuilding it under a later
        // retirement epoch or asking the API to sign it again.
        for entry in std::fs::read_dir(self.root.join("artifacts")).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| storage("invalid artifact filename"))?;
            let identity = name
                .strip_suffix(".json")
                .ok_or_else(|| storage("invalid artifact filename"))?;
            if self
                .read_value::<ArtifactFile>(super::ARTIFACTS, identity)?
                .is_some()
            {
                continue;
            }
            let (bytes, sha256) = hash_file(&entry.path())?;
            let file = ArtifactFile {
                bytes,
                sha256,
                acknowledged: false,
            };
            let artifact = self.read_artifact_file(identity, &file)?;
            if artifact.envelope.protocol
                != trusted_server_provenance_encypher::preparation::protocol::PREPARATION_PROTOCOL
                || !artifact.target.is_well_formed()
                || self.retirement(identity)?.is_some()
            {
                return Err(storage("orphan artifact identity invalid"));
            }
            if !self.reserve(
                &format!("artifact:{identity}"),
                bytes + encode(&file)?.len() as u64,
            )? {
                return Err(StoreError::Quota);
            }
            self.put(super::ARTIFACTS, identity, &file)?;
        }
        for (identity, file) in self.read_all::<ArtifactFile>(super::ARTIFACTS)? {
            self.read_artifact_file(&identity, &file)?;
            self.reserve_identity(&identity)?;
            if !self.reserve(
                &format!("artifact:{identity}"),
                file.bytes + encode(&file)?.len() as u64,
            )? {
                return Err(StoreError::Quota);
            }
        }
        for (identity, file) in self.read_all::<ArtifactFile>(TEXT_OPERATIONS)? {
            self.reserve_identity(&identity)?;
            if hash_file(&self.object_path("text", &identity)?)?
                != (file.bytes, file.sha256.clone())
            {
                return Err(storage("retained text operation digest mismatch"));
            }
            if !self.reserve(
                &format!("text:{identity}"),
                file.bytes + encode(&file)?.len() as u64,
            )? {
                return Err(StoreError::Quota);
            }
        }
        for (sha, source) in self.read_all::<SourceRecord>(SOURCES)? {
            if !source.references.is_empty() {
                let path = self.object_path("source", &sha)?;
                if hash_file(&path)?.1 != sha {
                    return Err(storage("retained public source corrupt"));
                }
            }
        }
        Ok(())
    }

    pub(super) fn remove_artifact_file(&self, identity: &str) -> Result<(), StoreError> {
        let path = self.object_path("artifact", identity)?;
        match std::fs::remove_file(&path) {
            Ok(()) => sync_directory(
                path.parent()
                    .ok_or_else(|| storage("missing artifact directory"))?,
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
        self.release(&format!("artifact:{identity}"))
    }

    pub(super) fn remove_text_file(&self, identity: &str) -> Result<(), StoreError> {
        let path = self.object_path("text", identity)?;
        match std::fs::remove_file(&path) {
            Ok(()) => sync_directory(
                path.parent()
                    .ok_or_else(|| storage("missing text directory"))?,
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
        self.release(&format!("text:{identity}"))
    }

    pub(super) fn remove_unreferenced_sources(&self) -> Result<(), StoreError> {
        for (sha, source) in self.read_all::<SourceRecord>(SOURCES)? {
            if !source.references.is_empty() {
                continue;
            }
            let path = self.object_path("source", &sha)?;
            match std::fs::remove_file(&path) {
                Ok(()) => sync_directory(
                    path.parent()
                        .ok_or_else(|| storage("missing source directory"))?,
                )?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
            }
            self.release(&format!("source:{sha}"))?;
            // Keep the tiny nonpublic record as a durable admission tombstone.
        }
        Ok(())
    }
}
