use std::io::{Cursor, Write as _};

use docs_parity::cli_help::{
    CaptureContext, HelpAvailability, HelpRunner, HostedArtifact, HostedClient, HostedRun,
    Platform, capture_archive, import_authenticated, render_checked_help,
    transcript_record_fingerprint, validate_capture_archive,
};
use sha2::{Digest as _, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[derive(Default)]
struct FakeHelpRunner {
    paths: Vec<Vec<String>>,
}

impl HelpRunner for FakeHelpRunner {
    fn help(&mut self, path: &[String]) -> Result<String, String> {
        self.paths.push(path.to_vec());
        Ok(match path {
            [] => "Trusted Server CLI\n\nCommands:\n  config  Configuration\n  serve   Serve\n  help    Print help\n".to_owned(),
            [command] if command == "config" => {
                "Configuration\n\nCommands:\n  init  Initialize\n  help  Print help\n".to_owned()
            }
            _ => "Usage: ts command\n".to_owned(),
        })
    }
}

fn context(platform: Platform) -> CaptureContext {
    CaptureContext {
        schema_version: 1,
        repository: "IABTechLab/trusted-server".to_owned(),
        platform,
        source_sha: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        runner_name: "hosted-runner".to_owned(),
        runner_os: platform.as_str().to_owned(),
        runner_arch: "X64".to_owned(),
        uname: "fixture uname".to_owned(),
        rustc: "rustc fixture".to_owned(),
        node: "v24.12.0".to_owned(),
        tool_versions: "rust 1.95.0".to_owned(),
        run_id: 91,
        run_attempt: 2,
    }
}

fn outer_zip(platform: Platform, inner: &[u8]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    writer
        .start_file(
            format!("cli-help-{}.zip", platform.as_str()),
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .unix_permissions(0o644),
        )
        .expect("should create outer member");
    writer.write_all(inner).expect("should write outer member");
    writer
        .finish()
        .expect("should finish outer ZIP")
        .into_inner()
}

#[test]
fn recursive_capture_is_deterministic_bounded_and_platform_detected() {
    let mut runner = FakeHelpRunner::default();
    let first =
        capture_archive(&context(Platform::Linux), &mut runner).expect("should capture help");
    let second = capture_archive(&context(Platform::Linux), &mut FakeHelpRunner::default())
        .expect("should recapture help");
    let parsed =
        validate_capture_archive(&first, Platform::Linux).expect("should validate capture");

    assert_eq!(first, second, "inner archive should be deterministic");
    assert_eq!(
        runner.paths,
        [
            vec![],
            vec!["config".to_owned()],
            vec!["serve".to_owned()],
            vec!["config".to_owned(), "init".to_owned()],
        ]
    );
    assert!(parsed.help.contains("=== ts config init ==="));
    assert_eq!(parsed.provenance.platform, Platform::Linux);
    assert_eq!(parsed.provenance.help_length, parsed.help.len());
}

#[test]
fn capture_rejects_wrong_platform_malformed_help_and_command_explosion() {
    let archive = capture_archive(&context(Platform::Macos), &mut FakeHelpRunner::default())
        .expect("should capture macOS help");
    assert!(
        validate_capture_archive(&archive, Platform::Linux).is_err(),
        "platform mismatch should fail"
    );

    struct Exploding;
    impl HelpRunner for Exploding {
        fn help(&mut self, path: &[String]) -> Result<String, String> {
            if path.is_empty() {
                Ok(format!(
                    "Commands:\n{}",
                    (0..257)
                        .map(|index| format!("  command-{index}  command\n"))
                        .collect::<String>()
                ))
            } else {
                Ok("Usage\n".to_owned())
            }
        }
    }
    assert!(
        capture_archive(&context(Platform::Linux), &mut Exploding).is_err(),
        "more than 256 commands should fail"
    );
}

#[test]
fn capture_zip_rejects_preamble_and_trailing_bytes() {
    let archive = capture_archive(&context(Platform::Linux), &mut FakeHelpRunner::default())
        .expect("should create capture archive");
    let mut preamble = b"untrusted-preamble".to_vec();
    preamble.extend_from_slice(&archive);
    let mut trailing = archive.clone();
    trailing.extend_from_slice(b"untrusted-trailer");

    assert!(
        validate_capture_archive(&preamble, Platform::Linux).is_err(),
        "ZIP preamble must fail closed"
    );
    assert!(
        validate_capture_archive(&trailing, Platform::Linux).is_err(),
        "ZIP trailing bytes must fail closed"
    );
}

fn raw_zip(members: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    for (name, mode, contents) in members {
        writer
            .start_file(
                *name,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Stored)
                    .unix_permissions(*mode),
            )
            .expect("should create adversarial member");
        writer
            .write_all(contents)
            .expect("should write adversarial member");
    }
    writer
        .finish()
        .expect("should finish adversarial ZIP")
        .into_inner()
}

