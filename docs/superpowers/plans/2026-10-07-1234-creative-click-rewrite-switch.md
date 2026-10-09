# Creative Click Rewrite Switch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `[auction] rewrite_clicks` so creative click wrapping (`<a href>` / `<area href>` → signed `/first-party/click`) can be turned on or off independently of asset rewriting, without changing output for any existing config.

**Architecture:**

- Replace `to_abs` with a pure URL normalizer plus host policy on `Rewrite`. This "shared step" is written identically in #1231's PR.
- Add an `Option<bool>` field on `AuctionConfig` with two resolution methods.
- Give the lol_html rewrite pass a `CreativeFeatures { assets, clicks }` parameter, and register the asset and anchor handlers only when their feature is on.
- `<base>` removal and TSJS injection run whenever the pass runs.

**Tech Stack:** Rust 2024 (`trusted-server-core`, `trusted-server-cli`), `lol_html`, `url`, `serde`, EdgeZero typed app-config loader, Viceroy for `wasm32-wasip1` tests.

**Spec:** `docs/superpowers/specs/2026-10-07-1234-creative-click-rewrite-switch-design.md` (issue [#1234](https://github.com/IABTechLab/trusted-server/issues/1234)). Read it first.

**Status:** Validated by an exploratory implementation on 2026-10-07. Every code block below is the code that compiled, passed clippy and passed its tests. Each step's "Expected" line is the observed result.

## Global Constraints

- With `rewrite_clicks` unset, rewritten output must be byte-identical to today for both values of `rewrite_creatives` on `/auction` and inline SSAT/page-bids, and unchanged for `/first-party/proxy` HTML.
- `rewrite_clicks` is `Option<bool>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. `AuctionConfig` keeps `deny_unknown_fields`.
- Unset resolution: auction paths use `rewrite_clicks.unwrap_or(rewrite_creatives)`, and proxied HTML uses `rewrite_clicks.unwrap_or(true)`.
- `exclude_domains` applies to anchors. The #1231 include list never does.
- The shared matcher is `crate::proxy::is_host_allowed` (`proxy.rs:1242`), which is case-insensitive.
- No JavaScript, adapter, routing or dependency changes. `/first-party/click` and `/first-party/proxy-rebuild` stay routed.
- AGENTS.md rules apply:
  - example.com-style domains only;
  - `expect("should ...")` messages;
  - comments on their own line;
  - no local `use` inside functions;
  - every public item documented;
  - at most 7 function arguments.
- Commits use sentence case, imperative mood, and `git commit --signoff -S` with multiple `-m` flags. No prefixes, no attribution trailers, never a heredoc.

## Environment notes (learned during validation)

- `cargo test-fastly` runs the wasm test binary through Viceroy. If `viceroy` is installed under `~/.cargo/bin` but not on `PATH`, prefix commands with `PATH=$HOME/.cargo/bin:$PATH`. Validation used Viceroy 0.21.1; `.tool-versions` pins 0.17.0.
- Under Viceroy, one panicking test aborts the whole wasm test binary (exit 134) and hides the assertion message. To watch a test fail, run it alone:

  ```bash
  cargo test-fastly -p trusted-server-core --lib -- --exact <module::tests::name> --nocapture
  ```

  Read the `panicked at` line.

- A test that only adds a new symbol fails at compile time (`E0425`/`E0599`/`E0609`). That is the red step for API-shaped tests. Tests that only use existing APIs must be run against the old code to see a runtime red.
- The worktree may have no `docs/node_modules`. Use a checkout that has one:

  ```bash
  /path/to/checkout/docs/node_modules/.bin/prettier --config docs/.prettierrc ...
  ```

  `cd docs && npm run format` (check mode) still works.

## File Map

| File                                                                                  | Responsibility in this change                                                                                                               |
| ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/creative.rs`                                          | `normalize_creative_url`, `asset_target`/`click_target`, `CreativeFeatures`, conditional handler registration, caller wiring, matrix tests. |
| `crates/trusted-server-core/src/settings.rs`                                          | `Rewrite::should_proxy_asset` / `should_wrap_click` / `normalize`, removal of `is_excluded`, TOML tests.                                    |
| `crates/trusted-server-core/src/proxy.rs`                                             | `/first-party/sign` call-site migration, sign and proxied-HTML tests.                                                                       |
| `crates/trusted-server-core/src/auction_config_types.rs`                              | `rewrite_clicks`, `rewrites_auction_clicks` / `rewrites_proxied_clicks`, serialization tests.                                               |
| `crates/trusted-server-core/src/config_payload.rs`                                    | Blob round-trip tests.                                                                                                                      |
| `crates/trusted-server-core/src/auction/orchestrator.rs`                              | `rewrite_clicks: None` in the one exhaustive `AuctionConfig` literal.                                                                       |
| `crates/trusted-server-core/src/auction/formats.rs`                                   | Doc comment, debug log, `/auction` end-to-end test.                                                                                         |
| `crates/trusted-server-core/src/publisher.rs`                                         | Inline end-to-end test.                                                                                                                     |
| `crates/trusted-server-core/src/auction/endpoints.rs`                                 | Response doc comment.                                                                                                                       |
| `crates/trusted-server-cli/tests/config_env_overlay.rs`                               | Overlay tests for `TRUSTED_SERVER__AUCTION__REWRITE_CLICKS`.                                                                                |
| `trusted-server.example.toml`                                                         | Reworded `rewrite_creatives` comment and commented-out `rewrite_clicks` leaf.                                                               |
| `docs/guide/*.md`, `crates/trusted-server-core/src/auction/README.md`, `CHANGELOG.md` | Operator documentation.                                                                                                                     |

## Shared step

This section is identical in the #1231 and #1234 plans. Both PRs write this production code and this test set verbatim; whichever PR lands first ships them, and the other skips its shared-step task. `proxy::is_host_permitted` stays private in the shared step: #1231 makes it `pub(crate)` in the task that adds the include clause, and #1234 never needs it.

`crates/trusted-server-core/src/settings.rs`, imports (`:26`). The shared step uses only `is_host_allowed`, so it imports only that; importing `is_host_permitted` here would fail `-D warnings` as an unused import. #1231's include-clause task widens the import.

```rust
use crate::proxy::is_host_allowed;
```

`settings.rs`, replaces `impl Rewrite` (`:645-670`), removing `is_excluded` and its `#[allow(dead_code)]`:

```rust
impl Rewrite {
    /// Returns `true` when an asset URL on `host` should be rewritten to
    /// `/first-party/proxy`.
    ///
    /// The host must not match [`Self::exclude_domains`]. Matching is
    /// case-insensitive; see [`is_host_allowed`] for the pattern rules.
    #[must_use]
    pub fn should_proxy_asset(&self, host: &str) -> bool {
        !self.is_excluded_host(host)
    }

    /// Returns `true` when a click-through URL on `host` should be wrapped in
    /// `/first-party/click`.
    ///
    /// Only [`Self::exclude_domains`] applies to click-through links.
    #[must_use]
    pub fn should_wrap_click(&self, host: &str) -> bool {
        !self.is_excluded_host(host)
    }

    fn is_excluded_host(&self, host: &str) -> bool {
        self.exclude_domains
            .iter()
            .any(|pattern| is_host_allowed(host, pattern))
    }

    /// Trims and lowercases host patterns in place.
    ///
    /// Empty and bare `*` entries in `exclude_domains` are dropped with a
    /// warning: neither can match a host, so dropping them changes no behavior.
    fn normalize(&mut self) {
        let before = self.exclude_domains.len();
        self.exclude_domains = self
            .exclude_domains
            .iter()
            .map(|pattern| pattern.trim().to_ascii_lowercase())
            .filter(|pattern| !pattern.is_empty() && pattern != "*")
            .collect();
        if self.exclude_domains.len() < before {
            log::warn!(
                "rewrite.exclude_domains: removed empty or bare \"*\" entries, which never match a host"
            );
        }
    }
}
```

`settings.rs`, in `Settings::normalize_deserialized` (`:2988`), right after `self.proxy.normalize();`:

```rust
        self.rewrite.normalize();
```

`crates/trusted-server-core/src/creative.rs`, replaces `to_abs` and its comment (`:52-75`):

```rust
/// Normalizes a creative URL to an absolute HTTP(S) [`url::Url`].
///
/// Trims surrounding whitespace, resolves a protocol-relative `//host/...`
/// against `https:`, and accepts only `http://` and `https://` input (ASCII
/// case-insensitive). Returns `None` for empty, relative, non-network-scheme or
/// unparseable input. Applies no host policy; see
/// [`crate::settings::Rewrite::should_proxy_asset`] and
/// [`crate::settings::Rewrite::should_wrap_click`].
pub(super) fn normalize_creative_url(url: &str) -> Option<url::Url> {
    let trimmed = url.trim();
    let is_http = trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"));
    let is_https = trimmed
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"));
    if trimmed.starts_with("//") {
        url::Url::parse(&format!("https:{trimmed}")).ok()
    } else if is_http || is_https {
        url::Url::parse(trimmed).ok()
    } else {
        None
    }
}

/// Normalizes `raw` and returns it when asset policy allows proxying its host.
fn asset_target(settings: &Settings, raw: &str) -> Option<url::Url> {
    normalize_creative_url(raw).filter(|url| {
        url.host_str()
            .is_some_and(|host| settings.rewrite.should_proxy_asset(host))
    })
}

/// Normalizes `raw` and returns it when click policy allows wrapping its host.
fn click_target(settings: &Settings, raw: &str) -> Option<url::Url> {
    normalize_creative_url(raw).filter(|url| {
        url.host_str()
            .is_some_and(|host| settings.rewrite.should_wrap_click(host))
    })
}
```

`creative.rs` call sites (`:402`, `:409`, `:639-641`, `:726-727`, `:1285-1287`):

```rust
        // CssUrlRewriter::rewrite
        let Some(target) = asset_target(self.settings, value) else {
            return;
        };
        // ... later in the same function:
            let proxied = build_proxy_url(self.settings, target.as_str(), self.base_origin);

// proxy_if_abs
pub(super) fn proxy_if_abs(settings: &Settings, val: &str, base_origin: &str) -> Option<String> {
    asset_target(settings, val).map(|url| build_proxy_url(settings, url.as_str(), base_origin))
}

        // rewrite_srcset
        let rewritten = if let Some(target) = asset_target(settings, url) {
            build_proxy_url(settings, target.as_str(), base_origin)
        } else {
            url.to_owned()
        };

                // Anchor handler
                element!("a[href], area[href]", |el| {
                    if let Some(href) = el.get_attribute("href")
                        && let Some(target) = click_target(settings, &href)
                    {
                        let click = build_click_url(settings, target.as_str(), base_origin);
                        let _ = el.set_attribute("href", &click);
                        let _ = el.set_attribute("data-tsclick", &click);
                    }
                    Ok(())
                }),
```

`creative.rs` module doc (`:25-27`):

```rust
//! - `normalize_creative_url(&str) -> Option<Url>`: Normalizes an absolute or
//!   protocol-relative http(s) URL; returns `None` for relative input,
//!   non-network schemes and unparseable values. Host policy lives on
//!   [`crate::settings::Rewrite`].
```

`proxy.rs`, `handle_first_party_proxy_sign` (`:1625`): replace `:1672-1703` (from `let trimmed` up to the `is_host_permitted` check) with the block below, then at `:1723` and `:1726` use `target` in place of `parsed` and `target.as_str()` in place of `&abs`. The old parse, scheme and duplicate exclusion checks go away because the normalizer already guarantees an `http(s)` URL.

```rust
    let trimmed = payload.url.trim();
    let protocol_relative;
    // A protocol-relative target inherits the signing request's scheme before
    // normalization, so `//` and absolute input reach the same policy check.
    let candidate = if trimmed.starts_with("//") {
        protocol_relative = format!("{request_scheme}:{trimmed}");
        protocol_relative.as_str()
    } else {
        trimmed
    };
    let target = crate::creative::normalize_creative_url(candidate).ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "unsupported url".to_string(),
        })
    })?;
    let host = target.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "missing host".to_string(),
        })
    })?;
    if !settings.rewrite.should_proxy_asset(host) {
        log::debug!("sign request for `{host}` declined by rewrite policy");
        return Err(Report::new(TrustedServerError::Proxy {
            message: "unsupported url".to_string(),
        }));
    }
