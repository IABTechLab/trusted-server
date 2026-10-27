# Identity Provider Inputs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give identity providers the two request inputs the vendor providers need: a hashed email (HEM) that the page supplies through the first-party `ts-hem` cookie, and a declared first-party cookie such as a vendor's own ID cookie. Add the shared helpers the vendor providers reuse, and the operator page they document themselves on.

**Architecture:** The JS SDK (TSJS) normalizes and hashes the visitor's email in the browser and writes the digest to `ts-hem`. On each request, `EcContext::decide_writer` reads `ts-hem` into a `HashedEmail` and copies every other non-reserved cookie into a map. Both travel on `RequestIdentity` to the coordinator's `ResolveSignals`, where `ResolveInput::hashed_email()` and the new `ResolveInput::cookie(name)` expose them only to providers whose descriptor declares them. `ts-hem` is a Trusted Server identity cookie: it is stripped from Prebid Server and origin requests, never stored in KV, and no provider can declare it as a raw cookie. Two small modules, `integrations/vendor_eid.rs` and `integrations/vendor_request.rs`, hold parsing and request helpers that more than one vendor uses.

**Tech Stack:** Rust 1.95 (edition 2024), `trusted-server-core` (`hex`, `cookie`, `serde`, `error-stack`, all already dependencies), TypeScript in `crates/trusted-server-js/lib` (Vitest), VitePress docs.

**Spec:** `docs/superpowers/specs/2026-10-20-server-side-identity-providers-design.md`. Section 4.4 defines `ResolveInput` and `HashedEmail`, section 9 the privacy rules, section 12 the vendor examples. Section 12.4 names "an input that reads a named first-party cookie" as an additive extension; this plan adds it for resolving providers only. The framework plan `docs/superpowers/plans/2026-10-22-server-side-identity-providers.md` built everything this plan extends. Writing guide for provider authors: `docs/guide/identity-providers.md`.

**Required by:** every vendor plan dated 2026-10-28 (`2026-10-28-identity-provider-id5.md`, `-liveintent.md`, `-liveramp.md`, `-uid2.md`, `-connectid.md`, `-lockr-capture.md`).

## Global Constraints

- Follow `AGENTS.md`. Bare `cargo test` / `cargo build` fail at the workspace root; use the aliases (`cargo test-fastly`, `cargo clippy-fastly`).
- Errors use `error-stack` with `derive_more::Display` enums. `expect("should ...")`, never `unwrap()`. Tests follow Arrange-Act-Assert with descriptive assertion messages.
- No local imports inside functions. Comments on their own line above the code. Every public item has a doc comment; every builder method and getter is `#[must_use]`.
- Use only fictional data in code, tests and docs (`example.com` hosts, `person@example.com`).
- Never log a hashed email, a cookie value or a UID.
- The hashed email is never stored in KV, never forwarded to the origin, and never sent to Prebid Server. It reaches only providers that declare `Input::HashedEmail`.
- Cookie names compare exactly when stripping (no prefixes), and case-insensitively when deciding whether a name is reserved.
- Quick test command for a module: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib <module path>`.

## File Structure

| File                                                                             | Change | Responsibility                                                             |
| -------------------------------------------------------------------------------- | ------ | -------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/constants.rs`                                    | Modify | `COOKIE_TS_HEM`                                                            |
| `crates/trusted-server-core/src/cookies.rs`                                      | Modify | Add `ts-hem` to both strip lists                                           |
| `crates/trusted-server-core/src/ec/identity/input.rs`                            | Modify | `HashedEmail::from_cookie_hex`, `ResolveSignals::cookies`, `cookie()`      |
| `crates/trusted-server-core/src/ec/identity/descriptor.rs`                       | Modify | `Resolves::with_cookie`, cookie-name checks in `build()`, `Access::Cookie` |
| `crates/trusted-server-core/src/ec/identity/test_support.rs`                     | Modify | Harness `with_cookie`                                                      |
| `crates/trusted-server-core/src/ec/identity/request.rs`                          | Modify | `RequestIdentity` and `WriterDecision` carry the HEM and cookies           |
| `crates/trusted-server-core/src/ec/mod.rs`                                       | Modify | `provider_cookie_signals`, called from `decide_writer`                     |
| `crates/trusted-server-core/src/ec/identity/flush.rs`                            | Modify | Pass the HEM and cookies into `ResolveSignals`; HEM availability           |
| `crates/trusted-server-js/lib/src/core/hashed_email.ts`                          | Create | `setEmail`, `setHashedEmail`, `clearHashedEmail`, `normalizeEmail`         |
| `crates/trusted-server-js/lib/test/core/hashed_email.test.ts`                    | Create | Tests for the above                                                        |
| `crates/trusted-server-js/lib/src/core/{index,types}.ts`                         | Modify | Expose the three functions on `tsjs`                                       |
| `crates/trusted-server-core/src/integrations/vendor_eid.rs`                      | Create | Permissive parsing of vendor-returned complete EIDs                        |
| `crates/trusted-server-core/src/integrations/vendor_request.rs`                  | Create | `is_routable_ip`                                                           |
| `crates/trusted-server-core/src/integrations/mod.rs`                             | Modify | Declare the two helper modules                                             |
| `docs/guide/integrations/identity-vendors.md`                                    | Create | Operator page; each vendor plan adds its section                           |
| `docs/guide/identity-providers.md`, `docs/.vitepress/config.mts`, `CHANGELOG.md` | Modify | Author guide, sidebar, changelog                                           |

---

### Task 1: The `ts-hem` cookie name and its strips

`ts-hem` must never leave the edge. Adding it to `TS_IDENTITY_COOKIE_NAMES` strips it from Prebid Server requests (and, in Task 3, makes it undeclarable as a provider cookie). Adding it to `TS_ORIGIN_STRIPPED_COOKIE_NAMES` strips it from publisher-origin requests.

**Files:**

- Modify: `crates/trusted-server-core/src/constants.rs`
- Modify: `crates/trusted-server-core/src/cookies.rs` (the two constants and the test `strip_cookie_pairs_removes_exact_names_only`)

**Interfaces:**

- Produces: `pub const COOKIE_TS_HEM: &str = "ts-hem";` in `crate::constants`.

- [ ] **Step 1: Extend the strip test so it fails**