#[test]
fn capture_zip_rejects_extra_traversal_reordered_and_unsafe_mode_members() {
    let provenance = br#"{}"#;
    let cases = [
        raw_zip(&[
            ("cli-help.txt", 0o644, b"help"),
            ("provenance.json", 0o644, provenance),
            ("extra.txt", 0o644, b"extra"),
        ]),
        raw_zip(&[
            ("../cli-help.txt", 0o644, b"help"),
            ("provenance.json", 0o644, provenance),
        ]),
        raw_zip(&[
            ("provenance.json", 0o644, provenance),
            ("cli-help.txt", 0o644, b"help"),
        ]),
        raw_zip(&[
            ("cli-help.txt", 0o755, b"help"),
            ("provenance.json", 0o644, provenance),
        ]),
    ];

    for archive in cases {
        assert!(
            validate_capture_archive(&archive, Platform::Linux).is_err(),
            "adversarial archive class must fail closed"
        );
    }
}

#[test]
fn recursive_capture_ignores_wrapped_command_descriptions() {
    struct WrappedDescription;

    impl HelpRunner for WrappedDescription {
        fn help(&mut self, path: &[String]) -> Result<String, String> {
            match path {
                [] => Ok("Commands:\n  serve  Serve requests with a deliberately long description\n    continued description text\n".to_owned()),
                [command] if command == "serve" => Ok("Usage: ts serve\n".to_owned()),
                _ => panic!("wrapped text must not become a command"),
            }
        }
    }

    let archive = capture_archive(&context(Platform::Linux), &mut WrappedDescription)
        .expect("should ignore wrapped description");
    let capture =
        validate_capture_archive(&archive, Platform::Linux).expect("should validate capture");

    assert!(!capture.help.contains("=== ts continued ==="));
}

#[derive(Clone)]
struct FakeHostedClient {
    run: HostedRun,
    artifacts: Vec<HostedArtifact>,
}

impl HostedClient for FakeHostedClient {
    fn run(&mut self, _run_id: u64) -> Result<HostedRun, String> {
        Ok(self.run.clone())
    }

    fn artifacts(&mut self, _run_id: u64) -> Result<Vec<HostedArtifact>, String> {
        Ok(self.artifacts.clone())
    }
}

fn hosted_client() -> FakeHostedClient {
    let mut artifacts = Vec::new();
    for platform in [Platform::Linux, Platform::Macos] {
        let inner = capture_archive(&context(platform), &mut FakeHelpRunner::default())
            .expect("should make hosted inner capture");
        let outer = outer_zip(platform, &inner);
        artifacts.push(HostedArtifact {
            id: if platform == Platform::Linux { 1 } else { 2 },
            name: format!("cli-help-{}", platform.as_str()),
            size_in_bytes: outer.len() as u64,
            expired: false,
            created_at: 1_010,
            digest: format!("sha256:{:x}", Sha256::digest(&outer)),
            bytes: outer,
        });
    }
    FakeHostedClient {
        run: HostedRun {
            id: 91,
            run_attempt: 2,
            repository: "IABTechLab/trusted-server".to_owned(),
            event: "pull_request".to_owned(),
            conclusion: "success".to_owned(),
            pull_request: 1049,
            workflow_path: ".github/workflows/test.yml".to_owned(),
            head_sha: context(Platform::Linux).source_sha,
            started_at: 1_000,
            completed_at: 1_100,
        },
        artifacts,
    }
}

