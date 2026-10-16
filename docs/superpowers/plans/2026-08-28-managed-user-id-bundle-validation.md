# Managed User ID Bundle Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ts prebid bundle` fail before updating deployable metadata when a configured managed User ID name is unknown, ambiguous, or missing its required module from the freshly generated bundle manifest.

**Architecture:** Extend the CLI's focused TOML reader with managed User ID names, resolve them through the same checked-in JSON registry used by the JavaScript generator, invalidate any stale output manifest, and validate the newly generated manifest before patching hash/SRI metadata. Keep core vendor-neutral and retain the existing browser diagnostic as defense in depth.

**Tech Stack:** Rust 2024, Serde/serde_json, TOML/toml_edit, host-target CLI tests, Prettier Markdown formatting.

**Specification:** `docs/superpowers/specs/2026-08-21-liveramp-integration-design.md`

**Working tree:** Use the existing `issue-355-liveramp-integration` branch as explicitly requested by the user. Do not create a worktree and do not push without separate authorization.

---

## File structure

| File                                             | Responsibility                                                                                                                                       |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-cli/src/prebid_bundle.rs` | Parse managed names, load/resolve the registry, invalidate stale manifests, validate the generated manifest, and contain focused unit/command tests. |
| `docs/guide/integrations/prebid.md`              | Explain the registry-backed bundle failure and runtime fallback diagnostic.                                                                          |
| `docs/guide/configuration.md`                    | Replace the obsolete “not validated” configuration warning.                                                                                          |
| `trusted-server.example.toml`                    | Tell operators that the bundle command validates managed-name/module pairing.                                                                        |

No core, TypeScript runtime, JavaScript generator, registry schema, manifest producer, or public configuration shape changes are required.

## Task 1: Strictly parse managed User ID names

**Files:**

- Modify: `crates/trusted-server-cli/src/prebid_bundle.rs:32-37`
- Modify: `crates/trusted-server-cli/src/prebid_bundle.rs:182-248`
- Test: `crates/trusted-server-cli/src/prebid_bundle.rs:558-683`

- [ ] **Step 1: Write failing parser tests**

Add tests proving an absent list becomes empty, valid entries preserve order, and malformed values fail instead of being skipped:

```rust
#[test]
fn bundle_config_loader_reads_managed_user_id_names_in_order() {
    let (_temp, path) = write_config(
        r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"

[[integrations.prebid.managed_user_ids]]
name = "identityLink"

[[integrations.prebid.managed_user_ids]]
name = "pubCommonId"

[integrations.prebid.bundle]
adapters = ["rubicon"]
"#,
    );

    let config = load_bundle_config(&path).expect("should load managed names");

    assert_eq!(
        config.managed_user_id_names,
        ["identityLink", "pubCommonId"],
        "should preserve managed entry order"
    );
}

#[test]
fn bundle_config_loader_rejects_managed_entry_without_string_name() {
    let (_temp, path) = write_config(
        r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"
managed_user_ids = [{ params = { pid = "999" } }]

[integrations.prebid.bundle]
adapters = ["rubicon"]
"#,
    );

    let error = load_bundle_config(&path).expect_err("should reject missing managed name");

    assert!(
        error.contains("integrations.prebid.managed_user_ids[0].name"),
        "should identify the malformed managed entry: {error}"
    );
}
```

Add separate cases for:

- `managed_user_ids` being a string/table rather than an array;
- an array element being a string rather than a table;
- a missing `name`;
- a non-string `name`;
- an empty or whitespace-only `name`.

Also extend the existing missing-list test to assert `managed_user_id_names.is_empty()`.

- [ ] **Step 2: Run the CLI suite and verify the new tests fail**

Run:

```bash
./scripts/test-cli.sh
```

Expected: FAIL because `PrebidBundleConfig` has no `managed_user_id_names` field and no strict reader exists.

- [ ] **Step 3: Implement the focused TOML reader**

Extend the config structure:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrebidBundleConfig {
    pub adapters: Vec<String>,
    pub user_id_modules: Option<Vec<String>>,
    pub managed_user_id_names: Vec<String>,
    pub external_bundle_url: Option<String>,
}
```

