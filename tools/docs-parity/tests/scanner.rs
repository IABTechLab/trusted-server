use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const SUCCESS: i32 = 0;
const ERROR: i32 = 2;

struct TestRepository {
    directory: TempDir,
}

impl TestRepository {
    fn new(path: &str, contents: &[u8], kind: &str) -> Self {
        let directory = tempfile::tempdir().expect("should create test repository");
        run_git(directory.path(), &["init", "--quiet"]);
        let absolute = directory.path().join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("should create tracked file parent");
        }
        fs::write(&absolute, contents).expect("should write tracked file");
        run_git(directory.path(), &["add", "--", path]);

        let manifests = directory.path().join("tools/docs-parity/manifests");
        fs::create_dir_all(&manifests).expect("should create manifest directory");
        fs::write(
            manifests.join("tracked-files.toml"),
            format!(
                "version = 1\nmax_text_bytes = 1048576\nreviewed = true\n\n[[files]]\npath = \"{path}\"\nkind = \"{kind}\"\n"
            ),
        )
        .expect("should write tracked-files manifest");
        let maintained = if kind == "text" {
            format!(
                "version = 1\nreviewed = true\ncomments = []\n\n[[sources]]\npath = \"{path}\"\nmode = \"whole\"\ndisposition = \"include\"\n"
            )
        } else {
            "version = 1\nreviewed = true\nsources = []\ncomments = []\n".to_owned()
        };
        fs::write(manifests.join("maintained-sources.toml"), maintained)
            .expect("should write maintained-sources manifest");
        fs::write(
            manifests.join("sensitive-allowlist.toml"),
            "version = 1\nreviewed = true\nexceptions = []\n",
        )
        .expect("should write allowlist");
        fs::write(
            manifests.join("retired-identifiers.toml"),
            "version = 1\nidentifiers = []\n",
        )
        .expect("should write denylist");
        Self { directory }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn write_allowlist(&self, contents: &str) {
        fs::write(
            self.path()
                .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
            contents,
        )
        .expect("should write allowlist");
    }

    fn write_denylist(&self, contents: &str) {
        fs::write(
            self.path()
                .join("tools/docs-parity/manifests/retired-identifiers.toml"),
            contents,
        )
        .expect("should write denylist");
    }

    fn scan(&self) -> Output {
        Command::new(binary())
            .current_dir(self.path())
            .args(["scan", "--check"])
            .output()
            .expect("should execute docs-parity")
    }

    fn bootstrap(&self) -> Output {
        Command::new(binary())
            .current_dir(self.path())
            .args(["scan", "--bootstrap"])
            .output()
            .expect("should execute docs-parity")
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

fn status_code(output: &Output) -> i32 {
    output.status.code().expect("should exit normally")
}

fn diagnostic(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("diagnostic should be UTF-8")
}

fn exception(
    class: &str,
    path: &str,
    detector: &str,
    matched_content: &str,
    expiry: &str,
) -> String {
    format!(
        "version = 1\nreviewed = true\n\n[[exceptions]]\nclass = \"{class}\"\npath = \"{path}\"\ndetector = \"{detector}\"\nscope = \"exact-occurrence\"\nselector = \"bytes:0-{}\"\nfingerprint = \"{}\"\nowner = \"docs-owner\"\nrationale = \"Reviewed fixture with an exact content boundary.\"\nexpires_at = \"{expiry}\"\n",
        matched_content.len(),
        fingerprint(matched_content.as_bytes())
    )
}

fn exception_for_contents(
    class: &str,
    path: &str,
    detector: &str,
    contents: &[u8],
    matched_content: &str,
    expiry: &str,
) -> String {
    let start = contents
        .windows(matched_content.len())
        .position(|window| window == matched_content.as_bytes())
        .expect("matched fixture content should exist");
    exception(class, path, detector, matched_content, expiry).replace(
        &format!("selector = \"bytes:0-{}\"", matched_content.len()),
        &format!(
            "selector = \"bytes:{start}-{}\"",
            start + matched_content.len()
        ),
    )
}

fn fingerprint(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}

#[test]
fn allowlist_requires_explicit_review_attestation() {
    let repository = TestRepository::new("notes.txt", b"safe text\n", "text");
    repository.write_allowlist("version = 1\nexceptions = []\n");
    let result = repository.scan();
    assert_eq!(status_code(&result), ERROR);
    assert!(diagnostic(&result).contains("reviewed"));
}

#[test]
fn service_id_cannot_use_historical_example_class() {
    let value = "AbCdEf1234567890GhIj";
    let contents = format!("service_id = \"{value}\"\n");
    let repository = TestRepository::new("fastly.toml", contents.as_bytes(), "text");
    repository.write_allowlist(&exception_for_contents(
        "historical_example",
        "fastly.toml",
        "service_id",
        contents.as_bytes(),
        value,
        "2099-01-01T00:00:00Z",
    ));
    let result = repository.scan();
    assert_eq!(status_code(&result), ERROR);
    assert!(diagnostic(&result).contains("incompatible"));
}

fn assert_detected(path: &str, contents: &[u8], kind: &str, detector: &str) {
    let repository = TestRepository::new(path, contents, kind);

    let result = repository.scan();

    assert_eq!(status_code(&result), ERROR, "finding should fail");
    assert!(
        diagnostic(&result).contains(&format!("sensitive finding [{detector}] in {path}")),
        "diagnostic should identify detector and path: {}",
        diagnostic(&result)
    );
}

fn assert_allowlisted(
    path: &str,
    contents: &[u8],
    kind: &str,
    detector: &str,
    matched_content: &str,
    class: &str,
) {
    let repository = TestRepository::new(path, contents, kind);
    repository.write_allowlist(&exception_for_contents(
        class,
        path,
        detector,
        contents,
        matched_content,
        "2099-01-01T00:00:00Z",
    ));

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "typed exception should pass: {}",
        diagnostic(&result)
    );
}

#[test]
fn domain_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = format!("https://portal.{}", "private-corp.internal/path");
    assert_detected("notes.txt", value.as_bytes(), "text", "domain");
    assert_allowlisted(
        "notes.txt",
        value.as_bytes(),
        "text",
        "domain",
        &value,
        "vendor_url",
    );
}

