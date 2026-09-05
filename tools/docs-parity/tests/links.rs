use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use docs_parity::markdown::{
    CommandOutput, CommandRunner, CurlTransport, ExternalException, ExternalRequest,
    ExternalResponse, ExternalTransport, LinkSource, LinkSourceSet, ProcessCommandRunner, Sleeper,
    check_external_links, check_local_links,
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
                "export default { srcExclude: ['internal/**', 'private.md'], themeConfig: { nav: [{ link: '/' }] } }\n",
            ),
            ("docs/index.md", "# Home\n[guide](/guide/)\n"),
            ("docs/private.md", "# Private\n"),
            (
                "docs/guide/index.md",
                "# Guide\n## Flow\n```mermaid\ngraph TD\n```\n",
            ),
            (
                "docs/internal/note.md",
                "# Internal\n[repository](/README.md)\n",
            ),
            ("notes/included.md", "# Included repository note\n"),
            ("static/logo.png", "not really a png fixture"),
            (
                "tools/docs-parity/manifests/pages.toml",
                concat!(
                    "version = 1\nreviewed = true\nsite_root = \"docs\"\n",
                    "vitepress_config = \"docs/.vitepress/config.mts\"\n\n",
                    "[[pages]]\nkind = \"live\"\npath = \"docs/index.md\"\nroute = \"/\"\nnavigation = true\n\n",
                    "[[pages]]\nkind = \"live\"\npath = \"docs/guide/index.md\"\nroute = \"/guide/\"\nnavigation = false\n",
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
            "docs/private.md",
            "notes/included.md",
            "static/logo.png",
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
                "\n[[files]]\npath = \"{path}\"\nkind = \"{}\"\n",
                if path.ends_with(".png") {
                    "binary"
                } else {
                    "text"
                }
            ));
            if path.ends_with(".png") {
                continue;
            }
            let include = path.ends_with(".md") && path != "docs/private.md";
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
fn semantic_image_destinations_are_checked() {
    let sources = [source(
        "docs/guide/source.md",
        LinkSourceSet::Public,
        "![missing diagram](missing.png)\n",
    )];

    assert!(local_error(&sources).contains("missing.png"));
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
            "[rules](/CLAUDE.md#build-and-test-commands)\n",
        ),
        source(
            "CLAUDE.md",
            LinkSourceSet::Repository,
            "# Build and Test Commands\n",
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
fn public_links_reject_every_spelling_of_an_excluded_markdown_source() {
    for destination in ["private.md", "/docs/private.md", "/private"] {
        let repository = PublicationRepository::new();
        write_file(
            repository.path(),
            "docs/index.md",
            &format!("# Home\n[private]({destination})\n[guide](/guide/)\n"),
        );

        let result = repository.check();

        assert_eq!(status_code(&result), ERROR, "{destination} must fail");
        assert!(
            diagnostic(&result).contains("excluded source"),
            "{destination} must identify the excluded source: {}",
            diagnostic(&result)
        );
    }
}

#[test]
fn included_repository_and_binary_targets_remain_distinct_from_exclusions() {
    let repository = PublicationRepository::new();
    write_file(
        repository.path(),
        "README.md",
        concat!(
            "# Repository\n",
            "[included](notes/included.md)\n",
            "[binary](static/logo.png)\n",
            "[public](/guide/)\n",
        ),
    );

    let clean = repository.check();
    assert_eq!(
        status_code(&clean),
        SUCCESS,
        "included text and binary paths must resolve: {}",
        diagnostic(&clean)
    );

    write_file(
        repository.path(),
        "README.md",
        "# Repository\n[bad binary anchor](static/logo.png#bytes)\n[public](/guide/)\n",
    );
    let anchored_binary = repository.check();
    assert_eq!(status_code(&anchored_binary), ERROR);
    assert!(diagnostic(&anchored_binary).contains("non-Markdown target"));
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
            "[duplicate]({target_name}#same-heading-1) [encoded]({target_name}#named%2Danchor) [punctuation]({target_name}#build-deployment-errors)\n"
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
fn public_and_repository_heading_slugs_follow_their_renderers() {
    let public_name = format!("public.{}", "md");
    let public_target = source(
        "docs/guide/public.md",
        LinkSourceSet::Public,
        concat!(
            "# 123 Déjà  vu & API\n",
            "# **Formatted** `code` &amp; _text_ {#exact-id}\n",
            "# Repeat\n",
            "# Repeat\n",
        ),
    );
    let public_source = source(
        "docs/guide/source.md",
        LinkSourceSet::Public,
        &format!(
            "[normalized]({public_name}#_123-deja-vu-api)\n[explicit]({public_name}#exact-id)\n[duplicate]({public_name}#repeat-1)\n"
        ),
    );
    check_local_links(&[public_source, public_target], &[], &[])
        .expect("VitePress heading slugs should resolve");

    let repository_target = source(
        "notes/target.md",
        LinkSourceSet::Repository,
        "# Build & Deployment Errors\n# 123 Start\n",
    );
    let repository_source = source(
        "README.md",
        LinkSourceSet::Repository,
        concat!(
            "[GitHub punctuation](notes/target.md#build--deployment-errors)\n",
            "[GitHub digit](notes/target.md#123-start)\n",
        ),
    );
    check_local_links(&[repository_source, repository_target], &[], &[])
        .expect("GitHub heading slugs should resolve");
}

#[test]
fn vitepress_heading_text_ignores_image_alt_but_keeps_rendered_inline_text() {
    let target_name = format!("target.{}", "md");
    let target = source(
        "docs/guide/target.md",
        LinkSourceSet::Public,
        concat!(
            "# Before ![alt](https://example.invalid/image.png) After\n",
            "# **Bold** [linked](https://example.invalid/) `code` _after_\n",
        ),
    );
    let links = source(
        "docs/guide/source.md",
        LinkSourceSet::Public,
        &format!(
            "[image heading]({target_name}#before-after)\n[inline text]({target_name}#bold-linked-code-after)\n"
        ),
    );

    check_local_links(&[links, target], &[], &[])
        .expect("heading text should match the pinned VitePress renderer");

    let repository_target = source(
        "notes/target.md",
        LinkSourceSet::Repository,
        "# Before ![alt](https://example.invalid/image.png) After\n",
    );
    let repository_source = source(
        "README.md",
        LinkSourceSet::Repository,
        "[image heading](notes/target.md#before-alt-after)\n",
    );
    check_local_links(&[repository_source, repository_target], &[], &[])
        .expect("GitHub heading text should retain image alt text");
}

#[test]
fn vitepress_explicit_heading_ids_reject_collisions_without_auto_suffixing() {
    for markdown in ["# Foo\n# Bar {#foo}\n", "# Bar {#foo}\n# Baz {#foo}\n"] {
        let target = source("docs/guide/target.md", LinkSourceSet::Public, markdown);
        let error = local_error(&[target]);
        assert!(
            error.contains("duplicate explicit heading id"),
            "explicit collision must fail: {error}"
        );
    }

    let target_name = format!("target.{}", "md");
    let target = source(
        "docs/guide/target.md",
        LinkSourceSet::Public,
        "# Bar {#foo}\n# Foo\n",
    );
    let links = source(
        "docs/guide/source.md",
        LinkSourceSet::Public,
        &format!("[custom]({target_name}#foo) [auto]({target_name}#foo-1)\n"),
    );
    check_local_links(&[links, target], &[], &[])
        .expect("an auto heading after a custom ID should receive a suffix");
}

#[test]
fn invalid_explicit_heading_ids_fail_closed() {
    let target = source(
        "docs/guide/target.md",
        LinkSourceSet::Public,
        "# Unsafe {#two words}\n",
    );
    let error = local_error(&[target]);
    assert!(error.contains("explicit heading id"));
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
fn commonmark_events_cover_multiline_references_html_images_and_code() {
    let target = format!("target.{}", "md");
    let missing = format!("missing.{}", "md");
    let sources = [
        source(
            "docs/guide/source.md",
            LinkSourceSet::Public,
            &format!(
                concat!(
                    "[multiline](\n  {target}#target-heading\n)\n",
                    "[reference][target]\n\n[target]: {target}#html-anchor\n\n",
                    "<DIV\n  id=\"source-html\">\n",
                    "  <A HREF=\"{target}#target-heading\">HTML link</A>\n</DIV>\n",
                    "![image](../asset.png)\n<https://docs.example.com/path>\n",
                    "<!-- <a href=\"{missing}\" id=\"ignored\"> -->\n",
                    "    [indented code]({missing})\n",
                    "`[inline code]({missing})`\n",
                    "```md\n[code fence]({missing})\n```\n",
                ),
                target = target,
                missing = missing,
            ),
        ),
        source(
            "docs/guide/target.md",
            LinkSourceSet::Public,
            "# Target *heading*\n<span id=\"html-anchor\"></span>\n",
        ),
        source(
            "docs/asset.png",
            LinkSourceSet::Repository,
            "not parsed as an image fixture",
        ),
    ];

    check_local_links(&sources, &[], &[]).expect("CommonMark destinations should resolve");
}

#[test]
fn percent_decoding_rejects_invalid_utf8_and_residual_encoded_octets() {
    let target = format!("target.{}", "md");
    for fragment in ["%FF", "%252D"] {
        let sources = [
            source(
                "docs/guide/source.md",
                LinkSourceSet::Public,
                &format!("[bad]({target}#{fragment})\n"),
            ),
            source("docs/guide/target.md", LinkSourceSet::Public, "# Target\n"),
        ];
        let error = local_error(&sources);
        assert!(error.contains("percent"), "{fragment} must fail: {error}");
    }
}

#[test]
fn query_percent_encoding_is_validated_with_one_strict_decode() {
    let target = format!("target.{}", "md");
    let valid = [
        source(
            "docs/guide/source.md",
            LinkSourceSet::Public,
            &format!("[valid]({target}?next=%2Fguide%2F#target)\n"),
        ),
        source("docs/guide/target.md", LinkSourceSet::Public, "# Target\n"),
    ];
    check_local_links(&valid, &[], &[]).expect("one encoded query pass should be valid");

    for query in ["bad=%ZZ", "bad=%", "bad=%FF", "bad=%252F"] {
        let sources = [
            source(
                "docs/guide/source.md",
                LinkSourceSet::Public,
                &format!("[invalid]({target}?{query}#target)\n"),
            ),
            source("docs/guide/target.md", LinkSourceSet::Public, "# Target\n"),
        ];
        let error = local_error(&sources);
        assert!(error.contains("percent"), "{query} must fail: {error}");
    }
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
    ExternalResponse {
        status,
        headers,
        header_bytes: 0,
        body_bytes: 0,
    }
}

#[derive(Default)]
struct FakeCommandRunner {
    outputs: VecDeque<Result<CommandOutput, String>>,
    invocations: Vec<(String, Vec<String>, usize)>,
}

struct FileHeadCommandRunner {
    file_url: String,
    observed_header_copies: usize,
}

impl CommandRunner for FileHeadCommandRunner {
    fn run(
        &mut self,
        program: &str,
        arguments: &[String],
        maximum_output_bytes: usize,
    ) -> Result<CommandOutput, String> {
        if program != "curl"
            || arguments.first().map(String::as_str) != Some("--disable")
            || arguments.iter().any(|argument| argument == "--dump-header")
            || !arguments.iter().any(|argument| argument == "--head")
        {
            return Err("HEAD command framing is not isolated and singular".to_owned());
        }
        let mut local = arguments.to_vec();
        for option in ["--proto", "--proto-redir"] {
            let index = local
                .iter()
                .position(|argument| argument == option)
                .ok_or_else(|| format!("missing {option}"))?;
            local[index + 1] = "=file".to_owned();
        }
        let url_index = local
            .iter()
            .position(|argument| argument == "--url")
            .ok_or_else(|| "missing --url".to_owned())?;
        local[url_index + 1] = self.file_url.clone();

        let mut runner = ProcessCommandRunner;
        let actual = runner.run("curl", &local, maximum_output_bytes)?;
        let marker = b"\nDOCS_PARITY_COUNTS:";
        let marker_start = actual
            .stdout
            .windows(marker.len())
            .rposition(|window| window == marker)
            .ok_or_else(|| "file HEAD output has no count trailer".to_owned())?;
        if actual.stdout.get(marker_start + marker.len()..) != Some(b"0:0\n") {
            return Err("file HEAD output has unexpected write-out counts".to_owned());
        }
        let file_headers = &actual.stdout[..marker_start];
        self.observed_header_copies = file_headers
            .windows(b"Content-Length:".len())
            .filter(|window| *window == b"Content-Length:")
            .count();

        let mut headers = b"HTTP/1.1 200 OK\r\n".to_vec();
        headers.extend_from_slice(file_headers);
        if !headers.ends_with(b"\r\n\r\n") {
            headers.extend_from_slice(b"\r\n");
        }
        let mut stdout = headers.clone();
        stdout.extend_from_slice(format!("\nDOCS_PARITY_COUNTS:{}:0\n", headers.len()).as_bytes());
        Ok(CommandOutput {
            success: actual.success,
            status_code: actual.status_code,
            stdout,
        })
    }
}

impl CommandRunner for FakeCommandRunner {
    fn run(
        &mut self,
        program: &str,
        arguments: &[String],
        maximum_output_bytes: usize,
    ) -> Result<CommandOutput, String> {
        self.invocations
            .push((program.to_owned(), arguments.to_vec(), maximum_output_bytes));
        self.outputs
            .pop_front()
            .ok_or_else(|| "no scripted command output".to_owned())?
    }
}

fn curl_output(status: &str, headers: &[(&str, &str)], body: &[u8]) -> CommandOutput {
    let mut header = format!("HTTP/1.1 {status}\r\n").into_bytes();
    for (name, value) in headers {
        header.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    header.extend_from_slice(b"\r\n");
    let mut stdout = header.clone();
    stdout.extend_from_slice(body);
    stdout.extend_from_slice(
        format!("\nDOCS_PARITY_COUNTS:{}:{}\n", header.len(), body.len()).as_bytes(),
    );
    CommandOutput {
        success: true,
        status_code: Some(0),
        stdout,
    }
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
fn malformed_or_inconsistent_http_date_retry_after_uses_local_delay() {
    for value in [
        "Xxx, 01 Jan 1970 00:00:05 GMT",
        "Fri, 01 Jan 1970 00:00:05 GMT",
        "Thu, 1 Jan 1970 00:00:05 GMT",
        "Thu, 01 Jan 01970 00:00:05 GMT",
        "Thu, 01 Jan 1970 0:00:05 GMT",
    ] {
        let sources = [source(
            "README.md",
            LinkSourceSet::Repository,
            "[site](https://docs.example.com/)\n",
        )];
        let mut transport = FakeTransport {
            responses: vec![response(429, None, Some(value)), response(200, None, None)].into(),
            requests: Vec::new(),
        };
        let mut sleeper = FakeSleeper::default();

        check_external_links(&sources, &[], 0, &mut transport, &mut sleeper)
            .expect("malformed date should fall back to the local retry delay");
        assert_eq!(sleeper.delays, vec![1], "{value} must not be honored");
    }
}

#[test]
fn external_checker_accepts_relative_redirects_and_rejects_final_errors() {
    let sources = [source(
        "README.md",
        LinkSourceSet::Repository,
        "[site](https://docs.example.com/start)\n",
    )];
    let mut transport = FakeTransport {
        responses: vec![
            response(302, Some("/final"), None),
            response(204, None, None),
        ]
        .into(),
        requests: Vec::new(),
    };
    let mut sleeper = FakeSleeper::default();
    check_external_links(&sources, &[], 0, &mut transport, &mut sleeper)
        .expect("relative HTTPS redirect should pass");
    assert_eq!(transport.requests[1].url, "https://docs.example.com/final");

    let (error, transport, _) = external_error(
        "[site](https://docs.example.com/start)\n",
        vec![response(404, None, None)],
    );
    assert!(error.contains("final status 404"));
    assert_eq!(transport.requests.len(), 1);
}

#[test]
fn curl_transport_get_uses_exact_bounded_nonredirecting_arguments_and_counts_bytes() {
    let runner = FakeCommandRunner {
        outputs: vec![Ok(curl_output(
            "200 OK",
            &[("Content-Type", "text/plain")],
            b"body",
        ))]
        .into(),
        invocations: Vec::new(),
    };
    let mut transport = CurlTransport::new(runner);
    let request = ExternalRequest {
        method: "GET".to_owned(),
        url: "https://docs.example.com/path".to_owned(),
        timeout_seconds: 15,
        maximum_body_bytes: 64 * 1024,
    };

    let response = transport.send(&request).expect("curl output should parse");
    let runner = transport.into_runner();

    assert_eq!(response.status, 200);
    assert!(response.header_bytes > 0);
    assert_eq!(response.body_bytes, 4);
    assert_eq!(runner.invocations.len(), 1);
    let (program, arguments, maximum_output_bytes) = &runner.invocations[0];
    assert_eq!(program, "curl");
    assert_eq!(
        arguments,
        &[
            "--disable",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "0",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--max-filesize",
            "65536",
            "--dump-header",
            "-",
            "--output",
            "-",
            "--write-out",
            "\nDOCS_PARITY_COUNTS:%{size_header}:%{size_download}\n",
            "--request",
            "GET",
            "--url",
            "https://docs.example.com/path",
        ]
    );
    assert_eq!(*maximum_output_bytes, 64 * 1024 + 64 * 1024 + 128);
}

#[test]
fn curl_transport_head_emits_headers_exactly_once() {
    let runner = FakeCommandRunner {
        outputs: vec![Ok(curl_output(
            "204 No Content",
            &[("Content-Length", "0")],
            b"",
        ))]
        .into(),
        invocations: Vec::new(),
    };
    let mut transport = CurlTransport::new(runner);
    let request = ExternalRequest {
        method: "HEAD".to_owned(),
        url: "https://docs.example.com/path".to_owned(),
        timeout_seconds: 15,
        maximum_body_bytes: 64 * 1024,
    };

    let response = transport.send(&request).expect("HEAD output should parse");
    let runner = transport.into_runner();

    assert_eq!(response.status, 204);
    assert_eq!(response.body_bytes, 0);
    assert_eq!(
        runner.invocations[0].1,
        [
            "--disable",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "0",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--max-filesize",
            "65536",
            "--output",
            "-",
            "--write-out",
            "\nDOCS_PARITY_COUNTS:%{size_header}:%{size_download}\n",
            "--head",
            "--url",
            "https://docs.example.com/path",
        ]
    );
}

#[test]
fn curl_transport_parses_head_fallback_statuses_through_the_shared_state_machine() {
    for unsupported in ["405 Method Not Allowed", "501 Not Implemented"] {
        let runner = FakeCommandRunner {
            outputs: vec![
                Ok(curl_output(unsupported, &[], b"")),
                Ok(curl_output("200 OK", &[], b"body")),
            ]
            .into(),
            invocations: Vec::new(),
        };
        let mut transport = CurlTransport::new(runner);
        let sources = [source(
            "README.md",
            LinkSourceSet::Repository,
            "[site](https://docs.example.invalid/)\n",
        )];
        let mut sleeper = FakeSleeper::default();

        check_external_links(&sources, &[], 0, &mut transport, &mut sleeper)
            .expect("parsed unsupported HEAD should fall back to GET");
        let runner = transport.into_runner();
        assert!(
            runner.invocations[0]
                .1
                .iter()
                .any(|value| value == "--head")
        );
        assert!(
            !runner.invocations[0]
                .1
                .iter()
                .any(|value| value == "--dump-header")
        );
        assert!(runner.invocations[1].1.iter().any(|value| value == "GET"));
        assert!(
            runner.invocations[1]
                .1
                .iter()
                .any(|value| value == "--dump-header")
        );
    }
}

#[test]
fn actual_file_curl_head_uses_one_header_frame_and_the_production_parser_path() {
    let directory = tempfile::tempdir().expect("should create file HEAD fixture");
    let payload = directory.path().join("payload.bin");
    fs::write(&payload, b"bounded local payload").expect("should write file HEAD payload");
    let runner = FileHeadCommandRunner {
        file_url: format!("file://{}", payload.display()),
        observed_header_copies: 0,
    };
    let mut transport = CurlTransport::new(runner);
    let request = ExternalRequest {
        method: "HEAD".to_owned(),
        url: "https://file-head.example.invalid/".to_owned(),
        timeout_seconds: 15,
        maximum_body_bytes: 64 * 1024,
    };

    let response = transport
        .send(&request)
        .expect("actual file HEAD framing should pass the production parser");
    let runner = transport.into_runner();

    assert_eq!(response.status, 200);
    assert!(response.header_bytes > 0);
    assert_eq!(response.body_bytes, 0);
    assert_eq!(runner.observed_header_copies, 1);
}

#[test]
fn curl_transport_rejects_unsafe_requests_and_malformed_command_output() {
    for request in [
        ExternalRequest {
            method: "POST".to_owned(),
            url: "https://docs.example.com/".to_owned(),
            timeout_seconds: 15,
            maximum_body_bytes: 64 * 1024,
        },
        ExternalRequest {
            method: "GET".to_owned(),
            url: "http://docs.example.com/".to_owned(),
            timeout_seconds: 15,
            maximum_body_bytes: 64 * 1024,
        },
        ExternalRequest {
            method: "GET".to_owned(),
            url: "https://user:password@docs.example.com/".to_owned(),
            timeout_seconds: 15,
            maximum_body_bytes: 64 * 1024,
        },
    ] {
        let mut transport = CurlTransport::new(FakeCommandRunner::default());
        assert!(transport.send(&request).is_err());
        assert!(transport.into_runner().invocations.is_empty());
    }

    for output in [
        CommandOutput {
            success: true,
            status_code: Some(0),
            stdout: b"not HTTP\nDOCS_PARITY_COUNTS:8:0\n".to_vec(),
        },
        curl_output("999 Nope", &[], b""),
        CommandOutput {
            success: false,
            status_code: Some(7),
            stdout: Vec::new(),
        },
    ] {
        let runner = FakeCommandRunner {
            outputs: vec![Ok(output)].into(),
            invocations: Vec::new(),
        };
        let mut transport = CurlTransport::new(runner);
        let request = ExternalRequest {
            method: "HEAD".to_owned(),
            url: "https://docs.example.com/".to_owned(),
            timeout_seconds: 15,
            maximum_body_bytes: 64 * 1024,
        };
        assert!(transport.send(&request).is_err());
    }
}

#[test]
fn production_command_runner_stops_reading_at_the_stdout_bound() {
    let mut runner = ProcessCommandRunner;
    let arguments = [
        "-c".to_owned(),
        "while :; do printf 1234567890; done".to_owned(),
    ];

    let error = runner
        .run("sh", &arguments, 32)
        .expect_err("unbounded stdout must be terminated");

    assert!(error.contains("stdout exceeds 32 bytes"));
}

#[test]
fn curl_disable_first_ignores_ambient_curlrc_without_network_access() {
    let directory = tempfile::tempdir().expect("should create curl home");
    let payload = directory.path().join("payload.txt");
    let side_effect = directory.path().join("curlrc-output.txt");
    fs::write(&payload, "local payload").expect("should write local payload");
    fs::write(
        directory.path().join(".curlrc"),
        format!("output = \"{}\"\n", side_effect.display()),
    )
    .expect("should write isolated curl config");
    let url = format!("file://{}", payload.display());

    let control = Command::new("curl")
        .env("CURL_HOME", directory.path())
        .args(["--silent", "--show-error", "--url", &url])
        .output()
        .expect("should execute curl control");
    assert!(control.status.success());
    assert_eq!(
        fs::read_to_string(&side_effect).expect("curlrc should affect control"),
        "local payload"
    );
    fs::remove_file(&side_effect).expect("should reset bounded fixture side effect");

    let isolated = Command::new("curl")
        .env("CURL_HOME", directory.path())
        .args(["--disable", "--silent", "--show-error", "--url", &url])
        .output()
        .expect("should execute isolated curl");
    assert!(isolated.status.success());
    assert_eq!(isolated.stdout, b"local payload");
    assert!(!side_effect.exists(), "--disable must ignore .curlrc");
}

#[test]
fn response_header_names_use_the_complete_nonempty_rfc_token_grammar() {
    let source = "[site](https://docs.example.invalid/)\n";
    let valid_name = "x!#$%&'*+-.^_`|~";
    let valid = ExternalResponse {
        status: 200,
        headers: BTreeMap::from([(valid_name.to_owned(), "ok".to_owned())]),
        header_bytes: 64,
        body_bytes: 0,
    };
    let sources = [self::source("README.md", LinkSourceSet::Repository, source)];
    let mut transport = FakeTransport {
        responses: vec![valid].into(),
        requests: Vec::new(),
    };
    let mut sleeper = FakeSleeper::default();
    check_external_links(&sources, &[], 0, &mut transport, &mut sleeper)
        .expect("every RFC field-name token character should be accepted");

    for name in ["", "bad name", "bad\u{0001}name", "tést"] {
        let response = ExternalResponse {
            status: 200,
            headers: BTreeMap::from([(name.to_owned(), "ok".to_owned())]),
            header_bytes: 64,
            body_bytes: 0,
        };
        let (error, _, _) = external_error(source, vec![response]);
        assert!(error.contains("header"), "{name:?} must fail: {error}");
    }

    for name in ["", "bad name", "bad\u{0001}name", "tést"] {
        let runner = FakeCommandRunner {
            outputs: vec![Ok(curl_output("200 OK", &[(name, "value")], b""))].into(),
            invocations: Vec::new(),
        };
        let mut transport = CurlTransport::new(runner);
        let request = ExternalRequest {
            method: "HEAD".to_owned(),
            url: "https://docs.example.invalid/".to_owned(),
            timeout_seconds: 15,
            maximum_body_bytes: 64 * 1024,
        };
        assert!(transport.send(&request).is_err(), "{name:?} must fail");
    }

    let runner = FakeCommandRunner {
        outputs: vec![Ok(curl_output("200 OK", &[(valid_name, "value")], b""))].into(),
        invocations: Vec::new(),
    };
    let mut transport = CurlTransport::new(runner);
    let request = ExternalRequest {
        method: "HEAD".to_owned(),
        url: "https://docs.example.invalid/".to_owned(),
        timeout_seconds: 15,
        maximum_body_bytes: 64 * 1024,
    };
    transport
        .send(&request)
        .expect("curl parsing should accept the same RFC token grammar");
}

#[test]
fn external_response_header_and_body_bounds_fail_closed() {
    let source = "[site](https://docs.example.com/)\n";
    let mut cases = Vec::new();

    let mut too_many = BTreeMap::new();
    for index in 0..129 {
        too_many.insert(format!("x-{index}"), "ok".to_owned());
    }
    cases.push(ExternalResponse {
        status: 200,
        headers: too_many,
        header_bytes: 1024,
        body_bytes: 0,
    });
    cases.push(ExternalResponse {
        status: 200,
        headers: BTreeMap::from([("x".repeat(257), "ok".to_owned())]),
        header_bytes: 1024,
        body_bytes: 0,
    });
    cases.push(ExternalResponse {
        status: 200,
        headers: BTreeMap::from([("x-test".to_owned(), "x".repeat(8 * 1024 + 1))]),
        header_bytes: 16 * 1024,
        body_bytes: 0,
    });
    cases.push(ExternalResponse {
        status: 200,
        headers: BTreeMap::new(),
        header_bytes: 64 * 1024 + 1,
        body_bytes: 0,
    });
    cases.push(ExternalResponse {
        status: 200,
        headers: BTreeMap::new(),
        header_bytes: 32,
        body_bytes: 64 * 1024 + 1,
    });
    cases.push(ExternalResponse {
        status: 200,
        headers: BTreeMap::from([("bad name".to_owned(), "ok".to_owned())]),
        header_bytes: 32,
        body_bytes: 0,
    });
    cases.push(ExternalResponse {
        status: 200,
        headers: BTreeMap::from([("x".repeat(256), "v".repeat(8 * 1024 - 255))]),
        header_bytes: 16 * 1024,
        body_bytes: 0,
    });

    for response in cases {
        let (error, _, _) = external_error(source, vec![response]);
        assert!(
            error.contains("header") || error.contains("body"),
            "bound failure must be specific: {error}"
        );
    }
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
    assert!(diagnostic(&diagram).contains("missing prose heading"));
}

#[test]
fn typed_page_tombstones_are_exact_and_require_live_replacements() {
    let repository = PublicationRepository::new();
    let pages_path = repository
        .path()
        .join("tools/docs-parity/manifests/pages.toml");
    let pages = fs::read_to_string(&pages_path).expect("should read pages manifest");
    let typed_pages = pages
        + concat!(
            "\n[[pages]]\n",
            "kind = \"tombstone\"\n",
            "route = \"/retired\"\n",
            "replacement = \"/guide/\"\n",
        );
    fs::write(&pages_path, typed_pages).expect("should write typed page manifest");
    write_file(
        repository.path(),
        "tools/docs-parity/manifests/orphans.toml",
        concat!(
            "version = 1\nreviewed = true\n\n",
            "[[exceptions]]\n",
            "kind = \"tombstone\"\n",
            "route = \"/retired\"\n",
            "replacement = \"/guide/\"\n",
            "owner = \"docs-team\"\n",
            "reason = \"Route retained for a bounded migration.\"\n",
            "expires_at = \"2099-01-01T00:00:00Z\"\n",
        ),
    );

    let clean = repository.check();
    assert_eq!(
        status_code(&clean),
        SUCCESS,
        "matching typed tombstone should pass: {}",
        diagnostic(&clean)
    );

    let orphans_path = repository
        .path()
        .join("tools/docs-parity/manifests/orphans.toml");
    let orphans = fs::read_to_string(&orphans_path).expect("should read orphans");
    fs::write(&orphans_path, orphans.replace("/retired", "/extra-retired"))
        .expect("should make tombstone inventories differ");
    let mismatch = repository.check();
    assert_eq!(status_code(&mismatch), ERROR);
    assert!(diagnostic(&mismatch).contains("tombstone inventory mismatch"));
}

#[test]
fn tombstones_reject_live_collisions_missing_replacements_and_stale_targets() {
    for tombstone in [
        "kind = \"tombstone\"\nroute = \"/guide/\"\nreplacement = \"/\"\n",
        "kind = \"tombstone\"\nroute = \"/retired\"\n",
        "kind = \"tombstone\"\nroute = \"/retired\"\nreplacement = \"/missing\"\n",
    ] {
        let repository = PublicationRepository::new();
        let pages_path = repository
            .path()
            .join("tools/docs-parity/manifests/pages.toml");
        let pages = fs::read_to_string(&pages_path).expect("should read pages");
        let typed_pages = pages + &format!("\n[[pages]]\n{tombstone}");
        fs::write(&pages_path, typed_pages).expect("should write pages");

        let result = repository.check();
        assert_eq!(status_code(&result), ERROR, "{tombstone} must fail");
    }

    let repository = PublicationRepository::new();
    let pages_path = repository
        .path()
        .join("tools/docs-parity/manifests/pages.toml");
    let pages = fs::read_to_string(&pages_path).expect("should read pages");
    fs::write(
        &pages_path,
        pages
            + concat!(
                "\n[[pages]]\nkind = \"tombstone\"\nroute = \"/retired\"\n",
                "replacement = \"/\"\n\n",
                "[[pages]]\nkind = \"tombstone\"\nroute = \"/retired\"\n",
                "replacement = \"/guide/\"\n",
            ),
    )
    .expect("should write duplicate tombstones");
    let duplicate = repository.check();
    assert_eq!(status_code(&duplicate), ERROR);
    assert!(
        diagnostic(&duplicate).contains("duplicate tombstone page"),
        "duplicate routes must fail before inventory comparison: {}",
        diagnostic(&duplicate)
    );
}

#[test]
fn mermaid_inventory_uses_semantic_exact_and_closed_fences() {
    for replacement in [
        "```mermaid-extra\ngraph TD\n```",
        "```mermaid {}\ngraph TD\n```",
        "```mermaid {naked}\ngraph TD\n```",
        "```mermaid {#}\ngraph TD\n```",
        "```mermaid\ngraph TD",
        "```mermaid\ngraph TD\n~~~",
    ] {
        let repository = PublicationRepository::new();
        let guide_path = repository.path().join("docs/guide/index.md");
        let guide = fs::read_to_string(&guide_path).expect("should read guide");
        fs::write(
            &guide_path,
            guide.replace("```mermaid\ngraph TD\n```", replacement),
        )
        .expect("should replace diagram fence");

        let result = repository.check();
        assert_eq!(status_code(&result), ERROR, "{replacement} must fail");
        assert!(diagnostic(&result).contains("mermaid"));
    }

    let repository = PublicationRepository::new();
    let guide_path = repository.path().join("docs/guide/index.md");
    let guide = fs::read_to_string(&guide_path).expect("should read guide");
    fs::write(
        &guide_path,
        guide.replace(
            "```mermaid",
            "``` mermaid {#flow-diagram .wide data-role=flow}",
        ),
    )
    .expect("should add supported fence attributes");
    let valid = repository.check();
    assert_eq!(
        status_code(&valid),
        SUCCESS,
        "valid mermaid attributes should pass: {}",
        diagnostic(&valid)
    );
}

#[test]
fn diagram_prose_must_be_a_heading_not_an_arbitrary_html_anchor() {
    let repository = PublicationRepository::new();
    write_file(
        repository.path(),
        "docs/guide/index.md",
        "# Guide\n<a id=\"flow\"></a>\n```mermaid\ngraph TD\n```\n",
    );

    let result = repository.check();

    assert_eq!(status_code(&result), ERROR);
    assert!(diagnostic(&result).contains("prose heading"));
}

#[test]
fn pages_manifest_is_bounded_before_deserialization() {
    let repository = PublicationRepository::new();
    let pages_path = repository
        .path()
        .join("tools/docs-parity/manifests/pages.toml");
    let pages = fs::read_to_string(&pages_path).expect("should read pages");
    fs::write(
        &pages_path,
        format!("{pages}\n# {}\n", "x".repeat(4 * 1024 * 1024)),
    )
    .expect("should write oversized pages manifest");

    let result = repository.check();

    assert_eq!(status_code(&result), ERROR);
    assert!(diagnostic(&result).contains("pages manifest exceeds"));
}
