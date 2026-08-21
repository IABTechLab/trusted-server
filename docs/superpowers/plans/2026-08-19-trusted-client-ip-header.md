# Trusted Client IP Header Implementation Plan

## Review remediation

- [x] Add a shared helper that removes the configured IP and authentication
      headers from a core request, with a failing unit test proving both names
      are removed while unrelated headers remain.
- [x] Invoke that helper from the outer request middleware in the Axum,
      Cloudflare, and Spin adapters so a shared multi-adapter configuration
      cannot expose either trust header to routing or integrations.
- [x] Add a regression assertion proving the Fastly entry-point `ClientInfo`
      address reaches request-scoped services, which are the EC input.
- [x] Remove the partial `X-Forwarded-For` hardening from this PR and its
      documentation. Handle trusted XFF reconstruction consistently across all
      adapters in a separate change.
- [x] Document that redaction protects logs and debug output but the secret is
      serialized into the Trusted Server application-config blob.
- [x] Retain `/.worktrees/` because this checkout has an active unrelated
      worktree under that path; removing the ignore would expose its contents.
- [x] Run formatting plus Fastly, Axum, Cloudflare, and Spin tests and
      target-matched Clippy checks.

## Review follow-up: header-safe shared secrets

PR review found that the request path reads the authentication field with
`HeaderValue::to_str`, while configuration originally accepted any string of at
least 32 UTF-8 bytes. A non-ASCII secret could therefore pass startup validation
but never authenticate a request. Whitespace accepted by `HeaderValue::to_str`
is also unsuitable because an intermediary may normalize it.

- [x] Add focused settings tests for the 31/32-byte boundary and rejection of
      non-ASCII, horizontal-tab, space, DEL, and other control bytes. Assert
      rejected values remain redacted, and run the tests first to demonstrate
      the current failure.
- [x] Accept only `shared_secret` bytes in the ASCII graphic range
      `0x21..=0x7e`, retaining the 32-byte minimum and redacted errors.
- [x] Update the configuration and Fastly guides to specify 32 or more ASCII
      graphic bytes and recommend hexadecimal or base64url generation.
- [x] Run focused settings tests, formatting, target-matched tests, Clippy, and
      the Fastly release build before updating the PR.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the reader IP behind an authenticated fronting CDN while preserving peer-IP fallback and stripping all trust headers before routing.

**Architecture:** Add an optional validated `TrustedClientIpConfig` to core settings, including fixed-size constant-time shared-secret verification. The Fastly adapter resolves the address exactly once from the original request, removes both static and configured spoofable headers, and shares the selected address with `ClientInfo` and response geo finalization.

**Tech Stack:** Rust 2024, Serde/TOML, validator, `sha2`, `subtle`, Fastly Compute SDK, Viceroy, Markdown/VitePress documentation.

---

## File Map

| File                                                   | Responsibility                                                                                                          |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/settings.rs`           | Define, deserialize, validate, redact, and authenticate the optional trusted-client-IP configuration.                   |
| `crates/trusted-server-core/src/http_util.rs`          | Treat `Fastly-Client-IP` as spoofable unless it was consumed before sanitization.                                       |
| `crates/trusted-server-adapter-fastly/src/platform.rs` | Resolve one authenticated header value or fall back to the SDK peer IP; construct `ClientInfo` with the resolved value. |
| `crates/trusted-server-adapter-fastly/src/compat.rs`   | Remove configured trust headers and the static spoofable-header set from the Fastly request.                            |
| `crates/trusted-server-adapter-fastly/src/main.rs`     | Resolve once before sanitization and feed the same IP into request services and geo finalization.                       |
| `trusted-server.example.toml`                          | Show the optional configuration with fictional values.                                                                  |
| `docs/guide/configuration.md`                          | Document fields, validation, fallback, and environment overrides.                                                       |
| `docs/guide/fastly.md`                                 | Document the front-door overwrite requirement and no-code routing limitation.                                           |

### Task 1: Add the validated trusted-client-IP configuration

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs:1-20`
- Modify: `crates/trusted-server-core/src/settings.rs:1871-1950`
- Test: `crates/trusted-server-core/src/settings.rs:2590-end`