#[test]
fn import_authenticates_run_artifacts_provenance_and_is_byte_stable() {
    let mut client = hosted_client();
    let first = import_authenticated(&mut client, 91, "0123456789abcdef0123456789abcdef01234567")
        .expect("should authenticate hosted captures");
    let second = import_authenticated(
        &mut hosted_client(),
        91,
        "0123456789abcdef0123456789abcdef01234567",
    )
    .expect("should repeat authenticated import");

    assert_eq!(
        first, second,
        "second import should produce identical bytes"
    );
    assert!(first.capture_manifest.contains("run_id = 91"));
    assert!(first.linux_help.contains("=== ts ==="));
    assert!(first.macos_help.contains("=== ts ==="));
}

#[test]
fn import_accepts_bounded_api_size_that_differs_from_download_length() {
    let mut client = hosted_client();
    client.artifacts[0].size_in_bytes += 1;

    import_authenticated(&mut client, 91, "0123456789abcdef0123456789abcdef01234567")
        .expect("API size is an independent bound, not a downloaded byte count");
}

#[test]
fn import_rejects_stale_mixed_unauthenticated_or_oversized_inputs() {
    let valid = hosted_client();
    let mut cases = Vec::new();
    let mut wrong_repository = valid.clone();
    wrong_repository.run.repository = "example/other".to_owned();
    cases.push(wrong_repository);
    let mut wrong_head = valid.clone();
    wrong_head.run.head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
    cases.push(wrong_head);
    let mut expired = valid.clone();
    expired.artifacts[0].expired = true;
    cases.push(expired);
    let mut stale = valid.clone();
    stale.artifacts[0].created_at = 999;
    cases.push(stale);
    let mut malformed_digest = valid.clone();
    malformed_digest.artifacts[0].digest = "sha256:00".to_owned();
    cases.push(malformed_digest);

    for mut client in cases {
        assert!(
            import_authenticated(&mut client, 91, "0123456789abcdef0123456789abcdef01234567",)
                .is_err(),
            "untrusted hosted input should fail"
        );
    }
}

fn transcript(records: &[(&str, &str)]) -> String {
    records
        .iter()
        .map(|(path, help)| format!("=== {path} ===\n{help}\n"))
        .collect()
}

fn overrides(linux_shared: &str, macos_shared: &str, linux_only: &str, macos_only: &str) -> String {
    format!(
        r#"version = 1
reviewed = true

[[annotations]]
command_path = "ts linux-only"
platform = "linux"
source_fingerprint = "{}"
owner = "documentation-maintainers"
rationale = "This command exists only on Linux."
expires_at = "2026-10-31T00:00:00Z"

[[annotations]]
command_path = "ts macos-only"
platform = "macos"
source_fingerprint = "{}"
owner = "documentation-maintainers"
rationale = "This command exists only on macOS."
expires_at = "2026-10-31T00:00:00Z"

[[overrides]]
command_path = "ts shared"
platform = "linux"
replacement = "canonical shared help\n"
source_fingerprint = "{}"
owner = "documentation-maintainers"
rationale = "Normalize host-specific executable spelling."
expires_at = "2026-10-31T00:00:00Z"

[[overrides]]
command_path = "ts shared"
platform = "macos"
replacement = "canonical shared help\n"
source_fingerprint = "{}"
owner = "documentation-maintainers"
rationale = "Normalize host-specific executable spelling."
expires_at = "2026-10-31T00:00:00Z"
"#,
        transcript_record_fingerprint("ts linux-only", linux_only),
        transcript_record_fingerprint("ts macos-only", macos_only),
        transcript_record_fingerprint("ts shared", linux_shared),
        transcript_record_fingerprint("ts shared", macos_shared),
    )
}

