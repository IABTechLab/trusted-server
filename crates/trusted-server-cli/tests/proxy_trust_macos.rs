//! macOS command failure tests use fake security commands, never a real keychain.
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

#[test]
fn keychain_query_failure_aborts_rotation_but_not_found_allows_it() {
    let home = tempfile::tempdir().expect("should create isolated home");
    let ca_dir = home.path().join("ca");
    let command = |action: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ts"));
        cmd.env("HOME", home.path())
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("PATH", home.path())
            .args(["dev", "proxy", "--ca-dir"])
            .arg(&ca_dir)
            .args(["ca", action]);
        cmd
    };
    assert!(
        command("path")
            .output()
            .expect("should create CA")
            .status
            .success()
    );
    let key = fs::read(ca_dir.join("ca-key.pem")).expect("should read original key");
    let security = home.path().join("security");
    fs::write(&security, "#!/bin/sh\nexit 1\n").expect("should write failing query tool");
    fs::set_permissions(&security, fs::Permissions::from_mode(0o700))
        .expect("should make executable");
    for action in ["install", "uninstall", "regenerate"] {
        assert!(
            !command(action)
                .output()
                .expect("should run CA command")
                .status
                .success()
        );
        assert_eq!(
            key,
            fs::read(ca_dir.join("ca-key.pem")).expect("should retain key")
        );
    }
    fs::write(
        &security,
        "#!/bin/sh\ntest \"$1\" = find-certificate || exit 99\nexit 44\n",
    )
    .expect("should write not-found query tool");
    assert!(
        command("regenerate")
            .output()
            .expect("should rotate absent CA")
            .status
            .success()
    );
    assert_ne!(
        key,
        fs::read(ca_dir.join("ca-key.pem")).expect("should read rotated key")
    );
}