```

```rust
    let mut base = target.clone();
    base.set_query(None);
    base.set_fragment(None);
    let proxied = crate::creative::build_proxy_url_with_extras(settings, target.as_str(), &extras);
```

### Shared tests

Every shared test builds `Rewrite` with `Rewrite::default()` and `.extend`/`.push` on its lists, or sets `settings.rewrite.exclude_domains` on a full `Settings`. None uses a `Rewrite { .. }` literal or `#[allow(clippy::field_reassign_with_default)]`. The code for each test is in the task that adds it in each plan.

| File          | Test                                                                 | Replaces or placement                                                                                    |
| ------------- | -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `settings.rs` | `rewrite_policy_matches_exclude_patterns_case_insensitively`         | replaces `test_rewrite_is_excluded`                                                                      |
| `settings.rs` | `rewrite_normalize_trims_lowercases_and_drops_inert_exclude_entries` | after the previous test                                                                                  |
| `settings.rs` | `rewrite_exclude_domains_match_mixed_case_entries_from_toml`         | after the previous test                                                                                  |
| `settings.rs` | `settings_load_normalizes_rewrite_exclude_domains`                   | after the previous test                                                                                  |
| `creative.rs` | `normalized` helper and `normalize_creative_url_conversions`         | replace `to_abs_conversions`, `to_abs_preserves_port_in_protocol_relative` and `to_abs_additional_cases` |
| `creative.rs` | `proxy_if_abs_respects_exclude_domains`                              | replaces `to_abs_respects_exclude_domains` and `to_abs_respects_wildcard_domains`                        |
| `creative.rs` | `unparseable_absolute_url_is_left_byte_identical`                    | after the previous test                                                                                  |
| `creative.rs` | `unparseable_absolute_click_url_is_left_byte_identical`              | after the previous test                                                                                  |
| `creative.rs` | `exclude_domains_match_case_insensitively_in_the_rewrite_pass`       | after the previous test                                                                                  |
| `proxy.rs`    | `proxy_sign_rejects_excluded_urls`                                   | rewritten in place: GET and POST, plus an uppercase target host                                          |
| `proxy.rs`    | `proxy_sign_rejects_excluded_urls_case_insensitively`                | after the previous test: mixed-case config entry, absolute and `//` input, GET and POST                  |

The `creative.rs` test module's `use super::{...}` list gains `normalize_creative_url` and `proxy_if_abs` and loses `to_abs`.

CHANGELOG: the shared step adds this entry as the first item under `## [Unreleased]` › `### Fixed` (create the heading if absent), in the same commit as the code:

```markdown
- `rewrite.exclude_domains` matching is now case-insensitive and ignores surrounding whitespace. Entries written with uppercase letters previously never matched and now take effect, so those hosts stop being proxied and click-wrapped, and `/first-party/sign` now rejects them with `502`; it used to sign them, or return `403` when the host was also outside `proxy.allowed_domains`. Empty and bare `"*"` `exclude_domains` entries, which never matched a host, are dropped at load with a warning. Absolute `http(s)` creative URLs that cannot be parsed (for example, a host containing a space) are now left untouched; previously the attribute was re-quoted and a link also gained `data-tsclick`. Audit `exclude_domains` for mixed-case entries before upgrading.
```

---

### Task 0: Pre-flight, check whether #1231 already landed the shared step

**Files:** none modified.

- [ ] **Step 1: Check main**

```bash
git fetch origin
git grep -n 'fn normalize_creative_url' origin/main -- crates/trusted-server-core/src/creative.rs
git grep -n 'fn should_wrap_click' origin/main -- crates/trusted-server-core/src/settings.rs
```

- [ ] **Step 2: Decide**

- Both greps print nothing: run **Task 1**.
- Both print matches: `git rebase origin/main`, **skip Task 1**, and confirm that `click_target` exists with `git grep -n 'fn click_target' -- crates/trusted-server-core/src/creative.rs`. It is part of the shared step, so it should. Note in the PR description that the shared step came from #1231.

---

### Task 1: Shared step (skip if Task 0 found it on main)

