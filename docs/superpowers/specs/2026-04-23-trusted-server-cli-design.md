# Technical Specification: Trusted Server CLI (`ts`)

**Status:** Draft
**Author:** Engineering
**Related work:** Runtime config PR `#647`, JS asset auditor PR `#633`
**Last updated:** 2026-04-23

---

## Table of Contents

1. [Overview](#1-overview)
2. [Goals](#2-goals)
3. [Non-goals](#3-non-goals)
4. [Primary personas](#4-primary-personas)
5. [Product and packaging](#5-product-and-packaging)
6. [Design principles](#6-design-principles)
7. [Top-level command surface](#7-top-level-command-surface)
8. [Shared CLI conventions](#8-shared-cli-conventions)
9. [`ts config`](#9-ts-config)
10. [`ts audit`](#10-ts-audit)
11. [`ts dev`](#11-ts-dev)
12. [`ts auth fastly`](#12-ts-auth-fastly)
13. [`ts provision fastly`](#13-ts-provision-fastly)
14. [Automation and JSON output](#14-automation-and-json-output)
15. [Errors and exit behavior](#15-errors-and-exit-behavior)
16. [Security considerations](#16-security-considerations)
17. [Open questions](#17-open-questions)
18. [Implementation plan](#18-implementation-plan)
19. [Appendix: Example command flows](#19-appendix-example-command-flows)

---

## 1. Overview

Trusted Server needs a first-class CLI to improve both operator experience and developer experience.

The CLI will:

- treat `trusted-server.toml` as the source of truth
- support local config initialization and validation
- support generation of a draft config from a JS asset audit
- support local development using runtime config
- support provider authentication
- support provisioning provider-side resources from local config

The first implementation target is Fastly, but the command surface must be designed so that Cloudflare, Akamai, and other adapters/providers can be added later without reshaping the entire UX.

This spec defines the v1 command surface and operational behavior for a new Rust crate built with `clap`.

---

## 2. Goals

### 2.1 Primary goals

1. **Make Trusted Server easier to operate on Fastly**
   - provision required Fastly resources
   - push config-derived data into remote stores
   - bind required resources to a Fastly Compute service

2. **Improve local development workflow**
   - use `trusted-server.toml` as runtime config input
   - provide a single local dev entrypoint

3. **Bootstrap config more easily**
   - create a baseline `trusted-server.toml`
   - generate a draft config from a website audit

4. **Support automation from day one where it matters**
   - stable exit behavior
   - machine-readable output for automation-oriented commands
   - explicit non-interactive execution for mutating commands

### 2.2 Architectural goals

1. **Preserve `trusted-server.toml` as source of truth**
2. **Separate local config operations from remote provider operations**
3. **Keep provider-specific behavior under provider-specific command trees**
4. **Design for future providers without overbuilding v1**

---

## 3. Non-goals

The following are explicitly out of scope for v1:

1. **General Fastly service lifecycle management**
   - creating Compute services
   - broad management of unrelated Fastly service configuration
   - full Fastly version activation workflows

2. **Destructive reconciliation**
   - deleting stores
   - pruning remote entries not present in local config
   - destroying bindings or provider resources

3. **Smart merge/editing of existing config files during audit**
   - `ts audit` generates fresh files
   - it does not merge into an existing `trusted-server.toml`

4. **Interactive config wizards**
   - `ts config init` is non-interactive in v1

5. **Template system for config initialization**
   - templates may be added later

6. **Authenticated page/session audit flows**
   - v1 audit targets a public URL

7. **Saved plan artifacts**
   - `plan` previews changes
   - `apply` recomputes from current state

---

## 4. Primary personas

The CLI must optimize for three jobs:

1. **Trusted Server developers**
   - primary user for `ts dev`
   - secondary user for `ts config init` and `ts config validate`

2. **Operators / platform engineers**
   - primary user for `ts auth` and `ts provision`

3. **Solution engineers / site owners / auditors**
   - primary user for `ts audit`
   - secondary user for config bootstrap

Priority ordering for v1 UX:

1. developer local workflow
2. operator provisioning workflow
3. auditor/bootstrap workflow

---

## 5. Product and packaging

- **Crate name:** `trusted-server-cli`
- **Installed binary name:** `ts`
- **Implementation language:** Rust
- **Argument parser:** `clap`

The CLI is a single binary with multiple subcommands, not multiple standalone tools.

---

## 6. Design principles

### 6.1 Config-first model

`trusted-server.toml` is the source of truth in v1.

That means:

- `ts config ...` works with the local config file
- `ts audit ...` generates a local config file
- `ts dev ...` runs from the local config file
- `ts provision ...` derives remote provider state from the local config file

Remote provider state is applied or synchronized from the config; it is not the primary source of truth.

### 6.2 Strong local/remote separation

The command tree must clearly separate:

- **local config operations** under `ts config`
- **remote provider operations** under `ts auth` and `ts provision`

When users say “write to the config store” in the Fastly context, that means:

- `trusted-server.toml` → Fastly remote config store

That is a provisioning operation, not a local config operation.

### 6.3 Future-provider-friendly command shape

Top-level commands should be task-oriented, with provider names nested beneath provider-specific tasks.

This keeps the UX extensible for future support such as:

- Fastly
- Cloudflare
- Akamai
- additional local adapters

### 6.4 Scriptable where it matters

Automation-oriented commands should support:

- stable exit codes
- `--json` where results are naturally structured
- non-interactive execution for mutating commands

### 6.5 Safety first

Mutating remote operations must be:

- previewable via `plan`
- confirmed by default
- non-destructive in v1
- idempotent by design
- fail-fast on error

---

## 7. Top-level command surface

The v1 top-level command surface is:

```text
ts config ...
ts audit ...
ts dev ...
ts auth <provider> ...
ts provision <provider> ...
```

### 7.1 Concrete v1 commands

```text
ts config init
ts config validate

ts audit <url>

ts dev

ts auth fastly login
ts auth fastly status
ts auth fastly logout

ts provision fastly plan
ts provision fastly apply
```

### 7.2 Future expansion

This shape intentionally leaves room for:

```text
ts auth cloudflare ...
ts auth akamai ...

ts provision cloudflare ...
ts provision akamai ...
```

without needing to redesign the top-level UX.

---

## 8. Shared CLI conventions

### 8.1 Main config path

The main Trusted Server config file defaults to:

```text
./trusted-server.toml
```

Commands that consume or write the main config file should use:

```text
--config <path>
```

This applies to:

- `ts config init`
- `ts config validate`
- `ts dev`
- `ts provision fastly plan`
- `ts provision fastly apply`
- `ts audit`

### 8.2 File overwrite behavior

Commands that write files must fail if the target file already exists, unless the user passes:

```text
--force
```

This applies to:

- `ts config init`
- `ts audit`

### 8.3 Human vs machine output

- Human-readable output is the default
- `--json` is supported only on commands that are natural automation candidates
- In JSON mode, stdout must contain machine-readable output only; human progress/logging must go to stderr

### 8.4 Confirmation behavior

Mutating remote apply operations must prompt for confirmation by default and accept:

```text
--yes
```

for non-interactive use.

### 8.5 Validation before execution

Commands that depend on `trusted-server.toml` must validate it before proceeding:

- `ts config validate`
- `ts dev`
- `ts provision fastly plan`
- `ts provision fastly apply`

### 8.6 Missing config behavior

If a command requires a config file and it does not exist:

- the command fails
- the error message must state the resolved path
- the error message should suggest `ts config init` or `--config <path>`

---

## 9. `ts config`

### 9.1 Scope

`ts config` is for local config file operations only.

In v1 it includes only:

- `init`
- `validate`

It does **not** include local mutation/editing commands such as `set`, `format`, `show`, or `doctor`.

### 9.2 `ts config init`

#### Purpose

Create a baseline `trusted-server.toml` starter file.

#### Behavior

- defaults to writing `./trusted-server.toml`
- supports `--config <path>`
- fails if the target file exists unless `--force` is supplied
- is non-interactive in v1

#### Output shape

The generated config should be:

- valid TOML
- a useful starter, not a bare minimum stub
- opinionated enough to guide users
- concise in comments

It should include:

- the required baseline structure for a usable Trusted Server config
- short comments explaining major sections
- placeholders or commented guidance where helpful

It should not be a giant in-file reference manual.

#### Non-goals

- no wizard in v1
- no template system in v1

### 9.3 `ts config validate`

#### Purpose

Validate a local `trusted-server.toml` without contacting remote providers.

#### Behavior

- defaults to reading `./trusted-server.toml`
- supports `--config <path>`
- fails if the file does not exist
- performs local-only validation

#### Validation categories

1. file existence/readability
2. TOML syntax
3. schema/structural validity
4. local semantic validity

Examples of local semantic validity include:

- required field presence
- invalid combinations of settings
- invalid enum-like values
- obviously malformed local values

#### Explicit non-goals

`ts config validate` does **not**:

- check provider auth
- contact Fastly or any remote provider
- verify remote store existence
- verify service IDs

#### Automation support

`ts config validate` must support:

```text
--json
```

Expected behavior:

- exit `0` if valid
- non-zero exit if invalid
- structured diagnostics in JSON mode

---

## 10. `ts audit`

### 10.1 Purpose

Audit a public URL’s JavaScript/third-party assets and generate:

1. a raw-ish audit artifact
2. a Trusted Server config draft

### 10.2 Command shape

```text
ts audit <url>
```

### 10.3 Input scope

v1 audit input is a **single public URL**.

Out of scope for v1:

- site crawling
- sitemap inputs
- HAR inputs
- local HTML file input
- authenticated/session-based browsing flows

### 10.4 Outputs

By default, `ts audit` writes two files:

- `./js-assets.toml`
- `./trusted-server.toml`

Supported path flags:

- `--js-assets <path>`
- `--config <path>`

Behavior:

- fail if either output path already exists
- `--force` allows overwrite
- users may suppress either file with dedicated flags

Suggested v1 flags:

- `--no-js-assets`
- `--no-config`

### 10.5 Relationship between outputs

#### `js-assets.toml`

This is the audit result artifact.

It preserves the website audit output in a dedicated file for inspection, debugging, and iteration.

#### `trusted-server.toml`

This is a draft config generated from:

1. the same baseline starter logic used by `ts config init`
2. audited discovery results applied into the relevant sections

The config draft should:

- include all required baseline config elements
- fill or replace the sections that the audit can meaningfully infer
- remain clearly a draft requiring review

### 10.6 Audit scope within the config

The audit currently applies primarily to:

- `integrations`
- publisher-related config

The audit is **not** expected to infer the entire Trusted Server domain model.

Accordingly, draft generation should:

- start from the baseline starter config
- populate audited sections
- leave other sections as defaults/placeholders/comments

### 10.7 Fresh generation only

`ts audit` is a generation command, not a merge/edit command.

It must not attempt to merge into an existing `trusted-server.toml`.

If the target config file exists:

- fail fast by default
- allow overwrite with `--force`

### 10.8 Runtime implementation note

The spec does not require a specific browser engine implementation.

Implementation may use a browser-driven strategy such as:

- Playwright
- Chromiumoxide
- another suitable browser automation approach

Browser engine choice is an implementation concern and remains open for evaluation during implementation.

### 10.9 Success and summary behavior

On successful audit, the command should:

- write the requested output files
- print a concise human-readable summary to stdout

Suggested summary content:

- audited URL
- page title, if available
- number of JS assets discovered
- number of third-party assets/tags discovered
- detected integrations
- output file paths
- notable warnings, if any

### 10.10 JSON output

`ts audit` does **not** require `--json` in v1.

---

## 11. `ts dev`

### 11.1 Purpose

Run Trusted Server locally using runtime configuration from `trusted-server.toml`.

### 11.2 Command shape

```text
ts dev
```

### 11.3 Adapter selection

`ts dev` must be adapter-aware via flags, not provider subcommands.

Supported v1 flags:

```text
--adapter <adapter>
-a <adapter>
```

The default adapter may be inferred or defaulted to the current primary local development target, which is expected to be Fastly in the first implementation.

### 11.4 Config behavior

- defaults to `./trusted-server.toml`
- supports `--config <path>`
- fails if config is missing
- validates config before starting

### 11.5 v1 scope

In v1, `ts dev` is intentionally thin.

Its user-facing contract is high-level:

> Run Trusted Server locally using the given config file and adapter.

The command should not expose implementation details such as file copying or adapter-specific project patching as part of the public CLI contract.

### 11.6 Future direction

Over time `ts dev` may become a richer local environment orchestrator, but v1 should start thin and stable.

### 11.7 JSON output

`ts dev` does not need `--json` in v1.

---

## 12. `ts auth fastly`

### 12.1 Purpose

Manage local credentials used for Fastly provisioning commands.

### 12.2 Command surface

```text
ts auth fastly login
ts auth fastly status
ts auth fastly logout
```

Provider ordering is provider-first by design and should be preserved for future providers.

### 12.3 Canonical usage model

Authentication is explicitly managed under `ts auth ...`.

Provisioning commands do not silently prompt for auth setup.

If auth is missing, provisioning commands must fail with a clear next step, such as:

- run `ts auth fastly login`
- or set `FASTLY_API_KEY`

### 12.4 Credential sources and precedence

Fastly credentials are resolved in this order:

1. `FASTLY_API_KEY`
2. stored credential from `ts auth fastly login`
3. otherwise fail

Environment credentials override stored credentials.

### 12.5 `ts auth fastly login`

#### Behavior

- prompts securely for the Fastly API key
- stores the credential in OS secure storage
- does not support plaintext file fallback in v1

#### Automation model

For automation and CI, users should provide:

```text
FASTLY_API_KEY
```

The CLI does not need a `--token` or `--token-stdin` flag in v1.

#### Secure storage policy

Stored credentials must use OS secure storage.

If secure storage is unavailable or unsupported:

- `login` fails
- users should use `FASTLY_API_KEY` instead

### 12.6 `ts auth fastly status`

#### Purpose

Report local auth status only.

#### Behavior

It should show whether:

- `FASTLY_API_KEY` is present
- a stored credential is present
- which source would win based on precedence

It does **not** need to validate the credential against the Fastly API in v1.

#### Automation support

`ts auth fastly status` should support:

```text
--json
```

### 12.7 `ts auth fastly logout`

#### Behavior

- removes the stored Fastly credential from secure storage
- does not modify environment variables

---

## 13. `ts provision fastly`

### 13.1 Purpose

Provision and synchronize the Fastly resources Trusted Server depends on, based on local config.

### 13.2 Command surface

```text
ts provision fastly plan
ts provision fastly apply
```

### 13.3 Service targeting

Provisioning operates against an existing Fastly Compute service.

The CLI does **not** create the service in v1.

Instead, provisioning commands require a service ID flag:

```text
--service-id <id>
```

In v1, this flag is required on every provisioning command.

### 13.4 Provider resource scope

v1 provisioning scope is limited to the remote resources Trusted Server directly depends on:

- config stores
- secret stores
- KV stores
- bindings/linkage of those resources to the Fastly Compute service

Out of scope for v1:

- general Fastly service configuration management
- unrelated provider resources
- service creation
- destructive lifecycle commands

### 13.5 Config as desired state

Provisioning computes desired provider state from `trusted-server.toml`.

Commands must:

1. load the config file
2. validate it locally
3. resolve credentials
4. inspect relevant remote state
5. compute a plan or execute it

### 13.6 `plan` / `apply` semantics

#### `ts provision fastly plan`

`plan` previews what changes the CLI would make.

In v1, plans should focus tightly on actionable changes only, such as:

- create resource
- adopt/reuse existing explicitly identified resource
- update remote store contents
- create or update required bindings

The plan should not attempt to be a broad remote drift report.

#### `ts provision fastly apply`

`apply` executes the same class of create/adopt/update/bind actions.

It must:

- recompute from current state rather than consume a saved plan artifact
- prompt for confirmation by default
- support `--yes` for automation/non-interactive use

### 13.7 Safety model

Provisioning in v1 must be:

- **non-destructive**
- **idempotent by design**
- **fail-fast on first error**

#### Non-destructive means

`apply` may:

- create missing resources
- adopt/reuse explicitly identified existing resources
- update resource contents where appropriate
- bind/link resources to the Compute service

`apply` must not:

- delete stores
- prune remote entries not represented in local config
- destroy bindings
- perform implicit destructive reconciliation

#### Idempotent means

Repeated `apply` runs with unchanged config and unchanged provider state should converge toward no-op or minimal-op behavior and must not create duplicate resources.

#### Fail-fast means

If an operation fails:

- stop immediately
- report completed actions up to the failure
- report the failed action
- rely on idempotency for safe retry after remediation

### 13.8 Resource ownership model

v1 should support conservative adoption/reuse of existing resources when they are explicitly identified by the desired configuration model.

Plans should clearly distinguish among:

- create
- adopt/reuse
- update

The CLI must not silently seize vaguely matching resources.

### 13.9 Ordering

Operations should run in sensible dependency order.

At minimum:

- a resource must exist before it can be updated or bound

A likely safe sequence is:

1. ensure/create stores
2. update store contents
3. bind/link stores to the service

However, exact ordering between update and binding does not need to be over-prescribed unless implementation reveals a correctness or safety requirement.

### 13.10 Fastly-specific resource identity

There is an unresolved design issue around resource identity in Fastly-related config and APIs.

Current system behavior appears mixed:

- some operations use names
- some management API calls use store IDs
- request signing currently relies on Fastly store IDs in config
- KV naming appears more name-oriented/customizable

This should remain an explicit open question for the team and may require refactoring before finalizing long-term CLI/provider UX.

### 13.11 Sensitive output rules

Provisioning output must never print secret values.

This applies to:

- human-readable output
- JSON output
- plan output
- apply output

Secret-bearing fields should use existing redaction mechanisms such as `Redacted<T>` where applicable.

### 13.12 JSON output

Both provisioning commands should support:

```text
--json
```

Commands requiring JSON support in v1:

- `ts provision fastly plan --json`
- `ts provision fastly apply --json`

---

## 14. Automation and JSON output

### 14.1 General rule

Only commands that are natural automation candidates need JSON support in v1.

### 14.2 Required JSON-supporting commands

- `ts config validate --json`
- `ts auth fastly status --json`
- `ts provision fastly plan --json`
- `ts provision fastly apply --json`

### 14.3 Commands that do not require JSON in v1

- `ts config init`
- `ts audit`
- `ts dev`
- `ts auth fastly login`
- `ts auth fastly logout`

---

## 15. Errors and exit behavior

### 15.1 General behavior

Commands should return non-zero exit codes on obvious operational failure.

Examples:

- missing required config file
- invalid TOML/schema
- missing auth for provider operations
- invalid or missing required flags
- browser launch or page load failure during audit
- provider API failure during provisioning
- file write failure

### 15.2 Apply confirmation behavior

For `ts provision fastly apply`:

- confirmation required by default for human use
- `--yes` bypasses confirmation

### 15.3 Error guidance

When a failure has a clear next step, the command should provide it.

Examples:

- missing config → suggest `ts config init`
- missing auth → suggest `ts auth fastly login` or `FASTLY_API_KEY`

---

## 16. Security considerations

### 16.1 Credential handling

- store local Fastly credentials only in OS secure storage
- no plaintext fallback credential files in v1
- use `FASTLY_API_KEY` for automation

### 16.2 Secret redaction

- never print secret values
- redact sensitive values in both human and machine output

### 16.3 Audit safety

Audit is expected to target public URLs only in v1. Authenticated session handling is out of scope.

---

## 17. Open questions

The following questions remain intentionally open and should be resolved before or during implementation.

### 17.1 Fastly resource identity model

Should provider resources be identified in config by:

- names
- IDs
- a normalized logical abstraction that maps to provider-specific identifiers

Current Fastly behavior is mixed and likely needs cleanup.

### 17.2 Secure storage implementation details

The CLI should use OS secure storage, but implementation details remain open, including:

- crate choice
- Linux runtime behavior and availability constraints
- testing strategy

### 17.3 Audit browser implementation

Implementation may use Playwright, Chromiumoxide, or another browser strategy. This should be decided during implementation based on reliability and bot-detection behavior.

### 17.4 Future-provider provisioning model

The top-level command surface is provider-friendly, but specific resource models for Cloudflare and Akamai are intentionally deferred.

---

## 18. Implementation plan

### 18.1 Crate creation

Create a new crate:

```text
crates/trusted-server-cli/
```

with binary:

```text
ts
```

### 18.2 Recommended implementation phases

#### Phase 1: CLI skeleton and command parsing

Implement the `clap` command tree and shared option parsing for:

- `config init`
- `config validate`
- `audit`
- `dev`
- `auth fastly login|status|logout`
- `provision fastly plan|apply`

Deliverables:

- stable command surface
- help text
- argument validation

#### Phase 2: Local config flows

Implement:

- baseline starter config generation
- local config validation
- shared config path resolution

Deliverables:

- `ts config init`
- `ts config validate`
- shared config loading/validation for `dev` and `provision`

#### Phase 3: Audit integration

Implement:

- single-URL audit execution
- `js-assets.toml` generation
- draft `trusted-server.toml` generation via shared starter baseline
- summary output

Deliverables:

- `ts audit <url>`

#### Phase 4: Fastly auth

Implement:

- secure prompt login
- OS secure storage integration
- local auth status
- logout behavior
- `FASTLY_API_KEY` precedence handling

Deliverables:

- `ts auth fastly login`
- `ts auth fastly status`
- `ts auth fastly logout`

#### Phase 5: Fastly provisioning

Implement:

- plan computation from config + remote state
- non-destructive apply
- service bindings
- JSON output
- confirmation flow

Deliverables:

- `ts provision fastly plan`
- `ts provision fastly apply`

#### Phase 6: Dev entrypoint

Implement:

- adapter-aware local dev command
- runtime config loading
- initial Fastly local adapter flow

Deliverables:

- `ts dev`

### 18.3 Internal architecture recommendations

The implementation should keep the following internal boundaries:

- CLI parsing layer
- shared config loading/validation layer
- audit service layer
- auth provider abstraction
- provision provider abstraction
- adapter abstraction for local dev

This will make future provider/adapter support easier without forcing the first implementation to over-generalize.

---

## 19. Appendix: Example command flows

### 19.1 Bootstrap from scratch

```bash
ts config init
ts config validate
ts auth fastly login
ts provision fastly plan --service-id svc_123
ts provision fastly apply --service-id svc_123
```

### 19.2 Bootstrap from audit

```bash
ts audit https://example.com
ts config validate
ts auth fastly login
ts provision fastly plan --service-id svc_123
ts provision fastly apply --service-id svc_123
```

### 19.3 Local development

```bash
ts config validate
ts dev -a fastly
```

### 19.4 Automation example

```bash
FASTLY_API_KEY=... ts provision fastly plan --service-id svc_123 --json
FASTLY_API_KEY=... ts provision fastly apply --service-id svc_123 --yes --json
```

---

## Summary

This spec defines a single `ts` CLI that is:

- config-first
- provider-extensible
- safe for remote operations
- pragmatic for local development
- scriptable where needed

The v1 surface is intentionally focused:

- local config init/validate
- single-URL audit to `js-assets.toml` + `trusted-server.toml`
- adapter-aware local dev
- Fastly auth
- Fastly plan/apply provisioning

while leaving open the provider identity and implementation details that still need team discussion.