In `cookies.rs`, replace the body of `strip_cookie_pairs_removes_exact_names_only`:

```rust
    #[test]
    fn strip_cookie_pairs_removes_exact_names_only() {
        let header = "ts-ec=abc; ts-eids=W10; __Host-ts-eids-s=eyJ2IjoxfQ; ts-hem=00ff; other=1; \
             ts-eids-extra=keep";

        let stripped = strip_cookie_pairs(header, TS_ORIGIN_STRIPPED_COOKIE_NAMES);

        assert_eq!(
            stripped, "ts-ec=abc; other=1; ts-eids-extra=keep",
            "should drop the ID and hashed-email cookies and keep ts-ec and a cookie that only \
             shares a prefix"
        );
    }
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib cookies::tests::strip_cookie_pairs_removes_exact_names_only`
Expected: FAIL, the stripped header still contains `ts-hem=00ff`.

- [ ] **Step 3: Add the constant and the strips**

In `constants.rs`, after `COOKIE_TS_EIDS_S`:

```rust
/// Cookie written by the Trusted Server JS SDK holding the lowercase hex
/// SHA-256 of the visitor's normalized email address. Read per request as the
/// identity providers' hashed-email input; never stored in KV.
pub const COOKIE_TS_HEM: &str = "ts-hem";
```

In `cookies.rs`, import it and extend both lists:

```rust
use crate::constants::{
    COOKIE_EUCONSENT_V2, COOKIE_GPP, COOKIE_GPP_SID, COOKIE_TS_EC, COOKIE_TS_EIDS,
    COOKIE_TS_EIDS_S, COOKIE_TS_HEM, COOKIE_US_PRIVACY,
};
```

```rust
pub const TS_IDENTITY_COOKIE_NAMES: &[&str] = &[
    COOKIE_TS_EC,
    COOKIE_TS_EIDS,
    COOKIE_TS_EIDS_S,
    COOKIE_TS_HEM,
];

/// Identity cookies removed from publisher-origin requests.
///
/// `ts-ec` still reaches the origin. The ID cookies
/// are removed so that they cannot push the origin's `Cookie` header past
/// common 8 KB header limits, and the hashed email never leaves the edge.
pub const TS_ORIGIN_STRIPPED_COOKIE_NAMES: &[&str] =
    &[COOKIE_TS_EIDS, COOKIE_TS_EIDS_S, COOKIE_TS_HEM];
```

- [ ] **Step 4: Run the cookie tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib cookies`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/constants.rs crates/trusted-server-core/src/cookies.rs
git commit -S --signoff -m "Strip the ts-hem hashed-email cookie from origin and Prebid Server requests"
```

---

### Task 2: Parse a `HashedEmail` from the cookie

`HashedEmail` already exists (32 digest bytes, redacting `Debug`, `to_hex`, `to_base64`), but until now only tests could build one. The cookie holds 64 hex digits. A malformed value is treated as absent rather than as an error, so a bad cookie never fails a request.

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/input.rs` (`impl HashedEmail`, tests)

**Interfaces:**

- Produces: `pub(crate) fn HashedEmail::from_cookie_hex(value: &str) -> Option<HashedEmail>`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module of `input.rs`:

```rust
    #[test]
    fn hashed_email_parses_the_cookie_hex_in_either_case() {
        let lower = HashedEmail::from_cookie_hex(&"ab".repeat(32)).expect("should parse lowercase");
        let upper = HashedEmail::from_cookie_hex(&"AB".repeat(32)).expect("should parse uppercase");

        assert_eq!(lower, upper, "should read both cases as one digest");
        assert_eq!(lower.as_bytes(), &[0xab; 32], "should decode the digest");
    }

    #[test]
    fn hashed_email_rejects_a_cookie_that_is_not_a_digest() {
        for value in [
            "",
            "ab",
            &"ab".repeat(31),
            &"ab".repeat(33),
            &"zz".repeat(32),
        ] {
            assert!(
                HashedEmail::from_cookie_hex(value).is_none(),
                "should reject `{value}`"
            );
        }
    }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib ec::identity::input::tests::hashed_email`
Expected: FAIL to compile, `no function or associated item named from_cookie_hex`.

- [ ] **Step 3: Implement**

In `impl HashedEmail`, before `from_bytes`, and update `from_bytes`'s doc comment:

```rust
    /// From the `ts-hem` cookie value: 64 hex digits of the digest, in
    /// either case. `None` for anything else, so a malformed cookie is
    /// treated as absent.
    #[must_use]
    pub(crate) fn from_cookie_hex(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(value, &mut bytes).ok()?;
        Some(Self(bytes))
    }

    /// From digest bytes. Test support only: the one runtime source is the
    /// `ts-hem` cookie, read with [`Self::from_cookie_hex`].
```

Change the doc comment on `ResolveInput::hashed_email` from "A hashed email. Never available in v1." to "The hashed email from the `ts-hem` cookie.". In `descriptor.rs`, change the doc comment on `Input::HashedEmail` to:

```rust
    /// A SHA-256 of a normalized email address, from the `ts-hem` cookie the
    /// JS SDK writes. Never stored.
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib ec::identity::input`
Expected: PASS. (`from_cookie_hex` is unused outside tests until Task 4; a dead-code warning is expected until then.)

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity/input.rs crates/trusted-server-core/src/ec/identity/descriptor.rs
git commit -S --signoff -m "Parse a hashed email from the ts-hem cookie value"
```

---

### Task 3: Declared first-party cookie input