Add a narrow helper; do not deserialize or validate vendor parameters:

```rust
fn read_managed_user_id_names(
    prebid: &toml::Value,
    config_path: &Path,
) -> CliResult<Vec<String>> {
    let Some(value) = prebid.get("managed_user_ids") else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or_else(|| {
        report_error(format!(
            "{} integrations.prebid.managed_user_ids must be an array of tables",
            config_path.display()
        ))
    })?;

    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let table = entry.as_table().ok_or_else(|| {
                report_error(format!(
                    "{} integrations.prebid.managed_user_ids[{index}] must be a table",
                    config_path.display()
                ))
            })?;
            let field = format!("integrations.prebid.managed_user_ids[{index}].name");
            let name = table.get("name").and_then(toml::Value::as_str).ok_or_else(|| {
                report_error(format!(
                    "{} {field} must be a non-empty string",
                    config_path.display()
                ))
            })?;
            if name.trim().is_empty() {
                return cli_error(format!(
                    "{} {field} must be a non-empty string",
                    config_path.display()
                ));
            }
            Ok(name.to_string())
        })
        .collect()
}
```

Call it from `load_bundle_config` and store the result. Keep full token, duplicate-name, params, and storage validation in core; the CLI validates only fields required for bundle consistency.

- [ ] **Step 4: Run the CLI suite and verify it passes**

Run: `./scripts/test-cli.sh`

Expected: all `trusted-server-cli` tests PASS.

- [ ] **Step 5: Commit locally**

```bash
git add crates/trusted-server-cli/src/prebid_bundle.rs
git commit -m "Parse managed User ID bundle inputs"
```

Do not push.

## Task 2: Resolve managed names through the shared registry

**Files:**

- Modify: `crates/trusted-server-cli/src/prebid_bundle.rs:10-12`
- Modify: `crates/trusted-server-cli/src/prebid_bundle.rs:121-128`
- Test: `crates/trusted-server-cli/src/prebid_bundle.rs:547-924`
- Read-only contract: `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json`

- [ ] **Step 1: Write failing registry-resolution tests**

Define tests around an in-memory registry:

```rust
#[test]
fn managed_names_resolve_aliases_to_registered_modules() {
    let registry = PrebidUserIdModuleRegistry {
        modules: vec![PrebidUserIdModuleRegistryEntry {
            module_name: "sharedIdSystem".to_string(),
            config_names: vec!["sharedId".to_string(), "pubCommonId".to_string()],
        }],
    };
    let registry_path = Path::new("user_id_modules.json");

    let required = resolve_managed_user_id_modules(
        &["pubCommonId".to_string(), "sharedId".to_string()],
        &registry,
        registry_path,
    )
    .expect("should resolve aliases");

    assert_eq!(
        required,
        [
            RequiredPrebidUserIdModule {
                config_name: "pubCommonId".to_string(),
                module_name: "sharedIdSystem".to_string(),
            },
            RequiredPrebidUserIdModule {
                config_name: "sharedId".to_string(),
                module_name: "sharedIdSystem".to_string(),
            },
        ],
        "should retain each managed name while allowing a shared module"
    );
}
```

Add cases proving:

- `identityLink` resolves to `identityLinkIdSystem` from the actual checked-in registry;
- an unknown name fails and identifies the name plus registry path;
- a synthetic name mapped to two distinct modules fails and lists both candidates deterministically;
- an empty managed-name list returns an empty requirement list.

- [ ] **Step 2: Run the CLI suite and verify the tests fail**

Run: `./scripts/test-cli.sh`

Expected: FAIL because the registry types, loader, and resolver do not exist.

- [ ] **Step 3: Implement registry loading and deterministic resolution**

Add vendor-neutral types:

```rust
const USER_ID_REGISTRY_RELATIVE_PATH: &str =
    "src/integrations/prebid/user_id_modules.json";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrebidUserIdModuleRegistry {
    modules: Vec<PrebidUserIdModuleRegistryEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrebidUserIdModuleRegistryEntry {
    module_name: String,
    config_names: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequiredPrebidUserIdModule {
    config_name: String,
    module_name: String,
}
```