#[test]
fn bare_domain_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = format!("portal.{}", "private-corp.internal/path");
    assert_detected("notes.txt", value.as_bytes(), "text", "domain");
    assert_allowlisted(
        "notes.txt",
        value.as_bytes(),
        "text",
        "domain",
        &value,
        "vendor_url",
    );
}

#[test]
fn bare_domain_detector_does_not_match_source_member_prefixes() {
    let contents = b"result.contains(value); output.status.code(); document.body.append(node);\nexample.com.evil service.co.uk\n";
    let repository = TestRepository::new("fixture.rs", contents, "text");

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "source member names are not bare domains: {}",
        diagnostic(&result)
    );
}

#[test]
fn reserved_and_local_domain_hosts_do_not_require_exceptions() {
    let contents = b"https://service.example/path\nhttps://service.invalid/path\nhttps://service.test/path\nhttps://service.localhost/path\nhttps://example.com/path\nhttps://example.net/path\nhttps://example.org/path\nperson@service.example\nperson@service.invalid\nperson@service.test\nperson@service.localhost\n";
    let repository = TestRepository::new("notes.txt", contents, "text");

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "RFC-reserved and local hosts are synthetic: {}",
        diagnostic(&result)
    );
}

#[test]
fn email_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = format!("person@{}", "private-corp.internal");
    assert_detected("notes.txt", value.as_bytes(), "text", "email");
    assert_allowlisted(
        "notes.txt",
        value.as_bytes(),
        "text",
        "email",
        &value,
        "historical_example",
    );
}

#[test]
fn email_detector_ignores_url_userinfo_and_at_sign_filenames() {
    for value in [
        format!("https://user:pass@portal.{}", "private-corp.internal"),
        format!(
            "https://cdn.{}/{}@{}",
            "private-corp.internal", "banner", "2x.jpg"
        ),
    ] {
        let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");
        repository.write_allowlist(&exception_for_contents(
            "vendor_url",
            "notes.txt",
            "domain",
            value.as_bytes(),
            &value,
            "2099-01-01T00:00:00Z",
        ));

        let result = repository.scan();

        assert_eq!(
            status_code(&result),
            SUCCESS,
            "URL substrings are not email addresses: {}",
            diagnostic(&result)
        );
    }
}

#[test]
fn credential_shape_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = "super-secret-value-123";
    let contents = format!("api_secret = \"{value}\"\n");
    assert_detected(
        "fixture.toml",
        contents.as_bytes(),
        "text",
        "credential_shape",
    );
    assert_allowlisted(
        "fixture.toml",
        contents.as_bytes(),
        "text",
        "credential_shape",
        value,
        "hash_pinned_fake_credential_fixture",
    );
}

