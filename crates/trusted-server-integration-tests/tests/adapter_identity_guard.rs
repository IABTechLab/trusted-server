//! Guards that adapter crates reach identity only through the core identity app.
//!
//! Adapter crates supply platform services; the identity routes, the partner
//! registry and the EC lifecycle live in `trusted_server_core::ec::app`. This
//! test scans every adapter's non-test source for direct uses of identity
//! entry points. Uses that the EC lifecycle move has not yet taken into core
//! are listed in [`PENDING_LIFECYCLE_MOVE`]; an entry that no longer matches
//! fails the test so the list only shrinks.

use std::fs;
use std::path::{Path, PathBuf};

const ADAPTER_CRATES: &[&str] = &[
    "trusted-server-adapter-axum",
    "trusted-server-adapter-cloudflare",
    "trusted-server-adapter-fastly",
    "trusted-server-adapter-spin",
];

const IDENTITY_ENTRY_POINTS: &[&str] = &[
    "PartnerRegistry::from_config",
    "admin_ec_lookup_not_supported",
    "answer_batch_sync_locally",
    "answer_identify_locally",
    "dispatch_pull_sync",
    "ec_finalize_response",
    "generate_if_needed",
    "handle_admin_ec_lookup",
    "handle_admin_eids_lookup",
    "handle_batch_sync",
    "handle_identify",
];

/// Adapter uses that remain until the EC lifecycle and the identify and
/// batch-sync routes move into the core identity app, as `(file, symbol)`.
const PENDING_LIFECYCLE_MOVE: &[(&str, &str)] = &[
    (
        "trusted-server-adapter-axum/src/app.rs",
        "answer_batch_sync_locally",
    ),
    (
        "trusted-server-adapter-axum/src/app.rs",
        "answer_identify_locally",
    ),
    (
        "trusted-server-adapter-cloudflare/src/app.rs",
        "answer_batch_sync_locally",
    ),
    (
        "trusted-server-adapter-cloudflare/src/app.rs",
        "answer_identify_locally",
    ),
    (
        "trusted-server-adapter-fastly/src/app.rs",
        "answer_batch_sync_locally",
    ),
    (
        "trusted-server-adapter-fastly/src/app.rs",
        "answer_identify_locally",
    ),
    (
        "trusted-server-adapter-fastly/src/app.rs",
        "generate_if_needed",
    ),
    (
        "trusted-server-adapter-fastly/src/app.rs",
        "handle_batch_sync",
    ),
    (
        "trusted-server-adapter-fastly/src/app.rs",
        "handle_identify",
    ),
    (
        "trusted-server-adapter-fastly/src/main.rs",
        "ec_finalize_response",
    ),
    (
        "trusted-server-adapter-fastly/src/post_delivery.rs",
        "dispatch_pull_sync",
    ),
    (
        "trusted-server-adapter-spin/src/app.rs",
        "answer_batch_sync_locally",
    ),
    (
        "trusted-server-adapter-spin/src/app.rs",
        "answer_identify_locally",
    ),
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("should have a crates directory")
        .to_path_buf()
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("should read adapter source directory");
    for entry in entries {
        let path = entry.expect("should read directory entry").path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn brace_delta(line: &str) -> i64 {
    let opens = line.matches('{').count();
    let closes = line.matches('}').count();
    i64::try_from(opens).expect("should fit in i64")
        - i64::try_from(closes).expect("should fit in i64")
}

/// Returns the source with `#[cfg(test)]` items and comment lines removed.
fn non_test_source(source: &str) -> String {
    let mut kept = String::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            let mut depth = 0_i64;
            for item_line in lines.by_ref() {
                depth += brace_delta(item_line);
                let item_trimmed = item_line.trim_end();
                let item_ends = item_trimmed.ends_with(';') || item_trimmed.ends_with('}');
                if depth <= 0 && item_ends {
                    break;
                }
            }
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

fn adapter_uses() -> Vec<(String, &'static str)> {
    let crates = crates_dir();
    let mut uses = Vec::new();
    for adapter in ADAPTER_CRATES {
        let mut files = Vec::new();
        rust_files(&crates.join(adapter).join("src"), &mut files);
        for file in files {
            let source = fs::read_to_string(&file).expect("should read adapter source file");
            let code = non_test_source(&source);
            let relative = file
                .strip_prefix(&crates)
                .expect("should be under the crates directory")
                .to_string_lossy()
                .replace('\\', "/");
            for symbol in IDENTITY_ENTRY_POINTS {
                if code.contains(symbol) {
                    uses.push((relative.clone(), *symbol));
                }
            }
        }
    }
    uses
}

#[test]
fn adapters_reach_identity_only_through_the_core_app() {
    let uses = adapter_uses();

    let unexpected: Vec<_> = uses
        .iter()
        .filter(|(file, symbol)| !PENDING_LIFECYCLE_MOVE.contains(&(file.as_str(), *symbol)))
        .collect();

    assert!(
        unexpected.is_empty(),
        "should reach identity through trusted_server_core::ec::app, found direct uses: {unexpected:?}"
    );
}

#[test]
fn pending_lifecycle_entries_are_still_in_use() {
    let uses = adapter_uses();

    let stale: Vec<_> = PENDING_LIFECYCLE_MOVE
        .iter()
        .filter(|(file, symbol)| {
            !uses
                .iter()
                .any(|(used_file, used)| used_file == file && used == symbol)
        })
        .collect();

    assert!(
        stale.is_empty(),
        "should remove pending entries that adapters no longer use: {stale:?}"
    );
}

#[test]
fn non_test_source_drops_test_items_and_comments() {
    let source = "fn live() { handle_identify(); }\n// handle_batch_sync\n#[cfg(test)]\nmod tests {\n    fn t() {\n        dispatch_pull_sync();\n    }\n}\n#[cfg(test)]\nuse crate::ec_finalize_response;\nfn after() {}\n";

    let code = non_test_source(source);

    assert!(code.contains("handle_identify"), "should keep live code");
    assert!(
        !code.contains("handle_batch_sync"),
        "should drop comment lines"
    );
    assert!(
        !code.contains("dispatch_pull_sync"),
        "should drop test modules"
    );
    assert!(
        !code.contains("ec_finalize_response"),
        "should drop test-only items"
    );
    assert!(
        code.contains("fn after"),
        "should keep code after test items"
    );
}
