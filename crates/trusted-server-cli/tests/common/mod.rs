//! Shared git-repo fixture helpers for the integration tests.
//!
//! All operations go through `gix` — no subprocess, no `git` binary.
//! Commits use a fixed signature so they do not depend on ambient
//! `user.name` / `user.email` config and are deterministic across
//! runs (clean CI machines included).

// Each integration-test file `mod common;`s this and uses a subset
// of the helpers.
#![allow(dead_code)]

use std::fs;
use std::path::Path;

use gix::ObjectId;
use gix::bstr::BString;

/// Fixed signature for all fixture commits.
fn test_signature() -> gix::actor::Signature {
    gix::actor::Signature {
        name: BString::from("ts dev lint tests"),
        email: BString::from("tests@example.com"),
        time: gix::date::Time::new(1_700_000_000, 0),
    }
}

/// Initialise a fresh repository at `path`.
pub(crate) fn init_repo(path: &Path) -> gix::Repository {
    gix::init(path).expect("should init gix repo")
}

/// Stage every file currently in the working tree: write a blob per
/// file and rebuild the index from scratch. The `.git` directory is
/// skipped. Paths are stored with `/` separators relative to the
/// work directory.
pub(crate) fn stage_all(repo: &gix::Repository) {
    let work_dir = repo
        .workdir()
        .expect("fixture repo should have a work directory")
        .to_path_buf();

    let mut files: Vec<(BString, ObjectId)> = Vec::new();
    collect_files(repo, &work_dir, &work_dir, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut state = gix::index::State::new(repo.object_hash());
    for (path, oid) in files {
        state.dangerously_push_entry(
            gix::index::entry::Stat::default(),
            oid,
            gix::index::entry::Flags::empty(),
            gix::index::entry::Mode::FILE,
            path.as_ref(),
        );
    }
    state.sort_entries();

    let mut file = gix::index::File::from_state(state, repo.index_path());
    file.write(gix::index::write::Options::default())
        .expect("should write index file");
}

/// Recursively collect `(relative_path, blob_id)` for every file
/// under `dir`, skipping the `.git` directory.
fn collect_files(
    repo: &gix::Repository,
    work_dir: &Path,
    dir: &Path,
    out: &mut Vec<(BString, ObjectId)>,
) {
    for entry in fs::read_dir(dir).expect("should read fixture directory") {
        let entry = entry.expect("should read directory entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("should read file type");
        if file_type.is_dir() {
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            collect_files(repo, work_dir, &path, out);
        } else if file_type.is_file() {
            let content = fs::read(&path).expect("should read fixture file");
            let oid = repo
                .write_blob(&content)
                .expect("should write blob")
                .detach();
            let rel = path
                .strip_prefix(work_dir)
                .expect("file should be under work dir");
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            out.push((BString::from(rel_str.as_bytes()), oid));
        }
    }
}

/// Build a tree from the current index and commit it to `HEAD`,
/// parented on the current `HEAD` commit (if any).
pub(crate) fn commit_all(repo: &gix::Repository, message: &str) -> ObjectId {
    commit_index_to_ref(repo, "HEAD", message)
}

/// Like [`commit_all`] but commits to an explicit branch ref
/// (e.g. `refs/heads/feature`).
pub(crate) fn commit_all_as_branch(
    repo: &gix::Repository,
    branch_ref: &str,
    message: &str,
) -> ObjectId {
    commit_index_to_ref(repo, branch_ref, message)
}

fn commit_index_to_ref(repo: &gix::Repository, target_ref: &str, message: &str) -> ObjectId {
    // Build a tree from the index entries via the tree editor.
    let index = repo.index().expect("should read index");
    let empty_tree_id = repo.empty_tree().id;
    let mut editor = repo
        .edit_tree(empty_tree_id)
        .expect("should create tree editor");
    for entry in index.entries() {
        let path = entry.path(&index);
        editor
            .upsert(
                path.to_string(),
                gix::object::tree::EntryKind::Blob,
                entry.id,
            )
            .expect("should upsert index entry into tree");
    }
    let tree_id = editor.write().expect("should write tree").detach();

    let parents: Vec<ObjectId> = repo
        .head_id()
        .ok()
        .map(|id| vec![id.detach()])
        .unwrap_or_default();

    let sig = test_signature();
    let mut author_time_buf = gix::date::parse::TimeBuf::default();
    let mut committer_time_buf = gix::date::parse::TimeBuf::default();
    repo.commit_as(
        sig.to_ref(&mut committer_time_buf),
        sig.to_ref(&mut author_time_buf),
        target_ref,
        message,
        tree_id,
        parents,
    )
    .expect("should write commit")
    .detach()
}

/// Create `refs/heads/<branch>` pointing at the current `HEAD`
/// commit and move `HEAD` to it (symbolic).
pub(crate) fn create_and_checkout_branch(repo: &gix::Repository, branch: &str) {
    let head = repo.head_id().expect("HEAD should exist").detach();
    let full_ref = format!("refs/heads/{branch}");
    repo.reference(
        full_ref.as_str(),
        head,
        gix::refs::transaction::PreviousValue::Any,
        format!("create branch {branch}"),
    )
    .expect("should create branch ref");

    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::{FullName, Target};
    let full: FullName = full_ref
        .as_str()
        .try_into()
        .expect("should parse branch FullName");
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: BString::from(format!("checkout {branch}")),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(full),
        },
        name: "HEAD".try_into().expect("HEAD is a valid ref name"),
        deref: false,
    };
    repo.edit_reference(edit)
        .expect("should move HEAD to the new branch");
}