- [ ] **Step 1: Write parsing and default-behavior tests**

Add tests beside the existing settings tests. Build TOML from
`crate_test_settings_str()` and append:

```rust
#[test]
fn trusted_client_ip_is_absent_by_default() {
    let settings = Settings::from_toml(&crate_test_settings_str())
        .expect("should parse settings without trusted client IP");

    assert!(
        settings.trusted_client_ip.is_none(),
        "should leave trusted client IP disabled by default"
    );
}

#[test]
fn trusted_client_ip_parses_and_redacts_secret() {
    let toml = format!(
        "{}\n[trusted_client_ip]\nip_header = \"fastly-client-ip\"\nauth_header = \"x-ts-client-ip-auth\"\nshared_secret = \"unit-test-shared-secret-0123456789\"\n",
        crate_test_settings_str()
    );
    let settings = Settings::from_toml(&toml)
        .expect("should parse trusted client IP settings");
    let config = settings
        .trusted_client_ip
        .as_ref()
        .expect("should contain trusted client IP settings");

    assert_eq!(config.ip_header, "fastly-client-ip");
    assert_eq!(config.auth_header, "x-ts-client-ip-auth");
    assert_eq!(
        format!("{:?}", config.shared_secret),
        "[REDACTED]",
        "should redact shared secret"
    );
}
```

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin trusted_client_ip -- --nocapture
```

Expected: compilation fails because `Settings::trusted_client_ip` and
`TrustedClientIpConfig` do not exist.

- [ ] **Step 3: Add the minimum serializable settings shape**

Add the imports needed for fixed-size digest comparison:

```rust
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
```

Define the configuration immediately before `Settings`. Do not add the schema
validator attribute until Step 6, so the intermediate RED build remains valid:

```rust
/// Authenticated client-IP forwarding configuration for a trusted front door.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct TrustedClientIpConfig {
    /// Request header containing exactly one forwarded IPv4 or IPv6 address.
    pub ip_header: String,
    /// Request header containing the shared authentication secret.
    pub auth_header: String,
    /// Shared secret overwritten by the trusted front door on every request.
    #[validate(custom(function = validate_redacted_not_empty))]
    pub shared_secret: Redacted<String>,
}

impl TrustedClientIpConfig {
    /// Minimum accepted authentication secret length in ASCII graphic bytes.
    const MIN_SHARED_SECRET_LENGTH: usize = 32;

    /// Compares a request authentication value with the configured secret.
    #[must_use]
    pub fn authenticates(&self, candidate: &str) -> bool {
        let expected = Sha256::digest(self.shared_secret.expose().as_bytes());
        let actual = Sha256::digest(candidate.as_bytes());
        bool::from(expected.ct_eq(&actual))
    }
}
```

Add the optional nested field to `Settings`:

```rust
/// Optional authenticated client-IP forwarding configuration.
#[serde(default)]
#[validate(nested)]
pub trusted_client_ip: Option<TrustedClientIpConfig>,
```

- [ ] **Step 4: Add fail-closed configuration tests**

Use a small helper that appends a `[trusted_client_ip]` block to the standard
test TOML. Add individual tests proving that parsing/validation rejects:

```rust
#[test]
fn trusted_client_ip_rejects_identical_headers() { /* same name */ }

#[test]
fn trusted_client_ip_rejects_case_insensitive_identical_headers() {
    /* x-client-ip and X-Client-IP */
}

#[test]
fn trusted_client_ip_rejects_unsafe_ip_header() { /* ip_header = "host" */ }

#[test]
fn trusted_client_ip_rejects_unsafe_auth_header() { /* auth_header = "authorization" */ }

#[test]
fn trusted_client_ip_rejects_fastly_tls_bridge_headers() {
    // Exercise x-ts-tls-protocol and x-ts-tls-cipher in both positions.
}

#[test]
fn trusted_client_ip_rejects_empty_secret() { /* shared_secret = "" */ }

#[test]
fn trusted_client_ip_rejects_a_31_byte_secret() { /* shared_secret = 31 * "a" */ }