Load the exact file beneath the already-resolved JS library directory:

```rust
fn load_user_id_registry(
    js_lib_dir: &Path,
) -> CliResult<(PathBuf, PrebidUserIdModuleRegistry)> {
    let path = js_lib_dir.join(USER_ID_REGISTRY_RELATIVE_PATH);
    let contents = fs::read_to_string(&path).map_err(|error| {
        report_error(format!(
            "failed to read Prebid User ID registry {}: {error}",
            path.display()
        ))
    })?;
    let registry = serde_json::from_str(&contents).map_err(|error| {
        report_error(format!(
            "failed to parse Prebid User ID registry {}: {error}",
            path.display()
        ))
    })?;
    Ok((path, registry))
}
```

Implement `resolve_managed_user_id_modules` with these rules:

1. Collect matching `module_name` values for every exact `config_names` match.
2. Sort and deduplicate candidate modules for deterministic diagnostics.
3. Zero candidates: fail with managed name and registry path.
4. One candidate: return a requirement retaining both config and module names.
5. More than one candidate: fail with the managed name, registry path, and candidates.

Do not hardcode `identityLink`, `identityLinkIdSystem`, `liveramp.com`, or any other vendor/module name in production code.

- [ ] **Step 4: Run the CLI suite and verify it passes**

Run: `./scripts/test-cli.sh`

Expected: all CLI tests PASS, including the checked-in registry contract.

- [ ] **Step 5: Commit locally**

```bash
git add crates/trusted-server-cli/src/prebid_bundle.rs
git commit -m "Resolve managed User IDs through the bundle registry"
```

Do not push.

## Task 3: Require a fresh manifest containing every managed module

**Files:**

- Modify: `crates/trusted-server-cli/src/prebid_bundle.rs:121-180`
- Modify: `crates/trusted-server-cli/src/prebid_bundle.rs:419-454`
- Test: `crates/trusted-server-cli/src/prebid_bundle.rs:797-907`

- [ ] **Step 1: Make the fake generator express exact manifest behavior**

Replace `write_manifest: bool` with an optional complete JSON document:

```rust
struct FakeGenerator {
    generate_error: Option<String>,
    generate_calls: Vec<PrebidBundleGenerateRequest>,
    manifest: Option<serde_json::Value>,
}
```

When the option is `Some`, write that exact JSON value to `manifest.json`. When
it is `None`, return according to `generate_error` without writing a manifest.
Add a `fake_manifest(user_id_modules: serde_json::Value)` helper that returns the
otherwise-valid manifest object. This lets tests emit a valid array, a non-array
value, or a complete object with `userIdModules` removed. Update existing tests
without changing their intent.

- [ ] **Step 2: Write failing command-level consistency tests**

Add command-level tests proving:

```rust
#[test]
fn run_bundle_rejects_managed_name_when_manifest_omits_required_module() {
    let (_temp, config_path) = write_config(&managed_identity_link_config());
    let original = fs::read_to_string(&config_path).expect("should read original config");
    let output_root = tempfile::tempdir().expect("should create output root");
    let mut generator = FakeGenerator {
        generate_error: None,
        generate_calls: Vec::new(),
        manifest: Some(fake_manifest(serde_json::json!(["sharedIdSystem"]))),
    };
    let args = PrebidBundleArgs {
        config: config_path,
        out: output_root.path().join("prebid"),
    };

    let error = run_bundle(&args, &mut generator, &mut Vec::new(), &mut Vec::new())
        .expect_err("should reject missing managed module");

    assert!(error.contains("identityLink"), "should name managed config: {error}");
    assert!(
        error.contains("identityLinkIdSystem"),
        "should name required module: {error}"
    );
    assert!(
        error.contains("integrations.prebid.bundle.user_id_modules"),
        "should identify corrective field: {error}"
    );
    assert_eq!(
        fs::read_to_string(&args.config).expect("should reread config"),
        original,
        "should not patch metadata after consistency failure"
    );
}
```