#[test]
fn credential_shape_detector_ignores_source_expressions() {
    let contents = br#"
let password = credentials_parts.next()?.to_owned();
let credential = bypass.credential_secret_name.as_ref();
original.handlers[0].password = Redacted::new("true".to_string());
"#;
    let repository = TestRepository::new("fixture.rs", contents, "text");

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "source expressions are not credential values: {}",
        diagnostic(&result)
    );
}

#[test]
fn service_id_detector_has_positive_and_exact_expiry_allowlisted_fixtures() {
    let value = "AbCdEf1234567890GhIj";
    let contents = format!("service_id = \"{value}\"\n");
    assert_detected("fastly.toml", contents.as_bytes(), "text", "service_id");

    let repository = TestRepository::new("fastly.toml", contents.as_bytes(), "text");
    let mut record = exception_for_contents(
        "service_id",
        "fastly.toml",
        "service_id",
        contents.as_bytes(),
        value,
        "2026-09-30T00:00:00Z",
    );
    record = record.replace("owner = \"docs-owner\"", "owner = \"aram356\"");
    repository.write_allowlist(&record);
    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "approved service exception should pass before expiry: {}",
        diagnostic(&result)
    );
}

#[test]
fn encoded_token_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = "QWxhZGRpbjpvcGVuIHNlc2FtZTEyMzQ1Njc4OTA=";
    let contents = format!("encoded_token = \"{value}\"\n");
    assert_detected("fixture.toml", contents.as_bytes(), "text", "encoded_token");
    assert_allowlisted(
        "fixture.toml",
        contents.as_bytes(),
        "text",
        "encoded_token",
        value,
        "hash_pinned_fake_credential_fixture",
    );
}

#[test]
fn binary_string_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = format!("https://binary.{}", "private-corp.internal/asset");
    let mut contents = vec![0, 1, 2];
    contents.extend_from_slice(value.as_bytes());
    contents.extend_from_slice(&[0, 255]);
    assert_detected("asset.bin", &contents, "binary", "binary_string");
    assert_allowlisted(
        "asset.bin",
        &contents,
        "binary",
        "binary_string",
        &value,
        "vendor_url",
    );
}

#[test]
fn structured_lockfile_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = format!("https://user:secret@{}/pkg.tgz", "private-corp.internal");
    let contents = format!(
        "{{\"lockfileVersion\":3,\"packages\":{{\"node_modules/pkg\":{{\"resolved\":\"{value}\"}}}}}}"
    );
    assert_detected(
        "package-lock.json",
        contents.as_bytes(),
        "text",
        "lockfile_field",
    );
    assert_allowlisted(
        "package-lock.json",
        contents.as_bytes(),
        "text",
        "lockfile_field",
        &value,
        "vendor_url",
    );
}

#[test]
fn lockfiles_scan_non_url_secrets_and_select_the_structured_value_occurrence() {
    let secret = "fake-credential-value-123";
    let cargo = format!("version = 3\npassword = \"{secret}\"\n");
    assert_detected("Cargo.lock", cargo.as_bytes(), "text", "credential_shape");

    let value = format!("https://user:secret@{}/pkg.tgz", "private-corp.internal");
    let contents = format!("{{\"description\":\"{value}\",\"resolved\":\"{value}\"}}");
    let repository = TestRepository::new("package-lock.json", contents.as_bytes(), "text");
    let result = repository.bootstrap();
    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    let second = contents
        .rfind(&value)
        .expect("resolved occurrence should exist");
    assert!(manifest.contains(&format!("detector = \"lockfile_field\"\nscope = \"exact-occurrence\"\nselector = \"bytes:{second}-{}\"", second + value.len())), "structured selector must identify the resolved value: {manifest}");
}

#[test]
fn media_metadata_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = format!("person@{}", "private-corp.internal");
    let contents = png_text_chunk("Author", &value);
    assert_detected("asset.png", &contents, "binary", "media_metadata");
    assert_allowlisted(
        "asset.png",
        &contents,
        "binary",
        "media_metadata",
        &value,
        "historical_example",
    );
}

#[test]
fn identical_media_and_non_metadata_binary_values_remain_distinct_occurrences() {
    let value = format!("person@{}", "private-corp.internal");
    let mut contents = png_text_chunk("Author", &value);
    contents.push(0);
    contents.extend_from_slice(value.as_bytes());
    let repository = TestRepository::new("asset.png", &contents, "binary");
    let result = repository.bootstrap();
    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert!(manifest.contains("detector = \"media_metadata\""));
    assert!(manifest.contains("detector = \"binary_string\""));
}