#[test]
fn trusted_client_ip_accepts_a_32_byte_ascii_graphic_secret() {
    /* shared_secret = 32 * "a" */
}

#[test]
fn trusted_client_ip_rejects_non_header_safe_secrets_without_exposing_them() {
    // Exercise a >=32-byte non-ASCII value, HTAB, space, DEL, and another
    // control byte. Assert each validation error omits the rejected value.
}

#[test]
fn trusted_client_ip_rejects_malformed_header_names() { /* spaces/control bytes */ }

#[test]
fn trusted_client_ip_rejects_incomplete_section() { /* omit each required field */ }

#[test]
fn trusted_client_ip_authentication_is_exact() {
    let config = trusted_client_ip_test_config();
    assert!(config.authenticates("unit-test-shared-secret-0123456789"));
    assert!(!config.authenticates("wrong-secret"));
    assert!(!config.authenticates(" unit-test-shared-secret-0123456789"));
}
```

Each rejection assertion must check that `Settings::from_toml` returns an error,
not merely that `validate()` fails on a manually constructed value.

- [ ] **Step 5: Run the validation tests and verify RED**

Run the same filtered core test command. Expected: parsing tests pass, while the
unsafe/identical-header tests fail because cross-field validation is not yet
implemented.

- [ ] **Step 6: Implement header-name and shared-secret validation**

Add `#[validate(schema(function = "validate_trusted_client_ip_config"))]` to
`TrustedClientIpConfig`, then add helpers that parse names through
`http::HeaderName`, compare normalized lowercase names, and enforce the spec:

```rust
fn validate_trusted_client_ip_config(
    config: &TrustedClientIpConfig,
) -> Result<(), ValidationError> {
    let ip_header = http::HeaderName::from_bytes(config.ip_header.as_bytes())
        .map_err(|_| ValidationError::new("invalid_trusted_client_ip_header"))?;
    let auth_header = http::HeaderName::from_bytes(config.auth_header.as_bytes())
        .map_err(|_| ValidationError::new("invalid_trusted_client_ip_auth_header"))?;

    if ip_header == auth_header {
        return Err(ValidationError::new("duplicate_trusted_client_ip_headers"));
    }
    if ip_header.as_str() != "fastly-client-ip" && !ip_header.as_str().starts_with("x-") {
        return Err(ValidationError::new("unsafe_trusted_client_ip_header"));
    }
    if !auth_header.as_str().starts_with("x-") {
        return Err(ValidationError::new("unsafe_trusted_client_ip_auth_header"));
    }
    for reserved in ["x-ts-tls-protocol", "x-ts-tls-cipher"] {
        if ip_header.as_str() == reserved || auth_header.as_str() == reserved {
            return Err(ValidationError::new("reserved_trusted_client_ip_header"));
        }
    }
    let shared_secret = config.shared_secret.expose().as_bytes();
    if shared_secret.len() < TrustedClientIpConfig::MIN_SHARED_SECRET_LENGTH {
        return Err(ValidationError::new("short_trusted_client_ip_shared_secret"));
    }
    if !shared_secret.iter().all(|byte| matches!(byte, b'!'..=b'~')) {
        return Err(ValidationError::new("invalid_trusted_client_ip_shared_secret"));
    }
    Ok(())
}
```

Use descriptive validation messages if the validator API allows them without
duplicating logic. Do not include `shared_secret` in errors. Test the 31/32-byte
boundary plus non-ASCII, horizontal tab, space, DEL, and another control byte.

- [ ] **Step 7: Run the filtered core tests and verify GREEN**

Run the filtered command from Step 2. Expected: all `trusted_client_ip` tests
pass with no warnings.

- [ ] **Step 8: Run the complete native core suite**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin
```

Expected: PASS.

- [ ] **Step 9: Commit the settings contract**

```bash
git add crates/trusted-server-core/src/settings.rs
git commit -m "Add trusted client IP configuration"
```

### Task 2: Resolve and sanitize the authenticated Fastly client IP

**Files:**

- Modify: `crates/trusted-server-core/src/http_util.rs:35-65`
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs:690-730`
- Modify: `crates/trusted-server-adapter-fastly/src/compat.rs:50-105`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:115-165`
- Test: `crates/trusted-server-adapter-fastly/src/platform.rs:730-end`
- Test: `crates/trusted-server-adapter-fastly/src/compat.rs:65-110`

- [ ] **Step 1: Write resolver tests for the desired behavior**

In `platform.rs`, add a test helper returning a `TrustedClientIpConfig` with
fictional headers/secrets, plus focused tests for:

```rust
#[test]
fn resolve_client_ip_uses_peer_without_config() { /* None config */ }

