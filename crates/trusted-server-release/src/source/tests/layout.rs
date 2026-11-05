//! Repository layout, configuration and Git lookup checks.

use super::*;

#[test]
fn rejects_extensionless_rust_toolchain_before_other_checks() {
    let fixture = Fixture::new();
    write(&fixture.root, "rust-toolchain", b"nightly\n");
    write(
        &fixture.root,
        ".git/objects/info/alternates",
        b"/elsewhere\n",
    );
    assert_rejects(&fixture, "rust-toolchain");
}

#[test]
fn rejects_linked_worktree_gitfile_indirection() {
    let fixture = Fixture::new();
    fs::rename(
        fixture.root.join(".git"),
        fixture.parent.path().join("moved-git"),
    )
    .expect("should move git directory");
    write(&fixture.root, ".git", b"gitdir: ../moved-git\n");
    assert_rejects(&fixture, "unsupported");
}

#[test]
fn rejects_commondir_indirection() {
    let fixture = Fixture::new();
    write(&fixture.root, ".git/commondir", b"../other\n");
    assert_rejects(&fixture, "commondir");
}

#[test]
fn rejects_object_and_history_rewriting_channels() {
    for (path, needle) in [
        ("objects/info/alternates", ".git/objects/info/alternates"),
        (
            "objects/info/http-alternates",
            ".git/objects/info/http-alternates",
        ),
        ("info/grafts", ".git/info/grafts"),
        ("shallow", ".git/shallow"),
        ("info/attributes", ".git/info/attributes"),
        (
            "refs/replace/0000000000000000000000000000000000000000",
            ".git/refs/replace",
        ),
    ] {
        let fixture = Fixture::new();
        write(&fixture.root.join(".git"), path, b"fictional\n");
        assert_rejects(&fixture, needle);
    }
}

#[test]
fn rejects_packed_replace_refs() {
    let fixture = Fixture::new();
    let line = format!(
        "# pack-refs with: peeled fully-peeled sorted\n{} refs/replace/{}\n",
        fixture.commit, fixture.commit
    );
    write(&fixture.root, ".git/packed-refs", line.as_bytes());
    assert_rejects(&fixture, "refs/replace");
}

#[test]
fn rejects_unreviewed_git_config_keys() {
    for addition in [
        "[include]\n\tpath = /dev/null\n",
        "[includeIf \"gitdir:/\"]\n\tpath = /dev/null\n",
        "[core]\n\tfsmonitor = true\n",
        "[core]\n\thooksPath = /dev/null\n",
        "[core]\n\tsshCommand = ssh\n",
        "[core]\n\tworktree = /\n",
        "[filter \"lfs\"]\n\tclean = cat\n",
        "[diff \"x\"]\n\ttextconv = cat\n",
        "[extensions]\n\tobjectformat = sha256\n",
        "[user]\n\tname = Fictional\n",
    ] {
        let fixture = Fixture::new();
        let config = fixture.root.join(".git/config");
        let mut text = fs::read_to_string(&config).expect("should read fixture config");
        text.push_str(addition);
        fs::write(&config, text).expect("should write fixture config");
        assert_rejects(&fixture, "git config");
    }
}

#[test]
fn parses_only_allowlisted_config_values() {
    assert!(
        check_git_config("[core]\n\trepositoryformatversion = 0\n\tbare = false\n[remote \"origin\"]\n\turl = https://example.com/r.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n").is_ok(),
        "should accept allowlisted keys"
    );
    for text in [
        "[core]\n\trepositoryformatversion = 1\n",
        "[core]\n\tbare = true\n",
        "[core]\n\tfilemode = maybe\n",
        "[core]\n\tsymlinks = \"true\"\n",
        "[core]\n\tfilemode = true \\\n",
        "[remote \"origin\"]\n\tpushurl = https://example.com\n",
        "[core.x]\n\tfilemode = true\n",
        "\tfilemode = true\n",
    ] {
        assert!(
            check_git_config(text).is_err(),
            "should reject config {text:?}"
        );
    }
}