This task writes the [Shared step](#shared-step) code and tests verbatim. They are identical to #1231's Tasks 1–2.

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs` (import, `impl Rewrite`, `normalize_deserialized`, tests)
- Modify: `crates/trusted-server-core/src/creative.rs` (module doc, `to_abs`, `CssUrlRewriter::rewrite`, `proxy_if_abs`, `rewrite_srcset`, anchor handler, test imports, tests)
- Modify: `crates/trusted-server-core/src/proxy.rs` (`handle_first_party_proxy_sign`, tests)
- Modify: `CHANGELOG.md`

**Interfaces:**

- Produces:
  - `pub(super) fn normalize_creative_url(url: &str) -> Option<url::Url>`;
  - private `asset_target(&Settings, &str) -> Option<url::Url>` and `click_target(&Settings, &str) -> Option<url::Url>`;
  - `pub fn Rewrite::should_proxy_asset(&self, host: &str) -> bool`;
  - `pub fn Rewrite::should_wrap_click(&self, host: &str) -> bool`;
  - private `Rewrite::is_excluded_host` and `Rewrite::normalize`.
- Removes: `to_abs`, `Rewrite::is_excluded`. `proxy::is_host_permitted` stays private.

- [ ] **Step 1: Settings tests**

Replace `test_rewrite_is_excluded` in `settings.rs` with:

```rust
    #[test]
    fn rewrite_policy_matches_exclude_patterns_case_insensitively() {
        let mut rewrite = Rewrite::default();
        rewrite
            .exclude_domains
            .extend(["cdn.example.com", "*.example.org"].map(str::to_owned));

        for (host, expected) in [
            ("cdn.example.com", false),
            ("CDN.EXAMPLE.COM", false),
            ("example.org", false),
            ("a.b.example.org", false),
            ("evil-example.org", true),
            ("sub.cdn.example.com", true),
            ("other.example.com", true),
        ] {
            assert_eq!(
                rewrite.should_proxy_asset(host),
                expected,
                "should_proxy_asset(`{host}`) should be {expected}"
            );
            assert_eq!(
                rewrite.should_wrap_click(host),
                expected,
                "should_wrap_click(`{host}`) should be {expected}"
            );
        }
    }

    #[test]
    fn rewrite_normalize_trims_lowercases_and_drops_inert_exclude_entries() {
        let mut rewrite = Rewrite::default();
        rewrite
            .exclude_domains
            .extend(["  CDN.Example.com ", "", "*", "*.Example.ORG"].map(str::to_owned));

        rewrite.normalize();

        assert_eq!(
            rewrite.exclude_domains,
            vec!["cdn.example.com".to_owned(), "*.example.org".to_owned()],
            "should trim, lowercase, and drop empty and bare `*` entries"
        );
    }

    #[test]
    fn rewrite_exclude_domains_match_mixed_case_entries_from_toml() {
        let toml_str = crate_test_settings_str()
            + r#"
            [rewrite]
            exclude_domains = ["CDN.Example.com"]
            "#;

        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");

        assert!(
            !settings.rewrite.should_proxy_asset("cdn.example.com"),
            "should exclude a host whose config entry was written in mixed case"
        );
    }

    #[test]
    fn settings_load_normalizes_rewrite_exclude_domains() {
        let toml_str = crate_test_settings_str()
            + r#"
            [rewrite]
            exclude_domains = ["  CDN.Example.com ", "*", ""]
            "#;

        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");

        assert_eq!(
            settings.rewrite.exclude_domains,
            vec!["cdn.example.com".to_owned()],
            "should trim, lowercase, and drop inert entries when settings load"
        );
    }
```

`settings_load_normalizes_rewrite_exclude_domains` is the only test that proves `normalize` is wired into settings loading. The matcher is case-insensitive on its own, so the mixed-case TOML test would pass even without the `normalize_deserialized` call.

- [ ] **Step 2: Creative tests**

Change the `creative.rs` test module's import list to:

```rust
    use super::{
        CreativeCssProcessor, StreamProcessor as _, normalize_creative_url,
        process_auction_creative, proxy_if_abs, rewrite_creative_html,
        rewrite_inline_creative_html, rewrite_srcset, rewrite_style_urls, sanitize_creative_html,
    };
```

Replace `to_abs_conversions` and `to_abs_preserves_port_in_protocol_relative` with:

```rust
    fn normalized(raw: &str) -> Option<String> {
        normalize_creative_url(raw).map(|url| url.as_str().to_owned())
    }

    #[test]
    fn normalize_creative_url_conversions() {
        for (raw, expected) in [
            ("//cdn.example/x", Some("https://cdn.example/x")),
            ("HTTPS://cdn.example/x", Some("https://cdn.example/x")),
            ("http://cdn.example/x", Some("http://cdn.example/x")),
            ("   //cdn.example/y  ", Some("https://cdn.example/y")),
            ("   https://cdn.example/a   ", Some("https://cdn.example/a")),
            (
                "//cdn.example.com:8080/asset.js",
                Some("https://cdn.example.com:8080/asset.js"),
            ),
            (
                "//cdn.example.com:9443/img.png",
                Some("https://cdn.example.com:9443/img.png"),
            ),
            ("/local/x", None),
            ("", None),
            ("data:image/png;base64,abcd", None),
            ("javascript:alert(1)", None),
            ("mailto:test@example.com", None),
            ("blob:xyz", None),
            ("tel:+123", None),
            ("about:blank", None),
            ("https://exa mple.example/x", None),
        ] {
            assert_eq!(
                normalized(raw).as_deref(),
                expected,
                "should normalize `{raw}` to {expected:?}"
            );
        }
    }
```

Delete `to_abs_additional_cases`; its cases are in the table above. Replace `to_abs_respects_exclude_domains` and `to_abs_respects_wildcard_domains` with:

```rust
    #[test]
    fn proxy_if_abs_respects_exclude_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec![
            "trusted-cdn.example.com".to_owned(),
            "*.example.org".to_owned(),
        ];

        for excluded in [
            "https://trusted-cdn.example.com/lib.js",
            "//trusted-cdn.example.com/lib.js",
            "https://example.org/cdn.js",
            "//cdnjs.example.org/lib.js",
        ] {
            assert_eq!(
                proxy_if_abs(&settings, excluded, ""),
                None,
                "should leave excluded URL `{excluded}` unproxied"
            );
        }
        for proxied in [
            "https://other-cdn.example.com/lib.js",
            "//other-cdn.example.com/lib.js",
            "https://notexample.org/lib.js",
        ] {
            assert!(
                proxy_if_abs(&settings, proxied, "")
                    .is_some_and(|url| url.starts_with("/first-party/proxy?tsurl=")),
                "should proxy non-excluded URL `{proxied}`"
            );
        }
    }

    #[test]
    fn unparseable_absolute_url_is_left_byte_identical() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = "<img src='https://exa mple.example/x'>";

        let out = rewrite_creative_html(&settings, html);

        assert!(
            out.contains(html),
            "should leave an unparseable absolute URL untouched, including its quoting: {out}"
        );
    }

    #[test]
    fn unparseable_absolute_click_url_is_left_byte_identical() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = "<a href='https://exa mple.example/x'>x</a>";

        let out = rewrite_creative_html(&settings, html);

        assert!(
            out.contains(html),
            "should leave an unparseable click URL untouched, including its quoting: {out}"
        );
        assert!(
            !out.contains("data-tsclick"),
            "should not mark an unparseable link for the click guard: {out}"
        );
    }

    #[test]
    fn exclude_domains_match_case_insensitively_in_the_rewrite_pass() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .rewrite
            .exclude_domains
            .extend(["Landing.Example.com", "CDN.example.com"].map(str::to_owned));
        let html = r#"<a href="https://landing.example.com/page">x</a><img src="https://cdn.example.com/ad.png">"#;

        let out = rewrite_creative_html(&settings, html);

        assert!(
            out.contains(r#"<a href="https://landing.example.com/page">"#),
            "should leave a link raw when its host matches a mixed-case entry: {out}"
        );
        assert!(
            out.contains(r#"<img src="https://cdn.example.com/ad.png">"#),
            "should leave an asset raw when its host matches a mixed-case entry: {out}"
        );
    }
```

The unparseable tests assert `contains`, not equality: `rewrite_creative_html` injects the TSJS script tag even into markup with no `<body>`.

- [ ] **Step 3: Sign tests**

In `proxy.rs`, replace `proxy_sign_rejects_excluded_urls`, and add the case test directly after it:

```rust
    #[test]
    fn proxy_sign_rejects_excluded_urls() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.rewrite.exclude_domains = vec!["cdn.example".to_owned()];

            for method in [&Method::GET, &Method::POST] {
                for url in [
                    "https://cdn.example/asset.js",
                    "//cdn.example/asset.js",
                    "https://CDN.Example/asset.js",
                ] {
                    let req = build_proxy_sign_request(
                        method,
                        "https://edge.example/first-party/sign",
                        url,
                    );
                    let err: Report<TrustedServerError> =
                        handle_first_party_proxy_sign(&settings, &noop_services(), req)
                            .await
                            .expect_err("should reject excluded URL");

                    assert_eq!(
                        err.current_context().status_code(),
                        StatusCode::BAD_GATEWAY,
                        "{} should reject excluded URL `{url}` as unsupported",
                        method.as_str()
                    );
                }
            }
        });
    }

    #[test]
    fn proxy_sign_rejects_excluded_urls_case_insensitively() {
        futures::executor::block_on(async {
            let mut settings = create_test_settings();
            settings.rewrite.exclude_domains = vec!["CDN.Example".to_owned()];

            for method in [&Method::GET, &Method::POST] {
                for url in ["https://cdn.example/asset.js", "//cdn.example/asset.js"] {
                    let req = build_proxy_sign_request(
                        method,
                        "https://edge.example/first-party/sign",
                        url,
                    );
                    let err: Report<TrustedServerError> =
                        handle_first_party_proxy_sign(&settings, &noop_services(), req)
                            .await
                            .expect_err("should reject a host excluded by a mixed-case entry");

                    assert_eq!(
                        err.current_context().status_code(),
                        StatusCode::BAD_GATEWAY,
                        "{} should reject `{url}` excluded by a mixed-case entry as unsupported",
                        method.as_str()
                    );
                }
            }
        });
    }
```

- [ ] **Step 4: Verify red**

Run: `cargo test-fastly --no-run`

Expected: compile errors, including `error[E0432]: unresolved import super::normalize_creative_url` and `no method named should_proxy_asset found for struct settings::Rewrite` (also `should_wrap_click` and `normalize`).

Five tests use only existing APIs. Run each alone against the unmodified code, as described in the environment notes. Each panics:

- `unparseable_absolute_url_is_left_byte_identical`: the output re-quotes the value as `<img src="https://exa mple.example/x">`.
- `unparseable_absolute_click_url_is_left_byte_identical`: the output is `<a href="https://exa mple.example/x" data-tsclick="https://exa mple.example/x">`.
- `exclude_domains_match_case_insensitively_in_the_rewrite_pass`: the link is wrapped in `/first-party/click`.
- `proxy_sign_rejects_excluded_urls_case_insensitively`: sign returns `Response { status: 200 ... }`.
- `settings_load_normalizes_rewrite_exclude_domains`: the loaded list is not normalized.

- [ ] **Step 5: Implement**

Apply the [Shared step](#shared-step) code to `settings.rs`, `creative.rs` and `proxy.rs`. Delete `Rewrite::is_excluded` with its `#[allow(dead_code)]`.

Run: `git grep -n "to_abs\|is_excluded(" crates/`

Expected: no matches.

- [ ] **Step 6: Verify green**

Run: `cargo fmt --all && cargo test-fastly -- normalize_creative_url proxy_if_abs_respects unparseable_absolute exclude_domains_match_case proxy_sign_ rewrite_policy_matches rewrite_normalize_trims rewrite_exclude_domains_match settings_load_normalizes`

Expected: `16 passed` in `trusted-server-core`: the eleven shared tests plus the five existing `proxy_sign_*` tests.

- [ ] **Step 7: Lint and full suite**

Run: `cargo clippy-fastly && cargo test-fastly`

Expected: clippy exits 0 with no `#[allow]` attributes. Every crate passes `test-fastly`, and no existing rewrite-output assertion needs editing.

- [ ] **Step 8: CHANGELOG**