Add cases proving:

- the same managed config passes when the manifest contains `identityLinkIdSystem`;
- two managed names require both modules;
- two aliases backed by `sharedIdSystem` both pass with one manifest module;
- omission of `bundle.user_id_modules` passes when the fake generated manifest contains the default module;
- unknown and malformed names fail through `run_bundle` before `generate_calls`
  receives an entry, with the entire original config (including existing
  hash/SRI metadata) unchanged;
- an ambiguous name fails through the private registry-injected orchestration
  seam described in Step 6 before `generate_calls` receives an entry, with the
  entire original config (including existing hash/SRI metadata) unchanged;
- fake manifests with a missing or non-array `userIdModules` field fail
  manifest parsing;
- a missing required module never changes existing hash/SRI metadata.

- [ ] **Step 3: Write the failing stale-manifest regression test**

Prepopulate `<out>/manifest.json` with valid old metadata, make the fake generator return success without writing, then assert:

- `run_bundle` fails to read the generated manifest;
- the old manifest no longer exists;
- config metadata is unchanged.

Run: `./scripts/test-cli.sh`

Expected: FAIL because the current CLI accepts an old manifest and does not validate `userIdModules`.

- [ ] **Step 4: Extend manifest deserialization and validation**

```rust
#[derive(Debug, Deserialize)]
struct PrebidBundleManifest {
    #[serde(rename = "userIdModules")]
    user_id_modules: Vec<String>,
    sha256: String,
    sri: String,
    filename: String,
}

fn validate_managed_user_id_modules(
    requirements: &[RequiredPrebidUserIdModule],
    manifest: &PrebidBundleManifest,
    config_path: &Path,
) -> CliResult<()> {
    for requirement in requirements {
        if !manifest
            .user_id_modules
            .iter()
            .any(|module| module == &requirement.module_name)
        {
            return cli_error(format!(
                "{} configures managed User ID {:?}, which requires Prebid module {:?}, but the generated manifest omits it; add {:?} to integrations.prebid.bundle.user_id_modules and rerun `ts prebid bundle`",
                config_path.display(),
                requirement.config_name,
                requirement.module_name,
                requirement.module_name,
            ));
        }
    }
    Ok(())
}
```

Serde must reject a missing or non-array `userIdModules` field. Do not default it to an empty list.

- [ ] **Step 5: Invalidate only the exact old manifest before generation**

```rust
fn invalidate_manifest(path: &Path) -> CliResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => cli_error(format!(
            "failed to remove stale Prebid manifest {}: {error}",
            path.display()
        )),
    }
}
```

Do not delete the output directory or any generated bundle files.

- [ ] **Step 6: Add a private registry-injected orchestration seam**

Keep public command behavior in `run_bundle`, but move the post-registry workflow
into a private helper so command ordering can be tested with a synthetic
ambiguous registry:

```rust
struct PrebidBundleRunContext<'a> {
    current_dir: &'a Path,
    js_lib_dir: PathBuf,
    registry_path: &'a Path,
    registry: &'a PrebidUserIdModuleRegistry,
}

fn run_bundle_with_context(
    args: &PrebidBundleArgs,
    config: PrebidBundleConfig,
    context: PrebidBundleRunContext<'_>,
    generator: &mut dyn PrebidBundleGenerator,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<()> {
    // Resolve requirements before output mutation or generator invocation,
    // then perform generation, manifest validation, and metadata patching.
}
```

`run_bundle` must load config, determine the current/JS directories, load the
real registry, then delegate. Tests may pass a synthetic registry only to this
private helper.

- [ ] **Step 7: Wire the command in the specified order**

Implement the helper workflow in this order:

1. Load focused config.
2. Locate the JS directory.
3. Load the shared registry.
4. Resolve all managed names; return before generator invocation on failure.
5. Ensure the output directory is writable.
6. Invalidate only `<out>/manifest.json`.
7. Invoke the generator.
8. Load the newly created manifest.
9. Validate all resolved requirements.
10. Patch hash/SRI metadata.