LiveIntent needs its first-party `_lc2_fpi` cookie. A provider declares each cookie name with `Resolves::with_cookie(name)` and reads it with `input.cookie(name)?`. Like every other input, reading an undeclared name is a provider bug: it returns `UndeclaredInput` and is recorded even if the caller hides the error. A declared cookie is never required, so it does not affect eligibility. `build()` refuses names that are not RFC 6265 tokens, and Trusted Server's own identity cookies and the consent cookies, which providers read through `consent()` instead.

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/descriptor.rs` (`Resolves`, `ProviderDescriptorBuilder::build`, `Access`, tests)
- Modify: `crates/trusted-server-core/src/ec/identity/input.rs` (`ResolveSignals`, `ResolveInput::cookie`, the test `signals()` helper, tests)
- Modify: `crates/trusted-server-core/src/ec/identity/test_support.rs` (`HarnessSetup`, `ProviderHarness`, `signals()`)
- Modify: `crates/trusted-server-core/src/ec/identity/flush.rs` (`add_dispatch_results`)

**Interfaces:**

- Consumes: `TS_IDENTITY_COOKIE_NAMES` (now with `ts-hem`, Task 1), `CONSENT_COOKIE_NAMES` from `crate::cookies`.
- Produces:
  - `pub fn Resolves::with_cookie(self, name: &str) -> Self`
  - `pub(crate) fn Resolves::cookies(&self) -> &BTreeSet<String>`
  - `Access::Cookie` (displayed `cookie`)
  - `pub(crate) cookies: BTreeMap<String, String>` on `ResolveSignals`
  - `pub fn ResolveInput::cookie(&self, name: &str) -> Result<Option<&str>, Report<IdentityProviderError>>`
  - `pub fn HarnessSetup::with_cookie(self, name: &str, value: &str) -> Self` and the same on `ProviderHarness`

- [ ] **Step 1: Write the failing descriptor tests**

In `descriptor.rs` tests, add:

```rust
    #[test]
    fn keeps_declared_cookies() {
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(source("example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://a.example.com")
                    .with_cookie("_vendor_fpid")
                    .with_cookie("_vendor_fpid"),
            )
            .build()
            .expect("should build");

        let cookies: Vec<&str> = descriptor
            .resolves()
            .expect("should resolve")
            .cookies()
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(cookies, ["_vendor_fpid"], "should keep each name once");
    }
```

and append these cases to the `cases` vector in `build_rejects_invalid_descriptors`:

```rust
            (
                "cookie name with a separator",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie("vendor;id"),
                    ),
            ),
            (
                "empty cookie name",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie(""),
                    ),
            ),
            (
                "Trusted Server identity cookie",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie("TS-HEM"),
                    ),
            ),
            (
                "consent cookie",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie("euconsent-v2"),
                    ),
            ),
```

- [ ] **Step 2: Write the failing input test**

In `input.rs` tests, add a cookie to the `signals()` helper's struct literal:

```rust
            cookies: BTreeMap::from([("_vendor_fpid".to_owned(), "fpid-value".to_owned())]),
```

and add:

```rust
    #[test]
    fn declared_cookies_are_readable_and_undeclared_ones_are_recorded() {
        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_cookie("_vendor_fpid")
                .with_cookie("_vendor_other"),
        );
        let signals = signals();
        let input = ResolveInput::new(
            &descriptor,
            kinds(&descriptor, ResolveKind::Initial),
            &signals,
        );

        assert_eq!(
            input
                .cookie("_vendor_fpid")
                .expect("should read a declared cookie"),
            Some("fpid-value"),
            "should return the cookie value"
        );
        assert_eq!(
            input
                .cookie("_vendor_other")
                .expect("should read a declared cookie"),
            None,
            "should return None for a declared cookie the request lacks"
        );
        let error = input
            .cookie("_undeclared")
            .expect_err("should refuse an undeclared cookie");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::UndeclaredInput {
                    access: Access::Cookie
                }
            ),
            "should report UndeclaredInput"
        );
        assert_eq!(
            input.recorded_errors(),
            [Access::Cookie],
            "should record the undeclared read"
        );
    }
```

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib ec::identity`
Expected: FAIL to compile (`with_cookie`, `cookies`, `Access::Cookie` do not exist).

- [ ] **Step 4: Implement the descriptor side**

In `descriptor.rs`, import the reserved names:

```rust
use crate::cookies::{CONSENT_COOKIE_NAMES, TS_IDENTITY_COOKIE_NAMES};
```

Add `cookies: BTreeSet<String>` to `struct Resolves`, initialise it with `BTreeSet::new()` in `Resolves::new`, and add the builder method and getter:

```rust
    /// A first-party cookie the provider may read, such as the vendor's own
    /// ID cookie on the publisher's domain. Repeatable. It is passed when the
    /// request carries it and is never required. `build()` refuses Trusted
    /// Server's identity cookies and the consent cookies, which providers
    /// read through [`ResolveInput::consent`](super::input::ResolveInput::consent).
    #[must_use]
    pub fn with_cookie(mut self, name: &str) -> Self {
        self.cookies.insert(name.to_owned());
        self
    }
```

```rust
    /// The first-party cookies the provider may read.
    #[must_use]
    pub(crate) fn cookies(&self) -> &BTreeSet<String> {
        &self.cookies
    }
```

In `ProviderDescriptorBuilder::build`, inside the block that checks `resolves` (after the endpoint checks), add:

```rust
            for name in &resolves.cookies {
                if !is_cookie_name(name) {
                    return Err(fail(&format!("cookie `{name}` is not a valid cookie name")));
                }
                if is_reserved_cookie(name) {
                    return Err(fail(&format!(
                        "cookie `{name}` is a Trusted Server identity or consent cookie"
                    )));
                }
            }
```

and after `impl ProviderDescriptorBuilder`:

```rust
/// Whether `name` is an RFC 6265 cookie-name token.
fn is_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&byte))
}

/// Trusted Server's own identity cookies and the consent cookies, which no
/// provider reads directly.
fn is_reserved_cookie(name: &str) -> bool {
    TS_IDENTITY_COOKIE_NAMES
        .iter()
        .chain(CONSENT_COOKIE_NAMES)
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}
```

Add the variant to `enum Access`:

```rust
    /// A first-party cookie, without `with_cookie()` for its name.
    #[display("cookie")]
    Cookie,
```

- [ ] **Step 5: Implement the input side**

In `input.rs`, add the field to `ResolveSignals`:

```rust
    /// The request's first-party cookies other than Trusted Server's own and
    /// the consent cookies. A provider reads only the names it declared.
    pub(crate) cookies: BTreeMap<String, String>,
```

and the accessor on `ResolveInput`, after `hashed_email`:

```rust
    /// The value of the first-party cookie `name`, or `None` when the request
    /// does not carry it.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput { access: Cookie }` when the descriptor did not
    /// declare `name` with
    /// [`Resolves::with_cookie`](super::descriptor::Resolves::with_cookie).
    pub fn cookie(&self, name: &str) -> Result<Option<&str>, Report<IdentityProviderError>> {
        if !self
            .descriptor
            .resolves()
            .is_some_and(|resolves| resolves.cookies().contains(name))
        {
            return Err(self.record(Access::Cookie));
        }
        Ok(self.signals.cookies.get(name).map(String::as_str))
    }
```

- [ ] **Step 6: Give the harness and the coordinator the field**

In `test_support.rs`, add `cookies: BTreeMap<String, String>` to `HarnessSetup`, and:

```rust
    /// Adds a first-party cookie the request carries. Repeatable.
    #[must_use]
    pub fn with_cookie(mut self, name: &str, value: &str) -> Self {
        self.cookies.insert(name.to_owned(), value.to_owned());
        self
    }
```

On `ProviderHarness`, next to `with_hashed_email`:

```rust
    /// Adds a first-party cookie the request carries. Repeatable.
    #[must_use]
    pub fn with_cookie(self, name: &str, value: &str) -> Self {
        self.map_setup(|setup| setup.with_cookie(name, value))
    }
```

and in `ProviderHarness::signals`, add `cookies: self.setup.cookies.clone(),` to the `ResolveSignals` literal.

In `flush.rs` `add_dispatch_results`, add `cookies: BTreeMap::new(),` to the `ResolveSignals` literal for now. Task 4 replaces it with the request's cookies.

- [ ] **Step 7: Run the identity tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib ec::identity`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/ec/identity/
git commit -S --signoff -m "Let identity providers declare and read first-party cookies"
```

---

### Task 4: Carry the hashed email and cookies from the request

The writer decision is where `EcContext` already captures the per-request provider signals (user agent, `DNT`, page URL). This task captures `ts-hem` and the provider-readable cookies there too, carries them on `RequestIdentity`, and hands them to `ResolveSignals`. `request_facts` reports `Input::HashedEmail` as available when a valid `ts-hem` was present, so a provider that needs it becomes due only for signed-in visitors. The cookie map excludes Trusted Server's own cookies and the consent cookies, matched case-insensitively, the same set `build()` refuses. A malformed `Cookie` header yields neither signal.

**Files:**

- Modify: `crates/trusted-server-core/src/ec/identity/request.rs` (`RequestIdentity`, `WriterDecision`, `set_writer_decision`)
- Modify: `crates/trusted-server-core/src/ec/mod.rs` (`provider_cookie_signals`, `decide_writer`, the test-only `WriterDecision` literal, tests)
- Modify: `crates/trusted-server-core/src/ec/identity/flush.rs` (`request_facts`, `add_dispatch_results`, test literals)
- Modify: every other `WriterDecision { .. }` literal: `auction/endpoints.rs`, `ec/finalize.rs`, `publisher.rs`, `ec/identity/test_support.rs` (`hold_fixture`, `capture_fixture`)

**Interfaces:**

- Consumes: `COOKIE_TS_HEM` (Task 1), `HashedEmail::from_cookie_hex` (Task 2), `ResolveSignals::cookies` (Task 3).
- Produces: `RequestIdentity::hashed_email() -> Option<&HashedEmail>`, `RequestIdentity::cookies() -> &BTreeMap<String, String>`, `WriterDecision { hashed_email: Option<HashedEmail>, cookies: BTreeMap<String, String>, .. }`.

- [ ] **Step 1: Write the failing test**

In `ec/mod.rs` tests, add:

```rust
    #[test]
    fn decide_writer_captures_the_hashed_email_and_provider_cookies() {
        let request = Request::builder()
            .uri("https://www.example.com/a")
            .header("sec-fetch-dest", "document")
            .header(
                header::COOKIE,
                format!(
                    "ts-hem={}; _vendor_fpid=fpid-value; ts-ec=ignored; euconsent-v2=ignored",
                    "AB".repeat(32)
                ),
            )
            .body(EdgeBody::empty())
            .expect("should build the request");
        let mut context = EcContext::default();

        context.decide_writer(&request, true, true);

        assert_eq!(
            context.identity().hashed_email().map(HashedEmail::as_bytes),
            Some(&[0xab; 32]),
            "should read the hashed email from ts-hem"
        );
        let names: Vec<&str> = context
            .identity()
            .cookies()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            names,
            ["_vendor_fpid"],
            "should keep only cookies that are not Trusted Server or consent cookies"
        );
    }

    #[test]
    fn decide_writer_treats_a_malformed_hashed_email_as_absent() {
        let request = Request::builder()
            .uri("https://www.example.com/a")
            .header(header::COOKIE, "ts-hem=person@example.com")
            .body(EdgeBody::empty())
            .expect("should build the request");
        let mut context = EcContext::default();

        context.decide_writer(&request, true, true);

        assert!(
            context.identity().hashed_email().is_none(),
            "should ignore a ts-hem value that is not a digest"
        );
    }
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib ec::tests::decide_writer`
Expected: FAIL to compile (`hashed_email` and `cookies` are not methods of `RequestIdentity`).

- [ ] **Step 3: Carry the fields on `RequestIdentity`**

In `request.rs`, import `super::input::HashedEmail`, add `hashed_email: Option<HashedEmail>` and `cookies: BTreeMap<String, String>` to `RequestIdentity` and to `WriterDecision`, assign them in `set_writer_decision`:

```rust
        self.hashed_email = decision.hashed_email;
        self.cookies = decision.cookies;
```

and add the getters:

```rust
    /// The hashed email from the request's `ts-hem` cookie.
    #[must_use]
    pub(crate) fn hashed_email(&self) -> Option<&HashedEmail> {
        self.hashed_email.as_ref()
    }

    /// The request's first-party cookies other than Trusted Server's own and
    /// the consent cookies, for providers that declare them.
    #[must_use]
    pub(crate) fn cookies(&self) -> &BTreeMap<String, String> {
        &self.cookies
    }
```

- [ ] **Step 4: Read them in `decide_writer`**

In `ec/mod.rs`, update the imports:

```rust
use std::collections::BTreeMap;

use crate::constants::{COOKIE_TS_EC, COOKIE_TS_HEM};
use crate::cookies::{CONSENT_COOKIE_NAMES, TS_IDENTITY_COOKIE_NAMES, handle_request_cookies};

