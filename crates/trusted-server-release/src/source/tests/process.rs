//! Git plumbing uses the same bounded child supervision as release recipes.

use std::time::{Duration, Instant};

use super::*;

#[test]
fn git_plumbing_deadline_terminates_the_child_group() {
    let fixture = Fixture::new();
    let tools = fixture.parent.path().join("fake-tools");
    fs::create_dir(&tools).expect("should create fixture tools");
    let fake = tools.join("git");
    fs::write(&fake, b"#!/bin/sh\n/bin/sleep 2\n")
        .expect("should create finite hanging git fixture");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700))
        .expect("should make fixture executable");
    let environment = audit_environment(&[(OsString::from("PATH"), tools.into_os_string())])
        .expect("should audit fixture environment");
    let git = Git::locate(&fixture.root, &environment)
        .expect("should locate fixture git")
        .with_wall_limit(Duration::from_millis(50));
    let started = Instant::now();
    assert!(
        git.run(&["rev-parse", "HEAD"], 1024).is_err(),
        "should reject a child that exceeds the Git deadline"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "should terminate the sleeping child group before its normal exit"
    );
}