#[test]
fn retired_identifier_and_access_phrase_have_positive_and_allowlisted_fixtures() {
    for (kind, value) in [
        ("identifier", "retired-integration-name"),
        ("access_phrase", "Ask the internal team for access"),
    ] {
        let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");
        repository.write_denylist(&retired_record(kind, value));
        let failed = repository.scan();
        assert_eq!(status_code(&failed), ERROR, "denylist match should fail");
        assert!(
            diagnostic(&failed).contains("sensitive finding [retired_identifier] in notes.txt"),
            "diagnostic should identify denylist finding: {}",
            diagnostic(&failed)
        );

        repository.write_allowlist(&exception(
            "historical_example",
            "notes.txt",
            "retired_identifier",
            value,
            "2099-01-01T00:00:00Z",
        ));
        let allowed = repository.scan();
        assert_eq!(
            status_code(&allowed),
            SUCCESS,
            "narrow historical exception should pass: {}",
            diagnostic(&allowed)
        );
    }
}

#[test]
fn real_retired_manifest_contains_no_retired_plaintext() {
    let manifest = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("manifests/retired-identifiers.toml"),
    )
    .expect("should read real retired manifest");
    let expected_fingerprints = [
        "sha256:9d155035a90b33ea3c44f8ebfc4bad1c3a806cc5e9236fe342d07d620ca5264a",
        "sha256:a23c8f7a72d4ed48ef48e0d65fcb7e5b3de28b37bce3c18c4a027d38e5b97b69",
        "sha256:47e9ab61971c8228ee3a5a7e4e7f492a8a7d2ed05000612351d2798a1ecfc5d5",
        "sha256:262d2f5569ef006ffeb288d606696f930c70dbeec9986ae066554a1b7ba4348c",
        "sha256:d7ead80b23e0925b55d81d235f4ec7789ef76bbc39b64bd4856858607005a45e",
        "sha256:fabebfaf9f751578404dade1f6911c5211e55112155cd1eb37742ab5eeaa29f5",
        "sha256:c85965885f1663af8d435525074cc6a70c1548b8e310220540502b6a0e35def2",
        "sha256:3ced6222be77642263490f3cf2bd0a95b276965f2f85586d1d7a32c7c3219e0c",
        "sha256:524b06e6a628b58b984b7e29c803bf378268afd683b9941d11b88c68251cdc58",
        "sha256:2c40b81f8f43edb238641a6bd9c8439a028c11c83ffe46e6c5aee6190cc65210",
    ];
    assert!(
        !manifest.lines().any(|line| line.starts_with("value = ")),
        "real denylist must contain fingerprints, never retired plaintext fields"
    );
    assert_eq!(
        manifest.matches("[[identifiers]]").count(),
        expected_fingerprints.len(),
        "real denylist should contain the exact reviewed fingerprint set"
    );
    for fingerprint in expected_fingerprints {
        assert!(
            manifest.contains(fingerprint),
            "real denylist must contain each reviewed fingerprint"
        );
    }
}

#[test]
fn retired_kind_rejects_mislabeled_normalization_shapes() {
    for (kind, value, expected) in [
        (
            "identifier",
            "multiple retired words",
            "identifier denylist record must contain exactly one token",
        ),
        (
            "access_phrase",
            "single-token",
            "access-phrase denylist record must contain multiple words",
        ),
    ] {
        let repository = TestRepository::new("notes.txt", b"example.com\n", "text");
        repository.write_denylist(&retired_record(kind, value));

        let result = repository.scan();

        assert_eq!(status_code(&result), ERROR, "mislabeled shape should fail");
        assert!(
            diagnostic(&result).contains(expected),
            "diagnostic should identify kind/shape mismatch: {}",
            diagnostic(&result)
        );
    }
}

#[test]
fn retired_phrase_matching_does_not_join_code_fragments_across_punctuation() {
    let phrase = ["Review access through approved", "channel"].join(" ");
    let source = r#""Review access through approved " + "channel""#;
    let repository = TestRepository::new("audit.txt", source.as_bytes(), "text");
    repository.write_denylist(&retired_record("access_phrase", &phrase));

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "punctuation-separated source fragments are not the retired prose: {}",
        diagnostic(&result)
    );
}

