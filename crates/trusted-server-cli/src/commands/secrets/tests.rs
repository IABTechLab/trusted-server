use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Read as _;
use std::io::Write as _;
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::time::Duration;

use clap::Parser;
use edgezero_cli::fastly_secrets::{FastlySecretResponse, FastlySecretTransport};
use tempfile::TempDir;
use trusted_server_core::secret_bindings::{axum_secret_name, spin_secret_name};

use super::*;

#[test]
fn secrets_cli_reports_load_diagnostics_without_removed_values() {
    for (config, expected) in [
        (
            CONFIG.replace(
                "domain = \"publisher.example.com\"",
                "domian = \"publisher.example.com\"",
            ),
            "domain",
        ),
        (
            CONFIG.replace(
                "[publisher]",
                "[publisher]\nproxy_secret = 'fictional-removed-credential'",
            ),
            "publisher.proxy_secret",
        ),
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.temp.path().join("trusted-server.toml"), config)
            .expect("should write invalid config");
        let (result, output) =
            fixture.run(&["plan", "--adapter", "fastly"], &mut FakeInput::default());
        let error = result.expect_err("should retain actionable configuration diagnostics");
        assert!(error.contains("could not load effective unresolved app configuration:"));
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("fictional-removed-credential"));
        assert!(output.is_empty());
        assert!(fixture.transport.methods.borrow().is_empty());
    }
}

#[test]
fn secrets_cli_push_validation_precedes_provider_plan_and_input() {
    let fixture = Fixture::new();
    fs::write(
        fixture.temp.path().join("trusted-server.toml"),
        CONFIG.replace(
            "domain = \"publisher.example.com\"",
            "domain = \"invalid/domain\"",
        ),
    )
    .expect("should write structurally valid but push-invalid config");
    for args in [
        vec!["plan", "--adapter", "fastly"],
        vec![
            "check",
            "--adapter",
            "fastly",
            "--plan",
            "missing-plan.json",
        ],
        vec![
            "set",
            "publisher.proxy",
            "--adapter",
            "fastly",
            "--plan",
            "missing-plan.json",
            "--stdin",
            "--yes",
        ],
    ] {
        let mut input = FakeInput::default();
        let (result, output) = fixture.run(&args, &mut input);
        let error = result.expect_err("should reject before deployment resolution or plan reads");
        assert!(
            error.contains("could not validate effective unresolved app configuration:"),
            "{error}"
        );
        assert!(error.contains("domain"), "{error}");
        assert!(output.is_empty());
        assert_eq!(input.reads, 0);
        assert!(fixture.transport.methods.borrow().is_empty());
    }
}

#[test]
fn secrets_cli_fixture_clears_ambient_selectors_but_allows_explicit_environment_tests() {
    let fixture = Fixture::new();
    temp_env::with_vars(
        [
            ("FASTLY_SERVICE_ID", Some("ConflictingFictionalService")),
            (
                "EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME",
                Some("other-fictional-store"),
            ),
            (
                "EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME",
                Some("other-fictional-config"),
            ),
            (
                "EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME",
                Some("other-fictional-kv"),
            ),
        ],
        || {
            fixture.plan();
            for adapter in ["axum", "spin"] {
                let (result, output) =
                    fixture.run(&["plan", "--adapter", adapter], &mut FakeInput::default());
                result.expect("should ignore developer selectors");
                let plan: Value = serde_json::from_str(&output).expect("should render native plan");
                assert_eq!(plan["resources"]["runtime_secret_store"], SECRET_STORE_ID);
            }
        },
    );
    let (result, _) = fixture.run_with_env(
        &["plan", "--adapter", "fastly"],
        &mut FakeInput::default(),
        &[("FASTLY_SERVICE_ID", Some("ConflictingFictionalService"))],
    );
    assert!(
        result
            .expect_err("should still test explicit service environment selection")
            .contains("conflicts")
    );
    let (result, output) = fixture.run_with_env(
        &["plan", "--adapter", "axum"],
        &mut FakeInput::default(),
        &[(
            "EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME",
            Some("explicit-fictional-store"),
        )],
    );
    result.expect("should retain explicit store environment selection coverage");
    let plan: Value = serde_json::from_str(&output).expect("should render selected mapping");
    assert_eq!(
        plan["resources"]["runtime_secret_store"],
        "explicit-fictional-store"
    );
}

// Run in a subprocess so Terminal reads real process stdin and /dev/tty without
// redirecting descriptors or changing terminal state in the parallel test runner.
#[test]
#[ignore = "subprocess entry point for real pipe and PTY tests"]
fn secrets_cli_terminal_subprocess() {
    let mode = std::env::var("TS_TEST_TERMINAL_MODE").expect("should select fictional input mode");
    let fixture = Fixture::new();
    fixture.plan();
    let plan = fixture.temp.path().join("plan.json").display().to_string();
    let mut args = vec![
        "set",
        "publisher.proxy",
        "--adapter",
        "fastly",
        "--plan",
        &plan,
        "--yes",
    ];
    if mode != "hidden" {
        args.push("--stdin");
    }
    let (result, output) = fixture.run(&args, &mut Terminal);
    assert!(!output.contains("fictional terminal credential"));
    match mode.as_str() {
        "pipe" | "hidden" => {
            assert_eq!(
                result.expect("should accept exact real input"),
                RunOutcome::Success
            );
            assert_eq!(fixture.transport.mutation_paths.borrow().len(), 1);
            assert_eq!(
                fixture
                    .transport
                    .values
                    .borrow()
                    .values()
                    .next()
                    .map(String::as_str),
                Some("ICBmaWN0aW9uYWwgdGVybWluYWwgY3JlZGVudGlhbCAg")
            );
        }
        "line-break" | "terminal" => {
            let error = result.expect_err("should reject unsafe stdin");
            assert!(
                error.contains(if mode == "terminal" {
                    "requires piped input"
                } else {
                    "ends with a line break"
                }),
                "{error}"
            );
            assert!(!error.contains("fictional terminal credential"));
            assert!(fixture.transport.mutation_paths.borrow().is_empty());
            assert!(fixture.transport.values.borrow().is_empty());
        }
        _ => panic!("unknown fictional input mode"),
    }
}

