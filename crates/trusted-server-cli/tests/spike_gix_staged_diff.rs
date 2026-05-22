//! Spike: prove that gix can give us per-blob hunk information for
//! files staged in the index relative to the HEAD tree, with new-side
//! line numbers. Once this test passes, the chosen entry points are
//! pinned for the `staged_added_lines()` implementation in Phase 4.
//!
//! No shell, no `git` binary anywhere. Fixture setup uses gix
//! exclusively: `write_blob` + `edit_tree` + `commit_as` for the HEAD
//! commit; `gix::index::State` for the staged index.

use std::collections::HashMap;

use gix::ObjectId;
use gix::bstr::BString;
use tempfile::tempdir;

#[test]
fn staged_blob_diff_yields_new_side_line_numbers() {
    let temp = tempdir().expect("should create tempdir");
    let repo_path = temp.path();
    let repo = gix::init(repo_path).expect("should init gix repo");

    // Commit 1: a.txt with three lines.
    let blob1 = repo
        .write_blob(b"alpha\nbeta\ngamma\n")
        .expect("should write blob1")
        .detach();
    let tree1 = build_tree_with_file(&repo, "a.txt", blob1);
    let _commit1 = commit_tree(&repo, tree1, "initial", &[]);

    // Stage a modification adding a new line at position 2 (without
    // touching the working tree — the index points at the new blob
    // directly).
    let blob2 = repo
        .write_blob(b"alpha\nNEW LINE\nbeta\ngamma\n")
        .expect("should write blob2")
        .detach();
    write_index(&repo, &[("a.txt", blob2)]);

    // Conceptual operation: enumerate index-vs-HEAD changes, then
    // for each modified blob produce hunks with new-side line numbers.
    let added = staged_added_lines(&repo).expect("should collect staged added lines");

    assert_eq!(added.len(), 1, "should have one added line: {added:?}");
    let (path, line_no, content) = &added[0];
    assert_eq!(path.to_string(), "a.txt", "path");
    assert_eq!(*line_no, 2usize, "new-side line number");
    assert_eq!(content, "NEW LINE", "content");
}

// === Gix-only fixture helpers ===

/// Fixed signature for test commits — independent of ambient
/// user.name / user.email so the test runs identically on clean CI
/// machines.
fn test_signature() -> gix::actor::Signature {
    gix::actor::Signature {
        name: BString::from("ts dev lint tests"),
        email: BString::from("tests@example.com"),
        time: gix::date::Time::new(1_700_000_000, 0),
    }
}

fn build_tree_with_file(repo: &gix::Repository, name: &str, blob_id: ObjectId) -> ObjectId {
    let empty_tree_id = repo.empty_tree().id;
    let mut editor = repo
        .edit_tree(empty_tree_id)
        .expect("should create tree editor");
    editor
        .upsert(name, gix::object::tree::EntryKind::Blob, blob_id)
        .expect("should upsert blob entry");
    editor.write().expect("should write tree").detach()
}

fn commit_tree(
    repo: &gix::Repository,
    tree_id: ObjectId,
    message: &str,
    parents: &[ObjectId],
) -> ObjectId {
    let sig = test_signature();
    let mut author_time_buf = gix::date::parse::TimeBuf::default();
    let mut committer_time_buf = gix::date::parse::TimeBuf::default();
    repo.commit_as(
        sig.to_ref(&mut committer_time_buf),
        sig.to_ref(&mut author_time_buf),
        "HEAD",
        message,
        tree_id,
        parents.iter().copied(),
    )
    .expect("should write commit and update HEAD")
    .detach()
}

/// Write a fresh index containing exactly the listed entries. Bypasses
/// the working tree — the staged diff machinery only reads the index,
/// not the working tree.
fn write_index(repo: &gix::Repository, entries: &[(&str, ObjectId)]) {
    let mut state = gix::index::State::new(repo.object_hash());
    for (path, oid) in entries {
        let path_bytes: BString = BString::from(path.as_bytes());
        state.dangerously_push_entry(
            gix::index::entry::Stat::default(),
            *oid,
            gix::index::entry::Flags::empty(),
            gix::index::entry::Mode::FILE,
            path_bytes.as_ref(),
        );
    }
    state.sort_entries();

    let index_path = repo.index_path();
    let mut file = gix::index::File::from_state(state, index_path);
    file.write(gix::index::write::Options::default())
        .expect("should write index file");
}

// === Conceptual operation under test ===

type Added = Vec<(BString, usize, String)>;

fn staged_added_lines(repo: &gix::Repository) -> Result<Added, Box<dyn std::error::Error>> {
    let head_tree_id = repo.head_commit()?.tree_id()?;
    let head_tree = repo.find_tree(head_tree_id)?;

    let mut head_map: HashMap<BString, ObjectId> = HashMap::new();
    for entry in head_tree.traverse().breadthfirst.files()? {
        if entry.mode.is_blob() {
            head_map.insert(entry.filepath, entry.oid);
        }
    }

    let index = repo.index()?;
    let mut index_map: HashMap<BString, ObjectId> = HashMap::new();
    for entry in index.entries() {
        if entry.mode.contains(gix::index::entry::Mode::FILE) {
            let path = entry.path(&index);
            index_map.insert(path.to_owned(), entry.id);
        }
    }

    let mut out: Added = Vec::new();
    let mut all_paths: Vec<&BString> = index_map.keys().chain(head_map.keys()).collect();
    all_paths.sort();
    all_paths.dedup();

    for path in all_paths {
        let head_id = head_map.get(path);
        let idx_id = index_map.get(path);
        let (old_bytes, new_bytes) = match (head_id, idx_id) {
            (Some(h), Some(i)) if h == i => continue, // unchanged
            (Some(h), Some(i)) => (read_blob(repo, *h)?, read_blob(repo, *i)?),
            (None, Some(i)) => (Vec::new(), read_blob(repo, *i)?),
            (Some(_), None) => continue, // Deletion: no added lines
            (None, None) => continue,
        };

        let old_text = String::from_utf8_lossy(&old_bytes).into_owned();
        let new_text = String::from_utf8_lossy(&new_bytes).into_owned();

        for (line_idx, line) in added_line_indices(&old_text, &new_text) {
            // line_idx is 0-based after-token index; convert to 1-based file line.
            out.push((path.clone(), line_idx + 1, line));
        }
    }

    Ok(out)
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