fn retired_record(kind: &str, value: &str) -> String {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    format!(
        "version = 1\n\n[[identifiers]]\nkind = \"{kind}\"\nfingerprint = \"{}\"\nnormalized_length = {}\nword_count = {}\ncase_insensitive = true\nwhitespace_tolerant = {}\n",
        fingerprint(normalized.as_bytes()),
        normalized.len(),
        normalized.split_whitespace().count(),
        normalized.split_whitespace().count() > 1,
    )
}

#[test]
fn all_five_exception_classes_are_supported_with_narrow_detector_pairings() {
    let fixtures = [
        ("vendor_url", "domain"),
        ("hash_pinned_fake_credential_fixture", "credential_shape"),
        ("historical_example", "email"),
        ("project_owned_public_domain", "domain"),
    ];
    for (class, detector) in fixtures {
        let value = match detector {
            "domain" => format!("https://owned.{}", "private-corp.internal"),
            "credential_shape" => "fake-credential-value-123".to_owned(),
            "email" => format!("archived@{}", "private-corp.internal"),
            "service_id" => "AbCdEf1234567890GhIj".to_owned(),
            _ => unreachable!("fixture detector should be known"),
        };
        let contents = match detector {
            "credential_shape" => format!("secret = \"{value}\""),
            "service_id" => format!("service_id = \"{value}\""),
            _ => value.clone(),
        };
        assert_allowlisted(
            "notes.txt",
            contents.as_bytes(),
            "text",
            detector,
            &value,
            class,
        );
    }
}

#[test]
fn service_id_exception_rejects_inexact_path_owner_or_expiry() {
    let value = "AbCdEf1234567890GhIj";
    let contents = format!("service_id = \"{value}\"\n");
    for (path, owner, expiry, expected) in [
        (
            "nested/fastly.toml",
            "aram356",
            "2026-09-30T00:00:00Z",
            "service-ID exception path must be exactly fastly.toml",
        ),
        (
            "fastly.toml",
            "docs-owner",
            "2026-09-30T00:00:00Z",
            "service-ID exception owner must be aram356",
        ),
        (
            "fastly.toml",
            "aram356",
            "2026-10-01T00:00:00Z",
            "service-ID exception expiry must be 2026-09-30T00:00:00Z",
        ),
    ] {
        let repository = TestRepository::new(path, contents.as_bytes(), "text");
        let mut record = exception_for_contents(
            "service_id",
            path,
            "service_id",
            contents.as_bytes(),
            value,
            expiry,
        );
        record = record.replace("owner = \"docs-owner\"", &format!("owner = \"{owner}\""));
        repository.write_allowlist(&record);

        let result = repository.scan();

        assert_eq!(
            status_code(&result),
            ERROR,
            "inexact service record should fail"
        );
        assert!(
            diagnostic(&result).contains(expected),
            "diagnostic should identify the exact service rule: {}",
            diagnostic(&result)
        );
    }
}

#[test]
fn expired_exception_fails_at_and_after_its_expiry() {
    let value = format!("https://expired.{}", "private-corp.internal");
    let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");
    repository.write_allowlist(&exception(
        "vendor_url",
        "notes.txt",
        "domain",
        &value,
        "2000-01-01T00:00:00Z",
    ));

    let result = repository.scan();

    assert_eq!(status_code(&result), ERROR, "expired entry should fail");
    assert!(
        diagnostic(&result).contains("expired sensitive-data exception"),
        "diagnostic should identify expiry: {}",
        diagnostic(&result)
    );
}

#[test]
fn stale_fingerprint_fails_closed() {
    let value = format!("https://changed.{}", "private-corp.internal");
    let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");
    repository.write_allowlist(&exception(
        "vendor_url",
        "notes.txt",
        "domain",
        "https://stale.private-corp.internal",
        "2099-01-01T00:00:00Z",
    ));

    let result = repository.scan();

    assert_eq!(status_code(&result), ERROR, "stale hash should fail");
    assert!(
        diagnostic(&result).contains("sensitive finding [domain] in notes.txt"),
        "stale hash should leave finding unallowlisted: {}",
        diagnostic(&result)
    );
}

#[test]
fn renamed_path_fails_closed() {
    let value = format!("https://renamed.{}", "private-corp.internal");
    let repository = TestRepository::new("renamed.txt", value.as_bytes(), "text");
    repository.write_allowlist(&exception(
        "vendor_url",
        "original.txt",
        "domain",
        &value,
        "2099-01-01T00:00:00Z",
    ));

    let result = repository.scan();

    assert_eq!(status_code(&result), ERROR, "renamed path should fail");
    assert!(
        diagnostic(&result).contains("sensitive finding [domain] in renamed.txt"),
        "renamed path should not inherit an exception: {}",
        diagnostic(&result)
    );
}

