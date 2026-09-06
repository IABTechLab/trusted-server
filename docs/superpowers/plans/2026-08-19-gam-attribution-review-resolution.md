# GAM Attribution Review Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve all actionable comments from PR #1034 review 4966544291 without changing the default-off GAM attribution behavior.

**Architecture:** Attribute conflict handling stays at the integration aggregation boundary, while trusted static HTML validation stays at the rendering boundary. GPT attribution errors remain isolated inside the command queue but become observable through the existing guarded logger. Remaining edits are source-style, test-fixture, and documentation consistency changes.

**Tech Stack:** Rust 2024, JavaScript, TypeScript, Vitest, Cargo workspace aliases, Prettier.

---

### Task 1: Enforce safe publisher-tag attributes

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Modify: `crates/trusted-server-core/src/tsjs.rs`

- [ ] **Step 1: Add a failing duplicate-attribute aggregation test**

Add this second test injector with an explicitly conflicting later value and a
unique attribute:

```rust
struct ConflictingMetadataHeadInjector;

impl IntegrationHeadInjector for ConflictingMetadataHeadInjector {
    fn integration_id(&self) -> &'static str {
        "conflicting-metadata"
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        Vec::new()
    }

    fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("data-ts-gam-attribution", "false"),
            ("data-third-attribute", "third"),
        ]
    }
}
```

Register it after `StaticMetadataHeadInjector`. Update
`tsjs_script_tag_attributes_preserve_registration_order_and_default_empty` to
expect the original value once and all unique attributes in registration order.

```rust
assert_eq!(
    registry.tsjs_script_tag_attributes(),
    vec![
        ("data-ts-gam-attribution", "true"),
        ("data-test-order", "second"),
        ("data-third-attribute", "third"),
    ],
    "should keep the first value for duplicate names and preserve attribute order"
);
```

- [ ] **Step 2: Run the registry test and verify RED**

Run `cargo test_details -p trusted-server-core tsjs_script_tag_attributes_preserve_registration_order_and_default_empty`.

Expected: FAIL because the current flat-map returns both duplicate values.

- [ ] **Step 3: Implement first-wins deduplication**

Replace the flat-map with ordered accumulation:

```rust
let mut attributes = Vec::new();
for injector in &self.inner.head_injectors {
    for attribute in injector.tsjs_script_tag_attributes() {
        if !attributes.iter().any(|(name, _)| *name == attribute.0) {
            attributes.push(attribute);
        }
    }
}
attributes
```

- [ ] **Step 4: Run the registry test and verify GREEN**

Run the Step 2 command. Expected: PASS.

- [ ] **Step 5: Add failing invalid-attribute tests**

Add debug assertion tests in `tsjs.rs`:

```rust
#[test]
#[should_panic(expected = "should contain only lowercase ASCII letters, digits, and hyphens")]
fn publisher_tsjs_script_tag_rejects_invalid_attribute_name() {
    tsjs_script_tag_with_attributes(&["gpt"], &[("data-bad_name", "true")]);
}

#[test]
#[should_panic(expected = "should contain only lowercase ASCII letters, digits, and hyphens")]
fn publisher_tsjs_script_tag_rejects_empty_attribute_name() {
    tsjs_script_tag_with_attributes(&["gpt"], &[("", "true")]);
}

#[test]
fn publisher_tsjs_script_tag_rejects_html_sensitive_attribute_values() {
    for value in ["bad\"value", "bad&value", "bad<value", "bad>value"] {
        let result = std::panic::catch_unwind(|| {
            tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", value)]);
        });
        assert!(result.is_err(), "should reject HTML-sensitive value `{value}`");
    }
}
```

- [ ] **Step 6: Run validation tests and verify RED**

Run `cargo test_details -p trusted-server-core publisher_tsjs_script_tag_rejects`.

Expected: FAIL because both `#[should_panic]` tests return normally.

- [ ] **Step 7: Add debug-only validation**

Inside attribute rendering, assert that names are non-empty and contain only
lowercase ASCII letters, digits, and hyphens, and that values exclude `"`, `&`,
`<`, and `>`.

```rust
debug_assert!(
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
        }),
    "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
);
debug_assert!(
    !value.bytes().any(|byte| matches!(byte, b'"' | b'&' | b'<' | b'>')),
    "attribute value should not contain HTML-sensitive characters"
);
```

- [ ] **Step 8: Run adjacent Rust tests**

Run `cargo test_details -p trusted-server-core publisher_tsjs_script_tag` and
`cargo test_details -p trusted-server-core tsjs_script_tag_attributes`. Expected:
PASS.

Then run `cargo fmt --all -- --check` and `cargo clippy-fastly`. Expected: PASS
before committing.

- [ ] **Step 9: Commit the Rust hardening change**

```bash
git add crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-core/src/tsjs.rs
git commit -m "Harden publisher tag attributes"
```

### Task 2: Make bootstrap attribution failures observable

**Files:**

- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`
- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`

