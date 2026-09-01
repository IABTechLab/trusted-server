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
            "version = 1\nreviewed = true\nidentifiers = []\n",
        )
        .expect("should write denylist");
        Self { directory }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn add_text(&self, path: &str, contents: &str) {
        let absolute = self.path().join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("should create tracked file parent");
        }
        fs::write(&absolute, contents).expect("should write additional tracked file");
        run_git(self.path(), &["add", "-f", "--", path]);

        let manifests = self.path().join("tools/docs-parity/manifests");
        let tracked_path = manifests.join("tracked-files.toml");
        let mut tracked = fs::read_to_string(&tracked_path).expect("should read tracked manifest");
        tracked.push_str(&format!(
            "\n[[files]]\npath = \"{path}\"\nkind = \"text\"\n"
        ));
        fs::write(tracked_path, tracked).expect("should extend tracked manifest");

        let maintained_path = manifests.join("maintained-sources.toml");
        let mut maintained =
            fs::read_to_string(&maintained_path).expect("should read maintained manifest");
        maintained.push_str(&format!(
            "\n[[sources]]\npath = \"{path}\"\nmode = \"whole\"\ndisposition = \"include\"\n"
        ));
        fs::write(maintained_path, maintained).expect("should extend maintained manifest");
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
    let governed = governed_match(detector, matched_content);
    format!(
        "version = 1\nreviewed = true\n\n[[exceptions]]\nclass = \"{class}\"\npath = \"{path}\"\ndetector = \"{detector}\"\nscope = \"exact-occurrence\"\nselector = \"bytes:0-{}\"\nfingerprint = \"{}\"\nowner = \"docs-owner\"\nrationale = \"Reviewed fixture with an exact content boundary.\"\nexpires_at = \"{expiry}\"\n",
        governed.len(),
        fingerprint(governed.as_bytes())
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
    let governed = governed_match(detector, matched_content);
    let start = contents
        .windows(governed.len())
        .position(|window| window == governed.as_bytes())
        .expect("matched fixture content should exist");
    exception(class, path, detector, &governed, expiry).replace(
        &format!("selector = \"bytes:0-{}\"", governed.len()),
        &format!("selector = \"bytes:{start}-{}\"", start + governed.len()),
    )
}