#[test]
fn broad_domain_exception_is_rejected() {
    let value = format!("https://broad.{}", "private-corp.internal");
    let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");
    repository.write_allowlist(&exception(
        "vendor_url",
        "*.txt",
        "domain",
        &value,
        "2099-01-01T00:00:00Z",
    ));

    let result = repository.scan();

    assert_eq!(status_code(&result), ERROR, "broad path should fail");
    assert!(
        diagnostic(&result).contains("exception path must be an exact normalized path"),
        "diagnostic should reject broad scope: {}",
        diagnostic(&result)
    );
}

#[test]
fn check_mode_never_writes_manifest_bytes() {
    let repository = TestRepository::new("notes.txt", b"example.com\n", "text");
    let manifest_path = repository
        .path()
        .join("tools/docs-parity/manifests/sensitive-allowlist.toml");
    let before = fs::read(&manifest_path).expect("should read allowlist before check");

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "fictional content should be clean: {}",
        diagnostic(&result)
    );
    assert_eq!(
        fs::read(&manifest_path).expect("should read allowlist after check"),
        before,
        "check mode should not write"
    );
}

#[test]
fn bootstrap_writes_governed_candidates_but_never_self_approves_them() {
    let value = "AbCdEf1234567890GhIj";
    let contents = format!("service_id = \"{value}\"\n");
    let repository = TestRepository::new("fastly.toml", contents.as_bytes(), "text");

    let bootstrapped = repository.bootstrap();
    let allowlist_path = repository
        .path()
        .join("tools/docs-parity/manifests/sensitive-allowlist.toml");
    let candidates = fs::read_to_string(&allowlist_path).expect("should read candidates");
    let checked = repository.scan();

    assert_eq!(
        status_code(&bootstrapped),
        SUCCESS,
        "bootstrap should succeed: {}",
        diagnostic(&bootstrapped)
    );
    assert!(
        candidates.contains("reviewed = false")
            && candidates.contains("class = \"service_id\"")
            && candidates.contains("path = \"fastly.toml\"")
            && candidates.contains("owner = \"aram356\"")
            && candidates.contains("expires_at = \"2026-09-30T00:00:00Z\"")
            && !candidates.contains("REVIEW_REQUIRED"),
        "bootstrap should emit complete but unattested governance: {candidates}"
    );
    assert_eq!(
        status_code(&checked),
        ERROR,
        "unreviewed candidate manifest should fail check"
    );
    assert!(
        diagnostic(&checked).contains("sensitive-data candidates require review"),
        "diagnostic should require review attestation: {}",
        diagnostic(&checked)
    );
}

#[test]
fn bootstrap_ignores_derived_governance_fields_and_is_byte_stable() {
    let repository = TestRepository::new(
        "tools/docs-parity/manifests/sensitive-allowlist.toml",
        b"version = 1\n",
        "text",
    );
    repository.write_allowlist(&format!(
        "version = 1\nreviewed = true\n\n[[exceptions]]\nclass = \"vendor_url\"\npath = \"fixtures/example.test.ts\"\ndetector = \"domain\"\nscope = \"exact-occurrence\"\nselector = \"bytes:1-2\"\nfingerprint = \"{}\"\nowner = \"docs-owner\"\nrationale = \"Reviewed exact fixture occurrence.\"\nexpires_at = \"2099-01-01T00:00:00Z\"\n",
        fingerprint(b"x")
    ));

    let first_result = repository.bootstrap();
    let allowlist_path = repository
        .path()
        .join("tools/docs-parity/manifests/sensitive-allowlist.toml");
    let first = fs::read(&allowlist_path).expect("should read first bootstrap");
    let second_result = repository.bootstrap();
    let second = fs::read(&allowlist_path).expect("should read second bootstrap");

    assert_eq!(
        status_code(&first_result),
        SUCCESS,
        "first bootstrap should succeed: {}",
        diagnostic(&first_result)
    );
    assert_eq!(
        status_code(&second_result),
        SUCCESS,
        "second bootstrap should succeed: {}",
        diagnostic(&second_result)
    );
    assert_eq!(
        String::from_utf8(first.clone())
            .expect("candidate manifest should be UTF-8")
            .matches("[[exceptions]]")
            .count(),
        0,
        "a governance path field must not become repository content"
    );
    assert_eq!(first, second, "a second bootstrap must be byte-stable");
}