Add the [Shared step](#shared-step) CHANGELOG entry verbatim, as the first item under `## [Unreleased]` › `### Fixed` (create the heading if absent).

- [ ] **Step 9: Commit**

```bash
git add crates/trusted-server-core/src/creative.rs crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/proxy.rs CHANGELOG.md
git commit --signoff -S -m "Split creative URL normalization from rewrite host policy" -m "Replace to_abs with normalize_creative_url and move exclusion policy to Rewrite::should_proxy_asset and Rewrite::should_wrap_click, sharing the case-insensitive proxy host matcher. Normalize exclude_domains at load. This is the shared step for #1231 and #1234."
```

---

### Task 2: `rewrite_clicks` setting and resolution

**Files:**

- Modify: `crates/trusted-server-core/src/auction_config_types.rs` (field docs and field, `Default`, `impl AuctionConfig`, tests)
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs` (`test_no_providers_configured` literal)
- Modify: `crates/trusted-server-core/src/config_payload.rs` (tests)
- Modify: `crates/trusted-server-core/src/settings.rs` (tests)

**Interfaces:**

- Produces:
  - `pub rewrite_clicks: Option<bool>` on `AuctionConfig`;
  - `pub fn AuctionConfig::rewrites_auction_clicks(&self) -> bool`;
  - `pub fn AuctionConfig::rewrites_proxied_clicks(&self) -> bool`.

- [ ] **Step 1: Write the tests**

`auction_config_types.rs`, before `default_sanitize_creatives_is_not_serialized`:

```rust
    #[test]
    fn default_rewrite_clicks_is_unset_and_not_serialized() {
        let config = AuctionConfig::default();

        assert_eq!(
            config.rewrite_clicks, None,
            "should leave click rewriting unset by default"
        );
        let serialized = serde_json::to_value(&config).expect("should serialize defaults");
        assert!(
            serialized.get("rewrite_clicks").is_none(),
            "should omit the unset click setting from serialized config"
        );
    }

    #[test]
    fn explicit_rewrite_clicks_is_serialized() {
        for value in [true, false] {
            let config = AuctionConfig {
                rewrite_clicks: Some(value),
                ..AuctionConfig::default()
            };

            let serialized =
                serde_json::to_value(config).expect("should serialize explicit click setting");

            assert_eq!(
                serialized.get("rewrite_clicks"),
                Some(&serde_json::Value::Bool(value)),
                "should serialize explicit rewrite_clicks = {value}"
            );
        }
    }

    #[test]
    fn click_rewriting_resolves_per_entry_point() {
        // (rewrite_creatives, rewrite_clicks, auction clicks, proxied clicks)
        let cases = [
            (true, None, true, true),
            (false, None, false, true),
            (true, Some(false), false, false),
            (false, Some(true), true, true),
            (true, Some(true), true, true),
            (false, Some(false), false, false),
        ];

        for (rewrite_creatives, rewrite_clicks, auction, proxied) in cases {
            let config = AuctionConfig {
                rewrite_creatives,
                rewrite_clicks,
                ..AuctionConfig::default()
            };

            assert_eq!(
                config.rewrites_auction_clicks(),
                auction,
                "auction clicks for rewrite_creatives={rewrite_creatives} rewrite_clicks={rewrite_clicks:?}"
            );
            assert_eq!(
                config.rewrites_proxied_clicks(),
                proxied,
                "proxied clicks for rewrite_creatives={rewrite_creatives} rewrite_clicks={rewrite_clicks:?}"
            );
        }
    }
```

`config_payload.rs`, before `strings_that_look_like_json_scalars_round_trip_as_strings`:

```rust
    #[test]
    fn legacy_blob_without_rewrite_clicks_follows_rewrite_creatives() {
        for rewrite_creatives in [true, false] {
            let mut original = test_settings();
            original.auction.rewrite_creatives = rewrite_creatives;
            let data =
                serde_json::to_value(&original).expect("should serialize settings to JSON");
            assert!(
                data["auction"].get("rewrite_clicks").is_none(),
                "should omit unset rewrite_clicks from the payload"
            );

            let reconstructed = load_settings(&envelope_json(&original))
                .expect("should reconstruct settings without rewrite_clicks");

            assert_eq!(
                reconstructed.auction.rewrite_clicks, None,
                "should load a blob without rewrite_clicks as unset"
            );
            assert_eq!(
                reconstructed.auction.rewrites_auction_clicks(),
                rewrite_creatives,
                "should follow rewrite_creatives = {rewrite_creatives} when unset"
            );
        }
    }

    #[test]
    fn explicit_rewrite_clicks_survives_blob_round_trip() {
        for (rewrite_creatives, rewrite_clicks) in [(true, false), (false, true)] {
            let mut original = test_settings();
            original.auction.rewrite_creatives = rewrite_creatives;
            original.auction.rewrite_clicks = Some(rewrite_clicks);

            let reconstructed = load_settings(&envelope_json(&original))
                .expect("should reconstruct explicit rewrite_clicks");

            assert_eq!(
                reconstructed.auction.rewrite_clicks,
                Some(rewrite_clicks),
                "should preserve rewrite_clicks = {rewrite_clicks} with rewrite_creatives = {rewrite_creatives}"
            );
        }
    }
```

`settings.rs`, before `test_auction_rewrite_creatives_accepts_explicit_false`:

```rust
    #[test]
    fn test_auction_rewrite_clicks_accepts_explicit_values() {
        for value in [true, false] {
            let toml_str = crate_test_settings_str()
                + &format!(
                    r#"
            [auction]
            enabled = true
            rewrite_clicks = {value}
            "#
                );

            let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");

            assert_eq!(
                settings.auction.rewrite_clicks,
                Some(value),
                "should parse explicit rewrite_clicks = {value}"
            );
        }
    }
```

Append this assertion to `test_auction_creative_processing_defaults_when_omitted`:

```rust
        assert_eq!(
            settings.auction.rewrite_clicks, None,
            "click rewriting stays unset when the setting is omitted"
        );
```

- [ ] **Step 2: Verify red**

Run: `cargo test-fastly -p trusted-server-core --lib --no-run`

Expected: 11 errors:

- `E0560` (`AuctionConfig` has no field named `rewrite_clicks`), twice;
- `E0599` (`no method named rewrites_auction_clicks`), twice;
- `E0599` (`rewrites_proxied_clicks`), once;
- `E0609` (`no field rewrite_clicks`), six times.

- [ ] **Step 3: Implement**

`auction_config_types.rs`: replace the first paragraph of the `rewrite_creatives` doc comment, and add the field after `pub rewrite_creatives: bool,`:

```rust
    /// Rewrite winning-bid creative asset URLs (images, scripts, styles,
    /// media, iframes, CSS `url()`) to first-party `/first-party/proxy`
    /// endpoints, applied after sanitization when
    /// [`Self::sanitize_creatives`] is enabled.
    ///
    /// Bidder `<base>` removal and creative TSJS injection run whenever this
    /// or click rewriting ([`Self::rewrites_auction_clicks`]) is on.
    ///
    /// The default stays omitted from serialized config blobs to avoid adding
    /// this field when it has no effect. Any rollback across schema versions
    /// still requires restoring the matching old-schema blob with the old
    /// binary.
    #[serde(
        default = "default_rewrite_creatives",
        skip_serializing_if = "is_default_rewrite_creatives"
    )]
    pub rewrite_creatives: bool,

    /// Wrap creative click-through links (`<a href>`, `<area href>`) in signed
    /// `/first-party/click` redirects.
    ///
    /// Unset keeps each path's existing behavior: auction creatives follow
    /// [`Self::rewrite_creatives`], and HTML fetched through
    /// `/first-party/proxy` keeps wrapping. An explicit value applies to every
    /// path. Unset is omitted from serialized config blobs, so older binaries
    /// keep loading them; any explicit value is serialized and rejected by
    /// binaries that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_clicks: Option<bool>,
```

In `impl Default for AuctionConfig`, after `rewrite_creatives: default_rewrite_creatives(),`, add `rewrite_clicks: None,`. At the top of `impl AuctionConfig`:

```rust
    /// Whether auction creatives (`POST /auction` and inline SSAT/page-bids)
    /// wrap click-through links.
    ///
    /// Unset [`Self::rewrite_clicks`] follows [`Self::rewrite_creatives`].
    #[must_use]
    pub fn rewrites_auction_clicks(&self) -> bool {
        self.rewrite_clicks.unwrap_or(self.rewrite_creatives)
    }

    /// Whether HTML fetched through `/first-party/proxy` wraps click-through
    /// links.
    ///
    /// Unset [`Self::rewrite_clicks`] keeps wrapping, as before the setting
    /// existed.
    #[must_use]
    pub fn rewrites_proxied_clicks(&self) -> bool {
        self.rewrite_clicks.unwrap_or(true)
    }
```

In `auction/orchestrator.rs`, `test_no_providers_configured`, add `rewrite_clicks: None,` after `rewrite_creatives: true,`. It is the only `AuctionConfig` literal without `..Default::default()`.

- [ ] **Step 4: Verify green**

Run: `cargo test-fastly -p trusted-server-core --lib -- auction_config_types:: rewrite_clicks test_auction_creative_processing_defaults`

Expected: `15 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/auction_config_types.rs crates/trusted-server-core/src/auction/orchestrator.rs crates/trusted-server-core/src/config_payload.rs crates/trusted-server-core/src/settings.rs
git commit --signoff -S -m "Add an auction rewrite_clicks setting that follows rewrite_creatives when unset"
```

---

### Task 3: Gate asset and click handlers in the rewrite pass

**Files:**

- Modify: `crates/trusted-server-core/src/creative.rs` (`process_*`, wrappers and their docs, `CreativeFeatures`, `rewrite_creative_html_impl`, tests)
- Modify: `crates/trusted-server-core/src/auction/formats.rs` (doc comment, debug log, test)
- Modify: `crates/trusted-server-core/src/publisher.rs` (test)
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs` (doc comment)

**Interfaces:**

- Consumes: `AuctionConfig::rewrites_auction_clicks()` (Task 2), `asset_target` and `click_target` (Task 1).
- Produces, private to `creative.rs`:

```rust
struct CreativeFeatures { assets: bool, clicks: bool }
impl CreativeFeatures { const ALL: Self; fn any(self) -> bool; }
fn rewrite_creative_html_impl(
    settings: &Settings,
    markup: &str,
    base_origin: &str,
    inject_tsjs: bool,
    max_output_size: usize,
    features: CreativeFeatures,
) -> String;
```

- [ ] **Step 1: Matrix fixtures and helpers**

At the top of `creative.rs` `mod tests`, after the `use super::{...}` block:

```rust
    use crate::settings::Settings;

    const MATRIX_FIXTURE_WITH_BODY: &str = r#"<html><head><base href="https://base.example.com/"><style>.b{background:url(https://cdn.example.com/bg.png)}</style></head><body><img src="https://cdn.example.com/ad.png" srcset="https://cdn.example.com/ad-2x.png 2x"><div style="background:url(https://cdn.example.com/inline.png)"></div><a href="https://landing.example.com/page">Ad</a><map><area href="https://landing.example.com/area"></map><a href="https://excluded.example.com/page">Excluded</a><a href="mailto:ads@example.com">Mail</a></body></html>"#;
    const MATRIX_FIXTURE_FRAGMENT: &str = r#"<base href="https://base.example.com/"><img src="https://cdn.example.com/ad.png" srcset="https://cdn.example.com/ad-2x.png 2x"><div style="background:url(https://cdn.example.com/inline.png)"></div><a href="https://landing.example.com/page">Ad</a><map><area href="https://landing.example.com/area"></map><a href="https://excluded.example.com/page">Excluded</a><a href="mailto:ads@example.com">Mail</a>"#;
    const INLINE_ORIGIN: &str = "https://www.example.com";

    #[derive(Debug, Clone, Copy)]
    enum CreativePath {
        Auction,
        Inline,
    }

    fn process_for_path(settings: &Settings, path: CreativePath, html: &str) -> String {
        match path {
            CreativePath::Auction => process_auction_creative(settings, html),
            CreativePath::Inline => {
                super::process_inline_auction_creative(settings, INLINE_ORIGIN, html)
            }
        }
    }

    fn matrix_settings(assets: bool, clicks: Option<bool>) -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.auction.sanitize_creatives = false;
        settings.auction.rewrite_creatives = assets;
        settings.auction.rewrite_clicks = clicks;
        settings.rewrite.exclude_domains = vec!["excluded.example.com".to_owned()];
        settings
    }
```

- [ ] **Step 2: Matrix and unset-pin tests**

`MATRIX_DEFAULT_AUCTION_OUTPUT` and `MATRIX_DEFAULT_INLINE_OUTPUT` are the exact output of `MATRIX_FIXTURE_WITH_BODY` on `main`, before this change. To capture them, add a throwaway test to a scratch worktree of `main` that builds the same settings without `rewrite_clicks` (`create_test_settings()`, `sanitize_creatives = false`, `rewrite_creatives = true`, `exclude_domains = ["excluded.example.com"]`). Have it print `process_auction_creative` and `process_inline_auction_creative(.., "https://www.example.com", ..)` twice each, run it with `--nocapture`, and confirm the two runs match. Then remove the scratch worktree. Signatures are deterministic for the fixed test settings, and the injected script tag carries no hash.

```rust
    #[test]
    fn asset_and_click_switches_combine_on_auction_and_inline_paths() {
        for path in [CreativePath::Auction, CreativePath::Inline] {
            for (fixture, has_body) in [
                (MATRIX_FIXTURE_WITH_BODY, true),
                (MATRIX_FIXTURE_FRAGMENT, false),
            ] {
                for (assets, clicks) in [(true, true), (true, false), (false, true), (false, false)]
                {
                    let settings = matrix_settings(assets, Some(clicks));
                    let label = format!("{path:?} assets={assets} clicks={clicks} body={has_body}");
                    let prefix = match path {
                        CreativePath::Auction => "",
                        CreativePath::Inline => INLINE_ORIGIN,
                    };

                    let out = process_for_path(&settings, path, fixture);

                    let expected_proxied = match (assets, has_body) {
                        (false, _) => 0,
                        (true, true) => 4,
                        (true, false) => 3,
                    };
                    assert_eq!(
                        out.matches("/first-party/proxy?tsurl=").count(),
                        expected_proxied,
                        "{label}: proxied asset URL count: {out}"
                    );
                    assert_eq!(
                        out.contains(&format!("src=\"{prefix}/first-party/proxy?tsurl=")),
                        assets,
                        "{label}: image proxied only when assets are on: {out}"
                    );
                    assert_eq!(
                        out.contains(r#"src="https://cdn.example.com/ad.png""#),
                        !assets,
                        "{label}: image raw only when assets are off: {out}"
                    );

                    // Each wrapped `<a>` and `<area>` carries the click URL in
                    // href and data-tsclick.
                    let expected_click_urls = if clicks { 4 } else { 0 };
                    assert_eq!(
                        out.matches("/first-party/click?tsurl=").count(),
                        expected_click_urls,
                        "{label}: click URL count: {out}"
                    );
                    assert_eq!(
                        out.contains(&format!("href=\"{prefix}/first-party/click?tsurl=")),
                        clicks,
                        "{label}: anchor wrapped only when clicks are on: {out}"
                    );
                    assert_eq!(
                        out.contains(r#"href="https://landing.example.com/page""#),
                        !clicks,
                        "{label}: anchor raw only when clicks are off: {out}"
                    );
                    assert_eq!(
                        out.contains("data-tsclick"),
                        clicks,
                        "{label}: data-tsclick only when clicks are on: {out}"
                    );
                    assert!(
                        out.contains(r#"href="https://excluded.example.com/page""#),
                        "{label}: excluded anchor always stays raw: {out}"
                    );
                    assert!(
                        out.contains(r#"href="mailto:ads@example.com""#),
                        "{label}: mailto anchor always stays raw: {out}"
                    );

                    assert_eq!(
                        out.contains("<base"),
                        !(assets || clicks),
                        "{label}: <base> removed whenever either switch is on: {out}"
                    );
                    let expected_tsjs =
                        usize::from(matches!(path, CreativePath::Auction) && (assets || clicks));
                    assert_eq!(
                        out.matches("/static/tsjs=").count(),
                        expected_tsjs,
                        "{label}: TSJS injected once on /auction when either switch is on: {out}"
                    );
                    if !assets && !clicks {
                        assert_eq!(
                            out, fixture,
                            "{label}: should pass through byte for byte with both switches off"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unset_rewrite_clicks_preserves_existing_output() {
        for path in [CreativePath::Auction, CreativePath::Inline] {
            let unset = matrix_settings(true, None);
            let explicit = matrix_settings(true, Some(true));
            assert_eq!(
                process_for_path(&unset, path, MATRIX_FIXTURE_WITH_BODY),
                process_for_path(&explicit, path, MATRIX_FIXTURE_WITH_BODY),
                "{path:?}: unset rewrite_clicks with rewrite_creatives = true should match full rewriting"
            );

            let disabled = matrix_settings(false, None);
            assert_eq!(
                process_for_path(&disabled, path, MATRIX_FIXTURE_WITH_BODY),
                MATRIX_FIXTURE_WITH_BODY,
                "{path:?}: unset rewrite_clicks with rewrite_creatives = false should pass through"
            );
        }
    }

    // Captured from `main` before `rewrite_clicks` existed: default settings must
    // keep producing these bytes on both auction paths.
    const MATRIX_DEFAULT_AUCTION_OUTPUT: &str = r#"<html><head><style>.b{background:url("/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fbg.png&tstoken=xi0bmg4Ik6UU7KsG5z_quFeN5BGt_G8bt4oxgD3HiH8")}</style></head><body><script src="/static/tsjs=tsjs-unified.min.js" id="trustedserver-js"></script><img src="/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fad.png&tstoken=FU5VJC5ElXC43PfbzzR-TBVu0h4TFKH82LkDw5NP4to" srcset="/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fad-2x.png&tstoken=2WEq5obXUSJyIHjXhx_MatPqzMJtU1_tb_ZMZTKU1m8 2x"><div style="background:url(&quot;/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Finline.png&tstoken=eV8lwVA2y6dcTtIC8RopnFTYnb1R9VZ9fqxbMaolCBY&quot;)"></div><a href="/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Fpage&tstoken=DxWOVjz0AiVAcRSPJO7I69TLT0fc8RaA7E7R0H7SmCY" data-tsclick="/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Fpage&tstoken=DxWOVjz0AiVAcRSPJO7I69TLT0fc8RaA7E7R0H7SmCY">Ad</a><map><area href="/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Farea&tstoken=n0VAVAbY32ikTeYoKdkeb-GvNxbiepF_ul97VakLqUE" data-tsclick="/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Farea&tstoken=n0VAVAbY32ikTeYoKdkeb-GvNxbiepF_ul97VakLqUE"></map><a href="https://excluded.example.com/page">Excluded</a><a href="mailto:ads@example.com">Mail</a></body></html>"#;
    const MATRIX_DEFAULT_INLINE_OUTPUT: &str = r#"<html><head><style>.b{background:url("https://www.example.com/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fbg.png&tstoken=xi0bmg4Ik6UU7KsG5z_quFeN5BGt_G8bt4oxgD3HiH8")}</style></head><body><img src="https://www.example.com/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fad.png&tstoken=FU5VJC5ElXC43PfbzzR-TBVu0h4TFKH82LkDw5NP4to" srcset="https://www.example.com/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fad-2x.png&tstoken=2WEq5obXUSJyIHjXhx_MatPqzMJtU1_tb_ZMZTKU1m8 2x"><div style="background:url(&quot;https://www.example.com/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Finline.png&tstoken=eV8lwVA2y6dcTtIC8RopnFTYnb1R9VZ9fqxbMaolCBY&quot;)"></div><a href="https://www.example.com/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Fpage&tstoken=DxWOVjz0AiVAcRSPJO7I69TLT0fc8RaA7E7R0H7SmCY" data-tsclick="https://www.example.com/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Fpage&tstoken=DxWOVjz0AiVAcRSPJO7I69TLT0fc8RaA7E7R0H7SmCY">Ad</a><map><area href="https://www.example.com/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Farea&tstoken=n0VAVAbY32ikTeYoKdkeb-GvNxbiepF_ul97VakLqUE" data-tsclick="https://www.example.com/first-party/click?tsurl=https%3A%2F%2Flanding.example.com%2Farea&tstoken=n0VAVAbY32ikTeYoKdkeb-GvNxbiepF_ul97VakLqUE"></map><a href="https://excluded.example.com/page">Excluded</a><a href="mailto:ads@example.com">Mail</a></body></html>"#;

    #[test]
    fn default_settings_rewrite_matrix_fixture_byte_for_byte_as_before() {
        let settings = matrix_settings(true, None);

        for (path, expected) in [
            (CreativePath::Auction, MATRIX_DEFAULT_AUCTION_OUTPUT),
            (CreativePath::Inline, MATRIX_DEFAULT_INLINE_OUTPUT),
        ] {
            assert_eq!(
                process_for_path(&settings, path, MATRIX_FIXTURE_WITH_BODY),
                expected,
                "{path:?}: default settings should match the pre-rewrite_clicks output exactly"
            );
        }
    }
```

