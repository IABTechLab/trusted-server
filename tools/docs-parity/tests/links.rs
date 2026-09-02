use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use docs_parity::markdown::{
    ExternalException, ExternalRequest, ExternalResponse, ExternalTransport, LinkSource,
    LinkSourceSet, Sleeper, check_external_links, check_local_links,
};
use tempfile::TempDir;

const SUCCESS: i32 = 0;
const ERROR: i32 = 2;

struct PublicationRepository {
    directory: TempDir,
}

impl PublicationRepository {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("should create publication repository");
        run_git(directory.path(), &["init", "--quiet"]);
        let files = [
            ("README.md", "# Repository\n[public guide](/guide/)\n"),
            (
                "docs/.vitepress/config.mts",
                "export default { srcExclude: ['internal/**'], themeConfig: { nav: [{ link: '/' }] } }\n",
            ),
            ("docs/index.md", "# Home\n[guide](/guide/)\n"),
            (
                "docs/guide/index.md",
                "# Guide\n## Flow\n```mermaid\ngraph TD\n```\n",
            ),
            (
                "docs/internal/note.md",
                "# Internal\n[repository](/README.md)\n",
            ),
            (
                "tools/docs-parity/manifests/pages.toml",
                concat!(
                    "version = 1\nreviewed = true\nsite_root = \"docs\"\n",
                    "vitepress_config = \"docs/.vitepress/config.mts\"\n\n",
                    "[[pages]]\npath = \"docs/index.md\"\nroute = \"/\"\nnavigation = true\n\n",
                    "[[pages]]\npath = \"docs/guide/index.md\"\nroute = \"/guide/\"\nnavigation = false\n",
                ),
            ),
            (
                "tools/docs-parity/manifests/diagrams.toml",
                concat!(
                    "version = 1\nreviewed = true\n\n",
                    "[[diagrams]]\npath = \"docs/guide/index.md\"\nselector = \"mermaid:1\"\n",
                    "prose_anchor = \"flow\"\nowner = \"docs-team\"\n",
                ),
            ),
            (
                "tools/docs-parity/manifests/orphans.toml",
                "version = 1\nreviewed = true\n",
            ),
        ];
        for (path, contents) in files {
            write_file(directory.path(), path, contents);
        }
        let all_paths = [
            "README.md",
            "docs/.vitepress/config.mts",
            "docs/guide/index.md",
            "docs/index.md",
            "docs/internal/note.md",
            "tools/docs-parity/manifests/diagrams.toml",
            "tools/docs-parity/manifests/maintained-sources.toml",
            "tools/docs-parity/manifests/orphans.toml",
            "tools/docs-parity/manifests/pages.toml",
            "tools/docs-parity/manifests/tracked-files.toml",
        ];
        let mut tracked = "version = 1\nmax_text_bytes = 4194304\nreviewed = true\n".to_owned();
        let mut maintained = "version = 1\nreviewed = true\n".to_owned();
        for path in all_paths {
            tracked.push_str(&format!(
                "\n[[files]]\npath = \"{path}\"\nkind = \"text\"\n"
            ));
            let include = path.ends_with(".md");
            maintained.push_str(&format!(
                "\n[[sources]]\npath = \"{path}\"\nmode = \"whole\"\ndisposition = \"{}\"\n{}",
                if include { "include" } else { "exclude" },
                if include {
                    ""
                } else {
                    "exclude_kind = \"non_documentation\"\n"
                }
            ));
        }
        write_file(
            directory.path(),
            "tools/docs-parity/manifests/tracked-files.toml",
            &tracked,
        );
        write_file(
            directory.path(),
            "tools/docs-parity/manifests/maintained-sources.toml",
            &maintained,
        );
        run_git(directory.path(), &["add", "--all"]);
        Self { directory }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn check(&self) -> Output {
        Command::new(binary())
            .current_dir(self.path())
            .args(["links", "--local", "--check"])
            .output()
            .expect("should execute local links")
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_docs-parity"))
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("path should have a parent"))
        .expect("should create fixture parent");
    fs::write(path, contents).expect("should write fixture");
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

fn source(path: &str, set: LinkSourceSet, markdown: &str) -> LinkSource {
    LinkSource {
        path: path.to_owned(),
        set,
        markdown: markdown.to_owned(),
    }
}

fn local_error(sources: &[LinkSource]) -> String {
    format!(
        "{:?}",
        check_local_links(sources, &[], &[]).expect_err("links should fail")
    )
}

#[test]
fn every_active_source_set_checks_missing_local_targets() {
    for set in [
        LinkSourceSet::Repository,
        LinkSourceSet::MaintainedInternal,
        LinkSourceSet::Public,
    ] {
        let missing = format!("missing.{}", "md");
        let markdown = format!("[missing]({missing})\n");
        let sources = [source("docs/source.md", set, &markdown)];
        assert!(
            local_error(&sources).contains(&missing),
            "dead link should fail in {set:?}"
        );
    }
}

#[test]
fn semantic_markdown_links_resolve_files_queries_and_vitepress_routes() {
    let target = format!("target.{}", "md");
    let markdown = format!(
        "# Guide\n[relative]({target}?mode=full#two)\n[route](/guide/target#explicit)\n`[not a link](missing.{})`\n```md\n[also not a link](missing.{})\n```\n",
        "md", "md"
    );
    let sources = [
        source("docs/guide/index.md", LinkSourceSet::Public, &markdown),
        source(
            "docs/guide/target.md",
            LinkSourceSet::Public,
            "# Two\n# Two\n<a id=\"explicit\"></a>\n",
        ),
    ];

    check_local_links(&sources, &[], &[]).expect("semantic links should resolve");
}

#[test]
fn root_relative_repository_links_resolve_from_the_repository_root() {
    let sources = [
        source(
            ".claude/commands/check.md",
            LinkSourceSet::Repository,
            "[rules](/CLAUDE.md#build--test-commands)\n",
        ),
        source(
            "CLAUDE.md",
            LinkSourceSet::Repository,
            "# Build and Test Commands {#build--test-commands}\n",
        ),
        source(
            "docs/internal/note.md",
            LinkSourceSet::MaintainedInternal,
            "[public route](/guide/)\n",
        ),
        source("docs/guide/index.md", LinkSourceSet::Public, "# Guide\n"),
    ];

    check_local_links(&sources, &[], &[]).expect("repository-root link should resolve");
}

#[test]
fn anchors_use_duplicate_slugs_explicit_ids_and_strict_percent_decoding() {
    let target_name = format!("target.{}", "md");
    let target = source(
        "docs/guide/target.md",
        LinkSourceSet::Public,
        "# Same heading\n# Same heading\n# Build & Deployment Errors\n<a id=\"named-anchor\"></a>\n",
    );
    let valid = source(
        "docs/guide/source.md",
        LinkSourceSet::Public,
        &format!(
            "[duplicate]({target_name}#same-heading-1) [encoded]({target_name}#named%2Danchor) [punctuation]({target_name}#build--deployment-errors)\n"
        ),
    );
    check_local_links(&[valid, target.clone()], &[], &[])
        .expect("duplicate and explicit anchors should resolve");

    let missing = source(
        "docs/guide/source.md",
        LinkSourceSet::Public,
        &format!("[bad]({target_name}#same-heading-2)\n"),
    );
    assert!(local_error(&[missing, target.clone()]).contains("same-heading-2"));

    let malformed = source(
        "docs/guide/source.md",
        LinkSourceSet::Public,
        &format!("[bad]({target_name}#bad%2)\n"),
    );
    assert!(local_error(&[malformed, target]).contains("percent"));
}

#[test]
fn setext_headings_are_available_as_semantic_anchors() {
    let target_name = format!("target.{}", "md");
    let sources = [
        source(
            "docs/guide/source.md",
            LinkSourceSet::Public,
            &format!("[target]({target_name}#setext-heading)\n"),
        ),
        source(
            "docs/guide/target.md",
            LinkSourceSet::Public,
            "Setext Heading\n==============\n",
        ),
    ];

    check_local_links(&sources, &[], &[]).expect("setext heading should resolve");
}

#[test]
fn tombstones_orphans_and_excluded_source_links_fail_closed() {
    let public = source(
        "docs/index.md",
        LinkSourceSet::Public,
        "[gone](/guide/retired) [source](../private.md)\n",
    );
    let private = source("private.md", LinkSourceSet::Repository, "# Not published\n");
    let error = local_error(&[public, private]);
    assert!(error.contains("retired") || error.contains("private"));

    let orphan = source(
        "docs/guide/orphan.md",
        LinkSourceSet::Public,
        "# Unlisted\n",
    );
    let error = format!(
        "{:?}",
        check_local_links(&[orphan], &["docs/guide/listed.md".to_owned()], &[])
            .expect_err("unlisted page should fail")
    );
    assert!(error.contains("orphan"));
}

#[derive(Default)]
struct FakeTransport {
    responses: VecDeque<ExternalResponse>,
    requests: Vec<ExternalRequest>,
}

impl ExternalTransport for FakeTransport {
    fn send(&mut self, request: &ExternalRequest) -> Result<ExternalResponse, String> {
        self.requests.push(request.clone());
        self.responses
            .pop_front()
            .ok_or_else(|| "no scripted response".to_owned())
    }
}

#[derive(Default)]
struct FakeSleeper {
    delays: Vec<u64>,
}

impl Sleeper for FakeSleeper {
    fn sleep_seconds(&mut self, seconds: u64) {
        self.delays.push(seconds);
    }
}

fn response(status: u16, location: Option<&str>, retry_after: Option<&str>) -> ExternalResponse {
    let mut headers = BTreeMap::new();
    if let Some(location) = location {
        headers.insert("location".to_owned(), location.to_owned());
    }
    if let Some(retry_after) = retry_after {
        headers.insert("retry-after".to_owned(), retry_after.to_owned());
    }
    ExternalResponse { status, headers }
}

fn external_error(
    markdown: &str,
    responses: Vec<ExternalResponse>,
) -> (String, FakeTransport, FakeSleeper) {
    let sources = [source("README.md", LinkSourceSet::Repository, markdown)];
    let mut transport = FakeTransport {
        responses: responses.into(),
        requests: Vec::new(),
    };
    let mut sleeper = FakeSleeper::default();
    let error = format!(
        "{:?}",
        check_external_links(&sources, &[], 1_788_220_800, &mut transport, &mut sleeper)
            .expect_err("external links should fail")
    );
    (error, transport, sleeper)
}

#[test]
fn external_checker_uses_head_then_get_only_when_head_is_unsupported() {
    let sources = [source(
        "README.md",
        LinkSourceSet::Repository,
        "[site](https://docs.example.com/page)\n",
    )];
    let mut transport = FakeTransport {
        responses: vec![response(405, None, None), response(200, None, None)].into(),
        requests: Vec::new(),
    };
    let mut sleeper = FakeSleeper::default();

    check_external_links(&sources, &[], 1_788_220_800, &mut transport, &mut sleeper)
        .expect("GET fallback should pass");
    assert_eq!(transport.requests.len(), 2);
    assert_eq!(transport.requests[0].method, "HEAD");
    assert_eq!(transport.requests[1].method, "GET");
}

#[test]
fn external_checker_bounds_redirects_and_rejects_loops_and_credentials() {
    let (loop_error, _, _) = external_error(
        "[site](https://docs.example.com/a)\n",
        vec![
            response(302, Some("https://docs.example.com/b"), None),
            response(302, Some("https://docs.example.com/a"), None),
        ],
    );
    assert!(loop_error.contains("loop"));

    let redirects = (0..6)
        .map(|index| {
            response(
                302,
                Some(&format!("https://docs.example.com/{index}")),
                None,
            )
        })
        .collect();
    let (depth_error, _, _) = external_error("[site](https://docs.example.com/start)\n", redirects);
    assert!(depth_error.contains("redirect"));

    let (credential_error, transport, _) =
        external_error("[site](https://user:password@docs.example.com/)\n", vec![]);
    assert!(credential_error.contains("credential"));
    assert!(transport.requests.is_empty());
}

#[test]
fn external_checker_retries_at_most_three_times_with_bounded_delays() {
    let (error, transport, sleeper) = external_error(
        "[site](https://docs.example.com/)\n",
        vec![
            response(429, None, Some("5")),
            response(500, None, Some("invalid")),
            response(503, None, Some("31")),
        ],
    );
    assert!(error.contains("attempt"));
    assert_eq!(transport.requests.len(), 3);
    assert_eq!(sleeper.delays, vec![5, 2]);
}

#[test]
fn external_checker_covers_all_sets_and_enforces_request_bounds() {
    let sources = [
        source(
            "README.md",
            LinkSourceSet::Repository,
            "[a](https://a.example.com/)\n",
        ),
        source(
            "docs/internal/note.md",
            LinkSourceSet::MaintainedInternal,
            "[b](https://b.example.com/)\n",
        ),
        source(
            "docs/index.md",
            LinkSourceSet::Public,
            "[c](https://c.example.com/)\n",
        ),
    ];
    let mut transport = FakeTransport {
        responses: vec![
            response(200, None, None),
            response(200, None, None),
            response(200, None, None),
        ]
        .into(),
        requests: Vec::new(),
    };
    let mut sleeper = FakeSleeper::default();

    check_external_links(&sources, &[], 0, &mut transport, &mut sleeper)
        .expect("all source sets should pass");
    assert_eq!(transport.requests.len(), 3);
    assert!(transport.requests.iter().all(|request| {
        request.timeout_seconds == 15 && request.maximum_body_bytes == 64 * 1024
    }));
}

#[test]
fn valid_http_date_retry_after_is_honored_only_within_the_bound() {
    let sources = [source(
        "README.md",
        LinkSourceSet::Repository,
        "[site](https://docs.example.com/)\n",
    )];
    let mut transport = FakeTransport {
        responses: vec![
            response(429, None, Some("Thu, 01 Jan 1970 00:00:05 GMT")),
            response(200, None, None),
        ]
        .into(),
        requests: Vec::new(),
    };
    let mut sleeper = FakeSleeper::default();

    check_external_links(&sources, &[], 0, &mut transport, &mut sleeper)
        .expect("bounded HTTP-date retry should pass");
    assert_eq!(sleeper.delays, vec![5]);
}

#[test]
fn external_exceptions_are_exact_typed_owned_and_expiring() {
    let sources = [source(
        "README.md",
        LinkSourceSet::Repository,
        "[site](https://docs.example.com/)\n",
    )];
    let exception = ExternalException {
        url: "https://docs.example.com/".to_owned(),
        owner: "docs-team".to_owned(),
        reason: "Host blocks automated HEAD and GET requests".to_owned(),
        expires_at: "2026-09-02T00:00:00Z".to_owned(),
    };
    let mut transport = FakeTransport::default();
    let mut sleeper = FakeSleeper::default();
    check_external_links(
        &sources,
        std::slice::from_ref(&exception),
        1_788_307_199,
        &mut transport,
        &mut sleeper,
    )
    .expect("active exact exception should pass");
    assert!(transport.requests.is_empty());

    let error = format!(
        "{:?}",
        check_external_links(
            &sources,
            &[exception],
            1_788_307_200,
            &mut transport,
            &mut sleeper,
        )
        .expect_err("exception should fail at expiry")
    );
    assert!(error.contains("expired"));
}

#[test]
fn publication_records_check_pages_navigation_reachability_and_diagrams() {
    let repository = PublicationRepository::new();
    let clean = repository.check();
    assert_eq!(
        status_code(&clean),
        SUCCESS,
        "complete publication fixture should pass: {}",
        diagnostic(&clean)
    );

    let pages_path = repository
        .path()
        .join("tools/docs-parity/manifests/pages.toml");
    let pages = fs::read_to_string(&pages_path).expect("should read pages manifest");
    fs::write(
        &pages_path,
        pages.replacen("navigation = true", "navigation = false", 1),
    )
    .expect("should break navigation record");
    let navigation = repository.check();
    assert_eq!(status_code(&navigation), ERROR);
    assert!(diagnostic(&navigation).contains("navigation inventory mismatch"));
}

#[test]
fn unlisted_orphan_and_missing_diagram_prose_fail_closed() {
    let repository = PublicationRepository::new();
    let index_path = repository.path().join("docs/index.md");
    fs::write(&index_path, "# Home\n").expect("should remove guide reachability");
    let orphan = repository.check();
    assert_eq!(status_code(&orphan), ERROR);
    assert!(diagnostic(&orphan).contains("orphan inventory mismatch"));

    fs::write(&index_path, "# Home\n[guide](/guide/)\n").expect("should restore guide link");
    let diagram_path = repository
        .path()
        .join("tools/docs-parity/manifests/diagrams.toml");
    let diagrams = fs::read_to_string(&diagram_path).expect("should read diagrams manifest");
    fs::write(
        diagram_path,
        diagrams.replace("prose_anchor = \"flow\"", "prose_anchor = \"missing\""),
    )
    .expect("should break diagram prose record");
    let diagram = repository.check();
    assert_eq!(status_code(&diagram), ERROR);
    assert!(diagnostic(&diagram).contains("missing prose anchor"));
}