#[test]
fn governance_free_text_cannot_hide_sensitive_content() {
    let repository = TestRepository::new("notes.txt", b"safe fixture\n", "text");
    let sensitive = format!("https://governance.{}", "private-corp.internal");
    repository.write_allowlist(&format!(
        "version = 1\nreviewed = true\n\n[[exceptions]]\nclass = \"vendor_url\"\npath = \"notes.txt\"\ndetector = \"domain\"\nscope = \"exact-occurrence\"\nselector = \"bytes:0-1\"\nfingerprint = \"{}\"\nowner = \"docs-owner\"\nrationale = \"Reviewed at {sensitive}.\"\nexpires_at = \"2099-01-01T00:00:00Z\"\n",
        fingerprint(b"x")
    ));

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        ERROR,
        "sensitive rationale should fail"
    );
    assert!(
        diagnostic(&result).contains("sensitive value in governance free-text"),
        "diagnostic should reject sensitive governance prose: {}",
        diagnostic(&result)
    );
}

#[test]
fn governance_comments_are_rejected_even_when_they_contain_retired_prose() {
    let phrase = ["retired", "administrative", "access phrase"].join(" ");
    let repository = TestRepository::new("notes.txt", b"safe fixture\n", "text");
    repository.write_denylist(&format!(
        "# {phrase}\n{}",
        retired_record("access_phrase", &phrase)
    ));

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        ERROR,
        "governance comment should fail"
    );
    assert!(
        diagnostic(&result).contains("governance manifest must be comment-free"),
        "diagnostic should reject comment-bearing governance: {}",
        diagnostic(&result)
    );
}

#[test]
fn hash_characters_inside_governance_strings_are_not_comments() {
    let repository = TestRepository::new("notes#fixture.txt", b"safe fixture\n", "text");

    let result = repository.scan();

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "a quoted hash should remain valid TOML data: {}",
        diagnostic(&result)
    );
}

#[test]
fn bootstrap_preserves_exact_attestation_but_reopens_review_for_a_new_finding() {
    let first_value = format!("https://first.{}", "private-corp.internal/path");
    let repository = TestRepository::new("notes.txt", first_value.as_bytes(), "text");
    let initial = repository.bootstrap();
    assert_eq!(
        status_code(&initial),
        SUCCESS,
        "initial bootstrap should succeed: {}",
        diagnostic(&initial)
    );
    let allowlist_path = repository
        .path()
        .join("tools/docs-parity/manifests/sensitive-allowlist.toml");
    let candidates = fs::read_to_string(&allowlist_path).expect("should read candidates");
    repository.write_allowlist(&candidates.replace("reviewed = false", "reviewed = true"));
    let reviewed = fs::read(&allowlist_path).expect("should read reviewed allowlist");

    let unchanged = repository.bootstrap();
    assert_eq!(
        status_code(&unchanged),
        SUCCESS,
        "unchanged bootstrap should succeed: {}",
        diagnostic(&unchanged)
    );
    assert_eq!(
        fs::read(&allowlist_path).expect("should reread reviewed allowlist"),
        reviewed,
        "an exact reviewed allowlist must remain byte-identical"
    );

    let second_value = format!("https://second.{}", "private-corp.internal/path");
    fs::write(
        repository.path().join("notes.txt"),
        format!("{first_value}\n{second_value}\n"),
    )
    .expect("should add a finding");
    run_git(repository.path(), &["add", "--", "notes.txt"]);
    let updated = repository.bootstrap();
    let updated_allowlist =
        fs::read_to_string(&allowlist_path).expect("should read updated allowlist");
    let checked = repository.scan();

    assert_eq!(
        status_code(&updated),
        SUCCESS,
        "updated bootstrap should succeed: {}",
        diagnostic(&updated)
    );
    assert!(
        updated_allowlist.contains("reviewed = false")
            && updated_allowlist.matches("[[exceptions]]").count() == 2,
        "a new finding must reopen the complete candidate set: {updated_allowlist}"
    );
    assert_eq!(
        status_code(&checked),
        ERROR,
        "new candidates must fail check until reviewed"
    );
    assert!(
        diagnostic(&checked).contains("sensitive-data candidates require review"),
        "diagnostic should require renewed attestation: {}",
        diagnostic(&checked)
    );
}

