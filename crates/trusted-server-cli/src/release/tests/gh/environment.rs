//! gh resolution, version qualification, environment isolation and
//! credential redaction.

use super::*;

// Resolution and version.

#[test]
fn resolves_explicit_path_to_canonical_target() {
    let fake = Fake::script("exit 0");
    let link = fake.file("gh-link");
    symlink(&fake.gh, &link).expect("should create symlink");

    let runner = resolve(GhLocation::Explicit(&link), &caller(&[("GH_TOKEN", TOKEN)]))
        .expect("should resolve explicit gh");

    assert_eq!(
        runner.executable(),
        fake.gh.canonicalize().expect("should canonicalize"),
        "should retain the canonical executable target"
    );
}

#[test]
fn resolves_from_trusted_path_once_ignoring_relative_entries() {
    let fake = Fake::script("exit 0");
    let path = format!("relative/bin:{}", fake.directory.path().display());

    let runner = resolve(
        GhLocation::SearchPath,
        &caller(&[("GH_TOKEN", TOKEN), ("PATH", &path)]),
    )
    .expect("should resolve gh from absolute PATH entry");

    assert_eq!(
        runner.executable(),
        fake.gh.canonicalize().expect("should canonicalize"),
        "should retain the PATH-resolved target"
    );
    assert_eq!(
        context(resolve(
            GhLocation::SearchPath,
            &caller(&[("GH_TOKEN", TOKEN), ("PATH", "relative/bin")])
        ))
        .0,
        ReleaseError::UnsupportedTool,
        "should not resolve gh through relative PATH entries"
    );
    assert_eq!(
        context(resolve(
            GhLocation::Explicit(Path::new("gh")),
            &caller(&[("GH_TOKEN", TOKEN)])
        ))
        .0,
        ReleaseError::UnsupportedTool,
        "should require an absolute explicit path"
    );
}

#[test]
fn rejects_unsupported_gh_version_with_diagnostic() {
    for prelude in [
        "echo 'gh version 2.96.0 (2026-01-01)'; exit 0\n",
        "echo 'gh version 2.97.0-rc1 (2026-01-01)'; exit 0\n",
        "echo 'gh version 2.97.00'; exit 0\n",
        "echo 'not gh'; exit 0\n",
        "exit 1\n",
    ] {
        let fake = Fake::with_version(prelude, "exit 0");

        let (error, text) = context(fake.runner(test_bounds()));

        assert!(
            matches!(
                error,
                ReleaseError::UnsupportedTool | ReleaseError::ExitFailure
            ),
            "should reject {prelude:?}: {error:?}"
        );
        if error == ReleaseError::UnsupportedTool {
            assert!(
                text.contains("2.97.0") && text.contains("does not authenticate"),
                "should explain the exact version requirement: {text}"
            );
        }
    }
}

// Environment isolation.