- [ ] **Step 3: End-to-end tests**

`auction/formats.rs`, before `sanitize_creatives_defaults_to_disabled`:

```rust
    #[test]
    fn convert_to_openrtb_response_wraps_clicks_without_rewriting_assets() {
        let mut settings = make_settings();
        settings.auction.sanitize_creatives = false;
        settings.auction.rewrite_creatives = false;
        settings.auction.rewrite_clicks = Some(true);
        let auction_request = make_auction_request();
        let result = make_result(make_complete_creative_bid());

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert creative with click rewriting only");
        let adm = response_adm(response);

        assert!(
            adm.contains("/first-party/click?tsurl=") && adm.contains("data-tsclick"),
            "should wrap the landing link: {adm}"
        );
        assert!(
            !adm.contains("/first-party/proxy?tsurl="),
            "should not proxy any asset: {adm}"
        );
        assert!(
            adm.contains(r#"src="https://cdn.example.com/ad.png""#),
            "should keep the image URL direct: {adm}"
        );
        assert!(
            adm.contains("tsjs-unified.min.js"),
            "should inject the creative runtime for the click guard: {adm}"
        );
    }
```

`publisher.rs`, before `build_bid_map_uses_request_origin_for_inline_urls`:

```rust
        #[test]
        fn build_bid_map_keeps_inline_anchors_raw_when_clicks_are_off() {
            let mut settings = test_settings();
            settings.auction.rewrite_creatives = true;
            settings.auction.rewrite_clicks = Some(false);
            settings.publisher.domain = "example.com".to_string();

            let mut winning_bids = HashMap::new();
            let mut bid = make_bid(
                "atf_sidebar_ad",
                1.50,
                "examplessp",
                "abc123",
                "https://ssp.example.com/win",
                "https://ssp.example.com/bill",
            );
            bid.creative = Some(
                "<html><body><a href=\"https://landing.example.com/page\"><img src=\"https://cdn.example.com/pixel.png\"></a></body></html>"
                    .to_string(),
            );
            winning_bids.insert("atf_sidebar_ad".to_string(), bid);

            let map = build_bid_map(&winning_bids, PriceGranularity::Dense, &settings, "", false);
            let adm = map
                .get("atf_sidebar_ad")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("adm"))
                .and_then(|v| v.as_str())
                .expect("should include a rewritten adm");

            assert!(
                adm.contains("https://example.com/first-party/proxy?tsurl="),
                "should still proxy the image with an absolute URL, got: {adm}"
            );
            assert!(
                adm.contains("href=\"https://landing.example.com/page\""),
                "should leave the landing link raw, got: {adm}"
            );
            assert!(
                !adm.contains("data-tsclick") && !adm.contains("/first-party/click"),
                "should not wrap the landing link, got: {adm}"
            );
        }
```

- [ ] **Step 4: Verify red**

Run each test alone, as described in the environment notes. Expected:

- `asset_and_click_switches_combine_on_auction_and_inline_paths` panics with "Auction assets=true clicks=false body=true: click URL count".
- `convert_to_openrtb_response_wraps_clicks_without_rewriting_assets` panics with "should wrap the landing link". Nothing was rewritten.
- `build_bid_map_keeps_inline_anchors_raw_when_clicks_are_off` panics with "should leave the landing link raw". The anchor was wrapped.
- `unset_rewrite_clicks_preserves_existing_output` and `default_settings_rewrite_matrix_fixture_byte_for_byte_as_before` **pass**. They pin today's behavior.

- [ ] **Step 5: Add `CreativeFeatures`**

Above the `rewrite_creative_html_impl` doc comment:

```rust
/// Which rewrite features one creative pass applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CreativeFeatures {
    /// Proxy eligible asset URLs through `/first-party/proxy`.
    assets: bool,
    /// Wrap eligible click-through links in `/first-party/click`.
    clicks: bool,
}

impl CreativeFeatures {
    /// Both features on: the behavior of the public rewrite wrappers.
    const ALL: Self = Self {
        assets: true,
        clicks: true,
    };

    /// Whether the pass needs to run at all.
    fn any(self) -> bool {
        self.assets || self.clicks
    }
}
```

Extend the `rewrite_creative_html_impl` doc comment's sentence to read "...`max_output_size` bounds the rewritten result; `features` selects which handlers run. `<base>` removal and TSJS injection always run, since callers only invoke the pass when at least one feature is on." Add `features: CreativeFeatures,` as the last parameter.

- [ ] **Step 6: Build the handler vector conditionally**

Inside `rewrite_creative_html_impl`, after `let overflowed = ...;`, build the vector, then hand it to `HtmlRewriter::new`. Each block below is the existing handler moved verbatim with its comment. Only the indentation changes, so review with `git diff -w`.

```rust
    let mut element_content_handlers = vec![
        // (existing `<base>` removal handler with its comment)
        // (existing `<body>` TSJS handler)
    ];
    if features.assets {
        element_content_handlers.extend([
            // (existing handlers, unchanged and in their current order: img,
            //  script[src], link[href], video/audio/source, object[data],
            //  embed[src], input[src], SVG image/use, [style], text!("style"),
            //  iframe, [srcset], [imagesrcset])
        ]);
    }
    if features.clicks {
        element_content_handlers.push(
            // Click-through links
            element!("a[href], area[href]", |el| {
                if let Some(href) = el.get_attribute("href")
                    && let Some(target) = click_target(settings, &href)
                {
                    let click = build_click_url(settings, target.as_str(), base_origin);
                    let _ = el.set_attribute("href", &click);
                    let _ = el.set_attribute("data-tsclick", &click);
                }
                Ok(())
            }),
        );
    }
    let mut rewriter = HtmlRewriter::new(
        HtmlSettings {
            element_content_handlers,
            ..HtmlSettings::default()
        },
        // (existing output sink closure, unchanged)
    );
```

`extend([...])` with an array of `element!`/`text!` invocations type-checks with no annotations, because every invocation yields the same tuple type. The body-less TSJS fallback keeps `if inject_tsjs && !injected_ts_creative.get() && !rewritten.is_empty()`.

- [ ] **Step 7: Wire the callers**

```rust
pub(crate) fn process_auction_creative(settings: &Settings, raw: &str) -> String {
    process_auction_creative_with_rewriter(settings, raw, |sanitized, features| {
        rewrite_creative_html_impl(settings, sanitized, "", true, MAX_CREATIVE_SIZE, features)
    })
}

pub(crate) fn process_inline_auction_creative(
    settings: &Settings,
    base_origin: &str,
    raw: &str,
) -> String {
    process_auction_creative_with_rewriter(settings, raw, |sanitized, features| {
        rewrite_creative_html_impl(
            settings,
            sanitized,
            base_origin,
            false,
            MAX_CREATIVE_SIZE,
            features,
        )
    })
}
```

In `process_auction_creative_with_rewriter`, change the closure type to `rewrite: impl FnOnce(&str, CreativeFeatures) -> String`, and replace the `if settings.auction.rewrite_creatives { ... }` tail with:

```rust
    let features = CreativeFeatures {
        assets: settings.auction.rewrite_creatives,
        clicks: settings.auction.rewrites_auction_clicks(),
    };
    if features.any() {
        rewrite(&sanitized, features)
    } else {
        sanitized
    }
```

Public wrappers: pass `CreativeFeatures::ALL` as the sixth argument in `rewrite_creative_html` (`"", true, MAX_CREATIVE_SIZE`), `rewrite_inline_creative_html` (`base_origin, false, MAX_CREATIVE_SIZE`) and, for now, `rewrite_proxied_html` (`"", true, MAX_REWRITABLE_BODY_SIZE`). Task 4 changes the last one.

- [ ] **Step 8: Doc comments**

- `process_auction_creative`: "Sanitization is controlled by [`...sanitize_creatives`], asset rewriting by [`...rewrite_creatives`], and click wrapping by [`...rewrites_auction_clicks`]. With all three disabled the creative is returned exactly as the bidder sent it."
- `process_inline_auction_creative`: "Applies the same opt-in sanitization and the same asset and click switches as [`process_auction_creative`]. Proxy and click URLs are emitted as absolute URLs against `base_origin` without injecting the creative TSJS bundle."
- `rewrite_creative_html`: replace the stale `tsjs-creative` bullet with:
  - "`<a href>` / `<area href>` → `/first-party/click?tsurl=…`, copied into `data-tsclick`";
  - "Injects the unified TSJS bundle (`/static/tsjs=tsjs-unified.min.js`), whose creative runtime installs the click guard, once at the top of `<body>`."

  Add: "Rewrites assets and clicks unconditionally; the auction settings are applied by the auction processing entry points, not here."

- `rewrite_inline_creative_html`: "its click guard is unnecessary for click URLs that are already absolute here".
- `auction/endpoints.rs` response doc: "Creative HTML is inlined in each bid's `adm` field after optional sanitization ([`auction.sanitize_creatives`][...]). First-party asset rewriting ([`auction.rewrite_creatives`][...]) and click wrapping ([`auction.rewrite_clicks`][...]) are enabled by default. Bidder `<base>` removal and creative TSJS injection run when either is on." This replaces the stale "mandatory server-side sanitization".
- `auction/formats.rs`, on `convert_to_openrtb_response`: name `rewrite_clicks` next to `rewrite_creatives` and add the intra-doc link target `/// [`AuctionConfig::rewrite_clicks`]: crate::auction_config_types::AuctionConfig::rewrite_clicks`. Add `clicks {}` to the "Processed creative" debug log, fed by `settings.auction.rewrites_auction_clicks()`.
- `auction/formats.rs`, the comment above `serialize_renderer` in `convert_to_openrtb_response`: "sanitization is opt-in, asset rewriting and click wrapping are on by default, and with all three disabled the creative ships exactly as the bidder returned it."
- `creative.rs` module docs:
  - add the goal "Route click-through links through a signed first-party click redirect.", and call the proxied list "asset URLs";
  - add key behaviors for `<a href>`/`<area href>` wrapping with `data-tsclick`, for `exclude_domains` applying to assets and links, and for `<base>` removal;
  - add a "Switches" list. Auction creatives rewrite assets on `rewrite_creatives` and wrap links on `rewrites_auction_clicks`, and an unset `rewrite_clicks` follows `rewrite_creatives`. `<base>` removal and TSJS run when either is on, and with both off the pass is skipped. Proxied HTML always rewrites assets and wraps links unless `rewrite_clicks` is explicitly `false`. The public wrappers rewrite both.
  - Link the `AuctionConfig` items with reference definitions at the end of the module docs, because `creative.rs` does not import `AuctionConfig`.