#[test]
fn resolve_client_ip_accepts_authenticated_ipv4() { /* 198.51.100.7 */ }

#[test]
fn resolve_client_ip_accepts_authenticated_ipv6() { /* 2001:db8::7 */ }

#[test]
fn resolve_client_ip_falls_back_for_missing_authentication() { /* no auth */ }

#[test]
fn resolve_client_ip_falls_back_for_empty_authentication() { /* auth = "" */ }

#[test]
fn resolve_client_ip_falls_back_for_wrong_authentication() { /* wrong auth */ }

#[test]
fn resolve_client_ip_falls_back_for_duplicate_authentication() {
    // append_header twice; do not use set_header
}

#[test]
fn resolve_client_ip_falls_back_for_missing_ip_header() { /* valid auth only */ }

#[test]
fn resolve_client_ip_falls_back_for_invalid_ip_forms() {
    // Separate assertions for whitespace, port, IPv6 zone, comma list, empty.
}

#[test]
fn resolve_client_ip_falls_back_for_duplicate_ip_headers() {
    // append_header twice.
}
```

Pass a documentation address such as `203.0.113.9` as the explicit peer value
so unit tests do not depend on SDK connection metadata. Build non-UTF-8 cases
with `fastly::http::HeaderValue::from_bytes` and test both auth and IP fields.

- [ ] **Step 2: Run the Fastly resolver tests and verify RED**

Run:

```bash
cargo test-fastly resolve_client_ip -- --nocapture
```

Expected: compilation fails because `resolve_client_ip` does not exist. If
Viceroy again fails before executing tests because the macOS native certificate
keychain is unavailable, record that environmental blocker and still require
the compile phase to succeed after implementation.

- [ ] **Step 3: Implement a single-value reader and resolver**

Import `TrustedClientIpConfig` and add:

```rust
fn single_header_str<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    let mut values = req.get_header_all(name);
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