use self::identity::input::HashedEmail;
```

Add, after `parse_ec_from_request`:

```rust
/// The hashed email from the `ts-hem` cookie and the cookies identity
/// providers may read: every first-party cookie except Trusted Server's own
/// and the consent cookies. A malformed `Cookie` header gives neither.
fn provider_cookie_signals(
    req: &Request<EdgeBody>,
) -> (Option<HashedEmail>, BTreeMap<String, String>) {
    let Some(jar) = handle_request_cookies(req).ok().flatten() else {
        return (None, BTreeMap::new());
    };
    let hashed_email = jar
        .get(COOKIE_TS_HEM)
        .and_then(|cookie| HashedEmail::from_cookie_hex(cookie.value()));
    let cookies = jar
        .iter()
        .filter(|cookie| {
            !TS_IDENTITY_COOKIE_NAMES
                .iter()
                .chain(CONSENT_COOKIE_NAMES)
                .any(|reserved| reserved.eq_ignore_ascii_case(cookie.name()))
        })
        .map(|cookie| (cookie.name().to_owned(), cookie.value().to_owned()))
        .collect();
    (hashed_email, cookies)
}
```

In `decide_writer`, before `set_writer_decision`, call it and pass the results:

```rust
        let (hashed_email, cookies) = provider_cookie_signals(req);
        self.identity.set_writer_decision(WriterDecision {
            writer,
            is_real_browser,
            user_agent,
            dnt,
            page_url,
            hashed_email,
            cookies,
        });
```

Extend `decide_writer`'s doc comment: "Also captures the user agent, `DNT`, the page URL without query or fragment, the hashed email from the `ts-hem` cookie, and the first-party cookies identity providers may declare." In the test-only constructor at the bottom of `impl EcContext` that builds a `WriterDecision`, add `hashed_email: None, cookies: BTreeMap::new(),`.

- [ ] **Step 5: Hand them to providers**

In `flush.rs` `request_facts`, add a row to the availability table:

```rust
        (Input::HashedEmail, identity.hashed_email().is_some()),
```

In `add_dispatch_results`, replace `hashed_email: None,` with `hashed_email: identity.hashed_email().cloned(),` and the Task 3 placeholder `cookies: BTreeMap::new(),` with `cookies: identity.cookies().clone(),`.

- [ ] **Step 6: Fix the remaining `WriterDecision` literals**

Add these two lines to every remaining `WriterDecision { .. }` literal (compile errors list them: tests in `auction/endpoints.rs`, `ec/finalize.rs`, `ec/identity/flush.rs`, `publisher.rs`, and `hold_fixture` and `capture_fixture` in `ec/identity/test_support.rs`):

```rust
                hashed_email: None,
                cookies: BTreeMap::new(),
```

Use `std::collections::BTreeMap::new()` where the module does not import `BTreeMap`.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib ec`
Expected: PASS.

Run: `cargo test-fastly`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/
git commit -S --signoff -m "Read the hashed email and provider cookies on the writer request"
```

---

### Task 5: TSJS functions that write `ts-hem`

The page owns the email address; the edge only ever sees its digest. `setEmail` normalizes the address the way UID2 and most ID vendors expect (trim, lowercase, and for Gmail drop dots and any `+suffix` from the local part), hashes it with WebCrypto, and stores the lowercase hex digest. `setHashedEmail` stores a digest the page already has. `clearHashedEmail` removes it on sign-out. The cookie is `Path=/; Secure; SameSite=Lax` with a 30-day `Max-Age`, not `HttpOnly`, since the page writes it.

**Files:**

- Create: `crates/trusted-server-js/lib/src/core/hashed_email.ts`
- Create: `crates/trusted-server-js/lib/test/core/hashed_email.test.ts`
- Modify: `crates/trusted-server-js/lib/src/core/index.ts`, `crates/trusted-server-js/lib/src/core/types.ts`

**Interfaces:**

- Produces: `normalizeEmail(email: string): string | undefined`, `setEmail(email: string): Promise<boolean>`, `setHashedEmail(hex: string): boolean`, `clearHashedEmail(): void`, and the same three (not `normalizeEmail`) on `window.tsjs`.

- [ ] **Step 1: Write the failing tests**

Create `test/core/hashed_email.test.ts`:

```ts
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import {
  clearHashedEmail,
  normalizeEmail,
  setEmail,
  setHashedEmail,
} from '../../src/core/hashed_email'

const EXAMPLE_HASH = 'ab'.repeat(32)

describe('core/hashed_email', () => {
  let writes: string[]

  beforeEach(() => {
    writes = []
    Object.defineProperty(document, 'cookie', {
      configurable: true,
      get: () => '',
      set: (assignment: string) => {
        writes.push(assignment)
      },
    })
  })

  afterEach(() => {
    delete (document as unknown as { cookie?: string }).cookie
  })

  it('normalizes case, whitespace and Gmail aliases', () => {
    expect(normalizeEmail('  Person@Example.COM ')).toBe('person@example.com')
    expect(normalizeEmail('first.last+news@gmail.com')).toBe(
      'firstlast@gmail.com'
    )
    expect(normalizeEmail('first.last+news@example.com')).toBe(
      'first.last+news@example.com'
    )
    expect(normalizeEmail('not-an-address')).toBeUndefined()
    expect(normalizeEmail('@example.com')).toBeUndefined()
  })

  it('stores a hex digest in ts-hem', () => {
    expect(setHashedEmail(EXAMPLE_HASH.toUpperCase())).toBe(true)
    expect(writes).toEqual([
      `ts-hem=${EXAMPLE_HASH}; Path=/; Secure; SameSite=Lax; Max-Age=2592000`,
    ])
  })

  it('refuses a value that is not a digest', () => {
    expect(setHashedEmail('person@example.com')).toBe(false)
    expect(writes).toEqual([])
  })

  it('hashes the normalized email without storing the address', async () => {
    const stored = await setEmail(' Person@Example.com ')

    const expected = Array.from(
      new Uint8Array(
        await crypto.subtle.digest(
          'SHA-256',
          new TextEncoder().encode('person@example.com')
        )
      ),
      (byte) => byte.toString(16).padStart(2, '0')
    ).join('')
    expect(stored).toBe(true)
    expect(writes).toEqual([
      `ts-hem=${expected}; Path=/; Secure; SameSite=Lax; Max-Age=2592000`,
    ])
    expect(writes[0]).not.toContain('example.com')
  })

  it('clears ts-hem', () => {
    clearHashedEmail()
    expect(writes).toEqual(['ts-hem=; Path=/; Secure; SameSite=Lax; Max-Age=0'])
  })
})
```

- [ ] **Step 2: Run it and see it fail**

Run (with the Node 24 pinned in `.tool-versions`, for example through `mise exec --`): `cd crates/trusted-server-js/lib && npx vitest run test/core/hashed_email.test.ts`
Expected: FAIL, cannot resolve `../../src/core/hashed_email`.

- [ ] **Step 3: Implement**

Create `src/core/hashed_email.ts`:

```ts
// Writes the first-party `ts-hem` cookie: the lowercase hex SHA-256 of the
// visitor's normalized email address. TS reads it per request as the identity
// providers' hashed-email input and never stores it, forwards it to the
// origin, or sends it to Prebid Server.