- [ ] **Step 9: Verify green**

Run: `cargo fmt --all && cargo test-fastly -p trusted-server-core --lib -- creative:: auction::formats publisher::`

Expected: `554 passed; 0 failed`, including every pre-existing rewrite test.

Run: `cargo clippy-fastly`

Expected: exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/trusted-server-core/src/creative.rs crates/trusted-server-core/src/auction/formats.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/auction/endpoints.rs
git commit --signoff -S -m "Gate creative asset and click rewriting on separate switches" -m "The rewrite pass registers asset handlers only when rewrite_creatives is on and the anchor handler only when click rewriting resolves on. Base removal and TSJS injection run whenever either is on."
```

---

### Task 4: Apply an explicit `rewrite_clicks` to HTML fetched through `/first-party/proxy`

**Files:**

- Modify: `crates/trusted-server-core/src/creative.rs` (`rewrite_proxied_html`)
- Test: `crates/trusted-server-core/src/proxy.rs`

- [ ] **Step 1: Write the test**

Before `html_response_rewrite_preserves_non_standard_port`:

```rust
    #[test]
    fn proxied_html_click_wrapping_follows_explicit_rewrite_clicks() {
        let html = r#"<html><head><base href="https://base.example.com/"></head><body><a href="https://landing.example.com/page"><img src="https://cdn.example.com/ad.png"></a><a href="https://excluded.example.com/page">Excluded</a></body></html>"#;
        // (rewrite_creatives, rewrite_clicks, expect wrapped clicks)
        let cases = [
            (true, None, true),
            (false, None, true),
            (true, Some(true), true),
            (true, Some(false), false),
            (false, Some(false), false),
            (false, Some(true), true),
        ];

        for (rewrite_creatives, rewrite_clicks, expect_clicks) in cases {
            let mut settings = create_test_settings();
            settings.auction.rewrite_creatives = rewrite_creatives;
            settings.auction.rewrite_clicks = rewrite_clicks;
            settings.rewrite.exclude_domains = vec!["excluded.example.com".to_owned()];
            let label =
                format!("rewrite_creatives={rewrite_creatives} rewrite_clicks={rewrite_clicks:?}");
            let req = build_http_request(Method::GET, "https://edge.example.com/first-party/proxy");
            let mut response = build_http_response(StatusCode::OK, EdgeBody::from(html));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );

            let body = response_body_string(
                finalize(
                    &settings,
                    &req,
                    "https://cdn.example.com/creative.html",
                    response,
                )
                .expect("should finalize proxied HTML"),
            );

            assert!(
                body.contains("/first-party/proxy?tsurl="),
                "{label}: proxied HTML always proxies assets: {body}"
            );
            // A wrapped landing link carries the click URL in href and data-tsclick.
            assert_eq!(
                body.matches("/first-party/click?tsurl=").count(),
                if expect_clicks { 2 } else { 0 },
                "{label}: click wrapping in proxied HTML: {body}"
            );
            assert_eq!(
                body.matches("data-tsclick").count(),
                usize::from(expect_clicks),
                "{label}: data-tsclick in proxied HTML: {body}"
            );
            if !expect_clicks {
                let landing_href = body
                    .split("<a href=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next());
                assert_eq!(
                    landing_href,
                    Some("https://landing.example.com/page"),
                    "{label}: landing link keeps its raw href: {body}"
                );
            }
            assert!(
                body.contains(r#"<a href="https://excluded.example.com/page">Excluded</a>"#),
                "{label}: excluded link always stays raw: {body}"
            );
            assert!(
                !body.contains("<base"),
                "{label}: proxied HTML always removes <base>: {body}"
            );
            assert!(
                body.contains("/static/tsjs="),
                "{label}: proxied HTML always receives the runtime: {body}"
            );
        }
    }
```

- [ ] **Step 2: Verify red**

Run the test alone. Expected: a panic with "rewrite_creatives=true rewrite_clicks=Some(false): click wrapping in proxied HTML".

- [ ] **Step 3: Implement**

In `rewrite_proxied_html`, replace `CreativeFeatures::ALL` with:

```rust
        CreativeFeatures {
            assets: true,
            clicks: settings.auction.rewrites_proxied_clicks(),
        },
```

Append this paragraph to its doc comment: "Asset rewriting here is unconditional. Click wrapping follows an explicit `[auction] rewrite_clicks`; when that is unset, links keep being wrapped regardless of `rewrite_creatives`."

Fix the `CreativeHtmlProcessor` doc comment, which names `rewrite_creative_html` although the processor calls `rewrite_proxied_html`:

```rust
/// Stream processor for HTML fetched through `/first-party/proxy` that rewrites
/// asset and click-through URLs to first-party endpoints.
///
/// This processor buffers input chunks and processes the complete HTML document
/// when the stream ends, using [`rewrite_proxied_html`] internally. Asset URLs are
/// always proxied; links are wrapped unless `[auction] rewrite_clicks` is
/// explicitly `false`.
```

- [ ] **Step 4: Verify green**

Run: `cargo test-fastly -p trusted-server-core --lib -- proxy::tests::proxied_html proxy::tests::auction_rewrite_setting_does_not_change`

Expected: `2 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/creative.rs crates/trusted-server-core/src/proxy.rs
git commit --signoff -S -m "Apply an explicit rewrite_clicks to HTML fetched through the first-party proxy"
```

---

### Task 5: Example config and CLI overlay coverage

**Files:**

- Modify: `trusted-server.example.toml`
- Test: `crates/trusted-server-cli/tests/config_env_overlay.rs`

- [ ] **Step 1: Write the overlay tests**

Add a constant after `SANITIZE_ENV`:

```rust
const CLICKS_ENV: &str = "TRUSTED_SERVER__AUCTION__REWRITE_CLICKS";
```

Before `config_validate_explains_legacy_provider_list_migration`:

```rust
fn pushed_auction_with_env(
    project: &MigratedProject,
    key: &str,
    raw_value: &str,
) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["config", "push", "--adapter", "axum", "--manifest"])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .args(["--yes", "--no-diff"])
        .current_dir(project.directory.path())
        .env(key, raw_value)
        .output()
        .expect("should run ts config push");
    assert!(
        output.status.success(),
        "config push should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let local_store_path = project
        .directory
        .path()
        .join(".edgezero/local-config-trusted_server_config.json");
    let local_store: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(local_store_path).expect("should read pushed local config"),
    )
    .expect("should parse local config store");
    let envelope_json = local_store
        .as_object()
        .and_then(|entries| entries.values().next())
        .and_then(serde_json::Value::as_str)
        .expect("should contain a blob envelope");
    let envelope: serde_json::Value =
        serde_json::from_str(envelope_json).expect("should parse blob envelope");
    envelope["data"]["auction"].clone()
}

#[test]
fn rewrite_clicks_environment_override_applies_when_leaf_present() {
    let project = migrated_project();
    let mut document = fs::read_to_string(&project.config_path)
        .expect("should read migrated config")
        .parse::<DocumentMut>()
        .expect("should parse migrated config");
    document["auction"]["rewrite_clicks"] = value(true);
    fs::write(&project.config_path, document.to_string()).expect("should write config");

    let auction = pushed_auction_with_env(&project, CLICKS_ENV, "false");

    assert_eq!(
        auction["rewrite_clicks"],
        serde_json::Value::Bool(false),
        "pushed config should contain the rewrite_clicks environment override"
    );
}