/// Selects an authenticated forwarded address or the immediate peer fallback.
#[must_use]
pub(crate) fn resolve_client_ip(
    req: &Request,
    peer_ip: Option<IpAddr>,
    config: Option<&TrustedClientIpConfig>,
) -> Option<IpAddr> {
    let Some(config) = config else {
        return peer_ip;
    };
    let Some(auth) = single_header_str(req, &config.auth_header) else {
        return peer_ip;
    };
    if !config.authenticates(auth) {
        return peer_ip;
    }
    single_header_str(req, &config.ip_header)
        .and_then(|value| value.parse::<IpAddr>().ok())
        .or(peer_ip)
}
```

Keep failures non-fatal and do not log header values. If debug logs are added,
log only a fixed reason category.

- [ ] **Step 4: Pass the resolved value into `ClientInfo`**

Change the constructor signature and assignment:

```rust
pub fn client_info_from_request(req: &Request, client_ip: Option<IpAddr>) -> ClientInfo {
    ClientInfo {
        client_ip,
        // existing TLS/JA4/server fields unchanged
    }
}
```

Add a direct unit test proving a supplied address is preserved even though a
synthetic Fastly request has no client connection metadata. This is the
regression test for shared geo/`ClientInfo` wiring: Step 9 derives the geo input
back from this constructed `ClientInfo`, rather than retaining an independent
parallel value.

In `main.rs`, update the existing call immediately to pass the already-captured
peer value:

```rust
let client_info = client_info_from_request(&req, client_ip);
```

This is a compile-preserving signature migration only; behavior is still the
old peer-IP behavior until Step 9 wires the resolver.

- [ ] **Step 5: Write the static sanitization test and verify RED**

Extend `compat.rs` tests using the function's current one-argument signature:

```rust
#[test]
fn sanitize_fastly_forwarded_headers_strips_fastly_client_ip_without_config() {
    // Set Fastly-Client-IP, sanitize, assert absent.
}
```

Run:

```bash
cargo test-fastly sanitize_fastly_forwarded_headers -- --nocapture
```

Expected: the new test fails because `Fastly-Client-IP` is still preserved.

- [ ] **Step 6: Strip the static header and migrate the helper signature**

Add `"fastly-client-ip"` to `SPOOFABLE_FORWARDED_HEADERS`. Change the Fastly
compatibility helper to accept an intentionally unused
`Option<&TrustedClientIpConfig>` while preserving its current static loop:

Import `trusted_server_core::settings::TrustedClientIpConfig` in `compat.rs`.

```rust
pub(crate) fn sanitize_fastly_forwarded_headers(
    req: &mut fastly::Request,
    _config: Option<&TrustedClientIpConfig>,
) {
    // Existing static loop unchanged.
}
```

Update existing compat test calls and the entry-point call to pass `None`. Run
the Step 5 command again. Expected: PASS (or the documented Viceroy environment
failure after successful compilation). This leaves the code green before the
next behavior test.

- [ ] **Step 7: Write configured sanitization tests and verify RED**

Now that the two-argument API compiles, add:

```rust
#[test]
fn sanitize_fastly_forwarded_headers_strips_configured_trust_headers() {
    // Set custom IP and auth fields, sanitize with Some(config), assert absent.
}
```

Also cover a configuration whose `ip_header` is `fastly-client-ip` to prove
double removal is harmless. Run the focused sanitization command. Expected: the
custom-header test fails because the config argument is not yet consumed.

- [ ] **Step 8: Implement configured sanitization and verify GREEN**

Remove the configured IP/auth headers first, then loop over the static list.
Never read or log their values. Run the focused sanitization command again.
Expected: PASS (or the documented post-compilation Viceroy environment failure).

- [ ] **Step 9: Wire one resolved address through the entry point**

In `main.rs`, after `settings_snapshot` is created and before sanitization:

Add `resolve_client_ip` to the existing `crate::platform` import.

```rust
let trusted_client_ip = settings_snapshot
    .as_deref()
    .and_then(|settings| settings.trusted_client_ip.as_ref());
let resolved_client_ip = resolve_client_ip(
    &req,
    req.get_client_ip_addr(),
    trusted_client_ip,
);
compat::sanitize_fastly_forwarded_headers(&mut req, trusted_client_ip);
```

Remove the later direct `req.get_client_ip_addr()` capture. Pass
`resolved_client_ip` to `client_info_from_request`, then derive the geo/finalize
input from that object:

```rust
let client_info = client_info_from_request(&req, resolved_client_ip);
let client_ip = client_info.client_ip;
```

Leave both calls to `apply_entry_point_finalize_headers(..., client_ip)`
unchanged. This creates one stored source of truth: the address inserted into
request services and the address passed to response geo are both read from the
same `ClientInfo` construction. The `client_info_from_request` preservation test
from Step 4 proves that selected addresses cross this boundary unchanged.

The startup-error path has no settings snapshot, so it passes `None`, uses the
peer address, and still strips `Fastly-Client-IP` through the static list.

- [ ] **Step 10: Run focused tests and verify GREEN**

Run:

```bash
cargo test-fastly resolve_client_ip -- --nocapture
cargo test-fastly sanitize_fastly_forwarded_headers -- --nocapture
```

Expected: PASS when Viceroy can run. On the known keychain failure, require both
commands to compile the Fastly Wasm test binary successfully before the same
external Viceroy startup error.

- [ ] **Step 11: Verify Fastly compilation**

Run:

```bash
cargo check-fastly
```

Expected: PASS with no warnings.

- [ ] **Step 12: Commit the Fastly behavior**

```bash
git add \
  crates/trusted-server-core/src/http_util.rs \
  crates/trusted-server-adapter-fastly/src/platform.rs \
  crates/trusted-server-adapter-fastly/src/compat.rs \
  crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Resolve authenticated forwarded client IP"