fn terminal_test_command(mode: &str) -> std::process::Command {
    let mut command =
        std::process::Command::new(std::env::current_exe().expect("should find test binary"));
    command.args([
        "--ignored",
        "--exact",
        "commands::secrets::tests::secrets_cli_terminal_subprocess",
        "--nocapture",
    ]);
    command.env("TS_TEST_TERMINAL_MODE", mode);
    command
}

#[test]
fn secrets_cli_real_pipes_preserve_bytes_and_reject_trailing_line_breaks_without_writes() {
    for suffix in ["", "\n", "\r", "\r\n"] {
        let mut child = terminal_test_command(if suffix.is_empty() {
            "pipe"
        } else {
            "line-break"
        })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("should start isolated real-input test");
        write!(
            child.stdin.take().expect("should have pipe"),
            "  fictional terminal credential  {suffix}"
        )
        .expect("should pipe fictional bytes");
        let output = child
            .wait_with_output()
            .expect("should collect real-input result");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("fictional terminal credential"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("fictional terminal credential"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn secrets_cli_real_pty_rejects_stdin_and_keeps_hidden_input_private() {
    for mode in ["terminal", "hidden"] {
        let executable = std::env::current_exe().expect("should find test binary");
        let shell_command = format!(
            "'{}' --ignored --exact commands::secrets::tests::secrets_cli_terminal_subprocess --nocapture",
            executable.display().to_string().replace('\'', "'\\''")
        );
        let mut child = std::process::Command::new("script")
            .args([
                "--quiet",
                "--return",
                "--command",
                &shell_command,
                "/dev/null",
            ])
            .env("TS_TEST_TERMINAL_MODE", mode)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("real PTY tests require util-linux script");
        let mut stdin = child.stdin.take().expect("should have PTY input pipe");
        let mut stdout = child.stdout.take().expect("should have PTY output pipe");
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0; 1024];
            while let Ok(length) = stdout.read(&mut buffer) {
                if length == 0 || sender.send(buffer[..length].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut output = Vec::new();
        let mut sent = false;
        loop {
            match receiver.recv_timeout(Duration::from_secs(15)) {
                Ok(bytes) => {
                    output.extend(bytes);
                    if mode == "hidden"
                        && !sent
                        && String::from_utf8_lossy(&output).contains("Credential, input hidden:")
                    {
                        // rpassword prints the prompt before disabling echo. Wait for
                        // that transition before simulating a person typing.
                        std::thread::sleep(Duration::from_millis(100));
                        stdin
                            .write_all(b"  fictional terminal credential  \n")
                            .expect("should type fictional hidden input");
                        stdin.flush().expect("should deliver hidden input");
                        sent = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    child.kill().expect("should stop stalled PTY test");
                    panic!("real PTY input test timed out");
                }
            }
        }
        reader.join().expect("should collect PTY reader");
        let result = child.wait_with_output().expect("should wait for PTY test");
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!String::from_utf8_lossy(&output).contains("fictional terminal credential"));
        assert!(!String::from_utf8_lossy(&result.stderr).contains("fictional terminal credential"));
        assert_eq!(sent, mode == "hidden");
    }
}

#[test]
fn secrets_cli_hidden_input_preserves_permitted_whitespace_and_requires_confirmation() {
    let fixture = Fixture::new();
    fixture.plan();
    let mut input = FakeInput {
        value: b"  fictional proxy credential  ".to_vec(),
        interactive: true,
        lines: vec!["yes\n".to_owned()],
        ..FakeInput::default()
    };
    let (result, output) = fixture.set("publisher.proxy", &[], &mut input);
    result.expect("should accept confirmed hidden input");
    assert_eq!(input.reads, 1);
    assert!(!output.contains("fictional proxy credential"));
    assert_eq!(
        fixture
            .transport
            .values
            .borrow()
            .values()
            .next()
            .map(String::as_str),
        Some("ICBmaWN0aW9uYWwgcHJveHkgY3JlZGVudGlhbCAg")
    );
}

#[test]
fn secrets_cli_real_overlay_policy_and_service_selection_changes_precede_input() {
    let fixture = Fixture::new();
    fixture.plan();
    let path = fixture.temp.path().join("plan.json").display().to_string();
    let mut command = fixture.parse(&[
        "set",
        "tinybird.auction",
        "--adapter",
        "fastly",
        "--plan",
        &path,
        "--stdin",
        "--yes",
    ]);
    let SecretsCommand::Set(args) = &mut command else {
        panic!("should parse set")
    };
    args.target.config.no_env = false;
    let mut input = FakeInput::default();
    assert!(
        execute(
            &command,
            &fastly::NativeProvider {
                transport: Some(&fixture.transport)
            },
            &mut input,
            &mut Vec::new()
        )
        .is_err()
    );
    assert_eq!(input.reads, 0);
    assert!(
        fixture
            .set(
                "tinybird.auction",
                &[
                    "--stdin",
                    "--yes",
                    "--service-id",
                    "DifferentFictionalService"
                ],
                &mut input
            )
            .0
            .is_err()
    );
    assert_eq!(input.reads, 0);
    assert!(fixture.transport.mutation_paths.borrow().is_empty());
}

#[test]
fn secrets_cli_wrong_store_selection_never_reads_input_or_creates_resources() {
    let fixture = Fixture::new();
    fixture.plan();
    let path = fixture.temp.path().join("edgezero.toml");
    let mut manifest = fs::read_to_string(&path).expect("should read fixture manifest");
    manifest.push_str(
        r#"
[[environment.variables]]
name = "secret-store-selector"
env = "EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME"
value = "different-fictional-store"
"#,
    );
    fs::write(path, manifest).expect("should set existing deployment selector");
    let mut input = FakeInput::default();
    let error = fixture
        .set("tinybird.auction", &["--stdin", "--yes"], &mut input)
        .0
        .expect_err("should reject an unprovisioned selected store");
    assert!(error.contains("does not exist"));
    assert_eq!(input.reads, 0);
    assert!(fixture.transport.mutation_paths.borrow().is_empty());
}

#[test]
fn secrets_cli_worker_plaintext_and_spin_default_collisions_fail_planning() {
    let fixture = Fixture::new();
    for adapter in ["cloudflare", "spin"] {
        let (result, output) =
            fixture.run(&["plan", "--adapter", adapter], &mut FakeInput::default());
        result.expect("should compile native plan");
        let plan: Value = serde_json::from_str(&output).expect("should emit native metadata");
        let name = plan["entries"][0]["native"]["native_name"]
            .as_str()
            .expect("should have native identity");
        let declaration = if adapter == "spin" {
            format!(
                "[component.trusted-server]\nsource='fictional.wasm'\n[variables.{name}]\nsecret=true\nrequired=true\ndefault='fictional-default'\n"
            )
        } else {
            format!("[vars]\n{name}='fictional-plaintext'\n")
        };
        fs::write(
            fixture.temp.path().join(if adapter == "spin" {
                "spin.toml"
            } else {
                "wrangler.toml"
            }),
            declaration,
        )
        .expect("should write conflicting declaration");
        assert!(
            fixture
                .run(&["plan", "--adapter", adapter], &mut FakeInput::default())
                .0
                .expect_err("should reject final native name collision")
                .contains("collid")
        );
    }
}

#[test]
fn secrets_cli_spin_requires_explicit_manifest_before_plan_or_check() {
    let fixture = Fixture::new();
    let (result, output) = fixture.run(&["plan", "--adapter", "spin"], &mut FakeInput::default());
    result.expect("should plan the explicitly selected Spin manifest");
    let plan = fixture.temp.path().join("spin-plan.json");
    fs::write(&plan, output).expect("should save the reviewed Spin plan");
    let path = fixture.temp.path().join("edgezero.toml");
    let manifest = fs::read_to_string(&path)
        .expect("should read deployment manifest")
        .replace("manifest = \"spin.toml\"\n", "");
    fs::write(path, manifest).expect("should remove the explicit Spin manifest path");
    assert!(fixture.temp.path().join("spin.toml").is_file());
    let plan = plan.display().to_string();

    for args in [
        vec!["plan", "--adapter", "spin"],
        vec!["check", "--adapter", "spin", "--plan", &plan],
    ] {
        let mut input = FakeInput::default();
        let (result, output) = fixture.run(&args, &mut input);
        assert_eq!(
            result.expect_err("should reject an implicit Spin manifest before using the plan"),
            "Spin secret planning requires an explicit adapter manifest"
        );
        assert!(output.is_empty(), "should not publish an unchecked plan");
        assert_eq!(input.reads, 0, "should not request a credential");
    }
    assert!(fixture.transport.methods.borrow().is_empty());
}

#[test]
fn secrets_cli_spin_declarations_use_selected_component_and_parse_as_toml() {
    let fixture = Fixture::new();
    let path = fixture.temp.path().join("edgezero.toml");
    let manifest = fs::read_to_string(&path)
        .expect("should read manifest")
        .replace(
            "component = \"trusted-server\"",
            "component = \"fictional-component\"",
        );
    fs::write(path, manifest).expect("should select existing component");
    fs::write(
        fixture.temp.path().join("spin.toml"),
        "[component.fictional-component]\nsource='fictional.wasm'\n",
    )
    .expect("should declare selected component");
    let (result, output) = fixture.run(&["plan", "--adapter", "spin"], &mut FakeInput::default());
    result.expect("should render selected component");
    let plan: Value = serde_json::from_str(&output).expect("should emit plan");
    for entry in plan["entries"].as_array().expect("should contain entries") {
        for snippet in entry["native"]["declarations"]
            .as_array()
            .expect("should contain declarations")
        {
            let parsed: toml::Value =
                toml::from_str(snippet.as_str().expect("should render snippet"))
                    .expect("should render valid native TOML");
            if let Some(components) = parsed.get("component") {
                assert!(components.get("fictional-component").is_some());
            }
        }
    }
}

#[test]
fn secrets_cli_axum_and_spin_render_effective_trusted_store_mapping() {
    let fixture = Fixture::new();
    let path = fixture.temp.path().join("edgezero.toml");
    let mut manifest = fs::read_to_string(&path).expect("should read manifest");
    manifest.push_str(
        r#"
[[environment.variables]]
name = "secret-store-selector"
env = "EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME"
value = "fictional-stage.secrets"
"#,
    );
    fs::write(path, manifest).expect("should configure trusted native mapping");
    for adapter in ["axum", "spin"] {
        let (result, output) =
            fixture.run(&["plan", "--adapter", adapter], &mut FakeInput::default());
        result.expect("should render effective selected native store");
        let plan: Value = serde_json::from_str(&output).expect("should emit plan");
        assert_eq!(
            plan["resources"]["runtime_secret_store"],
            "fictional-stage.secrets"
        );
        for entry in plan["entries"].as_array().expect("should have entries") {
            let key = entry["compiled"]["key"]
                .as_str()
                .expect("should have derived key");
            let expected = if adapter == "axum" {
                axum_secret_name("fictional-stage.secrets", key)
            } else {
                spin_secret_name("fictional-stage.secrets", key)
                    .expect("should render actual Spin mapping")
            };
            assert_eq!(entry["native"]["native_name"], expected);
            assert_eq!(entry["native"]["store_id"], SECRET_STORE_ID);
            if adapter == "spin" {
                for snippet in entry["native"]["declarations"]
                    .as_array()
                    .expect("should have declarations")
                {
                    assert!(
                        snippet
                            .as_str()
                            .expect("should have TOML snippet")
                            .contains(&expected)
                    );
                }
            }
        }
    }
}

#[test]
fn secrets_cli_plan_identity_is_portable_between_checkout_paths() {
    let first = Fixture::new();
    let second = Fixture::new();
    assert_eq!(first.plan(), second.plan());
}

#[test]
fn secrets_cli_spin_implicit_and_ambiguous_component_selection_reuses_lifecycle_rules() {
    let fixture = Fixture::new();
    let path = fixture.temp.path().join("edgezero.toml");
    let manifest = fs::read_to_string(&path)
        .expect("should read manifest")
        .replace("component = \"trusted-server\"", "");
    fs::write(path, manifest).expect("should remove explicit selector");
    let path = fixture.temp.path().join("spin.toml");
    fs::write(
        &path,
        "[component.only-component]\nsource='fictional.wasm'\n",
    )
    .expect("should declare a sole component");
    let (result, output) = fixture.run(&["plan", "--adapter", "spin"], &mut FakeInput::default());
    result.expect("should resolve a sole component implicitly");
    let plan: Value = serde_json::from_str(&output).expect("should emit plan");
    assert_eq!(plan["resources"]["component"], "only-component");
    assert!(
        plan["entries"][0]["native"]["declarations"][1]
            .as_str()
            .expect("should render component declaration")
            .contains("only-component")
    );
    fs::write(
        path,
        "[component.one]\nsource='one.wasm'\n[component.two]\nsource='two.wasm'\n",
    )
    .expect("should declare ambiguous components");
    assert!(
        fixture
            .run(&["plan", "--adapter", "spin"], &mut FakeInput::default())
            .0
            .is_err()
    );
    assert!(fixture.transport.methods.borrow().is_empty());
}

#[test]
fn secrets_cli_identical_handler_owners_share_physical_declarations() {
    let fixture = Fixture::new();
    fs::write(
        fixture.temp.path().join("trusted-server.toml"),
        format!("{CONFIG}\n[[handlers]]\npath='^/_ts/admin'\nusername='admin'\n"),
    )
    .expect("should configure identical handler owners");
    for adapter in ["spin", "cloudflare"] {
        let (result, output) =
            fixture.run(&["plan", "--adapter", adapter], &mut FakeInput::default());
        result.expect("should plan every owner of a shared native request");
        let plan: Value = serde_json::from_str(&output).expect("should decode shared-binding plan");
        let entries = plan["entries"]
            .as_array()
            .expect("should retain compiled owners");
        assert_eq!(entries.len(), 5);
        let handlers = entries
            .iter()
            .filter(|entry| entry["compiled"]["descriptor_id"] == "handler.password")
            .collect::<Vec<_>>();
        assert_eq!(handlers.len(), 2);
        assert_ne!(handlers[0]["entry_id"], handlers[1]["entry_id"]);
        assert_ne!(
            handlers[0]["compiled"]["field_path"],
            handlers[1]["compiled"]["field_path"]
        );
        for field in [
            "descriptor_id",
            "scope",
            "subject",
            "export",
            "target",
            "key",
        ] {
            assert_eq!(
                handlers[0]["compiled"][field],
                handlers[1]["compiled"][field]
            );
        }
        for field in ["adapter", "store_id", "key", "native_name"] {
            assert_eq!(handlers[0]["native"][field], handlers[1]["native"][field]);
        }
        let names = entries
            .iter()
            .map(|entry| {
                entry["native"]["native_name"]
                    .as_str()
                    .expect("should retain native identity metadata")
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), 4);
        let snippets = entries
            .iter()
            .flat_map(|entry| {
                entry["native"]["declarations"]
                    .as_array()
                    .expect("should retain a declaration list for every owner")
            })
            .map(|snippet| snippet.as_str().expect("should render declaration text"))
            .collect::<Vec<_>>();
        if adapter == "spin" {
            assert_eq!(
                handlers[0]["native"]["declarations"]
                    .as_array()
                    .expect("should declare first owner")
                    .len(),
                2
            );
            assert!(
                handlers[1]["native"]["declarations"]
                    .as_array()
                    .expect("should retain later owner metadata")
                    .is_empty()
            );
            assert_eq!(snippets.len(), names.len() * 2);
            let mut application = String::new();
            let mut component_header = None;
            let mut assignments = String::new();
            for snippet in snippets {
                if snippet.starts_with("[component.") {
                    let (header, body) = snippet
                        .split_once('\n')
                        .expect("should separate component header and assignment");
                    if let Some(previous) = component_header {
                        assert_eq!(header, previous);
                    } else {
                        component_header = Some(header);
                    }
                    assignments.push_str(body);
                    assignments.push('\n');
                } else {
                    application.push_str(snippet);
                    application.push('\n');
                }
            }
            // Follow the operator guide: keep application tables and merge all
            // component assignments beneath one component header.
            let merged = format!(
                "{application}\n{}\n{assignments}",
                component_header.expect("should render a component header")
            );
            let document: toml::Value = toml::from_str(&merged)
                .expect("should parse all deduplicated Spin declarations together");
            let variables = document["variables"]
                .as_table()
                .expect("should declare application variables");
            let component = document["component"]["trusted-server"]["variables"]
                .as_table()
                .expect("should declare component variables");
            assert_eq!(variables.len(), names.len());
            assert_eq!(component.len(), names.len());
            for name in names {
                assert_eq!(variables[name]["required"].as_bool(), Some(true));
                assert_eq!(variables[name]["secret"].as_bool(), Some(true));
                assert_eq!(
                    component[name].as_str(),
                    Some(format!("{{{{ {name} }}}}").as_str())
                );
            }
        } else {
            // Cloudflare plans expose every owner identity but never emit
            // plaintext [vars] declarations or credential values.
            assert!(snippets.is_empty());
            let document: toml::Value = toml::from_str(&snippets.join("\n"))
                .expect("should parse the empty Cloudflare declaration block");
            assert!(
                document
                    .as_table()
                    .expect("should retain a metadata-only plan")
                    .is_empty()
            );
        }
    }
    assert!(fixture.transport.methods.borrow().is_empty());
}

#[test]
fn secrets_cli_declaration_coalescing_checks_full_context_store_key_and_rendering() {
    let fixture = Fixture::new();
    fs::write(
        fixture.temp.path().join("trusted-server.toml"),
        format!("{CONFIG}\n[[handlers]]\npath='^/one'\nusername='one'\n[[handlers]]\npath='^/two'\nusername='two'\n"),
    ).expect("should configure distinct handler contexts");
    let SecretsCommand::Plan(args) = fixture.parse(&["plan", "--adapter", "spin"]) else {
        panic!("should parse the native plan command");
    };
    let loaded =
        load_settings(&args.target.config).expect("should load real handler configuration");
    let plan =
        compile_bindings(&loaded.settings).expect("should compile distinct handler contexts");
    let handlers = plan
        .entries()
        .iter()
        .filter(|binding| binding.descriptor_id() == "handler.password")
        .collect::<Vec<_>>();
    assert_eq!(handlers.len(), 3);
    let native =
        render_native_with_mapping("spin", SECRET_STORE_ID, handlers[0].key(), SECRET_STORE_ID)
            .expect("should render the first native request");
    let mut requests = BTreeMap::new();
    coalesce_native_declarations(&mut requests, handlers[0], &mut native.clone())
        .expect("should record the complete first request");
    let mut wrong_context = native.clone();
    assert!(coalesce_native_declarations(&mut requests, handlers[1], &mut wrong_context).is_err());
    let mut wrong_store = native.clone();
    wrong_store.store_id = "other-fictional-store".to_owned();
    let mut wrong_key = native.clone();
    wrong_key.key = handlers[1].key().to_owned();
    let mut wrong_adapter = native.clone();
    wrong_adapter.adapter = "cloudflare".to_owned();
    let mut wrong_declarations = native.clone();
    wrong_declarations
        .declarations
        .push("incompatible declaration".to_owned());
    for mut different in [wrong_store, wrong_key, wrong_adapter, wrong_declarations] {
        let original = different.clone();
        assert!(coalesce_native_declarations(&mut requests, handlers[0], &mut different).is_err());
        assert_eq!(different, original);
    }
    let mut identical = native.clone();
    coalesce_native_declarations(&mut requests, handlers[0], &mut identical)
        .expect("should share declarations only after exact request equality");
    assert!(identical.declarations.is_empty());
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests
            .get(&native.native_name)
            .expect("should retain the original complete rendering")
            .1,
        native
    );
}

#[test]
fn secrets_cli_plan_help_documents_shared_owner_declarations() {
    let help = TestCli::try_parse_from(["secrets", "plan", "--help"])
        .expect_err("should display native planning help")
        .to_string();
    assert!(help.contains("Every owner"));
    assert!(help.contains("Only the first"));
    assert!(help.contains("Spin component"));
}

const CONFIG: &str = r#"
[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
origin_url = "https://upstream.publisher.example.com"

[tinybird]
enabled = true
api_host = "api.example.com"
auction_dataset = "auction_events"

[[handlers]]
path = "^/_ts/admin"
username = "admin"
"#;

#[derive(Debug, Parser)]
struct TestCli {
    #[command(subcommand)]
    command: SecretsCommand,
}

#[derive(Default)]
struct FakeTransport {
    values: RefCell<BTreeMap<String, String>>,
    methods: RefCell<Vec<String>>,
    mutation_paths: RefCell<Vec<String>>,
    failure: RefCell<Option<String>>,
    resource_id: RefCell<Option<String>>,
}

impl FastlySecretTransport for FakeTransport {
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<FastlySecretResponse, String> {
        self.methods.borrow_mut().push(method.to_owned());
        if method != "GET" {
            self.mutation_paths.borrow_mut().push(path.to_owned());
        }
        let body = if method == "GET" && path.starts_with("/resources/stores/secret?") {
            json!({"data":[{"id": self.resource_id.borrow().as_deref().unwrap_or("fictional-secret-store"), "name":"trusted_server_secrets"}], "meta":{}})
        } else if method == "GET" && path.starts_with("/resources/stores/config?") {
            json!({"data":[{"id":"fictional-config-store", "name":"trusted_server_config"}], "meta":{}})
        } else if method == "GET" && path.starts_with("/resources/stores/kv?") {
            json!({"data":[{"id":"fictional-kv-store", "name":"trusted_server_kv"}], "meta":{}})
        } else if method == "GET" && path.contains("/secrets?") {
            json!({"data":self.values.borrow().keys().map(|name| json!({"name":name})).collect::<Vec<_>>(), "meta":{}})
        } else if matches!(method, "POST" | "PATCH") {
            let payload: Value = serde_json::from_str(body.expect("should have a request body"))
                .expect("should encode a provider JSON request");
            let key = payload["name"]
                .as_str()
                .expect("should have a key")
                .to_owned();
            let value = payload["secret"]
                .as_str()
                .expect("should have base64 bytes")
                .to_owned();
            let exists = self.values.borrow().contains_key(&key);
            if (method == "POST" && exists) || (method == "PATCH" && !exists) {
                return Ok(FastlySecretResponse {
                    status: 409,
                    body: "fictional conflict".to_owned(),
                });
            }
            self.values.borrow_mut().insert(key, value);
            // A provider can commit and then lose the response. The CLI must not retry.
            if let Some(failure) = self.failure.borrow().as_ref() {
                return Err(failure.clone());
            }
            json!({})
        } else {
            return Err(format!(
                "unexpected fictional provider operation {method} {path}"
            ));
        };
        Ok(FastlySecretResponse {
            status: 200,
            body: body.to_string(),
        })
    }
}

#[derive(Default)]
struct FakeInput {
    value: Vec<u8>,
    reads: usize,
    interactive: bool,
    lines: Vec<String>,
}

impl Interaction for FakeInput {
    fn interactive(&self) -> bool {
        self.interactive
    }
    fn line(&mut self) -> Result<String, String> {
        if self.lines.is_empty() {
            return Err("cancelled".to_owned());
        }
        Ok(self.lines.remove(0))
    }
    fn hidden(&mut self) -> Result<Vec<u8>, String> {
        self.reads += 1;
        Ok(self.value.clone())
    }
    fn stdin(&mut self, _: usize) -> Result<Vec<u8>, String> {
        self.reads += 1;
        Ok(self.value.clone())
    }
}

struct Fixture {
    temp: TempDir,
    transport: FakeTransport,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("should create fictional fixture");
        fs::write(
            temp.path().join("edgezero.toml"),
            r#"
[app]
name = "trusted-server"
[stores.secrets]
ids = ["trusted_server_secrets"]
default = "trusted_server_secrets"
[stores.config]
ids = ["trusted_server_config"]
default = "trusted_server_config"
[stores.kv]
ids = ["trusted_server_kv"]
default = "trusted_server_kv"
[adapters.fastly.adapter]
manifest = "fastly.toml"
[adapters.axum.adapter]
manifest = "axum.toml"
[adapters.cloudflare.adapter]
manifest = "wrangler.toml"
[adapters.spin.adapter]
manifest = "spin.toml"
component = "trusted-server"
"#,
        )
        .expect("should write manifest");
        fs::write(
            temp.path().join("fastly.toml"),
            "service_id = 'FictionalService1037'\n",
        )
        .expect("should write Fastly identity");
        fs::write(temp.path().join("trusted-server.toml"), CONFIG).expect("should write config");
        for path in ["axum.toml", "wrangler.toml"] {
            fs::write(temp.path().join(path), "name = 'fictional-app'\n")
                .expect("should write native fixture");
        }
        fs::write(temp.path().join("spin.toml"), "[application]\nname='fictional-app'\n[component.trusted-server]\nsource='fictional.wasm'\n")
            .expect("should write actual Spin component fixture");
        Self {
            temp,
            transport: FakeTransport::default(),
        }
    }

    fn parse(&self, args: &[&str]) -> SecretsCommand {
        let mut argv = vec!["secrets".to_owned()];
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        argv.extend([
            "--manifest".to_owned(),
            self.temp.path().join("edgezero.toml").display().to_string(),
            "--app-config".to_owned(),
            self.temp
                .path()
                .join("trusted-server.toml")
                .display()
                .to_string(),
            "--no-env".to_owned(),
        ]);
        TestCli::try_parse_from(argv)
            .expect("should parse real secrets arguments")
            .command
    }

    fn run(
        &self,
        args: &[&str],
        input: &mut dyn Interaction,
    ) -> (Result<RunOutcome, String>, String) {
        self.run_with_env(args, input, &[])
    }

    fn run_with_env(
        &self,
        args: &[&str],
        input: &mut dyn Interaction,
        overrides: &[(&str, Option<&str>)],
    ) -> (Result<RunOutcome, String>, String) {
        let mut environment = BTreeMap::from([
            ("FASTLY_SERVICE_ID", None),
            (
                "EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME",
                None,
            ),
            (
                "EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME",
                None,
            ),
            ("EDGEZERO__STORES__KV__TRUSTED_SERVER_KV__NAME", None),
        ]);
        environment.extend(overrides.iter().copied());
        temp_env::with_vars(environment.into_iter().collect::<Vec<_>>(), || {
            let command = self.parse(args);
            let mut output = Vec::new();
            let result = execute(
                &command,
                &fastly::NativeProvider {
                    transport: Some(&self.transport),
                },
                input,
                &mut output,
            );
            (
                result,
                String::from_utf8(output).expect("should emit UTF-8 metadata"),
            )
        })
    }

    fn plan(&self) -> Value {
        let (result, output) =
            self.run(&["plan", "--adapter", "fastly"], &mut FakeInput::default());
        result.expect("should compile plan through actual compiler and provider resolver");
        let plan: Value = serde_json::from_str(&output).expect("should emit a secret-free plan");
        fs::write(self.temp.path().join("plan.json"), output).expect("should write reviewed plan");
        plan
    }

    fn set(
        &self,
        role: &str,
        extra: &[&str],
        input: &mut FakeInput,
    ) -> (Result<RunOutcome, String>, String) {
        let plan = self.temp.path().join("plan.json").display().to_string();
        let mut args = vec!["set", role, "--adapter", "fastly", "--plan", &plan];
        args.extend_from_slice(extra);
        self.run(&args, input)
    }
}

#[test]
fn secrets_cli_plan_and_stdin_create_replace_use_actual_shared_transport() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    assert_eq!(
        plan["entries"]
            .as_array()
            .expect("should list implicit roles")
            .len(),
        4
    );
    let mut input = FakeInput {
        value: b"fictional-token-123".to_vec(),
        ..FakeInput::default()
    };
    let (result, output) = fixture.set("tinybird.auction", &["--stdin", "--yes"], &mut input);
    assert_eq!(
        result.expect("should create reviewed binding"),
        RunOutcome::Success
    );
    assert!(output.contains("created binding"));
    assert!(!output.contains("fictional-token-123"));
    assert_eq!(input.reads, 1);
    let entry = plan["entries"]
        .as_array()
        .expect("should list roles")
        .iter()
        .find(|entry| entry["compiled"]["descriptor_id"] == "tinybird.auction")
        .expect("should have Tinybird");
    let key = entry["compiled"]["key"]
        .as_str()
        .expect("should have generated key");
    assert_eq!(
        fixture
            .transport
            .values
            .borrow()
            .get(key)
            .map(String::as_str),
        Some("ZmljdGlvbmFsLXRva2VuLTEyMw==")
    );
    let (result, output) = fixture.set("tinybird.auction", &["--stdin", "--yes"], &mut input);
    result.expect("should replace the same deterministic key");
    assert!(output.contains("replaced existing binding"));
    assert_eq!(fixture.transport.values.borrow().len(), 1);
    assert_eq!(
        fixture.transport.mutation_paths.borrow().as_slice(),
        [
            "/resources/stores/secret/fictional-secret-store/secrets",
            "/resources/stores/secret/fictional-secret-store/secrets"
        ]
    );
    assert_eq!(
        fixture
            .transport
            .methods
            .borrow()
            .iter()
            .filter(|method| method.as_str() == "POST")
            .count(),
        1
    );
    assert_eq!(
        fixture
            .transport
            .methods
            .borrow()
            .iter()
            .filter(|method| method.as_str() == "PATCH")
            .count(),
        1
    );
}

#[test]
fn secrets_cli_tampering_stale_config_versions_overlay_and_resources_precede_input() {
    for mutation in [
        "plan_version",
        "binding_abi",
        "native_mapping_version",
        "environment_overlay",
        "effective_config_sha256",
        "resources",
        "entries",
    ] {
        let fixture = Fixture::new();
        let mut plan = fixture.plan();
        plan[mutation] = json!("tampered");
        fs::write(
            fixture.temp.path().join("plan.json"),
            serde_json::to_vec(&plan).expect("should encode mutation"),
        )
        .expect("should write mutated plan");
        let mut input = FakeInput {
            value: b"fictional-token-123".to_vec(),
            ..FakeInput::default()
        };
        assert!(
            fixture
                .set("tinybird.auction", &["--stdin", "--yes"], &mut input)
                .0
                .is_err()
        );
        assert_eq!(input.reads, 0);
        assert!(fixture.transport.mutation_paths.borrow().is_empty());
    }
    let fixture = Fixture::new();
    fixture.plan();
    fs::write(
        fixture.temp.path().join("trusted-server.toml"),
        CONFIG.replace("auction_events", "different_events"),
    )
    .expect("should mutate target dataset");
    let mut input = FakeInput::default();
    assert!(
        fixture
            .set("tinybird.auction", &["--stdin", "--yes"], &mut input)
            .0
            .is_err()
    );
    assert_eq!(input.reads, 0);
    let fixture = Fixture::new();
    fixture.plan();
    *fixture.transport.resource_id.borrow_mut() = Some("recreated-secret-store".to_owned());
    assert!(
        fixture
            .set("tinybird.auction", &["--stdin", "--yes"], &mut input)
            .0
            .is_err()
    );
    assert_eq!(input.reads, 0);
    assert!(fixture.transport.mutation_paths.borrow().is_empty());
}

#[test]
fn secrets_cli_unsupported_direct_set_rejects_before_provider_or_input() {
    let fixture = Fixture::new();
    for adapter in ["axum", "cloudflare", "spin"] {
        let mut input = FakeInput::default();
        let (result, _) = fixture.run(
            &[
                "set",
                "tinybird.auction",
                "--adapter",
                adapter,
                "--plan",
                "does-not-exist",
                "--stdin",
                "--yes",
            ],
            &mut input,
        );
        assert!(
            result
                .expect_err("should reject unsupported adapter")
                .contains("only for Fastly")
        );
        assert_eq!(input.reads, 0);
        assert!(fixture.transport.methods.borrow().is_empty());
    }
}

#[test]
fn secrets_cli_exact_invalid_bytes_and_provider_limits_never_mutate() {
    let fixture = Fixture::new();
    fixture.plan();
    for value in [
        b"fictional-token\n".to_vec(),
        vec![0xff],
        Vec::new(),
        vec![b'a'; 65_537],
    ] {
        let mut input = FakeInput {
            value,
            ..FakeInput::default()
        };
        let (result, output) = fixture.set("tinybird.auction", &["--stdin", "--yes"], &mut input);
        assert!(result.is_err());
        assert!(!output.contains("fictional-token"));
        assert!(fixture.transport.mutation_paths.borrow().is_empty());
    }
}

#[test]
fn secrets_cli_interactive_cancel_and_inactive_role_do_not_read_credentials() {
    let fixture = Fixture::new();
    fixture.plan();
    let mut input = FakeInput {
        interactive: true,
        lines: vec!["no\n".to_owned()],
        ..FakeInput::default()
    };
    assert!(fixture.set("tinybird.auction", &[], &mut input).0.is_err());
    assert_eq!(input.reads, 0);
    assert!(
        fixture
            .set("ec.pull", &["--stdin", "--yes"], &mut input)
            .0
            .is_err()
    );
    assert_eq!(input.reads, 0);
    assert!(fixture.transport.mutation_paths.borrow().is_empty());
}

#[test]
fn secrets_cli_provider_errors_are_sanitized_without_automatic_retry() {
    let fixture = Fixture::new();
    fixture.plan();
    *fixture.transport.failure.borrow_mut() = Some("reflected fictional-token-123".to_owned());
    let mut input = FakeInput {
        value: b"fictional-token-123".to_vec(),
        ..FakeInput::default()
    };
    let (result, output) = fixture.set("tinybird.auction", &["--stdin", "--yes"], &mut input);
    let error = result.expect_err("should report uncertain outcome");
    assert!(error.contains("unknown"));
    assert!(!error.contains("fictional-token-123"));
    assert!(!output.contains("fictional-token-123"));
    assert_eq!(fixture.transport.mutation_paths.borrow().len(), 1);
    assert_eq!(fixture.transport.values.borrow().len(), 1);
}

#[test]
fn secrets_cli_all_four_native_plans_and_presence_only_checks() {
    let fixture = Fixture::new();
    for adapter in ["fastly", "axum", "cloudflare", "spin"] {
        let (result, output) =
            fixture.run(&["plan", "--adapter", adapter], &mut FakeInput::default());
        result.expect("should render actual native mapping");
        let plan: Value = serde_json::from_str(&output).expect("should emit native plan");
        let entries = plan["entries"].as_array().expect("should have entries");
        assert_eq!(entries.len(), 4);
        let name = entries[0]["native"]["native_name"]
            .as_str()
            .expect("should render native identity");
        match adapter {
            "axum" => {
                assert!(name.starts_with("TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_TS1_"))
            }
            "spin" => {
                assert_eq!(name.len(), 92);
                assert_eq!(
                    entries[0]["native"]["declarations"]
                        .as_array()
                        .expect("should have declarations")
                        .len(),
                    2
                );
            }
            _ => assert!(name.starts_with("ts1_")),
        }
        fs::write(fixture.temp.path().join("native-plan.json"), output).expect("should save plan");
        let path = fixture
            .temp
            .path()
            .join("native-plan.json")
            .display()
            .to_string();
        let (result, output) = fixture.run(
            &["check", "--adapter", adapter, "--plan", &path],
            &mut FakeInput::default(),
        );
        assert!(output.contains("Presence only"));
        if matches!(adapter, "cloudflare" | "spin") {
            assert_eq!(
                result.expect("should distinguish unsupported presence check"),
                RunOutcome::Unsupported
            );
        } else {
            assert_eq!(
                result.expect("should distinguish missing binding"),
                RunOutcome::AssertionFailed
            );
        }
    }
    assert!(fixture.transport.mutation_paths.borrow().is_empty());
}

#[test]
fn secrets_cli_repeated_role_requires_reviewed_entry_id_not_array_order() {
    let fixture = Fixture::new();
    fs::write(fixture.temp.path().join("trusted-server.toml"), format!("{CONFIG}\n[[handlers]]\npath='^/one'\nusername='one'\n[[handlers]]\npath='^/two'\nusername='two'\n"))
        .expect("should add repeated implicit purpose");
    let plan = fixture.plan();
    let mut input = FakeInput {
        value: b"fictional-handler-password".to_vec(),
        ..FakeInput::default()
    };
    let error = fixture
        .set("handler.password", &["--stdin", "--yes"], &mut input)
        .0
        .expect_err("should not choose first repeated role");
    assert!(error.contains("--binding"));
    assert_eq!(input.reads, 0);
    let entry = plan["entries"]
        .as_array()
        .expect("should list entries")
        .iter()
        .filter(|entry| entry["compiled"]["descriptor_id"] == "handler.password")
        .nth(1)
        .expect("should have second handler");
    let raw_key = entry["compiled"]["key"]
        .as_str()
        .expect("should have generated key");
    assert!(
        fixture
            .set(
                "handler.password",
                &["--stdin", "--yes", "--binding", raw_key],
                &mut input
            )
            .0
            .is_err()
    );
    assert_eq!(input.reads, 0);
    let id = entry["entry_id"]
        .as_str()
        .expect("should have generated entry ID");
    fixture
        .set(
            "handler.password",
            &["--stdin", "--yes", "--binding", id],
            &mut input,
        )
        .0
        .expect("should write exact selected handler binding");
    assert_eq!(input.reads, 1);
    assert!(
        fixture.transport.values.borrow().contains_key(
            entry["compiled"]["key"]
                .as_str()
                .expect("should have derived key")
        )
    );
}