#[test]
fn bootstrap_assigns_historical_and_project_owned_domain_classes() {
    let historical = ["your-custom-", "domain.com"].concat();
    let project_owned = "https://iabtechlab.github.io/trusted-server/";
    let contents = format!("{historical}\n{project_owned}\n");
    let repository = TestRepository::new("notes.txt", contents.as_bytes(), "text");

    let result = repository.bootstrap();
    let candidates = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read candidates");

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "bootstrap should succeed: {}",
        diagnostic(&result)
    );
    assert_eq!(
        candidates.matches("class = \"historical_example\"").count(),
        1,
        "deleted CNAME literal should be historical"
    );
    assert!(
        candidates.contains("expires_at = \"2027-08-31T00:00:00Z\""),
        "historical CNAME decision expiry should be preserved"
    );
    assert_eq!(
        candidates
            .matches("class = \"project_owned_public_domain\"")
            .count(),
        1,
        "IAB GitHub Pages URL should be project-owned"
    );
}

#[test]
fn project_owned_class_requires_an_exact_or_subdomain_host_boundary() {
    let deceptive = "https://notiabtechlab.com.evil/path";
    let repository = TestRepository::new("notes.txt", deceptive.as_bytes(), "text");

    let result = repository.bootstrap();
    let candidates = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read candidates");

    assert_eq!(
        status_code(&result),
        SUCCESS,
        "bootstrap should succeed: {}",
        diagnostic(&result)
    );
    assert!(
        candidates.contains("class = \"vendor_url\"")
            && !candidates.contains("class = \"project_owned_public_domain\""),
        "lookalike host must not inherit project ownership: {candidates}"
    );
}

#[test]
fn identical_findings_require_distinct_occurrences_and_moving_one_invalidates_scope() {
    let value = format!("https://duplicate.{}", "private-corp.internal/path");
    let contents = format!("{value}\n{value}\n");
    let repository = TestRepository::new("notes.txt", contents.as_bytes(), "text");
    let first = occurrence_exception("vendor_url", "notes.txt", "domain", &value, 0, value.len());
    repository.write_allowlist(&first);
    let missing_second = repository.scan();
    assert_eq!(
        status_code(&missing_second),
        ERROR,
        "one record must not cover two identical occurrences"
    );

    let second_start = value.len() + 1;
    let two_records = format!(
        "{}{}",
        first,
        occurrence_exception_body(
            "vendor_url",
            "notes.txt",
            "domain",
            &value,
            second_start,
            second_start + value.len(),
        )
    );
    repository.write_allowlist(&two_records);
    let complete = repository.scan();
    assert_eq!(
        status_code(&complete),
        SUCCESS,
        "both exact occurrences should pass: {}",
        diagnostic(&complete)
    );

    fs::write(
        repository.path().join("notes.txt"),
        format!("prefix {value}\n{value}\n"),
    )
    .expect("should move both occurrences");
    run_git(repository.path(), &["add", "--", "notes.txt"]);
    let moved = repository.scan();
    assert_eq!(status_code(&moved), ERROR, "moved occurrence should fail");
    assert!(
        diagnostic(&moved).contains("sensitive finding [domain] in notes.txt")
            || diagnostic(&moved).contains("stale sensitive-data exception"),
        "moved bytes should invalidate exact scope: {}",
        diagnostic(&moved)
    );
}

fn occurrence_exception(
    class: &str,
    path: &str,
    detector: &str,
    value: &str,
    start: usize,
    end: usize,
) -> String {
    format!(
        "version = 1\nreviewed = true\n{}",
        occurrence_exception_body(class, path, detector, value, start, end)
    )
}

fn occurrence_exception_body(
    class: &str,
    path: &str,
    detector: &str,
    value: &str,
    start: usize,
    end: usize,
) -> String {
    format!(
        "\n[[exceptions]]\nclass = \"{class}\"\npath = \"{path}\"\ndetector = \"{detector}\"\nscope = \"exact-occurrence\"\nselector = \"bytes:{start}-{end}\"\nfingerprint = \"{}\"\nowner = \"docs-owner\"\nrationale = \"Reviewed fixture with an exact content boundary.\"\nexpires_at = \"2099-01-01T00:00:00Z\"\n",
        fingerprint(value.as_bytes())
    )
}

fn png_text_chunk(keyword: &str, value: &str) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(keyword.as_bytes());
    data.push(0);
    data.extend_from_slice(value.as_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&(u32::try_from(data.len()).expect("chunk should fit")).to_be_bytes());
    png.extend_from_slice(b"tEXt");
    png.extend_from_slice(&data);
    png.extend_from_slice(&[0, 0, 0, 0]);
    png
}