```

### Task 3: Document secure front-door configuration

**Files:**

- Modify: `trusted-server.example.toml:12-20`
- Modify: `docs/guide/configuration.md:50-90`
- Modify: `docs/guide/fastly.md:50-135`

- [ ] **Step 1: Add the commented example configuration**

Add after `[publisher]` in `trusted-server.example.toml`:

```toml
# Optional: trust a fronting CDN's reader IP only when it also supplies the
# matching shared secret. The front door must overwrite both headers.
# [trusted_client_ip]
# ip_header = "fastly-client-ip"
# auth_header = "x-ts-client-ip-auth"
# shared_secret = "replace-with-a-random-shared-secret"
```

- [ ] **Step 2: Add the configuration reference**

Add `[trusted_client_ip]` to the key-sections table and document:

- all three fields and their required status when the section exists;
- absence preserving peer-IP behavior;
- exact-one-value parsing and fail-to-peer behavior;
- safe header-name restrictions;
- `TRUSTED_SERVER__TRUSTED_CLIENT_IP__*` override names;
- redaction and random-secret requirements.

Use only fictional `example.com` domains and documentation IP ranges.

- [ ] **Step 3: Add Fastly front-door instructions**

Add a "CDN-fronted client IP" section explaining:

1. The public front door must overwrite the configured IP and auth headers on
   every backend request; preserving browser-supplied values is unsafe.
2. For VCL/Fastly chaining, overwrite `Fastly-Client-IP` from the initial
   `client.ip` and set the authentication header to the shared secret.
3. Configure the identical header names and secret in Trusted Server.
4. Direct or unauthenticated requests fall back to the immediate peer.
5. Fastly no-code request routing has no header-injection point; if it does not
   preserve the reader IP, this mechanism cannot recover it.

Link to Fastly's official `client.ip`, `Fastly-Client-IP`, and service-chaining
documentation. Do not include a deployable real secret.

- [ ] **Step 4: Format and inspect documentation**

Run:

```bash
(cd docs && npm run format)
git diff --check
git diff -- trusted-server.example.toml docs/guide/configuration.md docs/guide/fastly.md
```

Expected: formatting succeeds; only the intended example and guide sections
change; no whitespace errors.

- [ ] **Step 5: Commit documentation**

```bash
git add trusted-server.example.toml docs/guide/configuration.md docs/guide/fastly.md
git commit -m "Document trusted client IP forwarding"
```

### Task 4: Run final verification and review

**Files:**

- Verify: all files changed by Tasks 1-3

- [ ] **Step 1: Run formatting checks**

```bash
cargo fmt --all -- --check
(cd docs && npm run format)
```

Expected: PASS with no further diffs after formatting.

- [ ] **Step 2: Run target-matched tests**

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin
cargo test-fastly
```

Expected: all core and Fastly tests pass. If Viceroy remains blocked by the
native certificate keychain, preserve the complete error output and separately
confirm `cargo check-fastly` succeeds; do not report `cargo test-fastly` as
passing.

- [ ] **Step 3: Run target-matched compilation and linting**

```bash
cargo check-fastly
cargo clippy-fastly
```

Expected: PASS with warnings denied by the repository alias.

- [ ] **Step 4: Run unaffected-adapter regression tests required by CI**

Because `Settings` and the shared spoofable-header list are in core, run:

```bash
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: PASS.

- [ ] **Step 5: Review the final diff for security invariants**

Confirm from the diff that:

- no code trusts a forwarded IP without successful authentication;
- duplicate or non-UTF-8 fields cannot be accepted through a first-value API;
- no log or error formats `shared_secret` or request header values;
- both configured trust fields are removed before conversion/routing;
- `ClientInfo` and response geo receive the same resolved address;
- configuration absence preserves peer-IP behavior.

- [ ] **Step 6: Commit any verification-only corrections**

If verification required code changes, repeat the narrow failing test first,
then commit only the correction with an imperative sentence-case message. If no
changes were needed, do not create an empty commit.

- [ ] **Step 7: Request final code review**

Use `superpowers:requesting-code-review` against the branch diff from `main` and
address any correctness or security findings before handoff.