#[test]
fn rewrite_clicks_environment_override_is_ignored_without_leaf() {
    let project = migrated_project();

    let auction = pushed_auction_with_env(&project, CLICKS_ENV, "false");

    assert!(
        auction.get("rewrite_clicks").is_none(),
        "an override for a missing leaf should be ignored and the unset default omitted"
    );
}
```

- [ ] **Step 2: Run**

Run: `cargo test --package trusted-server-cli --target "$(rustc -vV | awk '/host:/ { print $2 }')" --test config_env_overlay rewrite_clicks`

Expected: `2 passed; 0 failed`.

These pin existing EdgeZero behavior, so there is no red step. The leaf-present test proves the override is read. The leaf-absent test proves an override for a missing leaf is ignored, so the spec's overlay caveat holds. If the leaf-absent test ever fails, update the spec and the configuration guide before continuing.

- [ ] **Step 3: Example TOML**

Replace the `rewrite_creatives` comment and line in `[auction]`:

```toml
# Rewrite winning-bid creative asset URLs (images, scripts, styles, media,
# iframes, CSS url()) to first-party /first-party/proxy endpoints (default
# true). Set false to leave asset URLs direct. Bidder <base> removal and
# creative TSJS injection run when this or rewrite_clicks is on. Sanitization
# is controlled separately by `sanitize_creatives` below. Restore true before
# rolling back to an older binary that rejects unknown fields.
rewrite_creatives = true
# Wrap creative click-through links (<a href>, <area href>) in signed
# /first-party/click redirects. Unset (the default) follows rewrite_creatives
# for auction creatives and keeps wrapping links in HTML fetched through
# /first-party/proxy; a set value applies to both. Leave it commented out to
# keep that behavior: any explicit value is stored in the pushed config, and
# binaries older than this setting reject it. Uncomment it before relying on
# TRUSTED_SERVER__AUCTION__REWRITE_CLICKS, which cannot create a missing leaf.
# rewrite_clicks = true
```

In the `sanitize_creatives` comment just below, replace the sentence that starts "Note that with `rewrite_creatives = true`" so it names both switches:

```toml
# (executable markup preserved). Note that with `rewrite_creatives` or
# `rewrite_clicks` on, the adm is still not untouched: eligible asset URLs
# and/or links are rewritten, bidder `<base>` elements are removed, and the
# creative TSJS runtime is injected. Enable it whenever creatives can render
# in a context that shares the publisher origin (its primary defence there);
# leave it off when creatives render in a
```

- [ ] **Step 4: Commit**

```bash
git add trusted-server.example.toml crates/trusted-server-cli/tests/config_env_overlay.rs
git commit --signoff -S -m "Document rewrite_clicks in the example config and pin its overlay behavior"
```

---

### Task 6: Operator documentation and CHANGELOG

**Files:**

- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/creative-processing.md`
- Modify: `docs/guide/auction-orchestration.md`
- Modify: `docs/guide/api-reference.md`
- Modify: `crates/trusted-server-core/src/auction/README.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: `configuration.md` `[auction]`**

- Table: change the `rewrite_creatives` description to "Rewrite winning-bid asset URLs through first-party endpoints". Add a row:

  ```markdown
  | `rewrite_clicks` | Boolean | unset | Wrap creative links in `/first-party/click`; unset follows `rewrite_creatives` |
  ```

  Prettier re-pads the whole table.

- Processing paragraph:
  - assets go to `/first-party/proxy`;
  - `rewrite_clicks` controls `<a href>`/`<area href>` and follows `rewrite_creatives` when unset;
  - `<base>` is removed, and TSJS injected on `POST /auction`, whenever either setting is on;
  - "With all three disabled" replaces "With both disabled";
  - an explicit `rewrite_clicks` applies to links in proxied HTML, and unset keeps wrapping them.
- Warning block:
  - add "any explicit `rewrite_clicks`" to the serialized values;
  - rollback note: "Older binaries tie click wrapping to `rewrite_creatives` in both directions: rolling back from `rewrite_creatives = true` with `rewrite_clicks = false` turns click wrapping back on, and rolling back from `rewrite_creatives = false` with `rewrite_clicks = true` turns it off.";
  - overlay note: "`rewrite_clicks` is unset by default and so cannot appear as a TOML leaf until you set it; add `rewrite_clicks = true` or `false` under `[auction]` before relying on `TRUSTED_SERVER__AUCTION__REWRITE_CLICKS`."

- [ ] **Step 2: `creative-processing.md`**

- Processing Triggers, item 1, append: "Click-through links are wrapped according to `[auction].rewrite_clicks`, which follows `rewrite_creatives` when unset."
- Item 2, append: "An explicitly set `[auction].rewrite_clicks` also applies to links in proxied HTML."
- "Auction Rewrite Control" intro: "Three auction settings control the processing applied to winning-bid `adm` ... Sanitization is opt-in (default `false`) and asset rewriting is enabled by default. Click wrapping is controlled by `rewrite_clicks`, which follows `rewrite_creatives` when unset; see [Assets and clicks](#assets-and-clicks)." Add `# rewrite_clicks unset: follows rewrite_creatives` to its TOML snippet.
- Its `sanitize_creatives` × `rewrite_creatives` table: asset URLs follow `rewrite_creatives` and links follow `rewrite_clicks`.
  - `false`/`false`: "Asset URLs stay direct. With `rewrite_clicks` unset or `false`, deliver the creative exactly as the bidder returned it (subject to the size cap); with `rewrite_clicks = true`, wrap links as described in [Assets and clicks](#assets-and-clicks)."
  - `true`/`false`: "...then deliver without asset rewriting. Sanitizer-accepted external resource and inline CSS URLs remain direct; links follow `rewrite_clicks` and stay direct when it is unset or `false`."
  - `false`/`true`: "Rewrite eligible resource/CSS URLs ... removing any bidder `<base>` element. Links are wrapped unless `rewrite_clicks = false`. Executable markup is preserved."
- After the sanitization warning in "Auction Rewrite Control", add an `### Assets and clicks` subsection containing:
  - the TOML snippet (`rewrite_creatives = true`, `# rewrite_clicks = false`);
  - the four-row table (assets × clicks → asset URLs, links, `<base>`, TSJS);
  - "SSAT/page-bids follows the same table with absolute URLs and never injects TSJS. `exclude_domains` applies to both assets and links.";
  - the `clickGuard` versus `rewrite_clicks` paragraph;
  - a `renderGuard` note: "With `rewrite_creatives = false` and `rewrite_clicks = true`, `POST /auction` still injects TSJS. A creative that turns on `tsCreativeConfig.renderGuard` can therefore still send assets inserted by its own script through `/first-party/sign` and `/first-party/proxy`, even though the server left the markup's asset URLs direct."
- "Anchors (Click Tracking)": add `**Controlled by**: [auction].rewrite_clicks (follows rewrite_creatives when unset)`, switch the example to `advertiser.example.com`, and show the `data-tsclick` attribute.

- [ ] **Step 3: Remaining guides**

- `auction-orchestration.md`:
  - mermaid note: `rewrite_creatives=false: asset URLs direct; clicks follow rewrite_clicks`;
  - flow: `├─[rewrite_creatives or rewrite_clicks] Rewrite assets and/or links, inject creative TSJS`;
  - Creative Processing paragraph: link to `/guide/creative-processing#assets-and-clicks`.
- `api-reference.md`: list `[auction].rewrite_clicks` with the other two settings.
- `auction/README.md`: add the `rewrite_clicks` bullet.

- [ ] **Step 4: CHANGELOG**

Under `### Added`, as the first entry:

```markdown
- Added `[auction].rewrite_clicks` to control creative click-through wrapping (`<a href>`/`<area href>` → signed `/first-party/click`) independently of asset rewriting. Unset (the default) follows `rewrite_creatives` for `POST /auction` and SSAT/page-bids and keeps wrapping links in HTML fetched through `/first-party/proxy`, so existing configs behave as before; an explicit value applies to every path. `rewrite_creatives` now governs asset URLs only; bidder `<base>` removal and creative TSJS injection run when either setting is on. Upgrading: deploy the binary first, then push a config that sets `rewrite_clicks`. Rolling back: remove any explicit `rewrite_clicks` (and its environment override), push the resulting config, then roll back the binary. Older binaries tie click wrapping to `rewrite_creatives` in both directions, so rolling back turns clicks back on for `rewrite_creatives = true` with `rewrite_clicks = false`, and off for `rewrite_creatives = false` with `rewrite_clicks = true`.
```

In the existing Unreleased `rewrite_creatives`/`sanitize_creatives` entry, replace "(proxy/click URL conversion, bidder `<base>` removal; creative TSJS injection on `POST /auction` only)." with:

```markdown
(asset URL conversion to `/first-party/proxy`, bidder `<base>` removal; creative TSJS injection on `POST /auction` only). Click-through wrapping is controlled by `[auction].rewrite_clicks`, which follows `rewrite_creatives` when unset.
```

- [ ] **Step 5: Format**

```bash
PRETTIER=/path/to/checkout/docs/node_modules/.bin/prettier
$PRETTIER --config docs/.prettierrc --write docs/guide/configuration.md docs/guide/creative-processing.md docs/guide/auction-orchestration.md docs/guide/api-reference.md
(cd docs && npm run format)
$PRETTIER --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"
```

Expected: the first `npm run format` before `--write` flags `guide/configuration.md`, because of the new table row width. After `--write`, both checks print "All matched files use Prettier code style!".

- [ ] **Step 6: Commit**

```bash
git add docs/guide CHANGELOG.md crates/trusted-server-core/src/auction/README.md
git commit --signoff -S -m "Document the rewrite_clicks creative setting"
```

---

### Task 7: Full verification

- [ ] **Step 1: Run every CI gate**

```bash
cargo fmt --all -- --check
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm && cargo clippy-cli && cargo clippy-codegen
cargo test-fastly && cargo test-fastly-reuse && cargo test-axum && cargo test-cloudflare && cargo test-spin
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
(cd crates/trusted-server-js/lib && npx vitest run && npm run format)
(cd docs && npm run format)
```

Expected: every command exits 0. The validation run's results are in the spec handoff.

- [ ] **Step 2: Map completion criteria to tests**

| Criterion                               | Test                                                                                                                                                                                                                     |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Unset output unchanged                  | `default_settings_rewrite_matrix_fixture_byte_for_byte_as_before`, `unset_rewrite_clicks_preserves_existing_output`, existing rewrite tests, unset rows of `proxied_html_click_wrapping_follows_explicit_rewrite_clicks` |
| Four-combination matrix on both paths   | `asset_and_click_switches_combine_on_auction_and_inline_paths`                                                                                                                                                           |
| Clicks off: raw href, no `data-tsclick` | matrix, `build_bid_map_keeps_inline_anchors_raw_when_clicks_are_off`                                                                                                                                                     |
| Clicks on, assets off                   | matrix, `convert_to_openrtb_response_wraps_clicks_without_rewriting_assets`                                                                                                                                              |
| `exclude_domains` applies to anchors    | matrix (excluded anchor), `rewrite_click_urls_excludes_blacklisted_domains`, `exclude_domains_match_case_insensitively_in_the_rewrite_pass`                                                                              |
| Proxied HTML rule                       | `proxied_html_click_wrapping_follows_explicit_rewrite_clicks`                                                                                                                                                            |
| Default omitted from the blob           | `default_rewrite_clicks_is_unset_and_not_serialized`, `legacy_blob_without_rewrite_clicks_follows_rewrite_creatives`, `rewrite_clicks_environment_override_is_ignored_without_leaf`                                      |
| Docs and CHANGELOG                      | Task 6                                                                                                                                                                                                                   |

- [ ] **Step 3: Hand off**

Open the PR through the `pr-creator` agent. Link the spec, state whether this PR introduced the shared step or rebased onto #1231's, and paste the rollout and rollback order from the spec.