- [ ] **Step 1: Extend the throwing-setConfig test and verify RED**

Install a warning spy with a complete test logger on `window.tsjs` before
running the bootstrap:

```typescript
const warn = vi.fn()
;(window as TestWindow).tsjs = {
  log: {
    setLevel: vi.fn(),
    getLevel: vi.fn(() => 'warn'),
    info: vi.fn(),
    warn,
    error: vi.fn(),
    debug: vi.fn(),
  },
}
```

After executing the queue, assert:

```typescript
expect(warn).toHaveBeenCalledWith(
  'GAM attribution targeting failed',
  expect.any(Error)
)
```

Run from `crates/trusted-server-js/lib`:

```bash
npx vitest run test/integrations/gpt/gpt_bootstrap.test.ts
```

Expected: FAIL because the catch block does not log.

- [ ] **Step 2: Add guarded warning emission**

```javascript
} catch (error) {
  // Attribution must not interrupt the existing bootstrap queue.
  ts.log && ts.log.warn && ts.log.warn("GAM attribution targeting failed", error);
}
```

- [ ] **Step 3: Run the focused Vitest and verify GREEN**

Run the Step 1 Vitest command. Expected: PASS.

- [ ] **Step 4: Apply consistency cleanup**

Change the JavaScript targeting value to `ts: "true"`, update the matching Rust
string in `gpt.rs` exactly as follows, and remove the unused `getConfig` member
from `MockGoogleTag`.

```rust
.find("gpt.setConfig({ targeting: { ts: \"true\" } })")
```

- [ ] **Step 5: Run focused tests and quality checks**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/gpt_bootstrap.test.ts
cd ../../..
cargo test_details -p trusted-server-core head_inserts_queue_gam_attribution_before_guard_and_ad_requests
cargo fmt --all -- --check
cargo clippy-fastly
```

Then run `npm run format` from `crates/trusted-server-js/lib`. Expected: PASS
with no unexpected changes.

- [ ] **Step 6: Commit the bootstrap fixes**

```bash
git add crates/trusted-server-core/src/integrations/gpt_bootstrap.js crates/trusted-server-core/src/integrations/gpt.rs crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts
git commit -m "Log GAM attribution bootstrap failures"
```

### Task 3: Make EdgeZero guidance capability-based

**Files:**

- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/integrations/gpt.md`

- [ ] **Step 1: Replace version-specific wording**

Use this GPT example wording:

```toml
# Keep this leaf present when the environment overlay cannot create a missing
# configuration leaf. Attribution remains off until explicitly enabled.
```

In the GPT guide, state: `The environment overlay cannot create a missing
configuration leaf.` Do not modify the integration fixture or unrelated
EdgeZero version references.

- [ ] **Step 2: Run documentation and diff checks**

Run `cd docs && npm run format`, then run `git diff --check` from the repository
root. Expected: both exit successfully with no unrelated changes.

- [ ] **Step 3: Commit the documentation cleanup**

```bash
git add trusted-server.example.toml docs/guide/integrations/gpt.md
git commit -m "Clarify GAM attribution overlay requirement"
```

### Task 4: Verify the complete review resolution

**Files:**

- Verify all files modified in Tasks 1-3

- [ ] **Step 1: Run formatting checks**

Run `cargo fmt --all -- --check`, the JS `npm run format`, and the docs
`npm run format`. Expected: PASS with no unexpected modifications.

- [ ] **Step 2: Run JavaScript tests**

Run `npx vitest run` from `crates/trusted-server-js/lib`. Expected: PASS.

- [ ] **Step 3: Run target-matched Rust tests**

Run `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, and
`cargo test-spin`. Expected: PASS.

- [ ] **Step 4: Run integration parity and CLI tests**

Run:

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
```

Expected: PASS.

- [ ] **Step 5: Run all clippy gates**

Run `cargo clippy-fastly`, `cargo clippy-axum`, `cargo clippy-cloudflare`,
`cargo clippy-cloudflare-wasm`, `cargo clippy-spin-native`, and
`cargo clippy-spin-wasm`. Expected: PASS.

- [ ] **Step 6: Audit the final diff**

Run `git status --short`, `git diff origin/main...HEAD --check`, and
`git log --oneline origin/main..HEAD`. Also run:

```bash
git diff --exit-code HEAD~3..HEAD -- crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml
git diff --name-only HEAD~3..HEAD
```

The fixture command must produce no output. The name-only command must contain
exactly these seven implementation files:

```text
crates/trusted-server-core/src/integrations/gpt.rs
crates/trusted-server-core/src/integrations/gpt_bootstrap.js
crates/trusted-server-core/src/integrations/registry.rs
crates/trusted-server-core/src/tsjs.rs
crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts
docs/guide/integrations/gpt.md
trusted-server.example.toml
```

Confirm all six actionable comments are addressed and no unrelated files
changed.

- [ ] **Step 7: Prepare thread replies**

Draft concise replies describing each change and its verification. Do not push
commits or post replies without explicit user authorization.