const HEM_COOKIE_NAME = 'ts-hem'

const ATTRIBUTES = 'Path=/; Secure; SameSite=Lax'

// Thirty days: the page sets it again on each sign-in.
const HEM_COOKIE_MAX_AGE = 30 * 24 * 60 * 60

const SHA256_HEX_PATTERN = /^[0-9a-f]{64}$/

const GMAIL_DOMAINS = new Set(['gmail.com', 'googlemail.com'])

/**
 * Normalizes an email address the way UID2 and most ID vendors hash it:
 * trims and lowercases, and for Gmail drops dots and any `+suffix` from the
 * local part.
 *
 * @param email the address the visitor gave
 * @returns the normalized address, or `undefined` when it is not an address
 */
export function normalizeEmail(email: string): string | undefined {
  const trimmed = email.trim().toLowerCase()
  const at = trimmed.indexOf('@')
  // One `@` only: `a@b@example.com` is not an address.
  if (
    at <= 0 ||
    at === trimmed.length - 1 ||
    trimmed.includes('@', at + 1) ||
    /\s/.test(trimmed)
  ) {
    return undefined
  }
  let local = trimmed.slice(0, at)
  const domain = trimmed.slice(at + 1)
  if (GMAIL_DOMAINS.has(domain)) {
    local = local.split('+')[0].replace(/\./g, '')
    if (local.length === 0) {
      return undefined
    }
  }
  return `${local}@${domain}`
}

function writeCookie(value: string, maxAge: number): void {
  try {
    document.cookie = `${HEM_COOKIE_NAME}=${value}; ${ATTRIBUTES}; Max-Age=${maxAge}`
  } catch {
    // Cookie access can throw in sandboxed frames; the hashed email is optional.
  }
}

/**
 * Stores an already-hashed email.
 *
 * @param hex the SHA-256 of the normalized address, as 64 hex digits
 * @returns whether the value was a digest and was stored
 */
export function setHashedEmail(hex: string): boolean {
  const value = hex.trim().toLowerCase()
  if (!SHA256_HEX_PATTERN.test(value)) {
    return false
  }
  writeCookie(value, HEM_COOKIE_MAX_AGE)
  return true
}

/**
 * Normalizes and hashes an email address in the browser, then stores the
 * digest. The address itself is never stored or sent.
 *
 * @param email the address the visitor gave
 * @returns whether a digest was stored
 */
export async function setEmail(email: string): Promise<boolean> {
  const normalized = normalizeEmail(email)
  const subtle = globalThis.crypto?.subtle
  if (normalized === undefined || subtle === undefined) {
    return false
  }
  const digest = await subtle.digest(
    'SHA-256',
    new TextEncoder().encode(normalized)
  )
  const hex = Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, '0')
  ).join('')
  return setHashedEmail(hex)
}

/** Removes the stored hashed email, for example on sign-out. */
export function clearHashedEmail(): void {
  writeCookie('', 0)
}
```

- [ ] **Step 4: Expose them on `tsjs`**

In `src/core/types.ts`, add to `interface TsjsApi` after `requestAds`:

```ts
  /**
   * Normalizes and hashes the visitor's email in the browser and stores the
   * SHA-256 in the first-party `ts-hem` cookie, the identity providers'
   * hashed-email input. Resolves to whether a digest was stored.
   */
  setEmail?(email: string): Promise<boolean>;
  /** Stores an already-computed SHA-256 (64 hex digits) in `ts-hem`. */
  setHashedEmail?(hex: string): boolean;
  /** Removes `ts-hem`, for example on sign-out. */
  clearHashedEmail?(): void;
```

In `src/core/index.ts`, import them and attach them after `api.requestAds = requestAds;`:

```ts
import { clearHashedEmail, setEmail, setHashedEmail } from './hashed_email'
```

```ts
api.setEmail = setEmail
api.setHashedEmail = setHashedEmail
api.clearHashedEmail = clearHashedEmail
```

and add `'setEmail'`, `'setHashedEmail'`, `'clearHashedEmail'` to the `methods` list in the `tsjs initialized` log call.

- [ ] **Step 5: Run the JS checks**

Run: `cd crates/trusted-server-js/lib && npx vitest run && npm run lint && npm run format`
Expected: PASS, and `format` leaves no diff.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-js/lib/src/core/ crates/trusted-server-js/lib/test/core/hashed_email.test.ts
git commit -S --signoff -m "Add tsjs functions that store a hashed email in ts-hem"
```

---

### Task 6: Shared vendor helpers

Two helpers are used by more than one vendor plan, so they land here once.

- `VendorEid`: vendors such as ID5 and Lockr return complete `OpenRTB` EIDs. `crate::openrtb::Eid` uses `deny_unknown_fields`, which is right for Trusted Server's own output but would fail a whole vendor response the day the vendor adds a field. `VendorEid` accepts unknown fields and turns one EID into a `ResolvedEid` under a declared source, keeping every non-empty UID with its `atype` and `ext`, and the EID-level `inserter`, `matcher`, `mm` and `ext`.
- `is_routable_ip`: vendors that match on the client IP answer "no match" for a loopback, private, link-local or carrier-grade NAT address (LiveIntent answers 204 for a loopback `pip`, which is what a local dev server sends). Leaving such an address out lets the vendor match on its other signals.