fn observed_environment(caller_pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    let fake = Fake::empty();
    let output = fake.file("env.txt");
    fake.install(&format!(
        "/usr/bin/env > '{}'\necho '[{{}}]'",
        output.display()
    ));
    let runner = resolve(GhLocation::Explicit(&fake.gh), &caller(caller_pairs))
        .expect("should resolve fake gh");

    run(&runner, AttemptDeadline::start(), &Cancellation::default()).expect("should run fake gh");

    fs::read_to_string(&output)
        .expect("should read observed environment")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[test]
fn child_environment_contains_only_reviewed_keys() {
    let environment = observed_environment(&[
        ("GH_TOKEN", TOKEN),
        ("GITHUB_TOKEN", FALLBACK_TOKEN),
        ("GH_HOST", "ghe.example.com"),
        ("GH_ENTERPRISE_TOKEN", "fictional-enterprise"),
        ("GITHUB_ENTERPRISE_TOKEN", "fictional-enterprise"),
        ("GH_REPO", "example/other"),
        ("GH_CONFIG_DIR", "/tmp/attacker-config"),
        ("GH_PATH", "/tmp/attacker-gh"),
        ("FASTLY_API_TOKEN", "fictional-provider"),
        ("HOME", "/home/operator"),
        ("PATH", "/opt/attacker/bin:/usr/bin"),
    ]);
    let allowed = [
        "DO_NOT_TRACK",
        "GH_CONFIG_DIR",
        "GH_NO_EXTENSION_UPDATE_NOTIFIER",
        "GH_NO_UPDATE_NOTIFIER",
        "GH_PROMPT_DISABLED",
        "GH_SPINNER_DISABLED",
        "GH_TOKEN",
        "HOME",
        "LC_ALL",
        "NO_COLOR",
        "PATH",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        // Added by the fake's own shell, not by the runner.
        "OLDPWD",
        "PWD",
        "SHLVL",
        "_",
    ];

    for key in environment.keys() {
        assert!(
            allowed.contains(&key.as_str()),
            "should not pass {key} to gh"
        );
    }
    assert_eq!(
        environment["GH_TOKEN"], TOKEN,
        "should prefer explicit GH_TOKEN"
    );
    assert_eq!(
        environment["PATH"], "/usr/bin:/bin",
        "should use minimal PATH"
    );
    assert_eq!(
        environment["GH_PROMPT_DISABLED"], "1",
        "should disable prompts"
    );
    let home = &environment["HOME"];
    assert!(
        !home.is_empty() && home != "/home/operator",
        "should isolate HOME"
    );
    for key in [
        "GH_CONFIG_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
    ] {
        assert_ne!(
            environment[key], "/tmp/attacker-config",
            "should isolate {key}"
        );
        assert!(
            Path::new(&environment[key]).is_absolute(),
            "should use absolute private {key}"
        );
    }
    assert!(
        !environment
            .values()
            .any(|value| value.contains(FALLBACK_TOKEN)
                || value.contains("fictional-enterprise")
                || value.contains("fictional-provider")),
        "should keep other credentials out of gh"
    );
}

#[test]
fn falls_back_to_github_token_only_as_gh_token() {
    let environment = observed_environment(&[("GITHUB_TOKEN", FALLBACK_TOKEN)]);

    assert_eq!(
        environment["GH_TOKEN"], FALLBACK_TOKEN,
        "should pass fallback as GH_TOKEN"
    );
    assert!(
        !environment.contains_key("GITHUB_TOKEN"),
        "should not pass GITHUB_TOKEN"
    );
}

#[test]
fn requires_explicit_token() {
    let fake = Fake::script("exit 0");

    for pairs in [vec![], vec![("GH_TOKEN", "")], vec![("GITHUB_TOKEN", "")]] {
        let (error, text) = context(resolve(GhLocation::Explicit(&fake.gh), &caller(&pairs)));

        assert_eq!(error, ReleaseError::Environment, "should require a token");
        assert!(
            text.contains("GH_TOKEN"),
            "should explain explicit token rule: {text}"
        );
    }
}

#[test]
fn rejects_loader_injection_variables() {
    let fake = Fake::script("exit 0");

    for key in [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
    ] {
        let (error, text) = context(resolve(
            GhLocation::Explicit(&fake.gh),
            &caller(&[("GH_TOKEN", TOKEN), (key, "/tmp/evil.so")]),
        ));

        assert_eq!(error, ReleaseError::Environment, "should reject {key}");
        assert!(
            text.contains(key),
            "should name the rejected variable: {text}"
        );
        assert!(!text.contains("/tmp/evil.so"), "should not echo the value");
    }
}

#[test]
fn debug_output_never_contains_token() {
    let fake = Fake::script("echo '[{}]'");
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");
    let failing = Fake::script("echo \"$GH_TOKEN\" >&2\nexit 1");
    let failing_runner = failing
        .runner(test_bounds())
        .expect("should resolve fake gh");

    let report = run(
        &failing_runner,
        AttemptDeadline::start(),
        &Cancellation::default(),
    )
    .expect_err("should fail");

    for text in [
        format!("{runner:?}"),
        format!("{runner:#?}"),
        format!("{report:?}"),
        format!("{report}"),
    ] {
        assert!(!text.contains(TOKEN), "should redact token from {text}");
    }
}

#[test]
fn successful_stderr_is_redacted() {
    let fake = Fake::script("echo \"warning for $GH_TOKEN\" >&2\necho '[{}]'");
    let runner = fake.runner(test_bounds()).expect("should resolve fake gh");

    let output = runner
        .run(
            &args(&["attestation", "verify"]),
            AttemptDeadline::start(),
            &Cancellation::default(),
        )
        .expect("should succeed");

    assert_eq!(
        output.stderr, "warning for [redacted]\n",
        "should expose only redacted stderr"
    );
}

#[test]
fn process_snapshot_captures_current_environment_once() {
    let snapshot = CallerEnvironment::from_process();

    assert_eq!(
        snapshot.get("PATH"),
        std::env::var_os("PATH").as_deref(),
        "should capture the process PATH"
    );
}
