use std::env;
use std::fs;
use std::io::{Cursor, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use docs_parity::dependency_snapshot::{
    DependencySnapshotContext, generate_archive, validate_archive,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

fn context() -> DependencySnapshotContext {
    DependencySnapshotContext {
        repository: "IABTechLab/trusted-server".to_owned(),
        source_sha: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        source_ref: "refs/heads/main".to_owned(),
        run_id: 42,
        run_attempt: 3,
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_docs-parity"))
}

fn run_git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .expect("should execute git");
    assert!(status.success(), "git command should succeed");
}

fn generate_from(repository: &Path) -> Output {
    Command::new(binary())
        .current_dir(repository)
        .env("GITHUB_REPOSITORY", "IABTechLab/trusted-server")
        .env("GITHUB_SHA", "0123456789abcdef0123456789abcdef01234567")
        .env("GITHUB_REF", "refs/heads/main")
        .env("GITHUB_RUN_ID", "42")
        .env("GITHUB_RUN_ATTEMPT", "3")
        .args([
            "dependency-snapshot",
            "generate",
            "--output",
            "snapshot.zip",
        ])
        .output()
        .expect("should execute dependency generator")
}

fn rewrite_snapshot(archive_bytes: &[u8], change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes)).expect("should open fixture ZIP");
    let mut member = archive.by_index(0).expect("should open fixture member");
    let mut json = Vec::new();
    member
        .read_to_end(&mut json)
        .expect("should read fixture JSON");
    let mut value: serde_json::Value = serde_json::from_slice(&json).expect("should parse fixture");
    change(&mut value);
    let json = serde_json::to_vec(&value).expect("should encode changed fixture");
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    writer
        .start_file(
            "dependency-snapshot.json",
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .unix_permissions(0o644),
        )
        .expect("should create changed fixture member");
    writer
        .write_all(&json)
        .expect("should write changed fixture");
    writer
        .finish()
        .expect("should finish changed fixture")
        .into_inner()
}

#[test]
fn version_zero_snapshot_is_deterministic_and_round_trips() {
    let root_lock = br#"version = 4

[[package]]
name = "alpha"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0123456789abcdef"
"#;
    let tool_lock = br#"version = 4

[[package]]
name = "bravo"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abcdef0123456789"
"#;

    let first =
        generate_archive(&context(), root_lock, tool_lock).expect("should generate archive");
    let second =
        generate_archive(&context(), root_lock, tool_lock).expect("should regenerate archive");
    let parsed = validate_archive(&first, &context()).expect("should validate generated archive");

    assert_eq!(first, second, "snapshot ZIP should be byte-stable");
    assert_eq!(parsed.version, 0);
    assert_eq!(parsed.sha, context().source_sha);
    assert_eq!(parsed.source_ref, "refs/heads/main");
    assert_eq!(parsed.job.correlator, "trusted-server-docs-parity-v1");
    assert_eq!(parsed.job.id, "42.3");
    assert_eq!(parsed.detector.version, "0.1.0");
    assert_eq!(parsed.manifests.len(), 2);
}

#[test]
fn snapshot_rejects_unknown_fields_stale_context_and_noncanonical_packages() {
    let malformed_lock = br#"version = 4

[[package]]
name = "not valid"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    assert!(
        generate_archive(&context(), malformed_lock, b"version = 4\n").is_err(),
        "invalid Cargo package identity should fail"
    );

    let archive = generate_archive(&context(), b"version = 4\n", b"version = 4\n")
        .expect("empty locks should be valid");
    let mut stale = context();
    stale.run_attempt = 4;
    assert!(
        validate_archive(&archive, &stale).is_err(),
        "authenticated context mismatch should fail"
    );

    let populated = generate_archive(
        &context(),
        b"version = 4\n\n[[package]]\nname = \"alpha\"\nversion = \"1.2.3\"\n",
        b"version = 4\n",
    )
    .expect("should generate populated fixture");
    for package_url in [
        "pkg:cargo/bravo@1.2.3",
        "pkg:cargo/alpha@9.9.9",
        "pkg:cargo/alpha@1.2.3?repository_url=https://example.invalid",
        "pkg:cargo/alpha@1.2.3#fragment",
    ] {
        let changed = rewrite_snapshot(&populated, |snapshot| {
            snapshot["manifests"]["Cargo.lock"]["resolved"]["alpha@1.2.3"]["package_url"] =
                serde_json::Value::String(package_url.to_owned());
        });
        assert!(
            validate_archive(&changed, &context()).is_err(),
            "package URL must exactly bind the Cargo key: {package_url}"
        );
    }
}

#[test]
fn snapshot_enforces_archive_json_record_and_string_bounds() {
    let oversized = vec![b'x'; 4 * 1024 * 1024 + 1];
    assert!(
        validate_archive(&oversized, &context()).is_err(),
        "oversized ZIP should fail before parsing"
    );

    let long_name = "x".repeat(2_049);
    let lock = format!("version = 4\n\n[[package]]\nname = \"{long_name}\"\nversion = \"1.0.0\"\n");
    assert!(
        generate_archive(&context(), lock.as_bytes(), b"version = 4\n").is_err(),
        "oversized schema strings should fail"
    );

    let archive = generate_archive(&context(), b"version = 4\n", b"version = 4\n")
        .expect("should generate framing fixture");
    let mut prefixed = b"preamble".to_vec();
    prefixed.extend_from_slice(&archive);
    assert!(
        validate_archive(&prefixed, &context()).is_err(),
        "ZIP preambles must fail closed"
    );
    let mut suffixed = archive;
    suffixed.extend_from_slice(b"trailer");
    assert!(
        validate_archive(&suffixed, &context()).is_err(),
        "ZIP trailing bytes must fail closed"
    );
}

#[test]
fn repository_generation_requires_bounded_tracked_regular_lockfiles() {
    let directory = tempfile::tempdir().expect("should create repository fixture");
    fs::create_dir_all(directory.path().join("tools/docs-parity"))
        .expect("should create tool directory");
    fs::write(directory.path().join("Cargo.lock"), "version = 4\n")
        .expect("should write root lock");
    fs::write(
        directory.path().join("tools/docs-parity/Cargo.lock"),
        "version = 4\n",
    )
    .expect("should write tool lock");
    run_git(directory.path(), &["init", "--quiet"]);
    run_git(
        directory.path(),
        &["add", "Cargo.lock", "tools/docs-parity/Cargo.lock"],
    );
    let clean = generate_from(directory.path());
    assert!(clean.status.success(), "bounded regular locks should pass");

    #[cfg(unix)]
    {
        let root_lock = directory.path().join("Cargo.lock");
        fs::remove_file(&root_lock).expect("should replace root lock");
        symlink("tools/docs-parity/Cargo.lock", &root_lock).expect("should create lock symlink");
        assert!(
            !generate_from(directory.path()).status.success(),
            "a tracked lockfile symlink must fail"
        );
        fs::remove_file(&root_lock).expect("should remove lock symlink");
        fs::write(&root_lock, vec![b'x'; 2 * 1024 * 1024 + 1])
            .expect("should write oversized lock");
        assert!(
            !generate_from(directory.path()).status.success(),
            "an oversized lockfile must fail before parsing"
        );
    }
}