**Files:**

- Create: `crates/trusted-server-core/src/integrations/vendor_eid.rs`
- Create: `crates/trusted-server-core/src/integrations/vendor_request.rs`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs`

**Interfaces:**

- Produces:
  - `pub(crate) struct VendorEid { pub source: String, pub uids: Vec<VendorUid>, pub inserter: Option<String>, pub matcher: Option<String>, pub mm: Option<i32>, pub ext: Option<serde_json::Value> }` (`Deserialize`)
  - `pub(crate) fn VendorEid::to_resolved(&self, source: &SourceDomain) -> Option<ResolvedEid>`
  - `pub(crate) fn is_routable_ip(ip: IpAddr) -> bool`

- [ ] **Step 1: Declare the modules**

In `integrations/mod.rs`, next to the other module declarations:

```rust
mod vendor_eid;
mod vendor_request;
```

- [ ] **Step 2: Write the failing tests**

Create `vendor_eid.rs` with only its tests module for now:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn keeps_every_uid_and_eid_field() {
        let eid: VendorEid = serde_json::from_value(json!({
            "source": "ids.example.com",
            "uids": [
                { "id": "first", "atype": 1, "ext": { "linkType": 2 } },
                { "id": "" },
                { "id": "second", "future": true }
            ],
            "inserter": "inserter.example.com",
            "matcher": "matcher.example.com",
            "mm": 2,
            "ext": { "segments": ["s1"] },
            "future": "ignored"
        }))
        .expect("should parse a vendor EID with unknown fields");
        let source: SourceDomain = "ids.example.com".parse().expect("should parse the source");

        let resolved = eid.to_resolved(&source).expect("should resolve");

        let ids: Vec<&str> = resolved.uids().iter().map(ResolvedUid::id).collect();
        assert_eq!(
            ids,
            ["first", "second"],
            "should keep non-empty UIDs in order"
        );
        assert_eq!(resolved.uids()[0].atype(), Some(1), "should keep the atype");
        assert_eq!(
            resolved.inserter(),
            Some("inserter.example.com"),
            "should keep inserter"
        );
        assert_eq!(
            resolved.matcher(),
            Some("matcher.example.com"),
            "should keep matcher"
        );
        assert_eq!(resolved.mm(), Some(2), "should keep mm");
        assert_eq!(
            resolved.ext(),
            Some(&json!({ "segments": ["s1"] })),
            "should keep the EID ext"
        );
    }

    #[test]
    fn an_eid_without_ids_resolves_to_none() {
        let eid: VendorEid =
            serde_json::from_value(json!({ "source": "ids.example.com", "uids": [{ "id": "" }] }))
                .expect("should parse");
        let source: SourceDomain = "ids.example.com".parse().expect("should parse the source");

        assert!(
            eid.to_resolved(&source).is_none(),
            "should skip an EID with no ID"
        );
    }
}
```

Create `vendor_request.rs` with only its tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_public_addresses_and_drops_local_ones() {
        for ip in ["192.0.2.10", "198.51.100.7", "2001:db8::1"] {
            assert!(
                is_routable_ip(ip.parse().expect("should parse")),
                "should keep {ip}"
            );
        }
        for ip in [
            "127.0.0.1",
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                !is_routable_ip(ip.parse().expect("should parse")),
                "should drop {ip}"
            );
        }
    }
}
```

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::vendor_`
Expected: FAIL to compile (`VendorEid`, `is_routable_ip` not found).

- [ ] **Step 4: Implement `vendor_eid.rs`**

Prepend to `vendor_eid.rs`:

```rust
//! Permissive parsing of complete EIDs that identity vendors return.
//!
//! [`crate::openrtb::Eid`] denies unknown fields, so it suits Trusted
//! Server's own output, not a vendor's input: one new vendor field would fail
//! the whole response.

use serde::Deserialize;

use crate::ec::identity::{ResolvedEid, ResolvedUid, SourceDomain};

/// One EID as a vendor returns it.
#[derive(Debug, Deserialize)]
pub(crate) struct VendorEid {
    pub source: String,
    #[serde(default)]
    pub uids: Vec<VendorUid>,
    pub inserter: Option<String>,
    pub matcher: Option<String>,
    pub mm: Option<i32>,
    pub ext: Option<serde_json::Value>,
}

/// One UID of a [`VendorEid`].
#[derive(Debug, Deserialize)]
pub(crate) struct VendorUid {
    pub id: String,
    pub atype: Option<i32>,
    pub ext: Option<serde_json::Value>,
}

impl VendorEid {
    /// The EID under `source`, with every UID and EID-level field the vendor
    /// sent. `None` when it holds no UID with an ID.
    pub(crate) fn to_resolved(&self, source: &SourceDomain) -> Option<ResolvedEid> {
        let mut uids = self
            .uids
            .iter()
            .filter(|uid| !uid.id.is_empty())
            .map(VendorUid::to_resolved);
        let mut eid = ResolvedEid::new(source, uids.next()?);
        for uid in uids {
            eid = eid.with_uid(uid);
        }
        if let Some(inserter) = &self.inserter {
            eid = eid.with_inserter(inserter);
        }
        if let Some(matcher) = &self.matcher {
            eid = eid.with_matcher(matcher);
        }
        if let Some(mm) = self.mm {
            eid = eid.with_mm(mm);
        }
        if let Some(ext) = &self.ext {
            eid = eid.with_ext(ext.clone());
        }
        Some(eid)
    }
}

impl VendorUid {
    fn to_resolved(&self) -> ResolvedUid {
        let mut uid = ResolvedUid::new(self.id.clone());
        if let Some(atype) = self.atype {
            uid = uid.with_atype(atype);
        }
        if let Some(ext) = &self.ext {
            uid = uid.with_ext(ext.clone());
        }
        uid
    }
}
```

- [ ] **Step 5: Implement `vendor_request.rs`**

Prepend to `vendor_request.rs`:

