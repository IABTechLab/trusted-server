//! Spike: prove that gix can compute a merge-base between two refs
//! and then run a tree-vs-tree diff with the same blob-diff
//! machinery the staged path uses. Locks in the API for
//! `changed_vs_added_lines()` in Phase 4.
//!
//! No shell, no `git` binary. All operations via gix.

use std::collections::HashMap;

use gix::ObjectId;
use gix::bstr::BString;
use tempfile::tempdir;

#[test]
fn merge_base_then_tree_diff_yields_added_lines() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let repo = gix::init(repo_path).expect("should init gix repo");

    // Base commit on `main`: a.txt = "one\n".
    let blob_base = repo
        .write_blob(b"one\n")
        .expect("should write base blob")
        .detach();
    let tree_base = build_tree(&repo, &[("a.txt", blob_base)]);
    let main_commit = commit_tree(&repo, tree_base, "main: first", &[], "HEAD");

    // Create branch `feature` pointing at HEAD.
    repo.reference(
        "refs/heads/feature",
        main_commit,
        gix::refs::transaction::PreviousValue::Any,
        "create feature branch",
    )
    .expect("should create feature ref");

    // Move HEAD to feature, commit an additional line.
    update_head_to(&repo, "refs/heads/feature");
    let blob_feature = repo
        .write_blob(b"one\ntwo\n")
        .expect("should write feature blob")
        .detach();
    let tree_feature = build_tree(&repo, &[("a.txt", blob_feature)]);
    let _feature_commit = commit_tree(
        &repo,
        tree_feature,
        "feature: add line",
        &[main_commit],
        "HEAD",
    );

    // Conceptual operation: merge-base("main", HEAD) → diff base-tree
    // vs HEAD-tree, emit added lines with new-side line numbers.
    let added = changed_vs_ref(&repo, "main").expect("should compute changed-vs added lines");

    assert_eq!(
        added,
        vec![("a.txt".into(), 2usize, "two".to_string())],
        "should report the single line the feature branch added"
    );
}

// === Fixture helpers ===

fn test_signature() -> gix::actor::Signature {
    gix::actor::Signature {
        name: BString::from("ts dev lint tests"),
        email: BString::from("tests@example.com"),
        time: gix::date::Time::new(1_700_000_000, 0),
    }
}

fn build_tree(repo: &gix::Repository, files: &[(&str, ObjectId)]) -> ObjectId {
    let empty_tree_id = repo.empty_tree().id;
    let mut editor = repo
        .edit_tree(empty_tree_id)
        .expect("should create tree editor");
    for (name, oid) in files {
        editor
            .upsert(*name, gix::object::tree::EntryKind::Blob, *oid)
            .expect("should upsert blob entry");
    }
    editor.write().expect("should write tree").detach()
}

fn commit_tree(
    repo: &gix::Repository,
    tree_id: ObjectId,
    message: &str,
    parents: &[ObjectId],
    target_ref: &str,
) -> ObjectId {
    let sig = test_signature();
    let mut author_time_buf = gix::date::parse::TimeBuf::default();
    let mut committer_time_buf = gix::date::parse::TimeBuf::default();
    repo.commit_as(
        sig.to_ref(&mut committer_time_buf),
        sig.to_ref(&mut author_time_buf),
        target_ref,
        message,
        tree_id,
        parents.iter().copied(),
    )
    .expect("should write commit")
    .detach()
}

fn update_head_to(repo: &gix::Repository, ref_name: &str) {
    // Move HEAD to point at the given ref (symbolic).
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::{FullName, Target};

    let full: FullName = ref_name.try_into().expect("should parse FullName from ref");
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: BString::from("checkout feature"),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(full),
        },
        name: "HEAD".try_into().expect("HEAD"),
        deref: false,
    };
    repo.edit_reference(edit)
        .expect("should update HEAD to symbolic ref");
}

// === Conceptual operation under test ===

type Added = Vec<(BString, usize, String)>;

fn changed_vs_ref(
    repo: &gix::Repository,
    reference: &str,
) -> Result<Added, Box<dyn std::error::Error>> {
    // Resolve base ref via the four-fallback order in spec
    // §"Base-ref resolution order".
    let base_id = resolve_base_ref(repo, reference)?;
    let head_id = repo.head_id()?.detach();
    let merge_base_id = repo.merge_base(base_id, head_id)?.detach();

    let base_tree_id = repo.find_commit(merge_base_id)?.tree_id()?.detach();
    let head_tree_id = repo.find_commit(head_id)?.tree_id()?.detach();

    let base_tree = repo.find_tree(base_tree_id)?;
    let head_tree = repo.find_tree(head_tree_id)?;

    let mut base_map: HashMap<BString, ObjectId> = HashMap::new();
    for entry in base_tree.traverse().breadthfirst.files()? {
        if entry.mode.is_blob() {
            base_map.insert(entry.filepath, entry.oid);
        }
    }
    let mut head_map: HashMap<BString, ObjectId> = HashMap::new();
    for entry in head_tree.traverse().breadthfirst.files()? {
        if entry.mode.is_blob() {
            head_map.insert(entry.filepath, entry.oid);
        }
    }

    let mut out: Added = Vec::new();
    let mut all_paths: Vec<&BString> = head_map.keys().chain(base_map.keys()).collect();
    all_paths.sort();
    all_paths.dedup();

    for path in all_paths {
        let old = base_map.get(path);
        let new = head_map.get(path);
        let (old_bytes, new_bytes) = match (old, new) {
            (Some(o), Some(n)) if o == n => continue,
            (Some(o), Some(n)) => (read_blob(repo, *o)?, read_blob(repo, *n)?),
            (None, Some(n)) => (Vec::new(), read_blob(repo, *n)?),
            (Some(_), None) => continue,
            (None, None) => continue,
        };

        let old_text = String::from_utf8_lossy(&old_bytes).into_owned();
        let new_text = String::from_utf8_lossy(&new_bytes).into_owned();
        for (line_idx, line) in added_line_indices(&old_text, &new_text) {
            out.push((path.clone(), line_idx + 1, line));
        }
    }

    Ok(out)
}

fn resolve_base_ref(
    repo: &gix::Repository,
    reference: &str,
) -> Result<ObjectId, Box<dyn std::error::Error>> {
    let candidates: [String; 4] = [
        reference.to_string(),
        format!("refs/heads/{reference}"),
        format!("refs/remotes/origin/{reference}"),
        format!("refs/tags/{reference}"),
    ];
    for candidate in &candidates {
        if let Ok(mut r) = repo.find_reference(candidate.as_str()) {
            let id = r.peel_to_id()?;
            return Ok(id.detach());
        }
    }
    Err(format!("ref `{reference}` not found; tried: {candidates:?}").into())
}

fn read_blob(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let obj = repo.find_object(id)?;
    Ok(obj.data.clone())
}

fn added_line_indices(before: &str, after: &str) -> Vec<(usize, String)> {
    use gix::diff::blob::{Algorithm, Diff, InternedInput};

    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Myers, &input);

    let after_lines: Vec<&str> = after.lines().collect();
    let mut out = Vec::new();
    for hunk in diff.hunks() {
        for token_idx in hunk.after.clone() {
            let line = after_lines
                .get(token_idx as usize)
                .copied()
                .unwrap_or("")
                .to_string();
            out.push((token_idx as usize, line));
        }
    }
    out
}