#[test]
fn checked_help_builds_a_deterministic_annotated_union() {
    let linux_shared = "linux spelling\n";
    let macos_shared = "macOS spelling\n";
    let linux_only = "Linux command\n";
    let macos_only = "macOS command\n";
    let linux = transcript(&[
        ("ts", "root\n"),
        ("ts linux-only", linux_only),
        ("ts shared", linux_shared),
    ]);
    let macos = transcript(&[
        ("ts", "root\n"),
        ("ts macos-only", macos_only),
        ("ts shared", macos_shared),
    ]);
    let manifest = overrides(linux_shared, macos_shared, linux_only, macos_only);

    let first = render_checked_help(&linux, &macos, manifest.as_bytes(), 1_780_000_000)
        .expect("should render annotated union");
    let second = render_checked_help(&linux, &macos, manifest.as_bytes(), 1_780_000_000)
        .expect("should render annotated union again");

    assert_eq!(first, second, "checked union should be byte-stable");
    assert_eq!(first.records.len(), 4);
    assert_eq!(first.records[0].command_path, "ts");
    assert_eq!(first.records[1].availability, HelpAvailability::LinuxOnly);
    assert_eq!(first.records[2].availability, HelpAvailability::MacosOnly);
    assert_eq!(first.records[3].help, "canonical shared help\n");
    assert!(first.rendered.contains("[platform: linux-only]"));
    assert!(first.rendered.contains("[platform: macos-only]"));
}

#[test]
fn checked_help_rejects_unannotated_platform_commands_and_stale_overrides() {
    let linux = transcript(&[("ts", "root\n"), ("ts linux-only", "Linux command\n")]);
    let macos = transcript(&[("ts", "root\n")]);
    let empty = b"version = 1\nreviewed = true\n";
    assert!(
        render_checked_help(&linux, &macos, empty, 1_780_000_000).is_err(),
        "platform-only command must have an annotation"
    );

    let mut stale = overrides(
        "linux spelling\n",
        "macOS spelling\n",
        "Linux command\n",
        "macOS command\n",
    );
    stale = stale.replacen("sha256:", "sha256:00", 1);
    let macos = transcript(&[("ts", "root\n"), ("ts macos-only", "macOS command\n")]);
    assert!(
        render_checked_help(&linux, &macos, stale.as_bytes(), 1_780_000_000).is_err(),
        "annotation fingerprints must bind one exact command record"
    );

    let linux = transcript(&[("ts", "Linux root\n")]);
    let macos = transcript(&[("ts", "macOS root\n")]);
    assert!(
        render_checked_help(&linux, &macos, empty, 1_780_000_000).is_err(),
        "shared command drift must require reconciling per-platform overrides"
    );
}

#[test]
fn checked_help_uses_the_injected_clock_for_override_expiry() {
    let command_help = "Linux command\n";
    let linux = transcript(&[("ts", "root\n"), ("ts linux-only", command_help)]);
    let macos = transcript(&[("ts", "root\n")]);
    let manifest = format!(
        r#"version = 1
reviewed = true

[[annotations]]
command_path = "ts linux-only"
platform = "linux"
source_fingerprint = "{}"
owner = "documentation-maintainers"
rationale = "This command exists only on Linux."
expires_at = "2026-10-31T00:00:00Z"
"#,
        transcript_record_fingerprint("ts linux-only", command_help)
    );

    render_checked_help(&linux, &macos, manifest.as_bytes(), 1_793_404_799)
        .expect("instant before expiry should pass");
    assert!(
        render_checked_help(&linux, &macos, manifest.as_bytes(), 1_793_404_800).is_err(),
        "expiry instant should fail"
    );
}

#[test]
fn checked_help_rejects_more_than_256_exact_command_records() {
    let mut transcript = "=== ts ===\nroot\n\n".to_owned();
    for index in 0..256 {
        transcript.push_str(&format!("=== ts command-{index} ===\ncommand help\n\n"));
    }

    assert!(
        render_checked_help(
            &transcript,
            &transcript,
            b"version = 1\nreviewed = true\n",
            1_780_000_000,
        )
        .is_err(),
        "the checked renderer must enforce the capture's 256-record bound"
    );
}
