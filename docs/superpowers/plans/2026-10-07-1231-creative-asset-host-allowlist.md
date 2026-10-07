# Creative asset host allowlist implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `[rewrite] include_domains`, an asset host allowlist for the creative rewrite pass and `/first-party/sign`, and make `exclude_domains` matching case-insensitive.

**Architecture:** Replace `creative::to_abs` with a pure normalizer, `normalize_creative_url`, and move host policy onto `Rewrite` as `should_proxy_asset` and `should_wrap_click`, built on the existing `proxy::is_host_allowed` and `proxy::is_host_permitted`. Asset call sites go through `asset_target`, the anchor handler through `click_target`, and `/first-party/sign` pairs the normalizer with `should_proxy_asset`. The include list is then one clause in `should_proxy_asset`.

**Tech Stack:** Rust 2024 (`trusted-server-core`), `url`, `lol_html`, `cssparser`, `serde`, `validator` 0.20.

**Spec:** `docs/superpowers/specs/2026-10-07-1231-creative-asset-host-allowlist-design.md`

**Status:** Validated. Every code block below was implemented on `7a0ecb4c`, driven red then green, and passed the gates in Task 7. Line numbers are against `7a0ecb4c`.

## Global Constraints

- Issue: [IABTechLab/trusted-server#1231](https://github.com/IABTechLab/trusted-server/issues/1231). Sibling: #1234.
- Setting: `[rewrite] include_domains`, `Vec<String>`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`. Never named `allowed_domains`.
- Empty `include_domains` keeps today's rewritten output byte-for-byte for every existing fixture.
- `exclude_domains` wins over `include_domains`. `include_domains` never applies to `<a href>` or `<area href>`.
- Pattern syntax for both lists: exact host, or `*.example.com` matching the apex and any subdomain. Case-insensitive, dot boundary, matched against `Url::host_str()` only.
- Load-time normalization: trim and ASCII-lowercase both lists. `exclude_domains` drops `""` and `*` with a `log::warn!`. `include_domains` rejects `""`, `*`, `*.` and any `*` other than a leading `*.` with a validation error.
- `/first-party/sign`: an excluded or off-list host returns `TrustedServerError::Proxy { message: "unsupported url" }` (`502`) for absolute and `//` input, through GET and POST. That check runs before `proxy.allowed_domains`; `403` stays reserved for `proxy.allowed_domains`.
- No JS, adapter, routing or dependency changes.
- Fictional data only: `example.com`, `example.net`, `example.org` hosts.
- Rust conventions from `AGENTS.md`: `expect("should ...")`, no `unwrap()` outside tests, no local `use` inside functions, comments above code, descriptive assertion messages, doc comments on public items.
- Tests build `Rewrite` with `Rewrite::default()` and push or extend its lists. Never write a `Rewrite { .. }` literal, and never assign a whole field on a `Default::default()` value: `clippy::field_reassign_with_default` rejects it under `-D warnings`.
- Commits: sentence case, imperative, no `fix:`-style prefixes, `--signoff -S`, no AI attribution lines.

## Coordination with #1234 (read before Task 1)

Both #1231 and #1234 rely on one shared refactor, reproduced verbatim in [Shared step](#shared-step). Whichever implementation PR lands first ships it. Tasks 1 and 2 are that shared step.

Before starting, check `main`:

```bash
git fetch origin main
git grep -n "fn normalize_creative_url" origin/main -- crates/trusted-server-core/src/creative.rs
git grep -n "fn should_proxy_asset" origin/main -- crates/trusted-server-core/src/settings.rs
```

- **Both found** (#1234 landed first): rebase onto `origin/main`, **skip Tasks 1 and 2**, and start at Task 3. Diff the merged code against [Shared step](#shared-step); if names differ, use the merged names and note it in the PR description.
- **Neither found:** do Tasks 1 and 2. In the PR description, say that this PR introduces the shared step and that #1234 should rebase and skip its equivalent.
- **Only one found:** stop and ask the coordinator; the split landed partially.

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

CHANGELOG: the shared step's entry goes under `## [Unreleased]` › `### Fixed`.

## File map

| File                                                             | Responsibility in this change                                                             |
| ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/settings.rs`                     | `Rewrite` fields, normalization, validation, policy methods, their unit tests.            |
| `crates/trusted-server-core/src/creative.rs`                     | `normalize_creative_url`, `asset_target`, `click_target`, call sites, rewrite-pass tests. |
| `crates/trusted-server-core/src/proxy.rs`                        | `is_host_permitted` visibility, `/first-party/sign` normalization and policy, sign tests. |
| `crates/trusted-server-core/src/config_payload.rs`               | Blob default-omission and round-trip tests.                                               |
| `docs/guide/*.md`, `trusted-server.example.toml`, `CHANGELOG.md` | Operator documentation.                                                                   |

Run Rust test steps with `cargo test-fastly -- <filters>`. Core tests run on `wasm32-wasip1` under Viceroy, which must be on `PATH` (`export PATH="$HOME/.cargo/bin:$PATH"` if `cargo install` put it there). A wasm test panic aborts the whole test binary, so when one test fails, the remaining tests in that binary do not run; filter to the tests you are working on, and rerun with `--nocapture` to see the panic message above the wasm backtrace.

---

### Task 1: Host policy methods and case-insensitive `exclude_domains` (shared step, part 1)

Skip if `fn should_proxy_asset` is already on `main` (see Coordination).

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs:26` (import), `:645-670` (`impl Rewrite`), `:2988-2990` (`normalize_deserialized`)
- Test: `settings.rs` tests, after `test_rewrite_is_excluded` (`:6523`)

**Interfaces:**

- Consumes: `crate::proxy::is_host_allowed(host: &str, pattern: &str) -> bool` (`proxy.rs:1242`, `pub(crate)`).
- Produces: `pub fn Rewrite::should_proxy_asset(&self, host: &str) -> bool`, `pub fn Rewrite::should_wrap_click(&self, host: &str) -> bool`, private `fn Rewrite::is_excluded_host(&self, host: &str) -> bool`, private `fn Rewrite::normalize(&mut self)`.

- [ ] **Step 1: Write the failing tests**

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
```

```rust
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
```

```rust
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
```

```rust
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

`settings_load_normalizes_rewrite_exclude_domains` is the only test here that proves `normalize` is wired into settings loading: the matcher is case-insensitive on its own, so the mixed-case TOML test would pass even without the `normalize_deserialized` call.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test-fastly -- rewrite_policy_matches rewrite_normalize_trims rewrite_exclude_domains_match settings_load_normalizes`

Expected: compile errors, `no method named should_proxy_asset found for struct settings::Rewrite` (and `should_wrap_click`, `normalize`).

- [ ] **Step 3: Implement**

Add the import and the methods from [Shared step](#shared-step) (`use crate::proxy::is_host_allowed;`, `should_proxy_asset`, `should_wrap_click`, `is_excluded_host`, `normalize`), and the `self.rewrite.normalize();` call. For this task only, keep `is_excluded` in place above the new methods; its two callers still exist until Task 2.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test-fastly -- rewrite_policy_matches rewrite_normalize_trims rewrite_exclude_domains_match settings_load_normalizes test_rewrite_is_excluded`

Expected: `5 passed`.

- [ ] **Step 5: Lint**

Run: `cargo fmt --all && cargo clippy-fastly`

Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/settings.rs
git commit --signoff -S -m "Add rewrite host policy methods and case-insensitive exclude matching"
```

---

### Task 2: Replace `to_abs` with `normalize_creative_url` (shared step, part 2)

Skip if `fn normalize_creative_url` is already on `main` (see Coordination).

**Files:**

- Modify: `crates/trusted-server-core/src/creative.rs:25-27`, `:52-75`, `:402-409`, `:639-641`, `:726-727`, `:1285-1287`, `:1497-1501` (test imports)
- Modify: `crates/trusted-server-core/src/proxy.rs:1672-1726`
- Modify: `crates/trusted-server-core/src/settings.rs` (remove `is_excluded` and `test_rewrite_is_excluded`)
- Test: `creative.rs` tests (`:1699-1738`, `:3418-3504`), `proxy.rs` `proxy_sign_rejects_excluded_urls` (`:2692-2715`)

**Interfaces:**

- Consumes: `Rewrite::should_proxy_asset`, `Rewrite::should_wrap_click` from Task 1.
- Produces: `pub(super) fn normalize_creative_url(url: &str) -> Option<url::Url>` (visible crate-wide, as `to_abs` was); private `fn asset_target(settings: &Settings, raw: &str) -> Option<url::Url>` and `fn click_target(settings: &Settings, raw: &str) -> Option<url::Url>`. Signatures of `proxy_if_abs`, `proxied_attr_value`, `rewrite_srcset`, `CssUrlRewriter::rewrite` and the `build_*_url` helpers do not change.

- [ ] **Step 1: Write the failing tests**

In `creative.rs` tests, change the `use super::{...}` list (`:1497-1501`) to:

```rust
    use super::{
        CreativeCssProcessor, StreamProcessor as _, normalize_creative_url,
        process_auction_creative, proxy_if_abs, rewrite_creative_html,
        rewrite_inline_creative_html, rewrite_srcset, rewrite_style_urls, sanitize_creative_html,
    };
```

Replace `to_abs_conversions` and `to_abs_preserves_port_in_protocol_relative` (`:1699-1738`) with:

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

Delete `to_abs_additional_cases` (`:3418-3428`); its cases are in the table above.

Replace `to_abs_respects_exclude_domains` and `to_abs_respects_wildcard_domains` (`:3445-3504`) with:

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
```

```rust
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
```

The unparseable test asserts `contains`, not equality: `rewrite_creative_html` injects the TSJS script tag even when the markup has no `<body>`, so the output is never equal to the input.

```rust
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

These two cover the click path, which `proxy_if_abs_respects_exclude_domains` and the asset-only unparseable test do not, and mixed-case entries that bypass load-time normalization because the test sets them directly.

In `proxy.rs`, replace `proxy_sign_rejects_excluded_urls` (`:2692-2715`):

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
```

Directly after it, add:

```rust
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

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test-fastly -- normalize_creative_url proxy_if_abs_respects unparseable_absolute exclude_domains_match_case proxy_sign_rejects_excluded`

Expected: `error[E0432]: unresolved import super::normalize_creative_url`. `unparseable_absolute_click_url_is_left_byte_identical`, `exclude_domains_match_case_insensitively_in_the_rewrite_pass` and `proxy_sign_rejects_excluded_urls_case_insensitively` use only existing APIs. Against the pre-change code they fail at runtime: the anchor gains `data-tsclick`, the mixed-case-excluded link is wrapped, and sign returns `200`. Run each alone with `--exact <name> --nocapture` to see the panic.

- [ ] **Step 3: Implement the normalizer, helpers and call sites**

Apply the `creative.rs` parts of [Shared step](#shared-step): `normalize_creative_url`, `asset_target`, `click_target`, the four call sites and the module doc.

- [ ] **Step 4: Migrate `/first-party/sign`**

Apply the `proxy.rs` parts of [Shared step](#shared-step): the new normalization block, and `target` in the `base`/`proxied` lines. `is_host_permitted` stays private until Task 4.

- [ ] **Step 5: Remove `is_excluded`**

Delete `Rewrite::is_excluded` (with its `#[allow(dead_code)]`) and the `test_rewrite_is_excluded` test (`settings.rs:6522-6546`); Task 1's tests cover its cases.

Run: `git grep -n "to_abs\|is_excluded(" crates/`

Expected: no matches.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test-fastly -- normalize_creative_url proxy_if_abs_respects unparseable_absolute exclude_domains_match_case proxy_sign_`

Expected: `12 passed` (the seven new or changed tests plus the five existing `proxy_sign_*` tests).

Then prove empty-list parity with the full suite:

Run: `cargo test-fastly`

Expected: all pass with no edits to any existing rewrite-output assertion. If one fails, the refactor changed output; fix the code, not the assertion.

- [ ] **Step 7: Lint**

Run: `cargo fmt --all && cargo clippy-fastly`

Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/creative.rs crates/trusted-server-core/src/proxy.rs crates/trusted-server-core/src/settings.rs
git commit --signoff -S -m "Split creative URL normalization from rewrite host policy" -m "Replace to_abs with normalize_creative_url and route asset, click and first-party sign decisions through Rewrite::should_proxy_asset and Rewrite::should_wrap_click."
```

---

### Task 3: `include_domains` setting, validation and blob compatibility

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs:636-643` (`struct Rewrite`), `Rewrite::normalize`, new validator before `validate_publisher_domain` (`:3384`)
- Test: `settings.rs` tests (before `test_auction_creative_processing_defaults_when_omitted`, `:6549`), `crates/trusted-server-core/src/config_payload.rs` tests (before `disabled_rewrite_creatives_survives_blob_round_trip`, `:739`)

**Interfaces:**

- Consumes: `Rewrite::normalize` from Task 1.
- Produces: `pub include_domains: Vec<String>` on `Rewrite`; private `fn validate_include_domains(patterns: &[String]) -> Result<(), ValidationError>`.

- [ ] **Step 1: Write the failing tests**

In `settings.rs` tests:

```rust
    #[test]
    fn rewrite_include_domains_are_normalized_from_toml() {
        let toml_str = crate_test_settings_str()
            + r#"
            [rewrite]
            include_domains = ["*.CDN.Example.com", "  img.example.net "]
            "#;

        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");

        assert_eq!(
            settings.rewrite.include_domains,
            vec!["*.cdn.example.com".to_owned(), "img.example.net".to_owned()],
            "should trim and lowercase include_domains entries"
        );
    }
```

```rust
    #[test]
    fn rewrite_include_domains_reject_malformed_entries() {
        for entry in ["", "   ", "*", "*.", "cdn.*.example.com", "**.example.com"] {
            let toml_str = crate_test_settings_str()
                + &format!("\n[rewrite]\ninclude_domains = [\"{entry}\"]\n");

            let result = Settings::from_toml(&toml_str);

            assert!(
                result.is_err(),
                "should reject include_domains entry `{entry}`"
            );
        }
    }
```

```rust
    #[test]
    fn rewrite_include_domains_default_to_empty() {
        let settings = create_test_settings();

        assert!(
            settings.rewrite.include_domains.is_empty(),
            "should default include_domains to an empty list"
        );
    }
```

In `config_payload.rs` tests:

```rust
    #[test]
    fn legacy_blob_without_rewrite_include_domains_loads_with_empty_list() {
        let data =
            serde_json::to_value(test_settings()).expect("should serialize settings to JSON");
        let rewrite = data
            .get("rewrite")
            .and_then(serde_json::Value::as_object)
            .expect("should serialize rewrite settings as an object");
        assert!(
            !rewrite.contains_key("include_domains"),
            "should omit the default include list from the payload"
        );

        let reconstructed = load_settings(&envelope_json(&test_settings()))
            .expect("should reconstruct settings without include_domains");

        assert!(
            reconstructed.rewrite.include_domains.is_empty(),
            "should load an empty include list from a blob without the key"
        );
    }
```

```rust
    #[test]
    fn rewrite_include_domains_survive_blob_round_trip() {
        let mut original = test_settings();
        original.rewrite.include_domains = vec!["*.cdn.example.com".to_owned()];

        let reconstructed = load_settings(&envelope_json(&original))
            .expect("should reconstruct settings with include_domains");

        assert_eq!(
            reconstructed.rewrite.include_domains,
            vec!["*.cdn.example.com".to_owned()],
            "should preserve the configured include list"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test-fastly -- rewrite_include_domains legacy_blob_without_rewrite`

Expected: five `error[E0609]: no field include_domains on type settings::Rewrite`.

- [ ] **Step 3: Implement**

Replace `struct Rewrite` (`settings.rs:636-643`):

```rust
#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Rewrite {
    /// Hosts never rewritten, for assets and click-through links.
    ///
    /// Each entry is an exact host or `*.example.com`, which matches the apex
    /// and any subdomain. Matching is case-insensitive. Wins over
    /// [`Self::include_domains`].
    #[serde(default)]
    pub exclude_domains: Vec<String>,
    /// When non-empty, the only hosts whose asset URLs are rewritten to
    /// `/first-party/proxy`.
    ///
    /// Same pattern syntax as [`Self::exclude_domains`]. Empty (the default)
    /// proxies every eligible asset URL. Never applies to click-through links.
    /// The default is omitted from serialized config so older binaries, which
    /// reject unknown fields, can still read it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(custom(function = validate_include_domains))]
    pub include_domains: Vec<String>,
}
```

At the end of `Rewrite::normalize`, after the `exclude_domains` warning block:

```rust
        self.include_domains = self
            .include_domains
            .iter()
            .map(|pattern| pattern.trim().to_ascii_lowercase())
            .collect();
```

Before `validate_publisher_domain`:

```rust
/// Rejects `rewrite.include_domains` entries that cannot name a host.
///
/// Runs after [`Rewrite::normalize`], so entries are already trimmed and
/// lowercased. An entry must be an exact host or `*.` followed by a non-empty
/// suffix with no further `*`.
fn validate_include_domains(patterns: &[String]) -> Result<(), ValidationError> {
    for pattern in patterns {
        let suffix = pattern.strip_prefix("*.").unwrap_or(pattern);
        if suffix.is_empty() || suffix.contains('*') {
            return Err(ValidationError::new("invalid_rewrite_include_domain"));
        }
    }
    Ok(())
}
```

The `validator` derive passes `&Vec<String>`, which coerces to `&[String]`, so no `clippy::ptr_arg` allowance is needed.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test-fastly -- rewrite_include_domains legacy_blob_without_rewrite`

Expected: `6 passed` (the five new tests plus `legacy_blob_without_rewrite_creatives_preserves_rewriting`).

- [ ] **Step 5: Lint**

Run: `cargo fmt --all && cargo clippy-fastly`

Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/config_payload.rs
git commit --signoff -S -m "Add rewrite include_domains setting with validation"
```

---

### Task 4: Apply `include_domains` to asset rewriting and `/first-party/sign`

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs` (import, `Rewrite::should_proxy_asset`)
- Modify: `crates/trusted-server-core/src/proxy.rs:1228` (`is_host_permitted` visibility), decline log line
- Test: `settings.rs` tests, `creative.rs` tests (before `rewrite_html_excludes_blacklisted_domains`), `proxy.rs` tests (before `proxy_sign_inherits_the_request_scheme_for_protocol_relative_urls`)

**Interfaces:**

- Consumes: `Rewrite::include_domains` (Task 3); `proxy::is_host_permitted` (made `pub(crate)` in Step 3); `rewrite_creative_html`, `rewrite_inline_creative_html(settings, base_origin, markup)`, `rewrite_proxied_html`, `rewrite_css_body(settings, css) -> Result<String, CssRewriteError>`; `build_proxy_sign_request(method, uri, target)` and `noop_services()` (existing `proxy.rs` test helpers).
- Produces: `should_proxy_asset` with the include clause. No new public items.

- [ ] **Step 1: Write the failing tests**

In `settings.rs` tests:

```rust
    #[test]
    fn rewrite_should_proxy_asset_honors_include_list() {
        let mut rewrite = Rewrite::default();
        rewrite
            .include_domains
            .extend(["*.example.com", "img.example.net"].map(str::to_owned));
        rewrite
            .exclude_domains
            .push("blocked.example.com".to_owned());

        for (host, expected) in [
            ("example.com", true),
            ("a.b.example.com", true),
            ("img.example.net", true),
            ("IMG.EXAMPLE.NET", true),
            ("evil-example.com", false),
            ("cdn.example.org", false),
            ("blocked.example.com", false),
        ] {
            assert_eq!(
                rewrite.should_proxy_asset(host),
                expected,
                "should_proxy_asset(`{host}`) should be {expected}"
            );
        }
        for host in ["cdn.example.org", "evil-example.com", "example.com"] {
            assert!(
                rewrite.should_wrap_click(host),
                "should wrap click for `{host}` regardless of include_domains"
            );
        }
        assert!(
            !rewrite.should_wrap_click("blocked.example.com"),
            "should not wrap a click to an excluded host"
        );
    }
```

In `creative.rs`, add `rewrite_css_body` and `rewrite_proxied_html` to the test `use super::{...}` list, giving:

```rust
    use super::{
        CreativeCssProcessor, StreamProcessor as _, normalize_creative_url,
        process_auction_creative, proxy_if_abs, rewrite_creative_html, rewrite_css_body,
        rewrite_inline_creative_html, rewrite_proxied_html, rewrite_srcset, rewrite_style_urls,
        sanitize_creative_html,
    };
```

Then add, before `rewrite_html_excludes_blacklisted_domains`:

```rust
    const ALLOWLIST_LISTED: &[&str] = &[
        "https://assets.example.com/a.css",
        "https://assets.example.com/p1.png",
        "https://assets.example.com/a.js",
        "https://assets.example.com/i.css",
        "https://assets.example.com/bg.png",
        "https://assets.example.com/set.png",
        "https://assets.example.com/img.png",
        "https://assets.example.com/lazy.png",
        "https://assets.example.com/s1.png",
        "https://assets.example.com/v.mp4",
        "https://assets.example.com/o.swf",
        "https://assets.example.com/e.swf",
        "https://assets.example.com/btn.png",
        "https://assets.example.com/icon.svg",
        "https://assets.example.com/sprite.svg",
        "https://assets.example.com/frame.html",
        "https://assets.example.com/inline.png",
    ];

    const ALLOWLIST_OFF_LIST: &[&str] = &[
        "https://cdn.example.net/a.css",
        "https://cdn.example.net/p2.png",
        "https://cdn.example.net/a.js",
        "https://cdn.example.net/i.css",
        "https://cdn.example.net/bg.png",
        "https://cdn.example.net/set.png",
        "https://cdn.example.net/img.png",
        "https://cdn.example.net/lazy.png",
        "https://cdn.example.net/s2.png",
        "https://cdn.example.net/v.mp4",
        "https://cdn.example.net/o.swf",
        "https://cdn.example.net/e.swf",
        "https://cdn.example.net/btn.png",
        "https://cdn.example.net/icon.svg",
        "https://cdn.example.net/sprite.svg",
        "https://cdn.example.net/frame.html",
        "https://cdn.example.net/inline.png",
    ];

    const ALLOWLIST_CREATIVE: &str = r#"<html><head>
<link rel="stylesheet" href="https://assets.example.com/a.css">
<link rel="stylesheet" href="https://cdn.example.net/a.css">
<link rel="preload" as="image" href="https://assets.example.com/p1.png" imagesrcset="https://assets.example.com/p1.png 1x, https://cdn.example.net/p2.png 2x">
<script src="https://assets.example.com/a.js"></script>
<script src="https://cdn.example.net/a.js"></script>
<style>
@import "https://assets.example.com/i.css";
@import "https://cdn.example.net/i.css";
.a { background: url(https://assets.example.com/bg.png); }
.b { background: url(https://cdn.example.net/bg.png); }
.c { background-image: image-set("https://assets.example.com/set.png" 1x, "https://cdn.example.net/set.png" 2x); }
</style>
</head><body>
<img src="https://assets.example.com/img.png" data-src="https://assets.example.com/lazy.png" srcset="https://assets.example.com/s1.png 1x, https://cdn.example.net/s2.png 2x">
<img src="https://cdn.example.net/img.png" data-src="https://cdn.example.net/lazy.png">
<video src="https://assets.example.com/v.mp4"></video>
<video src="https://cdn.example.net/v.mp4"></video>
<object data="https://assets.example.com/o.swf"></object>
<object data="https://cdn.example.net/o.swf"></object>
<embed src="https://assets.example.com/e.swf">
<embed src="https://cdn.example.net/e.swf">
<input type="image" src="https://assets.example.com/btn.png">
<input type="image" src="https://cdn.example.net/btn.png">
<svg><image href="https://assets.example.com/icon.svg"></image><use xlink:href="https://assets.example.com/sprite.svg"></use></svg>
<svg><image href="https://cdn.example.net/icon.svg"></image><use xlink:href="https://cdn.example.net/sprite.svg"></use></svg>
<iframe src="https://assets.example.com/frame.html"></iframe>
<iframe src="https://cdn.example.net/frame.html"></iframe>
<div style="background: url(https://assets.example.com/inline.png)"></div>
<div style="background: url(https://cdn.example.net/inline.png)"></div>
<a href="https://landing.example.net/offer">Offer</a>
<map><area href="https://landing.example.org/area" alt="Area"></map>
</body></html>"#;

    fn encoded_tsurl(url: &str) -> String {
        format!(
            "tsurl={}",
            url::form_urlencoded::byte_serialize(url.as_bytes()).collect::<String>()
        )
    }

    fn allowlist_settings() -> crate::settings::Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.include_domains = vec!["*.example.com".to_owned()];
        settings
    }

    fn assert_allowlist_applied(label: &str, out: &str) {
        for listed in ALLOWLIST_LISTED {
            assert!(
                out.contains(&encoded_tsurl(listed)),
                "{label}: should proxy listed URL `{listed}`: {out}"
            );
        }
        for off_list in ALLOWLIST_OFF_LIST {
            assert!(
                !out.contains(&encoded_tsurl(off_list)),
                "{label}: should not proxy off-list URL `{off_list}`: {out}"
            );
            assert!(
                out.contains(off_list),
                "{label}: should keep off-list URL `{off_list}` raw: {out}"
            );
        }
        for landing in [
            "https://landing.example.net/offer",
            "https://landing.example.org/area",
        ] {
            assert!(
                out.contains(&format!("/first-party/click?{}", encoded_tsurl(landing))),
                "{label}: should wrap click `{landing}` regardless of include_domains: {out}"
            );
        }
        assert_eq!(
            out.matches("data-tsclick=").count(),
            2,
            "{label}: should mark both click-through links"
        );
    }

    #[test]
    fn include_domains_limit_asset_rewriting_on_every_html_path() {
        let settings = allowlist_settings();

        assert_allowlist_applied(
            "auction",
            &rewrite_creative_html(&settings, ALLOWLIST_CREATIVE),
        );
        assert_allowlist_applied(
            "inline",
            &rewrite_inline_creative_html(&settings, "https://www.example.com", ALLOWLIST_CREATIVE),
        );
        assert_allowlist_applied(
            "proxied",
            &rewrite_proxied_html(&settings, ALLOWLIST_CREATIVE),
        );
    }

    #[test]
    fn include_domains_limit_css_body_rewriting() {
        let settings = allowlist_settings();
        let css = r#"@import "https://assets.example.com/i.css";
@import "https://cdn.example.net/i.css";
.a { background: url(https://assets.example.com/bg.png); }
.b { background: url(https://cdn.example.net/bg.png); }"#;

        let out = rewrite_css_body(&settings, css).expect("should rewrite CSS body");

        for listed in [
            "https://assets.example.com/i.css",
            "https://assets.example.com/bg.png",
        ] {
            assert!(
                out.contains(&encoded_tsurl(listed)),
                "should proxy listed CSS URL `{listed}`: {out}"
            );
        }
        for off_list in [
            "https://cdn.example.net/i.css",
            "https://cdn.example.net/bg.png",
        ] {
            assert!(
                out.contains(off_list) && !out.contains(&encoded_tsurl(off_list)),
                "should keep off-list CSS URL `{off_list}` raw: {out}"
            );
        }
    }
```

```rust
    #[test]
    fn host_in_both_lists_is_left_alone() {
        let mut settings = allowlist_settings();
        settings.rewrite.exclude_domains = vec!["assets.example.com".to_owned()];
        let html = r#"<img src="https://assets.example.com/img.png"><a href="https://assets.example.com/landing">x</a>"#;

        let out = rewrite_creative_html(&settings, html);

        assert!(
            out.contains(r#"<img src="https://assets.example.com/img.png">"#),
            "should leave an asset on a host in both lists raw: {out}"
        );
        assert!(
            out.contains(r#"<a href="https://assets.example.com/landing">"#)
                && !out.contains("data-tsclick"),
            "should leave an excluded click-through link raw: {out}"
        );
    }
```

In `proxy.rs` tests:

```rust
    #[test]
    fn proxy_sign_applies_rewrite_include_domains_for_get_and_post() {
        struct Case {
            name: &'static str,
            include_domains: &'static [&'static str],
            exclude_domains: &'static [&'static str],
            allowed_domains: &'static [&'static str],
            target: &'static str,
            expected: StatusCode,
        }

        let cases = [
            Case {
                name: "listed absolute",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "https://img.example.com/a.png",
                expected: StatusCode::OK,
            },
            Case {
                name: "listed protocol-relative",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "//img.example.com/a.png",
                expected: StatusCode::OK,
            },
            Case {
                name: "off-list absolute",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "https://img.example.net/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "off-list protocol-relative",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "//img.example.net/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "host in both lists",
                include_domains: &["*.example.com"],
                exclude_domains: &["img.example.com"],
                allowed_domains: &[],
                target: "https://img.example.com/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "off include list and off proxy allowlist",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &["*.example.org"],
                target: "https://img.example.net/a.png",
                expected: StatusCode::BAD_GATEWAY,
            },
            Case {
                name: "listed but off proxy allowlist",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &["*.example.org"],
                target: "https://img.example.com/a.png",
                expected: StatusCode::FORBIDDEN,
            },
            Case {
                name: "mixed-case host",
                include_domains: &["*.example.com"],
                exclude_domains: &[],
                allowed_domains: &[],
                target: "https://IMG.Example.COM/a.png",
                expected: StatusCode::OK,
            },
        ];

        let to_owned = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| (*value).to_owned()).collect()
        };

        futures::executor::block_on(async {
            for case in &cases {
                for method in [&Method::GET, &Method::POST] {
                    let label = format!("{} {}", method.as_str(), case.name);
                    let mut settings = create_test_settings();
                    settings.rewrite.include_domains = to_owned(case.include_domains);
                    settings.rewrite.exclude_domains = to_owned(case.exclude_domains);
                    settings.proxy.allowed_domains = to_owned(case.allowed_domains);
                    let req = build_proxy_sign_request(
                        method,
                        "https://edge.example.com/first-party/sign",
                        case.target,
                    );

                    let status =
                        match handle_first_party_proxy_sign(&settings, &noop_services(), req).await
                        {
                            Ok(response) => response.status(),
                            Err(error) => error.current_context().status_code(),
                        };

                    assert_eq!(
                        status, case.expected,
                        "{label} should return {}",
                        case.expected
                    );
                }
            }
        });
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run each separately, because a wasm panic stops the binary:

```bash
cargo test-fastly -- rewrite_should_proxy_asset_honors --nocapture
cargo test-fastly -- include_domains_limit_asset --nocapture
cargo test-fastly -- include_domains_limit_css --nocapture
cargo test-fastly -- proxy_sign_applies_rewrite_include_domains --nocapture
```

Expected failures:

- `should_proxy_asset(`evil-example.com`) should be false`
- `auction: should not proxy off-list URL `https://cdn.example.net/a.css``
- `should keep off-list CSS URL `https://cdn.example.net/i.css` raw`
- `GET off-list absolute should return 502 Bad Gateway`

`host_in_both_lists_is_left_alone` already passes here: `exclude_domains` wins without the include clause. It guards precedence after Step 3.

- [ ] **Step 3: Implement the include clause**

In `proxy.rs:1228`, widen `is_host_permitted`'s visibility only. This belongs to the include clause, not the shared step:

```rust
pub(crate) fn is_host_permitted<S: AsRef<str>>(allowed_domains: &[S], host: &str) -> bool {
```

In `settings.rs`, widen the import:

```rust
use crate::proxy::{is_host_allowed, is_host_permitted};
```

Replace `should_proxy_asset` and its doc comment:

```rust
    /// Returns `true` when an asset URL on `host` should be rewritten to
    /// `/first-party/proxy`.
    ///
    /// The host must not match [`Self::exclude_domains`], and must match
    /// [`Self::include_domains`] when that list is non-empty. Matching is
    /// case-insensitive; see [`is_host_allowed`] for the pattern rules.
    #[must_use]
    pub fn should_proxy_asset(&self, host: &str) -> bool {
        !self.is_excluded_host(host) && is_host_permitted(&self.include_domains, host)
    }
```

`is_host_permitted` returns `true` for an empty list, which is the "include list empty" half of the rule.

In `proxy.rs`, update the decline log in `handle_first_party_proxy_sign`:

```rust
        log::debug!(
            "sign request for `{host}` declined: host excluded or not in rewrite.include_domains"
        );
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test-fastly -- rewrite_should_proxy_asset_honors include_domains_limit host_in_both_lists proxy_sign_`

Expected: `12 passed`.

- [ ] **Step 5: Run the full core suite and lint**

Run: `cargo fmt --all && cargo test-fastly && cargo clippy-fastly`

Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/creative.rs crates/trusted-server-core/src/proxy.rs
git commit --signoff -S -m "Limit creative asset rewriting to rewrite include_domains" -m "Apply the include list to every asset handler and to first-party sign. Click-through links and fetch-time proxy checks are unchanged."
```

---

### Task 5: Rewrite `<link imagesrcset>` once on the inline path

On `<link rel="preload" imagesrcset>`, both the `link[href]` handler and the standalone `[imagesrcset]` handler rewrite `imagesrcset`. On the root-relative paths (`/auction`, proxied HTML) the second pass sees `/first-party/proxy?...`, a relative URL, and leaves it alone. On the inline path the first pass emits absolute `https://<base_origin>/first-party/proxy?...` URLs, so the second pass proxies them again, producing duplicated `tsurl` and `tstoken` parameters. The fix removes the `imagesrcset` handling from `link[href]`. `[imagesrcset]` already rewrites it once on every element.

**Files:**

- Modify: `crates/trusted-server-core/src/creative.rs` (`link[href]` handler in `rewrite_creative_html_impl`, `:1220-1242`)
- Test: `creative.rs` tests, before `host_in_both_lists_is_left_alone`

**Interfaces:** none change.

- [ ] **Step 1: Write the failing test and the root-relative guard**

```rust
    // `imagesrcset` is rewritten only by the `[imagesrcset]` handler. When the
    // `link[href]` handler also rewrote it, the inline path's absolute proxy
    // URLs were proxied a second time.
    #[test]
    fn inline_link_imagesrcset_is_proxied_once() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<link rel="preload" as="image" href="https://assets.example.com/p.png" imagesrcset="https://assets.example.com/p.png 1x">"#;

        let out = rewrite_inline_creative_html(&settings, "https://publisher.example.com", html);

        let imagesrcset = out
            .split("imagesrcset=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("should keep an imagesrcset attribute");
        assert_eq!(
            imagesrcset.matches("tsurl=").count(),
            1,
            "should proxy the imagesrcset candidate exactly once: {imagesrcset}"
        );
    }

    #[test]
    fn root_relative_link_imagesrcset_is_proxied_once() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<link rel="preload" as="image" href="https://assets.example.com/p.png" imagesrcset="https://assets.example.com/p.png 1x">"#;

        for (label, out) in [
            ("auction", rewrite_creative_html(&settings, html)),
            ("proxied", rewrite_proxied_html(&settings, html)),
        ] {
            let imagesrcset = out
                .split("imagesrcset=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("should keep an imagesrcset attribute");
            assert_eq!(
                imagesrcset.matches("tsurl=").count(),
                1,
                "{label}: should proxy the imagesrcset candidate exactly once: {imagesrcset}"
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify the inline one fails**

Run each alone (a wasm panic aborts the whole test binary):

```bash
cargo test-fastly -p trusted-server-core --lib -- --exact creative::tests::inline_link_imagesrcset_is_proxied_once --nocapture
cargo test-fastly -p trusted-server-core --lib -- --exact creative::tests::root_relative_link_imagesrcset_is_proxied_once
```

Expected:

- The inline test panics with "should proxy the imagesrcset candidate exactly once: https://publisher.example.com/first-party/proxy?tsurl=https%3A%2F%2Fpublisher.example.com%2Ffirst-party%2Fproxy&tsurl=...".
- The root-relative guard passes. Those paths were never affected.

- [ ] **Step 3: Implement**

In the `link[href]` handler, delete the `imagesrcset` block and keep the `href` handling:

```rust
                // Stylesheets and preloads
                element!("link[href]", |el| {
                    let rel = el
                        .get_attribute("rel")
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if rel.contains("stylesheet")
                        || rel.contains("preload")
                        || rel.contains("prefetch")
                    {
                        // `imagesrcset` is left to the `[imagesrcset]` handler, which
                        // also runs on this element; rewriting it here too would
                        // proxy the inline path's absolute proxy URLs a second time.
                        if let Some(p) =
                            proxied_attr_value(settings, el.get_attribute("href"), base_origin)
                        {
                            let _ = el.set_attribute("href", &p);
                        }
                    }
                    Ok(())
                }),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test-fastly -p trusted-server-core --lib -- imagesrcset include_domains_limit`

Expected: `9 passed`. This includes both new tests, `include_domains_limit_asset_rewriting_on_every_html_path` (whose inline case uses the realistic `https://www.example.com` origin), and the existing `imagesrcset` rewrite and sanitizer tests.

- [ ] **Step 5: Run the full core suite and lint**

Run: `cargo fmt --all && cargo test-fastly && cargo clippy-fastly`

Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/creative.rs
git commit --signoff -S -m "Rewrite link imagesrcset once on the inline creative path"
```

---

### Task 6: Operator documentation, example config and changelog

**Files:**

- Modify: `trusted-server.example.toml:127-129`
- Modify: `docs/guide/configuration.md:985-1068`
- Modify: `docs/guide/creative-processing.md:637-700`
- Modify: `docs/guide/first-party-proxy.md:173`, `:509-521`
- Modify: `docs/guide/api-reference.md:565-570`
- Modify: `CHANGELOG.md` (`## [Unreleased]`: `### Fixed` at `:34`, `### Added` at `:41`; both already exist)

- [ ] **Step 1: `trusted-server.example.toml`**

Replace lines 127-129 with:

```toml
# [rewrite]
# Hosts never rewritten: assets stay direct and click-through links are not
# wrapped. Supports "*.example.com" wildcards (matches the apex too).
# exclude_domains = ["*.cdn.example.com"]
# When non-empty, only asset URLs on these hosts are proxied; other assets load
# directly. Click-through links are not affected. Set this in TOML; environment
# overrides cannot replace arrays.
# include_domains = ["assets.example.com", "*.img.example.com"]
```

- [ ] **Step 2: `docs/guide/configuration.md`**

- Section intro (`:989`): "Control which hosts first-party rewriting covers."
- `[rewrite]` table (`:993-995`):

```markdown
| Field             | Type          | Required         | Description                                                                                         |
| ----------------- | ------------- | ---------------- | --------------------------------------------------------------------------------------------------- |
| `exclude_domains` | Array[String] | No (default: []) | Hosts never rewritten, for assets and click-through links                                           |
| `include_domains` | Array[String] | No (default: []) | When non-empty, the only hosts whose asset URLs are proxied. Does not apply to click-through links. |
```

- Add `include_domains = ["assets.example.com", "*.img.example.com"]` to the example and change the Environment Override text to "EdgeZero v0.0.4 cannot replace these arrays or address their elements by index. Edit `exclude_domains` and `include_domains` in TOML, then validate and push the file again."
- After the exact-pattern list, add:

```markdown
**Normalization and precedence**:

- Both lists are trimmed and lowercased at load, so matching is case-insensitive.
- `exclude_domains` wins when a host matches both lists.
- `include_domains` rejects empty entries, a bare `*`, and any `*` other than a
  leading `*.`. Empty and bare `*` entries in `exclude_domains` are dropped with
  a warning, because they never match a host.

::: tip
When both `include_domains` and `proxy.allowed_domains` are set, keep every
`include_domains` host inside `proxy.allowed_domains`. Otherwise a listed asset
is rewritten to a proxy URL that then fails with `403`.
:::
```

- [ ] **Step 3: `docs/guide/creative-processing.md`**

- Wildcard example (`:656-663`): replace `❌ cdn.example.com (no subdomain)` with `✅ cdn.example.com (base domain)`.
- Under `### Pattern Matching`, add "Matching is case-insensitive. Entries are trimmed and lowercased at load." and change the wildcard lead-in to "`*.` matches the base domain and any subdomain".
- After the Exclude Domains use cases, add:

```markdown
## Include Domains

Limit asset rewriting to hosts you list:

    [rewrite]
    include_domains = ["assets.example.com", "*.img.example.com"]

- When `include_domains` is empty (the default), every eligible asset URL is
  proxied.
- When it is non-empty, only asset URLs whose host matches an entry are
  rewritten to `/first-party/proxy`. Other assets keep their original URL, load
  directly, and do not receive the EC ID that proxied fetches append.
- `exclude_domains` wins when a host matches both lists.
- Click-through links (`<a href>`, `<area href>`) are not affected; they are
  still wrapped in `/first-party/click` unless excluded.
- HTML and CSS fetched through `/first-party/proxy` go through the same rewrite
  pass and follow the same list.
- `/first-party/sign` declines off-list hosts with a non-`403` error, so the
  creative runtime loads them directly.

Patterns use the same syntax as `exclude_domains`.
```

(In the guide the TOML is a fenced `toml` block.)

- [ ] **Step 4: `docs/guide/first-party-proxy.md`**

- Rename `### URL Rewrite Exclusions` (`:509`) to `### URL Rewrite Host Lists`, and after "URLs matching these patterns will NOT be rewritten to `/first-party/proxy`." add a fenced `toml` example with `include_domains = ["assets.example.com", "*.img.example.com"]` followed by: "When `include_domains` is non-empty, only asset URLs on matching hosts are rewritten. `exclude_domains` wins when a host matches both lists, and click-through links ignore `include_domains`."
- Before "A valid host outside a non-empty allowlist returns `403 Forbidden`." (`:173`), add the paragraph: "A host excluded by `rewrite.exclude_domains`, or outside a non-empty `rewrite.include_domains`, returns `502`. The creative runtime treats it as a non-policy failure and loads the URL directly. These checks run before `proxy.allowed_domains`."

- [ ] **Step 5: `docs/guide/api-reference.md`**

In `/first-party/sign` Error Responses, after the `413` bullet:

```markdown
- `502 Bad Gateway`: The target host matches `rewrite.exclude_domains` or is outside a non-empty `rewrite.include_domains`
```

- [ ] **Step 6: `CHANGELOG.md`**

Append to the existing `### Fixed` list under `## [Unreleased]`:

```markdown
- Inline SSAT/page-bids creatives no longer proxy `<link rel="preload" imagesrcset>` candidates twice. The `imagesrcset` attribute is now rewritten once, by the same handler on every creative path; previously the inline path's absolute proxy URLs were wrapped in a second `/first-party/proxy` URL.
- `rewrite.exclude_domains` matching is now case-insensitive and ignores surrounding whitespace. Entries written with uppercase letters previously never matched and now take effect, so those hosts stop being proxied and click-wrapped. Audit `exclude_domains` for mixed-case entries before upgrading.
```

Insert at the top of the existing `### Added` list under `## [Unreleased]`:

```markdown
- `[rewrite] include_domains` limits creative asset rewriting to listed hosts. When non-empty, only asset URLs on matching hosts are proxied through `/first-party/proxy`, so only those hosts receive the EC ID on proxied fetches. Other assets keep their original URL, and `/first-party/sign` returns `502` for them so the creative runtime loads them directly. Click-through links are not affected, and `exclude_domains` still wins.
```

- [ ] **Step 7: Format and check**

Run: `cd docs && npm ci && cd .. && docs/node_modules/.bin/prettier --config docs/.prettierrc --write docs/guide/configuration.md docs/guide/creative-processing.md docs/guide/first-party-proxy.md docs/guide/api-reference.md CHANGELOG.md`

Expected: only `configuration.md` changes (table column alignment); the rest report `(unchanged)`.

- [ ] **Step 8: Commit**

```bash
git add docs/guide/configuration.md docs/guide/creative-processing.md docs/guide/first-party-proxy.md docs/guide/api-reference.md trusted-server.example.toml CHANGELOG.md
git commit --signoff -S -m "Document rewrite include_domains and case-insensitive exclusions"
```

---

### Task 7: Full CI gate

**Files:** none.

- [ ] **Step 1: Format and lint**

```bash
cargo fmt --all -- --check
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm \
  && cargo clippy-spin-native && cargo clippy-spin-wasm && cargo clippy-cli && cargo clippy-codegen
```

Expected: all pass.

- [ ] **Step 2: Tests**

```bash
cargo test-fastly && cargo test-fastly-reuse && cargo test-axum && cargo test-cloudflare && cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
```

Expected on `7a0ecb4c` plus this change: `test-fastly` 3155 passed, 10 ignored (core 2908 passed, 6 ignored); `test-fastly-reuse` 219 passed; `test-axum` 43 passed; `test-cloudflare` 53 passed; `test-spin` 87 passed; parity 17 passed; CLI 733 passed.

- [ ] **Step 3: JS and docs**

```bash
cd crates/trusted-server-js/lib && npx vitest run && npm run format && cd -
cd docs && npm run format && cd -
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"
```

Expected: all pass. JS is unchanged.

- [ ] **Step 4: Spec conformance**

| Criterion                                                       | Evidence                                                                                               |
| --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| Empty list leaves existing rewrite assertions unchanged         | Task 2 Step 6 and Task 4 Step 5: full suite, no assertion edits                                        |
| Listed proxied, off-list raw, every handler, three HTML paths   | `include_domains_limit_asset_rewriting_on_every_html_path`, `include_domains_limit_css_body_rewriting` |
| Host in both lists left alone                                   | `host_in_both_lists_is_left_alone`, sign case "host in both lists"                                     |
| Case-insensitive; `*.example.com` matches apex                  | Task 1 and Task 4 policy tests, `rewrite_exclude_domains_match_mixed_case_entries_from_toml`           |
| Sign returns `502` off-list for absolute and `//`, GET and POST | `proxy_sign_applies_rewrite_include_domains_for_get_and_post`                                          |
| Anchors wrapped regardless of allowlist                         | `assert_allowlist_applied` click assertions                                                            |
| Default omitted from serialized blob                            | `legacy_blob_without_rewrite_include_domains_loads_with_empty_list`                                    |
| Docs, example TOML, CHANGELOG                                   | Task 6                                                                                                 |

- [ ] **Step 5: Open the PR**

Use the `pr-creator` agent. Link the spec and #1231. State whether this PR introduced the shared step (Tasks 1-2) or rebased onto #1234's.

## Notes

- **Unparseable absolute URLs.** Before this change an absolute-looking value that `url::Url::parse` rejects had its attribute rewritten to its own value, re-quoted with double quotes. After, it is left untouched. `unparseable_absolute_url_is_left_byte_identical` and `unparseable_absolute_click_url_is_left_byte_identical` pin the new behavior. Against the pre-change code both were observed failing: `<img src='https://exa mple.example/x'>` came back re-quoted, and the anchor also gained `data-tsclick`.