```rust
//! Helpers shared by the identity vendors' requests.

use std::net::IpAddr;

/// Whether `ip` can identify a visitor to a vendor: not loopback,
/// unspecified, private, link-local or carrier-grade NAT.
///
/// A vendor that matches on the client IP answers no match for such an
/// address, as it does for a request from a local dev server, so the IP is
/// left out instead.
pub(crate) fn is_routable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            let carrier_grade_nat = first == 100 && (second & 0xc0) == 64;
            !(v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || carrier_grade_nat)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_routable_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            let unique_local = (first & 0xfe00) == 0xfc00;
            let link_local = (first & 0xffc0) == 0xfe80;
            !(v6.is_loopback() || v6.is_unspecified() || unique_local || link_local)
        }
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p trusted-server-core --features test-utils --target x86_64-unknown-linux-gnu --lib integrations::vendor_`
Expected: PASS. Both modules are dead code in the library build until the first vendor plan uses them, so `cargo clippy-fastly` reports `dead_code` until then. Land this task together with the first vendor plan's PR, or run clippy after that plan's first task.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/integrations/
git commit -S --signoff -m "Add shared vendor EID parsing and routable-IP helpers"
```

---

### Task 7: Operator and author documentation

**Files:**

- Create: `docs/guide/integrations/identity-vendors.md`
- Modify: `docs/.vitepress/config.mts`, `docs/guide/identity-providers.md`, `CHANGELOG.md`

- [ ] **Step 1: Create the operator page**

Create `docs/guide/integrations/identity-vendors.md`. Each vendor plan adds one row to the table and one `##` section after "Hashed email":

````markdown
# Identity Vendors

Trusted Server ships server-side [identity providers](/guide/identity-providers) for several vendors. Each one is an integration under `[integrations.<id>]` that, once enabled, resolves or captures IDs at the edge and writes them to the Edge Cookie (EC) identity graph in KV. You don't need the vendor's browser module for this.

| Integration | Vendor | Kind | Sources | Needs |
| ----------- | ------ | ---- | ------- | ----- |

Criteo and The Trade Desk are not included. Their IDs live in the vendor's own third-party cookie, which Trusted Server cannot read at the publisher's edge. Browser ingest still carries those IDs from `ts-eids` when they are registered partners.

## Before you enable one

Every source a provider writes needs an `[[ec.partners]]` entry, or `ts config validate` refuses the config. When a provider declares more sources than you want, narrow it:

```toml
[[ec.partners]]
name = "Example ID"
source_domain = "ids.example.com"
openrtb_atype = 1
bidstream_enabled = true

[ec.identity.providers.<id>]
sources = ["ids.example.com"]
```

Provider timing, rate limits and ownership use the keys in the [Configuration Reference](/guide/configuration#identity-providers-ec-identity).

## Hashed email

Some providers need a hashed email, and others send one when it is present. The page supplies it after the visitor signs in. The JS SDK normalizes and hashes the address in the browser and stores only the SHA-256 in the first-party `ts-hem` cookie:

```js
window.tsjs.que.push(async () => {
  await window.tsjs.setEmail('person@example.com')
})

// Or, when the page already has the digest:
window.tsjs.setHashedEmail('<64 hex digits>')

// On sign-out:
window.tsjs.clearHashedEmail()
```

Normalization trims and lowercases the address. For Gmail it also drops dots and any `+suffix` from the local part. The cookie lasts 30 days.

Trusted Server reads `ts-hem` on each request and passes it only to providers that declare the hashed-email input. It never stores the digest in KV, and it strips the cookie before forwarding a request to the origin or to Prebid Server. A hashed email is personal data: set it only under the legal basis your consent platform records.
````

- [ ] **Step 2: Add the sidebar entry**

In `docs/.vitepress/config.mts`, in the integrations list, before `{ text: 'Lockr', ... }`:

```ts
            {
              text: 'Identity Vendors',
              link: '/guide/integrations/identity-vendors',
            },
```

- [ ] **Step 3: Update the author guide**

In `docs/guide/identity-providers.md`, replace "No vendor ships with the framework. Vendors arrive in their own specs." with:

```markdown
The vendor providers that ship with Trusted Server, listed in [Identity Vendors](/guide/integrations/identity-vendors), are worked examples of the same API.
```

After the bullet list describing `UndeclaredInput` and `InputUnavailable`, add:

```markdown
The hashed email (`Input::HashedEmail`) comes from the `ts-hem` cookie the page sets through the JS SDK, so it is present only for visitors who signed in. Treat it as wanted unless the vendor cannot work without it.

A provider can also read a first-party cookie on the publisher's domain, such as the vendor's own ID cookie. Declare each name with `Resolves::with_cookie(name)` and read it with `input.cookie(name)?`, which returns `None` when the request lacks it. Trusted Server's own cookies and the consent cookies cannot be declared. Consent comes from `consent()`.
```

- [ ] **Step 4: Update the changelog**

Under `### Added` in `CHANGELOG.md`, add:

```markdown
- `tsjs.setEmail`, `tsjs.setHashedEmail` and `tsjs.clearHashedEmail` store the SHA-256 of the visitor's normalized email in the first-party `ts-hem` cookie, the identity providers' hashed-email input. Trusted Server reads it per request, never stores it, and strips it from requests to the origin and to Prebid Server.
- Identity providers can declare and read first-party cookies with `Resolves::with_cookie` and `ResolveInput::cookie`.
```

- [ ] **Step 5: Format and run the gates**

Run: `cd docs && npm run format`
Run: `docs/node_modules/.bin/prettier --config docs/.prettierrc --check CHANGELOG.md`
Run: `cargo fmt --all -- --check && cargo test-fastly && cargo clippy-fastly`
Expected: PASS (see Task 6, Step 6, for the expected `dead_code` until the first vendor lands).

- [ ] **Step 6: Commit**

```bash
git add docs/ CHANGELOG.md
git commit -S --signoff -m "Document the hashed-email input and declared provider cookies"
```

## Self-Review Notes

- `ts-hem` is in `TS_IDENTITY_COOKIE_NAMES` (Prebid Server strip and the reserved-cookie check) and `TS_ORIGIN_STRIPPED_COOKIE_NAMES` (origin strip): Task 1.
- Malformed `ts-hem` is absent, never an error: Tasks 2 and 4.
- `HashedEmail` availability drives eligibility through `request_facts`: Task 4, Step 5.
- A declared cookie is never required and never affects eligibility; an undeclared read is recorded: Task 3.