fn governed_match(detector: &str, value: &str) -> String {
    let domain_value = detector == "domain";
    let embedded_url =
        matches!(detector, "binary_string" | "media_metadata") && value.contains("://");
    if !domain_value && !embedded_url {
        return value.to_owned();
    }
    value
        .split_once("://")
        .map_or(value, |(_scheme, remainder)| remainder)
        .split(['/', '?', '#'])
        .next()
        .expect("host")
        .rsplit('@')
        .next()
        .expect("host")
        .split(':')
        .next()
        .expect("host")
        .to_owned()
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

#[test]
fn binary_and_media_service_ids_cannot_bypass_service_governance() {
    let value = "AbCdEf1234567890GhIj";
    let text = format!("service_id = \"{value}\"");
    let mut binary = vec![0];
    binary.extend_from_slice(text.as_bytes());
    binary.push(0);
    let media = png_text_chunk("Service", &text);
    for (path, contents) in [("asset.bin", binary), ("asset.png", media)] {
        let repository = TestRepository::new(path, &contents, "binary");
        let mut record = exception_for_contents(
            "historical_example",
            path,
            "service_id",
            &contents,
            value,
            "2099-01-01T00:00:00Z",
        );
        record = record.replace("owner = \"docs-owner\"", "owner = \"wrong-owner\"");
        repository.write_allowlist(&record);
        let result = repository.scan();
        assert_eq!(status_code(&result), ERROR, "{path} must fail");
        assert!(diagnostic(&result).contains("incompatible"));
    }
}

#[test]
fn modern_long_and_punycode_domains_are_detected() {
    let modern = ["getpurpose", ".ai"].concat();
    for value in [
        modern.as_str(),
        "service.technology",
        "host.xn--p1ai",
        "service.co.uk",
    ] {
        assert_detected("notes.txt", value.as_bytes(), "text", "domain");
    }
}

#[test]
fn retired_manifest_requires_explicit_true_attestation() {
    for contents in [
        "version = 1\nidentifiers = []\n",
        "version = 1\nreviewed = false\nidentifiers = []\n",
    ] {
        let repository = TestRepository::new("notes.txt", b"safe text", "text");
        repository.write_denylist(contents);
        let result = repository.scan();
        assert_eq!(status_code(&result), ERROR);
        assert!(diagnostic(&result).contains("review"));
    }
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
        "tests/email.txt",
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
fn bare_domain_findings_select_only_host_bytes() {
    let value = format!("portal.{}/docs", "private-corp.internal");
    let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    let host = format!("portal.{}", "private-corp.internal");
    assert!(
        manifest.contains(&format!(
            "selector = \"bytes:0-{}\"\nfingerprint = \"{}\"",
            host.len(),
            fingerprint(host.as_bytes())
        )),
        "bare-domain selector and fingerprint must cover only host bytes: {manifest}"
    );
}

#[test]
fn domain_fingerprint_matches_exact_selected_host_bytes() {
    let host = "Portal.Private-Corp.Internal";
    let value = format!("HTTPS://{host}/docs");
    let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    let start = value.find(host).expect("host should exist");
    assert!(
        manifest.contains(&format!(
            "selector = \"bytes:{start}-{}\"\nfingerprint = \"{}\"",
            start + host.len(),
            fingerprint(host.as_bytes())
        )),
        "fingerprint must cover the exact selected bytes without case normalization: {manifest}"
    );
}

#[test]
fn bare_domain_detector_does_not_match_source_member_prefixes() {
    let contents = b"result.contains(value); output.status.code(); document.body.append(node);\nexample.com.evil\n";
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
fn domain_detector_rejects_template_path_and_code_member_false_positives() {
    for (path, contents) in [
        ("main.rs", ["format!(\"http", "://{}\");"].concat()),
        ("build.rs", ["let path = \"build", ".rs\";"].concat()),
        ("platform.rs", ["let x = prediction", ".name;"].concat()),
        (
            "app.rs",
            ["let x = state.settings.ec", ".partners;"].concat(),
        ),
    ] {
        let repository = TestRepository::new(path, contents.as_bytes(), "text");
        let result = repository.scan();
        assert_eq!(
            status_code(&result),
            SUCCESS,
            "{path}: {}",
            diagnostic(&result)
        );
    }
}

#[test]
fn domain_detector_rejects_non_code_paths_and_markup_tokens_but_keeps_link_hosts() {
    for (path, contents, references) in [
        (
            "docs/README.md",
            ["See build", ".rs and `CONTRIBUTING", ".md`."].concat(),
            vec![
                ["crates/example/build", ".rs"].concat(),
                "CONTRIBUTING.md".to_owned(),
            ],
        ),
        (
            "docs/README.md",
            ["[guide](getting-started", ".md) and configuration", ".md"].concat(),
            vec![
                ["docs/guide/getting-started", ".md"].concat(),
                ["docs/guide/configuration", ".md"].concat(),
            ],
        ),
        (
            ".dockerignore",
            ["dist/cache", ".name\n"].concat(),
            vec![["dist/cache", ".name"].concat()],
        ),
        (
            "config.txt",
            ["relative/path", ".name"].concat(),
            vec![["relative/path", ".name"].concat()],
        ),
        (
            "script.sh",
            ["# generated path build", ".rs\n"].concat(),
            vec![["build", ".rs"].concat()],
        ),
    ] {
        let repository = TestRepository::new(path, contents.as_bytes(), "text");
        for reference in references {
            repository.add_text(&reference, "no findings\n");
        }
        let result = repository.scan();
        assert_eq!(
            status_code(&result),
            SUCCESS,
            "{path}: {}",
            diagnostic(&result)
        );
    }
    let project_host = ["getpurpose", ".ai"].concat();
    let link = format!("[purpose](https://{project_host}/docs)");
    assert_detected("README.md", link.as_bytes(), "text", "domain");
}

#[test]
fn domain_detector_uses_repository_paths_instead_of_suffix_or_markup_suppression() {
    let service_host = ["service.example", ".rs"].concat();
    let project_host = ["getpurpose", ".ai"].concat();
    let untracked = ["untracked-guide", ".md"].concat();
    let configuration = ["docs/configuration", ".md"].concat();
    let contents = format!(
        "README.md `build.rs` [configuration]({configuration}) CONTRIBUTING.md\n\
         `{service_host}`\n\
         {untracked}\n\
         ```text\n{project_host}\n```\n\
         `{project_host}` and {project_host}\n"
    );
    let repository = TestRepository::new("README.md", contents.as_bytes(), "text");
    for path in ["build.rs", configuration.as_str(), "CONTRIBUTING.md"] {
        repository.add_text(path, "no findings\n");
    }

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert_eq!(
        manifest.matches("detector = \"domain\"").count(),
        5,
        "two non-path public hosts and all three project hosts must remain findings: {manifest}"
    );
    assert_eq!(
        manifest
            .matches(&fingerprint(project_host.as_bytes()))
            .count(),
        3,
        "Markdown markup must not suppress project domains: {manifest}"
    );
    assert!(
        manifest.contains(&fingerprint(service_host.as_bytes())),
        "source-looking public suffix must remain detectable when it is not a repository path: {manifest}"
    );
    assert!(
        manifest.contains(&fingerprint(untracked.as_bytes())),
        "a source-like suffix is not repository-path evidence by itself: {manifest}"
    );
}

#[test]
fn domain_detector_resolves_root_and_current_relative_path_tokens() {
    let dot_md = ".md";
    let dot_rs = ".rs";
    for (path, contents, referenced_paths) in [
        (
            ["README", dot_md].concat(),
            format!("See README{dot_md} and `CONTRIBUTING{dot_md}`.\n"),
            vec![["CONTRIBUTING", dot_md].concat()],
        ),
        (
            ["build", dot_rs].concat(),
            format!("let path = \"build{dot_rs}\";\n"),
            Vec::new(),
        ),
        (
            ["docs/configuration", dot_md].concat(),
            format!("[start](getting-started{dot_md}) and ../CONTRIBUTING{dot_md}\n"),
            vec![
                ["docs/getting-started", dot_md].concat(),
                ["CONTRIBUTING", dot_md].concat(),
            ],
        ),
        (
            ".gitignore".to_owned(),
            format!("docs/configuration{dot_md}\n"),
            vec![["docs/configuration", dot_md].concat()],
        ),
        (
            "script.sh".to_owned(),
            format!("cat README{dot_md}\n"),
            vec![["README", dot_md].concat()],
        ),
    ] {
        let repository = TestRepository::new(&path, contents.as_bytes(), "text");
        for referenced_path in referenced_paths {
            repository.add_text(&referenced_path, "no findings\n");
        }

        let result = repository.scan();

        assert_eq!(
            status_code(&result),
            SUCCESS,
            "{path}: {}",
            diagnostic(&result)
        );
    }
}

#[test]
fn source_domain_context_persists_across_lines_and_rejects_members() {
    let member_one = ["prediction", ".name"].concat();
    let member_two = ["state.settings.ec", ".partners"].concat();
    let member_three = ["document", ".body"].concat();
    let member_four = ["result", ".contains"].concat();
    let host = ["service.example", ".rs"].concat();
    for (path, contents) in [
        (
            "main.rs",
            format!(
                "let plain = {member_one};\nlet nested = {member_two};\nlet body = {member_three};\nlet result = {member_four}(value);\nlet host = \"\n{host}\";\n// {host}\n"
            ),
        ),
        (
            "app.js",
            format!(
                "const plain = {member_one};\nconst nested = {member_two};\nconst body = {member_three};\nconst result = {member_four}(value);\nconst host = `\n{host}`;\n// {host}\n"
            ),
        ),
    ] {
        let repository = TestRepository::new(path, contents.as_bytes(), "text");

        let result = repository.bootstrap();

        assert_eq!(
            status_code(&result),
            SUCCESS,
            "{path}: {}",
            diagnostic(&result)
        );
        let manifest = fs::read_to_string(
            repository
                .path()
                .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
        )
        .expect("should read bootstrap manifest");
        assert_eq!(
            manifest.matches("detector = \"domain\"").count(),
            2,
            "only the string and comment hosts must be findings: {manifest}"
        );
    }
}

#[test]
fn source_member_evidence_does_not_suppress_documented_host_shaped_tokens() {
    let member_one = ["prediction", ".name"].concat();
    let member_two = ["state.settings.ec", ".partners"].concat();
    let host = ["service.example", ".rs"].concat();
    let contents = format!("`{member_one}` and `{member_two}` are members; `{host}` is a host.\n");
    let repository = TestRepository::new("README.md", contents.as_bytes(), "text");
    repository.add_text(
        "src/main.rs",
        &format!("let first = {member_one};\nlet second = {member_two};\n"),
    );

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert_eq!(
        manifest.matches("detector = \"domain\"").count(),
        3,
        "source expressions must not suppress separate README occurrences: {manifest}"
    );
    assert!(manifest.contains(&fingerprint(host.as_bytes())));
}

#[test]
fn source_member_suppression_is_occurrence_specific() {
    let host = ["service.example", ".rs"].concat();
    let readme = format!("{host} in prose, `{host}` in code markup.\n");
    let source = format!("let member = {host};\nlet string = \"{host}\";\n// {host}\n");
    let repository = TestRepository::new("README.md", readme.as_bytes(), "text");
    repository.add_text("src/main.rs", &source);

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert_eq!(
        manifest.matches("detector = \"domain\"").count(),
        4,
        "README prose/markup and source string/comment must remain findings; only the expression is suppressed: {manifest}"
    );
}

#[test]
fn invalid_url_templates_do_not_create_domain_findings() {
    let repository = TestRepository::new("main.rs", b"let url = \"http://{\";\n", "text");

    let result = repository.scan();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
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
        "tests/email.txt",
        value.as_bytes(),
        "text",
        "email",
        &value,
        "historical_example",
    );
}

#[test]
fn email_detector_ignores_url_userinfo_and_at_sign_filenames() {
    let value = format!("https://user:pass@portal.{}", "private-corp.internal");
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

    let path_email = format!(
        "https://cdn.{}/{}@{}",
        "private-corp.internal", "banner", "2x.jpg"
    );
    let repository = TestRepository::new("notes.txt", path_email.as_bytes(), "text");
    assert_eq!(status_code(&repository.bootstrap()), SUCCESS);
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert!(!manifest.contains("detector = \"email\""));
}

#[test]
fn url_authority_overlap_does_not_hide_query_or_fragment_findings() {
    let value = format!(
        "https://example.com/path/{0}?host={0} https://example.com/#contact=person@{0}",
        "private-corp.internal"
    );
    let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");
    assert_eq!(status_code(&repository.bootstrap()), SUCCESS);
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert_eq!(manifest.matches("detector = \"email\"").count(), 1);
    assert_eq!(manifest.matches("detector = \"domain\"").count(), 2);
}

#[test]
fn url_path_components_use_normal_domain_context_rules() {
    let value = "https://example.com/rust-lang/crates.io-index/CONTRIBUTING.md/function.prototype.name/gpt.rs/mod.rs/prebid.rs";
    let repository = TestRepository::new("notes.txt", value.as_bytes(), "text");
    assert_eq!(status_code(&repository.bootstrap()), SUCCESS);
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert_eq!(manifest.matches("detector = \"domain\"").count(), 0);
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
        "tests/fixture.toml",
        contents.as_bytes(),
        "text",
        "credential_shape",
        value,
        "hash_pinned_fake_credential_fixture",
    );
}

#[test]
fn credential_shape_detects_quoted_and_unquoted_digitless_values() {
    let key = ["pass", "word"].concat();
    for contents in [
        format!("{key}=abcdefghijklmnop\n"),
        format!("{key}='abcdefghijklmnop'\n"),
        format!("{key}=abcdefghijklmnop123\n"),
        format!("{key}=\"abcdefghijklmnop123\"\n"),
    ] {
        assert_detected(".env", contents.as_bytes(), "text", "credential_shape");
    }
}

#[test]
fn credential_shape_keeps_config_punctuation_values() {
    for value in [
        "abcdefghijkl::mn",
        "abcdefghijkl.mn",
        "abcdefghijkl-mn",
        "!abcdefghijklmnop",
        "abc!def@ghi$jklmnop",
    ] {
        let contents = format!("password={value}\n");
        for path in [".env", "notes.txt"] {
            let repository = TestRepository::new(path, contents.as_bytes(), "text");
            assert_eq!(status_code(&repository.bootstrap()), SUCCESS);
            let manifest = fs::read_to_string(
                repository
                    .path()
                    .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
            )
            .expect("should read bootstrap manifest");
            assert!(
                manifest.contains("detector = \"credential_shape\""),
                "punctuation-bearing credential must be retained in {path}: {manifest}"
            );
        }
    }
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
        "tests/fixture.toml",
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
fn lockfiles_scan_domains_outside_structural_fields_without_duplicate_structural_hosts() {
    let prose_host = ["service.example", ".rs"].concat();
    let registry_host = ["registry.private-corp", ".internal"].concat();
    for (path, contents) in [
        (
            "package-lock.json",
            format!(
                "{{\"description\":\"https://{prose_host}/private\",\"resolved\":\"https://{registry_host}/pkg\"}}"
            ),
        ),
        (
            "Cargo.lock",
            format!(
                "version = 3\ndescription = \"https://{prose_host}/private\"\nsource = \"https://{registry_host}/pkg\"\n"
            ),
        ),
    ] {
        let repository = TestRepository::new(path, contents.as_bytes(), "text");

        let result = repository.bootstrap();

        assert_eq!(
            status_code(&result),
            SUCCESS,
            "{path}: {}",
            diagnostic(&result)
        );
        let manifest = fs::read_to_string(
            repository
                .path()
                .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
        )
        .expect("should read bootstrap manifest");
        assert_eq!(
            manifest.matches("detector = \"domain\"").count(),
            1,
            "nonstructural fields must receive general domain scanning without duplicating the structured host: {manifest}"
        );
        assert_eq!(
            manifest.matches("detector = \"lockfile_field\"").count(),
            1,
            "the structural field must retain its exact lockfile finding: {manifest}"
        );
    }
}

#[test]
fn structured_lockfiles_fail_closed_on_non_string_and_duplicate_url_fields() {
    for contents in [
        "version = 3\n\"source\" = \"https://private-corp.internal/index\"\n",
        "version = 3\npackage = { source = \"https://private-corp.internal/index\" }\n",
        "version = 3\npackage.source = \"https://private-corp.internal/index\"\n",
    ] {
        assert_detected("Cargo.lock", contents.as_bytes(), "text", "lockfile_field");
    }
    for contents in [
        r#"{"resolved":null}"#,
        r#"{"resolved":{}}"#,
        r#"{"resolved":[]}"#,
        r#"{"resolved":42}"#,
        r#"{"resolved":"https://one.private-corp.internal","resolved":"https://two.private-corp.internal"}"#,
    ] {
        let repository = TestRepository::new("package-lock.json", contents.as_bytes(), "text");
        let result = repository.scan();
        assert_eq!(status_code(&result), ERROR, "{contents}");
    }
    for contents in [
        "version = 3\nsource = []\n",
        "version = 3\nsource = \"https://one.private-corp.internal\"\nsource = \"https://two.private-corp.internal\"\n",
    ] {
        let repository = TestRepository::new("Cargo.lock", contents.as_bytes(), "text");
        let result = repository.scan();
        assert_eq!(status_code(&result), ERROR, "{contents}");
    }
}

#[test]
fn cargo_lock_structural_fields_follow_toml_semantics() {
    let sensitive = "https://private-corp.internal/index";
    for contents in [
        format!("version = 3\n'source' = '{sensitive}'\n"),
        format!("version = 3\n\"source\" = \"{sensitive}\"\n"),
        format!("version = 3\npackage.\"source\" = \"{sensitive}\"\n"),
        format!("version = 3\npackage = {{ metadata = {{ source = \"{sensitive}\" }} }}\n"),
        format!("version = 3\n[[package]]\nsource = \"{sensitive}\"\n"),
    ] {
        assert_detected("Cargo.lock", contents.as_bytes(), "text", "lockfile_field");
    }
}

#[test]
fn cargo_lock_ignores_field_decoys_in_comments_and_strings() {
    for contents in [
        "version = 3\n# source = \"https://private-corp.internal/index\"\n",
        "version = 3\ndescription = 'source = \"https://private-corp.internal/index\"'\n",
        "version = 3\nlabel = \"source\"\ndescription = \"https://private-corp.internal/index\"\n",
    ] {
        let repository = TestRepository::new("Cargo.lock", contents.as_bytes(), "text");

        let result = repository.bootstrap();

        assert_eq!(
            status_code(&result),
            SUCCESS,
            "field-like TOML text must remain general scan input: {contents}: {}",
            diagnostic(&result)
        );
        let manifest = fs::read_to_string(
            repository
                .path()
                .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
        )
        .expect("should read bootstrap manifest");
        assert_eq!(manifest.matches("detector = \"domain\"").count(), 1);
        assert_eq!(manifest.matches("detector = \"lockfile_field\"").count(), 0);
    }
}

#[test]
fn cargo_lock_rejects_non_string_containers_and_duplicate_fields() {
    for contents in [
        "version = 3\nsource = []\n",
        "version = 3\nsource = {}\n",
        "version = 3\nsource = { url = \"https://private-corp.internal/index\" }\n",
        "version = 3\nsource = \"https://one.private-corp.internal\"\nsource = \"https://two.private-corp.internal\"\n",
        "version = 3\npackage = { source = \"https://one.private-corp.internal\", source = \"https://two.private-corp.internal\" }\n",
    ] {
        let repository = TestRepository::new("Cargo.lock", contents.as_bytes(), "text");

        let result = repository.scan();

        assert_eq!(
            status_code(&result),
            ERROR,
            "unsupported or ambiguous field must fail closed: {contents}"
        );
    }
}

#[test]
fn cargo_lock_structural_selector_uses_the_ast_value_span() {
    let sensitive = "https://private-corp.internal/index";
    let escaped = r#"https://private-corp.internal/\u0069ndex"#;
    for (contents, raw) in [
        (
            format!("version = 3\ndescription = \"{sensitive}\"\nsource = \"{sensitive}\"\n"),
            sensitive,
        ),
        (format!("version = 3\nsource = \"{escaped}\"\n"), escaped),
    ] {
        let repository = TestRepository::new("Cargo.lock", contents.as_bytes(), "text");

        let result = repository.bootstrap();

        assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
        let manifest = fs::read_to_string(
            repository
                .path()
                .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
        )
        .expect("should read bootstrap manifest");
        let start = contents
            .rfind(raw)
            .expect("structural raw value should exist");
        assert!(
            manifest.contains(&format!(
                "selector = \"bytes:{start}-{}\"\nfingerprint = \"{}\"",
                start + raw.len(),
                fingerprint(raw.as_bytes())
            )),
            "selector must identify and fingerprint exact raw string content: {manifest}"
        );
    }
}

#[test]
fn escaped_json_lockfield_fingerprints_the_exact_selected_bytes() {
    let raw = r#"https:\/\/user:secret@private-corp.internal\/pkg"#;
    let contents = format!(r#"{{"resolved":"{raw}"}}"#);
    let repository = TestRepository::new("package-lock.json", contents.as_bytes(), "text");
    let result = repository.bootstrap();
    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("manifest");
    assert!(
        manifest.contains(&fingerprint(raw.as_bytes())),
        "raw selected bytes must be fingerprinted: {manifest}"
    );
}

#[test]
fn media_metadata_detector_has_positive_and_typed_allowlisted_fixtures() {
    let value = format!("person@{}", "private-corp.internal");
    let contents = png_text_chunk("Author", &value);
    assert_detected("asset.png", &contents, "binary", "media_metadata");
    assert_allowlisted(
        "tests/asset.png",
        &contents,
        "binary",
        "media_metadata",
        &value,
        "historical_example",
    );
}

#[test]
fn compressed_png_metadata_fails_closed() {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    append_png_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
    let mut metadata = b"Comment\0\0".to_vec();
    metadata.extend_from_slice(&[0x78, 0x9c, 3, 0, 0, 0, 0, 1]);
    append_png_chunk(&mut png, b"zTXt", &metadata);
    append_png_chunk(&mut png, b"IEND", &[]);
    let repository = TestRepository::new("asset.png", &png, "binary");
    let result = repository.scan();
    assert_eq!(status_code(&result), ERROR);
    assert!(diagnostic(&result).contains("compressed PNG zTXt"));
}

#[test]
fn png_text_keywords_enforce_printable_latin1_spacing_grammar() {
    for keyword in [
        vec![1],
        vec![0x7f],
        vec![0x80],
        vec![0x9f],
        b" leading".to_vec(),
        b"trailing ".to_vec(),
        b"double  space".to_vec(),
        Vec::new(),
        vec![b'a'; 80],
    ] {
        let mut data = keyword;
        data.push(0);
        data.extend_from_slice(b"person@private-corp.internal");
        let png = png_with_chunk(b"tEXt", &data);
        assert_eq!(
            status_code(&TestRepository::new("asset.png", &png, "binary").scan()),
            ERROR
        );
    }
    let mut missing_separator = b"Keyword".to_vec();
    missing_separator.extend_from_slice(b"person@private-corp.internal");
    let png = png_with_chunk(b"tEXt", &missing_separator);
    assert_eq!(
        status_code(&TestRepository::new("asset.png", &png, "binary").scan()),
        ERROR
    );

    for keyword in [b"Single space".as_slice(), &[0xa1, b'K'][..]] {
        let mut data = keyword.to_vec();
        data.push(0);
        data.extend_from_slice(b"person@private-corp.internal");
        let png = png_with_chunk(b"tEXt", &data);
        let repository = TestRepository::new("asset.png", &png, "binary");
        assert_eq!(status_code(&repository.bootstrap()), SUCCESS);
        let manifest = fs::read_to_string(
            repository
                .path()
                .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
        )
        .expect("should read bootstrap manifest");
        assert!(manifest.contains("detector = \"media_metadata\""));
    }
}

#[test]
fn identical_media_and_non_metadata_binary_values_remain_distinct_occurrences() {
    let value = format!("person@{}", "private-corp.internal");
    let mut contents = png_text_chunk("Author", &value);
    contents.truncate(contents.len() - 12);
    let mut ancillary = vec![0];
    ancillary.extend_from_slice(value.as_bytes());
    append_png_chunk(&mut contents, b"vpAg", &ancillary);
    append_png_chunk(&mut contents, b"IEND", &[]);
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
fn real_domain_manifest_excludes_repository_member_and_path_tokens() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("tool should live below the repository root");
    let manifest_text = fs::read_to_string(crate_root.join("manifests/sensitive-allowlist.toml"))
        .expect("should read real sensitive manifest");
    let manifest: toml::Value =
        toml::from_str(&manifest_text).expect("real sensitive manifest should be TOML");
    let forbidden = [
        ["function.prototype", ".name"].concat(),
        ["prebid", ".rs"].concat(),
        ["ec", ".partners"].concat(),
        ["when", ".zone"].concat(),
        ["script", ".sh"].concat(),
        ["slot", ".id"].concat(),
        ["user", ".id"].concat(),
        ["synthetic", ".rs"].concat(),
        ["AuctionRequest", ".id"].concat(),
        ["onboarding", ".md"].concat(),
        ["site", ".page"].concat(),
        ["seatbid", ".seat"].concat(),
        ["mediaTypes.banner", ".name"].concat(),
    ];
    for exception in manifest["exceptions"]
        .as_array()
        .expect("manifest exceptions should be an array")
    {
        if exception["detector"].as_str() != Some("domain") {
            continue;
        }
        let path = exception["path"].as_str().expect("exception path");
        let selector = exception["selector"]
            .as_str()
            .expect("exception selector")
            .strip_prefix("bytes:")
            .expect("byte selector");
        let (start, end) = selector.split_once('-').expect("selector range");
        let bytes = fs::read(repository_root.join(path)).expect("selected path should exist");
        let selected = &bytes[start.parse::<usize>().expect("start offset")
            ..end.parse::<usize>().expect("end offset")];
        assert!(
            !forbidden.iter().any(|value| selected == value.as_bytes()),
            "repository member/path token must not be a domain finding: {path}:{selector}"
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

#[test]
fn retired_identifier_matching_trims_common_prose_and_markdown_punctuation() {
    let token = "OldSecretTerm";
    let punctuation = [
        '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{', '}', '<', '>', '"', '\'', '`', '*',
        '~', '|',
    ];
    let contents = punctuation
        .iter()
        .map(|boundary| format!("{boundary}{token}{boundary}"))
        .collect::<Vec<_>>()
        .join(" ");
    let repository = TestRepository::new("audit.md", contents.as_bytes(), "text");
    repository.write_denylist(&retired_record("identifier", token));

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert_eq!(
        manifest
            .matches("detector = \"retired_identifier\"")
            .count(),
        punctuation.len(),
        "every punctuation-delimited occurrence must hash-match case-insensitively: {manifest}"
    );
}

#[test]
fn retired_identifier_matches_paired_markdown_delimiters_without_joining() {
    let token = "OldSecretTerm";
    let contents = format!("**{token}** ~~{token}~~ |{token}| Old*SecretTerm");
    let repository = TestRepository::new("audit.md", contents.as_bytes(), "text");
    repository.write_denylist(&retired_record("identifier", token));

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    assert_eq!(
        manifest
            .matches("detector = \"retired_identifier\"")
            .count(),
        3,
        "paired Markdown delimiters must match without joining across punctuation: {manifest}"
    );
}

#[test]
fn binary_retired_selectors_preserve_original_byte_offsets_across_invalid_utf8() {
    let token = "OldSecretTerm";
    let mut contents = vec![0xff];
    contents.extend_from_slice(token.as_bytes());
    contents.push(0xfe);
    contents.extend_from_slice(token.as_bytes());
    let repository = TestRepository::new("archive.bin", &contents, "binary");
    repository.write_denylist(&retired_record("identifier", token));

    let result = repository.bootstrap();

    assert_eq!(status_code(&result), SUCCESS, "{}", diagnostic(&result));
    let manifest = fs::read_to_string(
        repository
            .path()
            .join("tools/docs-parity/manifests/sensitive-allowlist.toml"),
    )
    .expect("should read bootstrap manifest");
    let second_start = 1 + token.len() + 1;
    for start in [1, second_start] {
        assert!(
            manifest.contains(&format!(
                "selector = \"bytes:{start}-{}\"\nfingerprint = \"{}\"",
                start + token.len(),
                fingerprint(token.as_bytes())
            )),
            "retired selector must use original binary offsets: {manifest}"
        );
    }
}

fn retired_record(kind: &str, value: &str) -> String {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    format!(
        "version = 1\nreviewed = true\n\n[[identifiers]]\nkind = \"{kind}\"\nfingerprint = \"{}\"\nnormalized_length = {}\nword_count = {}\ncase_insensitive = true\nwhitespace_tolerant = {}\n",
        fingerprint(normalized.as_bytes()),
        normalized.len(),
        normalized.split_whitespace().count(),
        normalized.split_whitespace().count() > 1,
    )
}

#[test]
fn all_five_exception_classes_are_supported_with_narrow_detector_pairings() {
    let fixtures = [
        ("vendor_url", "domain", "notes.txt"),
        (
            "hash_pinned_fake_credential_fixture",
            "credential_shape",
            "tests/fixture.rs",
        ),
        ("historical_example", "email", "tests/archive.txt"),
        ("project_owned_public_domain", "domain", "notes.txt"),
    ];
    for (class, detector, path) in fixtures {
        let value = match (class, detector) {
            ("project_owned_public_domain", "domain") => {
                format!(
                    "https://{}/trusted-server/",
                    ["iabtechlab.github", ".io"].concat()
                )
            }
            (_, "domain") => format!("https://owned.{}", ["private-corp", ".internal"].concat()),
            (_, "credential_shape") => "fake-credential-value-123".to_owned(),
            (_, "email") => format!("archived@{}", ["private-corp", ".internal"].concat()),
            (_, "service_id") => "AbCdEf1234567890GhIj".to_owned(),
            _ => unreachable!("fixture detector should be known"),
        };
        let contents = match detector {
            "credential_shape" => format!("secret = \"{value}\""),
            "service_id" => format!("service_id = \"{value}\""),
            _ => value.clone(),
        };
        assert_allowlisted(path, contents.as_bytes(), "text", detector, &value, class);
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
    let deceptive = "https://notiabtechlab.com.io/path";
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
fn exception_classes_require_provable_finding_semantics() {
    let private = ["owned.private-corp", ".internal"].concat();
    let private_url = format!("https://{private}/path");
    for class in ["project_owned_public_domain", "historical_example"] {
        let repository = TestRepository::new("notes.txt", private_url.as_bytes(), "text");
        repository.write_allowlist(&exception_for_contents(
            class,
            "notes.txt",
            "domain",
            private_url.as_bytes(),
            &private,
            "2099-01-01T00:00:00Z",
        ));
        let result = repository.scan();
        assert_eq!(
            status_code(&result),
            ERROR,
            "{class} must reject a generic vendor host"
        );
    }

    let owned = ["iabtechlab.github", ".io"].concat();
    let owned_url = format!("https://{owned}/trusted-server/");
    let repository = TestRepository::new("notes.txt", owned_url.as_bytes(), "text");
    repository.write_allowlist(&exception_for_contents(
        "vendor_url",
        "notes.txt",
        "domain",
        owned_url.as_bytes(),
        &owned,
        "2099-01-01T00:00:00Z",
    ));
    assert_eq!(
        status_code(&repository.scan()),
        ERROR,
        "project-owned hosts must not be mislabeled as vendors"
    );

    let credential = "abcdefghijklmnop";
    let contents = format!("{}word={credential}\n", "pass");
    let repository = TestRepository::new("notes.txt", contents.as_bytes(), "text");
    repository.write_allowlist(&exception_for_contents(
        "hash_pinned_fake_credential_fixture",
        "notes.txt",
        "credential_shape",
        contents.as_bytes(),
        credential,
        "2099-01-01T00:00:00Z",
    ));
    assert_eq!(
        status_code(&repository.scan()),
        ERROR,
        "fixture credentials require fixture-path evidence"
    );

    let binary_value = format!("historical@{}", ["private-corp", ".internal"].concat());
    let mut binary = vec![0];
    binary.extend_from_slice(binary_value.as_bytes());
    binary.push(0);
    let repository = TestRepository::new("archive.bin", &binary, "binary");
    repository.write_allowlist(&exception_for_contents(
        "historical_example",
        "archive.bin",
        "binary_string",
        &binary,
        &binary_value,
        "2099-01-01T00:00:00Z",
    ));
    assert_eq!(
        status_code(&repository.scan()),
        ERROR,
        "historical binary records require the approved artifact policy"
    );
}

#[test]
fn fake_credential_class_requires_exact_synthetic_evidence() {
    for (path, value) in [
        (".env", "latest-production-secret-123"),
        ("docs/setup.md", "latest-production-secret-123"),
        ("docs/setup.md", "contest-production-secret-123"),
        ("docs/setup.md", "exampled-production-secret-123"),
    ] {
        let contents = format!("password={value}\n");
        let repository = TestRepository::new(path, contents.as_bytes(), "text");
        repository.write_allowlist(&exception_for_contents(
            "hash_pinned_fake_credential_fixture",
            path,
            "credential_shape",
            contents.as_bytes(),
            value,
            "2099-01-01T00:00:00Z",
        ));
        assert_eq!(
            status_code(&repository.scan()),
            ERROR,
            "arbitrary production-looking credential must not qualify in {path}"
        );
    }

    for (path, value) in [
        ("tests/fixture.env", "abcdefghijklmnop123"),
        ("fixtures/example.env", "abcdefghijklmnop456"),
        ("config.example.toml", "abcdefghijklmnop789"),
    ] {
        let contents = format!("password={value}\n");
        assert_allowlisted(
            path,
            contents.as_bytes(),
            "text",
            "credential_shape",
            value,
            "hash_pinned_fake_credential_fixture",
        );
    }
}

#[test]
fn exception_class_detector_cross_misuse_is_rejected() {
    let domain = format!("service.{}", ["private-corp", ".internal"].concat());
    let domain_url = format!("https://{domain}/path");
    for class in ["hash_pinned_fake_credential_fixture", "service_id"] {
        let repository = TestRepository::new("notes.txt", domain_url.as_bytes(), "text");
        repository.write_allowlist(&exception_for_contents(
            class,
            "notes.txt",
            "domain",
            domain_url.as_bytes(),
            &domain,
            "2099-01-01T00:00:00Z",
        ));
        assert_eq!(
            status_code(&repository.scan()),
            ERROR,
            "{class}/domain must fail"
        );
    }

    let credential = "fake-credential-value-123";
    let credential_text = format!("secret=\"{credential}\"\n");
    for class in ["vendor_url", "project_owned_public_domain"] {
        let repository =
            TestRepository::new("tests/fixture.txt", credential_text.as_bytes(), "text");
        repository.write_allowlist(&exception_for_contents(
            class,
            "tests/fixture.txt",
            "credential_shape",
            credential_text.as_bytes(),
            credential,
            "2099-01-01T00:00:00Z",
        ));
        assert_eq!(
            status_code(&repository.scan()),
            ERROR,
            "{class}/credential_shape must fail"
        );
    }

    let lock_value = format!(
        "https://{}/pkg",
        ["registry.private-corp", ".internal"].concat()
    );
    let lockfile = format!("{{\"resolved\":\"{lock_value}\"}}");
    let repository = TestRepository::new("package-lock.json", lockfile.as_bytes(), "text");
    repository.write_allowlist(&exception_for_contents(
        "historical_example",
        "package-lock.json",
        "lockfile_field",
        lockfile.as_bytes(),
        &lock_value,
        "2099-01-01T00:00:00Z",
    ));
    assert_eq!(
        status_code(&repository.scan()),
        ERROR,
        "historical lockfile fields must fail"
    );
}

#[test]
fn identical_findings_require_distinct_occurrences_and_moving_one_invalidates_scope() {
    let value = format!("https://duplicate.{}", "private-corp.internal/path");
    let contents = format!("{value}\n{value}\n");
    let repository = TestRepository::new("notes.txt", contents.as_bytes(), "text");
    let host = governed_match("domain", &value);
    let host_offset = value.find(&host).expect("host");
    let first = occurrence_exception(
        "vendor_url",
        "notes.txt",
        "domain",
        &host,
        host_offset,
        host_offset + host.len(),
    );
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
            &host,
            second_start + host_offset,
            second_start + host_offset + host.len(),
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
    append_png_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
    append_png_chunk(&mut png, b"tEXt", &data);
    append_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn png_with_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    append_png_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
    append_png_chunk(&mut png, kind, data);
    append_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn append_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(u32::try_from(data.len()).expect("chunk should fit")).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = u32::MAX;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    png.extend_from_slice(&(!crc).to_be_bytes());
}