#[test]
fn rejects_fsmonitor_without_executing_it() {
    let fixture = Fixture::new();
    let marker = fixture.parent.path().join("fsmonitor-ran");
    let hook = fixture.parent.path().join("fsmonitor.sh");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display()))
        .expect("should write hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("should chmod hook");
    let config = fixture.root.join(".git/config");
    let mut text = fs::read_to_string(&config).expect("should read config");
    text.push_str(&format!("[core]\n\tfsmonitor = {}\n", hook.display()));
    fs::write(&config, text).expect("should write config");
    assert_rejects(&fixture, "git config");
    assert!(
        !marker.exists(),
        "should never execute the fsmonitor command"
    );
}

/// Audits an environment whose only PATH entry holds a fake `git` that leaves a marker.
fn fake_git_environment(fixture: &Fixture) -> (AuditedEnvironment, PathBuf) {
    let bin = fixture.parent.path().join("fake-bin");
    fs::create_dir_all(&bin).expect("should create fake bin");
    let marker = fixture.parent.path().join("fake-git-ran");
    let script = bin.join("git");
    fs::write(
        &script,
        format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
    )
    .expect("should write fake git");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("should chmod");
    let audited = audit_environment(&[(OsString::from("PATH"), bin.into_os_string())])
        .expect("should audit fake PATH");
    (audited, marker)
}

#[test]
fn config_audit_rejects_before_any_git_process_spawns() {
    let control = Fixture::new();
    let (audited, marker) = fake_git_environment(&control);
    assert!(
        approve(&control.root, &control.commit, &audited).is_err(),
        "should fail when the fake git exits nonzero"
    );
    assert!(
        marker.exists(),
        "control should prove the fake git is reachable"
    );

    let fixture = Fixture::new();
    let (audited, marker) = fake_git_environment(&fixture);
    let config = fixture.root.join(".git/config");
    let mut text = fs::read_to_string(&config).expect("should read config");
    text.push_str("[core]\n\tfsmonitor = true\n");
    fs::write(&config, text).expect("should write config");
    let text = format!(
        "{:?}",
        approve(&fixture.root, &fixture.commit, &audited)
            .err()
            .expect("should reject fsmonitor config")
    );
    assert!(
        text.contains("git config"),
        "should name the config audit: {text}"
    );
    assert!(
        !marker.exists(),
        "should reject before spawning any git process"
    );
}

#[test]
fn ignores_relative_path_entries_when_locating_git() {
    let fixture = Fixture::new();
    let (_, marker) = fake_git_environment(&fixture);
    let audited = audit_environment(&[(OsString::from("PATH"), OsString::from("fake-bin"))])
        .expect("should audit relative PATH");
    let text = format!(
        "{:?}",
        approve(
            &fixture.parent.path().join("source"),
            &fixture.commit,
            &audited
        )
        .err()
        .expect("should not find git on a relative entry")
    );
    assert!(
        text.contains("absolute PATH"),
        "should explain git lookup: {text}"
    );
    assert!(
        !marker.exists(),
        "should never run a relatively located git"
    );
}

#[test]
fn rejects_cargo_config_in_any_ancestor_directory() {
    for name in ["config", "config.toml"] {
        let fixture = Fixture::new();
        write(
            fixture.parent.path(),
            &format!(".cargo/{name}"),
            b"[build]\n",
        );
        assert_rejects(&fixture, ".cargo");
    }
}

#[test]
fn rejects_unreadable_packed_refs() {
    let fixture = Fixture::new();
    let packed = fixture.root.join(".git/packed-refs");
    write(
        &fixture.root,
        ".git/packed-refs",
        b"# pack-refs with: peeled\n",
    );
    fs::set_permissions(&packed, fs::Permissions::from_mode(0o000)).expect("should chmod");
    if fs::read(&packed).is_ok() {
        // Privileged runs can read mode-000 files, so the denial cannot be exercised.
        return;
    }
    assert_rejects(&fixture, "packed-refs");
}