Keep `external_bundle_url` output behavior unchanged. Add the synthetic
ambiguous-registry command test now and assert the fake generator has zero
calls and the original config remains byte-for-byte unchanged.

- [ ] **Step 8: Run the CLI suite and verify it passes**

Run: `./scripts/test-cli.sh`

Expected: all CLI tests PASS.

- [ ] **Step 9: Run formatting and CLI lint**

Run:

```bash
cargo fmt --all -- --check
cargo clippy-cli
```

Expected: both commands exit 0 with no warnings.

- [ ] **Step 10: Commit locally**

```bash
git add crates/trusted-server-cli/src/prebid_bundle.rs
git commit -m "Reject incomplete managed User ID bundles"
```

Do not push.

## Task 4: Document the build-time guard

**Files:**

- Modify: `docs/guide/integrations/prebid.md:487-527`
- Modify: `docs/guide/configuration.md:1270-1293`
- Modify: `trusted-server.example.toml:420-445`

- [ ] **Step 1: Replace obsolete unvalidated-pairing guidance**

Document these exact semantics in both guides:

- `ts prebid bundle` resolves each managed name through `user_id_modules.json`;
- unknown or ambiguous config names fail;
- the command confirms required modules in the newly generated manifest;
- failure identifies the managed name/module and does not update hash/SRI;
- the browser diagnostic remains useful for external, stale, or modified bundles;
- core remains vendor-neutral and continues to forward `params` opaquely.

Replace the example-file warning with concise wording such as:

```toml
# `ts prebid bundle` resolves every managed name through the checked-in User ID
# registry and fails if the generated manifest omits its required module.
```

- [ ] **Step 2: Verify documentation no longer claims the pairing is unvalidated**

Run:

```bash
rg -n "Nothing validates|not validated|pairing is not validated" \
  docs/guide/integrations/prebid.md \
  docs/guide/configuration.md \
  trusted-server.example.toml
```

Expected: no matches.

- [ ] **Step 3: Format and check documentation**

Run:

```bash
cd docs
npm run format:write
npm run format
```

Expected: Prettier writes any required formatting changes, then reports all
documentation files formatted.

- [ ] **Step 4: Commit locally**

```bash
git add docs/guide/integrations/prebid.md docs/guide/configuration.md trusted-server.example.toml
git commit -m "Document managed User ID bundle validation"
```

Do not push.

## Task 5: Final verification and local review

**Files:**

- Review: all files changed since `origin/issue-355-liveramp-integration`

- [ ] **Step 1: Run the focused gate**

```bash
./scripts/test-cli.sh
cargo clippy-cli
cargo fmt --all -- --check
cd docs && npm run format
```

Expected: every command exits 0; no test, lint, or formatting failures.

- [ ] **Step 2: Run the broader PR regression gate**

The branch also contains core and JS LiveRamp work, so run the repository-required relevant suites:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-codegen
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cd crates/trusted-server-js/lib
npx vitest run
node build-all.mjs
npm run format
```

Expected: all commands exit 0. If an environment-dependent suite cannot run, record the exact command and error rather than claiming it passed.

- [ ] **Step 3: Review the complete local diff**

```bash
git status --short --branch
git diff --check origin/issue-355-liveramp-integration...HEAD
git diff --stat origin/issue-355-liveramp-integration...HEAD
git log --oneline origin/issue-355-liveramp-integration..HEAD
```

Confirm:

- production CLI code contains no vendor name;
- registry and manifest are the only mapping/inclusion sources;
- malformed input cannot be silently skipped;
- stale manifests cannot be reused;
- metadata is patched only after validation;
- unrelated `main` changes are present only through the local merge commit;
- no secrets, Placement IDs, or envelope values were added.

- [ ] **Step 4: Request code review**

Invoke `@superpowers:requesting-code-review` against the final local diff and address any verified findings one at a time.

- [ ] **Step 5: Stop before remote mutation**

Report the local commits, verification evidence, and any remaining live-validation work. Do not push, update PR #1054, reply to GitHub comments, or change the draft state without explicit user authorization.
