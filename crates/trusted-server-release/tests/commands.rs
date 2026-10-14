use std::fs;
use std::process::Command;

use tempfile::tempdir;

#[test]
fn producer_dispatch_remains_closed_for_every_invocation() {
    let root = tempdir().expect("should create private process fixture");
    fs::write(root.path().join("sentinel"), b"untouched").expect("should create sentinel");
    for arguments in [
        vec![],
        vec!["--help"],
        vec!["--version"],
        vec!["preflight", "--require-qualified"],
        vec!["schema", "--schema-file", "override.json"],
        vec!["publish"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_trusted-server-release"))
            .args(&arguments)
            .current_dir(root.path())
            .output()
            .expect("should run closed producer binary");
        assert!(
            !output.status.success(),
            "should reject pending invocation {arguments:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "should emit no result or release bytes"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("release execution remains disabled"),
            "should explain pending execution gate"
        );
        assert_eq!(
            fs::read_dir(root.path())
                .expect("should inspect process writes")
                .count(),
            1,
            "should create no release output"
        );
    }
    assert_eq!(
        fs::read(root.path().join("sentinel")).expect("should read sentinel"),
        b"untouched",
        "should preserve existing operator files"
    );
}

#[test]
fn inherited_approval_labels_cannot_enable_pending_execution() {
    let output = Command::new(env!("CARGO_BIN_EXE_trusted-server-release"))
        .args(["preflight", "--require-qualified"])
        .env("TS_RELEASE_BUILD", "1")
        .env("TS_RELEASE_SOURCE_COMMIT", "a".repeat(40))
        .env("RELEASE_AUTHENTICATED", "true")
        .env("RELEASE_QUALIFIED", "true")
        .output()
        .expect("should run pending producer");
    assert!(
        !output.status.success(),
        "should reject inherited labels as execution authority"
    );
    assert!(
        output.stdout.is_empty(),
        "should return no release evidence"
    );
}
