# Design Spec: Jurisdiction Permission Model

**Status:** Proposed. The implementation is on the branch
`split/3-permissions` of the fork jwrosewell/trusted-server, which is row 4 of
the stack's pull requests that build this design, numbered as in
IABTechLab/trusted-server#777. Row 4's pull request
(IABTechLab/trusted-server#1345) carries the same commits on the branch
`feature/3-permissions` and is not yet merged to main. The stack starts at the
pull request that carries the design documents
(IABTechLab/trusted-server#1342). The pull request that carried the
implementation before (#1045) was closed on 2026-10-15 without merging.
Revised against that implementation, 2026-08-25. Revised again on 2026-09-01
for the `rules:` tree, which replaces the flat rule keys and retires
`[geo] default_country` (§3.2, §5.4, and the §12 record). Revised again on
2026-09-14 for permission signal modules, which replace the fixed signal
precedence with a configured order (§4, and the §13 record). Revised on
2026-10-06 to say module for what an operator selects, and on 2026-10-07 to
name a module by its crate folder, so the selector reads
`[permission-signal] modules`. Revised on 2026-10-15 after a review of this
page against the code at `split/3-permissions` (see the Review log), so that
§1 to §8 and the records in §10 to §13 state what that code does. Revised on
2026-10-27 for the policy file, which is never compiled into a build, as
agreed with IAB Tech Lab at the Trusted Server Touchbase on 2026-10-23, and
which is selected and supplied when a service is deployed, as decided by
51Degrees after the Touchbase (§3.1, §3.6 and §3.7). Row 4 builds that design,
and where this page says how the policy file reaches a deployment, it states
the code of row 4's pull request, IABTechLab/trusted-server#1345. Where this
page says what rows 9 and 11 add, it states the code of their pull requests,
IABTechLab/trusted-server#1350 and IABTechLab/trusted-server#1352.
**Author:** Engineering
**Issue references:** #779
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`
**Last updated:** 2026-10-27

> **Context.** PR #838 proposed a permission model whose review surfaced two
> classes of defect this spec exists to prevent. The first was silent
> inversion of consent-signal precedence, most seriously a present TCF string
> short-circuiting GPC/GPP/US-Privacy opt-outs through a rule fixed in code.
> The second was fail-open jurisdiction resolution when geolocation is
> disabled. Precedence is now explicit rather than silent, being the order a
> deployment configures, validated and logged at startup. The default order
> lets an answer given through a consent interface amend an opt-out the
> browser sent, and a deployment reverses that by listing the opt-out last
> (§4). The fail-open resolution is closed. The precedence rules (§4) and the
> failure-mode matrix (§6) are normative and are backed by tests in
> `crates/trusted-server-core/src/permission_signal/mod.rs`,
> `crates/trusted-server-core/src/ec/consent.rs` and
> `crates/trusted-server-adapter-axum/tests/permission_signals.rs`. One
> structural position of the 2026-07-31 draft is adopted in part. The policy
> stays a `permissions.yaml` file rather than rules in a `[permissions]`
> section of `trusted-server.toml`, and `[permissions]` names that file, so
> the file is selected and supplied when a service is deployed and never
> compiled into a build (§3.1). Every other draft position that was narrowed,
> simplified, or deferred is recorded in §11, and the move to configured
> precedence in §13.

---

## 1. Overview

The permission model replaces the hard-wired jurisdiction gate
(`allows_ec_creation` and its companions, now removed) with a single resolved
**permission set** per request. The data decisions Trusted Server itself makes
through this model are EC module execution, EC creation and withdrawal, EID
transmission into the bidstream, and sharing of the EC identifier beyond the
edge (§7). Server-side auction dispatch is not yet a consumer (§7.4).

The set is resolved from three inputs:

1. **Jurisdiction**, the country and optional region the request resolves to
   (§5).
2. **Policy**, a declarative map from jurisdiction to a baseline acquisition
   rule per permission, plus a declared signal policy (§3).
3. **Signals**, answered by the permission signal modules a deployment
   configures, in the order it configures (§4). Five implementations ship,
   being `gpc`, `gpp`, `us-privacy`, `tcf` and `mtm`, each a crate under
   `crates/permission-signal/`. The first four read the `Sec-GPC` header and the
   IAB GPP, US Privacy and TCF strings, and `mtm` reads the answer of the
   Preference Management Platform (PMP) under the Model Terms for Marketing
   (§4.6).

Issues #777 and #779 also envision publisher interaction and external services
as permission sources. The module seam is that source interface. A module
is a crate implementing `PermissionSignalModule`
(`crates/trusted-server-core/src/permission_signal/mod.rs`) that answers
`Grant`, `Revoke` or `Neutral` for each permission, so a new source is a new
module rather than a new resolution algorithm. Each adapter links the module
crates and lists them, in the default order, in its own
`shipped_signal_modules()` function, and `build_permission_signal_modules` in
core selects and orders them from `[permission-signal] modules` (§4, §4.7). A
module is not an integration, so none of the five crates registers through the
integration registry, ships browser JavaScript, or is selected in a page
integration's section.

Core is not silent about the schemes. Its consent pipeline (`consent/`) decodes
TCF, GPP and US Privacy strings, and the policy vocabulary in `permissions.rs`
names the opt-out sources (`gpc`, `gpp_sale_opt_out` and `us_privacy_opt_out`)
and the `signals.tcf` block. What each scheme says about each Data Use, such as
the TCF purpose map, lives in the module crate for that scheme, and core
answers for no scheme ahead of the modules (§4). §10 and §13 record the change.

Scope. The model governs decisions Trusted Server makes. A downstream protocol
receives the full regulatory context only where that protocol defines fields
for it, being the OpenRTB consent fields, which carry only the raw strings a
signal module vouched for (§4.4). The draft's stronger rule, that identity rows
carry normalized per-permission provenance and a digest and never a raw string,
is **not yet true**, because the identity-graph entry (`KvEntry` in
`ec/kv_types.rs`) stores the raw TCF and GPP strings that a module vouched for
alongside the row today. The normalized provenance model travels with the
providers-spec storage work and is recorded as deferred (§11).

## 2. Vocabulary: the IAB Privacy Taxonomy Data Uses

Permissions are named by **IAB Privacy Taxonomy Data Uses**, mapped from the
IAB TCF Europe purposes and used strictly as technical identifiers. No CMP or
TCF policy is implemented by naming them. This replaces the draft's
TCF-purpose-identifier vocabulary (`store-on-device`,
`select-personalised-ads`). The taxonomy adoption postdates the 2026-07-31
draft and follows the joint taxonomy work with the IAB Tech Lab.

The implementation (`crates/trusted-server-core/src/permissions.rs`) models
the vocabulary in two tiers:

- **Eleven named permissions**, one per TCF purpose 1 through 11, each with a
  Data Use identifier. All eleven are resolved against the session signals
  (§4). A present TCF record grants or revokes each mapped purpose, and `mtm`
  answers for all eleven when the PMP cookie is present (§4.6). Two
  purposes have no published Data Use yet, so purpose 1 uses a proposed
  `necessary.operations.storage` key and purpose 11 keeps its TCF identifier
  `select-basic-content`, both flagged for an upstream taxonomy addition.
- **Fifty-three additional taxonomy Data Uses**, carried so `permissions.yaml`
  can declare a policy flag for the whole taxonomy. No module gates on them
  today, and a signal reaches only some of them. The TCF module maps eight of
  them to a TCF purpose (`crates/permission-signal/tcf/src/mapping.rs`), and
  the opt-out list in the sample policy names four (`disclosure.sale`,
  `disclosure.share`, `disclosure.third_party_sale` and
  `advertising_marketing.third_party.targeted`), one of which TCF also maps. A
  signal therefore reaches eleven of the fifty-three under the sample policy,
  and the other forty-two keep their configured baseline. They exist for
  completeness, testing, and demonstration, and where no informed policy
  decision has been made the repository's sample sets them `denied`.

The two Data Uses that carry enforcement weight today are
`necessary.operations.storage` (TCF Purpose 1, storage) and
`advertising_marketing.first_party.targeted` (TCF Purpose 4, personalized-ad
selection). Module execution gates on whatever a module declares (the
built-in Edge Cookie modules declare storage), and sharing beyond the edge
gates on the storage plus personalized-ad pair (§7).

This is a deliberate departure from the draft's rule that a permission appears
only when it has both a signal mapping and an enforcement point. The eleven
named Data Uses all have the signal mapping, and the fifty-three further
Data Uses are declared policy rather than enforced policy, as no module gates
on them. The header of the repository's sample states this plainly, and the
`denied` default means an undeployed flag cannot silently authorize anything.
Policy validation still **rejects** any rule or flag that references an
identifier outside the modeled vocabulary, so a policy cannot name a Data Use
the code does not know.

The eleven named Data Uses, with the TCF purpose each maps from:

| #   | Data Use identifier                             | TCF purpose                                     |
| --- | ----------------------------------------------- | ----------------------------------------------- |
| 1   | `necessary.operations.storage`                  | Store and/or access information on a device     |
| 2   | `advertising_marketing.first_party.contextual`  | Use limited data to select advertising          |
| 3   | `advertising_marketing.profiling`               | Create profiles for personalised advertising    |
| 4   | `advertising_marketing.first_party.targeted`    | Use profiles to select personalised advertising |
| 5   | `advertising_marketing.personalize.profiling`   | Create profiles to personalise content          |
| 6   | `advertising_marketing.personalize.content`     | Use profiles to select personalised content     |
| 7   | `analytics.ad_reporting.measure_ad_performance` | Measure advertising performance                 |
| 8   | `analytics.ad_reporting.content_performance`    | Measure content performance                     |
| 9   | `analytics.ad_reporting.market_research`        | Understand audiences through statistics         |
| 10  | `necessary.operations.improve`                  | Develop and improve services                    |
| 11  | `select-basic-content`                          | Use limited data to select content              |

(The purpose names are the IAB names verbatim, including their original
spelling.)

## 3. Policy

### 3.1 The policy file is selected when a service is deployed

Policy lives in a human-editable permissions YAML document, which this spec
calls `permissions.yaml` after the name the code and the guides use. No
build carries a policy and no policy ships with Trusted Server, so the only
default is no policy at all. A service is given the policy for each domain
when it is deployed, and a domain without a valid policy file does not
start. On 2026-10-23, at the Touchbase, it was agreed with IAB Tech Lab
that neither the configuration nor the policy is ever compiled into the WASM
binary, so that Tech Lab can release one binary for every publisher, and
that a binary starts no domain unless it is given a valid policy. The
attestation issue, IABTechLab/trusted-server#1235, gives the reason as "An
attestation only means something if every publisher runs the same bytes."
How the file reaches a service, as this section describes it, was decided
by 51Degrees after the Touchbase. §3.6 lists what a build does carry.

Row 4 of the stack (IABTechLab/trusted-server#1345) builds this design, and
this section states the code of that pull request. Where this section says
what rows 9 and 11 add, it states the code of their pull requests,
IABTechLab/trusted-server#1350 and IABTechLab/trusted-server#1352, whose
branches carry row 4's commits. Row numbers here are those of
IABTechLab/trusted-server#777, and a branch keeps its own number. No build
carries a policy, because `DEFAULT_PERMISSION_RULES`,
`PermissionMaps::standard`, `validate_default_policy` and
`GeoConfig::validate_permission_policy`, which compiled the repository's
sample into core, checked it and parsed it, are gone. Each adapter keeps the
policy its domain loaded in its application state and hands it to every
request through `RuntimeServices::permission_policy` (`platform/types.rs` line
340). The Edge Cookie context passes that policy's rules (`ec/mod.rs`
line 461) to `default_jurisdiction`, `assemble_permissions` and
`storage_acquisition` in `ec/consent.rs`, and through
`ConsentPipelineInput::permission_maps` (`consent/mod.rs` line 91) to
`detect_jurisdiction` in `consent/jurisdiction.rs`, each of which takes the
rules as an argument. The services builder requires the policy as it requires
the stores (`platform/types.rs` lines 636 to 638), and each adapter refuses to
build its state from settings that carry none, through
`Settings::required_permission_policy` (`settings.rs` line 3936), so no
request is decided without a policy and nothing stands in for a missing one.
The tests read the sample through `PermissionPolicy::sample()` and
`PermissionMaps::sample()`, which only test builds and core's `test-utils`
feature compile (`permissions.rs` lines 895 to 943), and the `gpc`, `gpp`,
`tcf` and `us-privacy` crates list core with `test-utils` under their
`[dev-dependencies]` for them.

**Blobs, pointers and chunks.** A blob is a domain's settings as
`ts config push` stores them, wrapped in EdgeZero's envelope (`BlobEnvelope`,
which holds the settings as `data` with their SHA-256, the time the envelope
was made and a version). A Fastly config store entry holds at most 8,000
characters, so a longer value is split into chunks of 7,000 characters, each
an entry of its own (`CHUNK_PAYLOAD_TARGET`, `chunked_config.rs` line 42 in
`edgezero-adapter-fastly`), and a pointer, the entry at the value's own key
that names each chunk with its length and SHA-256.

**What row 4 builds.**

1. **The file.** The publisher's `permissions.yaml`, in the format it has
   today (§3.2). The push takes only a file whose name ends `.yaml`, and the
   policy's name is the file's stem, so `standard.yaml` is `standard`
   (`policy_name` in
   `crates/trusted-server-cli/src/commands/config/policy.rs`, called
   `policy.rs` below). The repository's sample is for testing and evaluation
   only, and no tool supplies it in place of the publisher's file.
2. **Selected when the service is deployed.** A domain's settings name the
   policy the domain runs in `[permissions] policy = "<name>"`
   (`PermissionsConfig`, `settings.rs` lines 1354 to 1364). `ts` wraps
   EdgeZero's argument types for `ts config push`, `ts config validate` and
   `ts config diff` in types of its own that add `--permissions <file>`, once
   for each file (`policy.rs` lines 45 to 79), and `ts config validate` gains
   no `--adapter`. Each of the three loads the settings with the
   `TRUSTED_SERVER__` environment overlay applied, being the environment
   variables named `TRUSTED_SERVER__` and a setting's path, which replace that
   setting, through EdgeZero's `deserialize_app_config_with_options` and its
   raw loader (`load_settings` and `prepare`, lines 263 to 331). An
   operator's `--no-env` leaves the overlay out of both loads, because
   `load_settings` turns the overlay off in its load options (line 270) and
   `prepare` hands the same options to the raw loader (line 302). The push
   runs the policy checks, adds to `[permissions]` `sha256`, the SHA-256 of
   the exact bytes of the named file in lower-case hexadecimal
   (`policy_sha256`, `permissions.rs` line 768), and, except on Axum (item 4),
   `text`, the file itself. It hands the result to EdgeZero with `no_env` set,
   whether or not the operator gave `--no-env` (line 110), so the overlay is
   applied at most once, and the digest is taken after it and before EdgeZero
   builds the envelope (`run_push_with`, lines 97 to 118).
   EdgeZero `v0.0.10` offers no public function that takes the settings as a
   document, because its typed push, validate and diff read them from the
   `--app-config` path (`config.rs` lines 452 to 470 in `edgezero-cli`), so
   `ts` writes the result to a temporary file under the system temp folder,
   never in the working tree, which is removed when the command ends, unless
   the command is interrupted at EdgeZero's confirmation prompt
   (`write_settings`, lines 439 to 451). The push then prints the policy's
   name and digest.

   The policy checks are that the settings name a policy and that a supplied
   file has that name (`named_policy`, line 410), that every supplied file is
   a `.yaml` policy in UTF-8 text that parses and that no two share a name
   (`read_policy_files`, line 352), and that neither the settings nor the
   overlay set `sha256` or `text`, which only the push writes
   (`refuse_fields_the_push_writes`, line 334). The push always runs them.
   `ts config validate` runs them when `--permissions` is given, leaving the
   text out because validation writes nothing, and without it says "the policy
   was not checked, because no --permissions file was given"
   (`run_validate_with`, lines 136 to 168). `ts config diff` compares the
   settings as the push would write them, digest included, so when the
   settings name a policy it refuses to run without `--permissions`
   (`run_diff`, lines 181 to 212). Every command refuses a `sha256` or `text`
   that the settings or the overlay set, with or without the flag, because
   each loads the settings through `load_settings`. `ts config migrate` and
   `ts audit generate` never run the policy checks, and migrate prints its
   `to do:` line instead (`2026-10-17-config-migrate-design.md`). The runtime
   loader checks the file against the blob's digest (item 7), and a later edit
   of the file never changes what an existing blob runs.

3. **Carried inside the one blob.** At row 4 a service reads one blob, at the
   logical store ID (on Fastly `RuntimeStoreConfig::logical()`, `main.rs` line
   330), so on Fastly, Cloudflare and Spin the policy's text travels inside
   it. EdgeZero's own push writes the blob on all three, and on Fastly splits
   it into chunks as it does any long blob. The repository's sample at row 4
   is 24,271 bytes with the SHA-256
   `1c432b6153ba1a74eb9c28441f0edf7fbd96c08701d1e9d3de01fe56b06572b5`, and
   written as a JSON string it adds 24,824 characters, so it adds three or
   four chunks to a blob already in chunks, and turns a blob that fitted in
   one entry into a pointer and four or five chunks. Each adapter reads the
   blob through the path it reads every blob, so row 4 brings no second writer
   and no reader of its own, and hands the runtime loader `PolicyOrigin::Blob`
   (the Fastly adapter's `app.rs` lines 232 to 238, Spin's `app.rs` line 137
   and Cloudflare's `app.rs` line 144). The loader,
   `settings_from_config_blob` (`config_payload.rs` lines 52 to 84), takes
   `text` out of the settings, checks its SHA-256 against `sha256`, parses it,
   and keeps the policy with the settings, which keep only its name and digest
   (`load_domain_policy`, lines 89 to 129), so a blob there without `text`
   fails to load.
4. **On Axum, a folder.** On Axum the push writes `policy` and `sha256`
   without `text`, and the deployment names a folder of policy files in the
   environment variable `TRUSTED_SERVER_PERMISSIONS_DIR`, where an empty value
   names none. `AxumPolicyFolder` (the Axum adapter's `platform.rs` lines 160
   to 216) reads `<folder>/<name>.yaml`, and refuses a name that is not a
   single file name inside the folder, a file it does not hold and a file that
   is not UTF-8 text. Axum's `build_state` hands it to the runtime loader as
   `PolicyOrigin::Source` when the process starts (`app.rs` line 106), so the
   loader checks the file's SHA-256 against `sha256`, the folder has to hold
   the bytes that were pushed, and Axum never uses a `text` that its blob
   carries.
5. **Cloudflare reads its blob from the config store.** A Worker variable
   holds at most 5 KB on every plan (Cloudflare's Workers
   [Limits](https://developers.cloudflare.com/workers/platform/limits/) page),
   and the sample alone is 24,271 bytes, so the policy cannot travel in the
   `TRUSTED_SERVER_CONFIG` variable that main's Cloudflare adapter reads its
   settings from (main's `src/lib.rs` line 27). EdgeZero's Cloudflare config
   store is a Workers KV namespace (the header of `config_store.rs` in
   `edgezero-adapter-cloudflare`), which holds values of up to 25 MiB
   (Cloudflare's Workers KV
   [Limits](https://developers.cloudflare.com/kv/platform/limits/) page), and
   `ts config push` writes the blob there (`push_config_entries` in that
   crate's `cli.rs`, through `wrangler kv bulk put`). So the Worker reads its
   blob from that namespace. A Workers KV read is asynchronous, and EdgeZero's
   `run_app` builds the application with the synchronous `Hooks::build_app`,
   so the Worker's fetch entry point calls the adapter's own `build_app`,
   which resolves the binding and the key as the push does, from
   `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` and `__KEY` with
   `trusted_server_config` as both defaults, awaits the read, builds the
   application from the settings, and serves it with EdgeZero's `dispatch_app`
   (the Cloudflare adapter's `lib.rs` lines 19 to 40, and `app.rs` lines 87 to
   159). A binding that does not open, a read that fails and a namespace with
   nothing at the key are each a configuration error, so the Worker answers
   500 from the startup error router, which logs the reason from row 54 (item
   7), and the synchronous `Hooks::routes` serves that router too, because it
   cannot read the store (`app.rs` lines 490 to 496). `wrangler.toml` and
   `wrangler.ci.toml` bind the config store as a namespace of its own,
   `trusted_server_config`, beside `TRUSTED_SERVER_KV`, and `cloudflare.toml`
   names the same binding. The variable bridge is gone from the adapter's
   code, manifests and `README.md`, from the integration tests' Cloudflare
   environment, which stores its blob in the local namespace with
   `wrangler kv key put`, from `scripts/smoke-cloudflare.sh`, which pushes
   into the namespace with `ts config push --adapter cloudflare --local`, and
   from `docs/guide/cloudflare.md`, with its hand copy and double encoding.
   The adapter's test `no_wrangler_manifest_carries_settings_in_a_variable`
   (`tests/config_defaults.rs`) checks that no manifest carries settings in a
   variable.
6. **One way in.** Core declares `PlatformPolicySource`
   (`platform/policy_source.rs` lines 13 to 23), a trait with one method,
   `policy_text`, that returns the text of the file a reference names, the
   reference being `policy` and `sha256` (`PolicyReference`, `permissions.rs`
   line 775), and `PolicyOrigin` (lines 27 to 32), which tells the runtime
   loader whether a domain's file comes from its blob or from a source. At row
   4 only Axum implements the trait, over the folder, because elsewhere the
   text arrives inside the blob. `load_policy` (`permissions.rs` line 821)
   checks the SHA-256 of the text against `sha256`, parses it, and keeps one
   parsed copy for each digest for the life of the instance, under whatever
   name it is loaded (`parsed_policy`, lines 862 to 875), so two domains that
   pin one digest share one copy. The runtime loader asks for that copy
   (`held_policy`, lines 851 to 858) before it asks a source for the text.
7. **Never started without a valid file.** A domain fails to load when its
   settings name no policy, or name one with no `sha256`, when the file is
   absent, which on Fastly, Cloudflare and Spin means a blob without `text`,
   when on Axum the file cannot be read from its folder or
   `TRUSTED_SERVER_PERMISSIONS_DIR` is unset, and when the file's SHA-256 does
   not match `sha256` or its YAML fails the checks of §3.3
   (`load_domain_policy` and `load_policy`). Each adapter's startup router
   answers with the status of the error's kind (`status_code` in `error.rs`),
   500 for a configuration or settings error and 503 for a key-value store
   error (`TrustedServerError::KvStore`), and logs the reason at error level,
   so nothing is served from the origin. Core reports each of these failures
   as `TrustedServerError::Configuration`, with a reason that names the
   policy, so every request for the domain answers 500. Every failed read of
   the config store is a configuration error as well, a store that cannot be
   opened included (`read_config_entry`, `settings_data.rs` lines 98 to 109),
   and Spin reports a store that does not open the same way (the Spin
   adapter's `app.rs` lines 112 to 121), so a config store that cannot be read
   and an entry or a chunk that is absent each answer 500. Row 4 moves Spin's
   startup router from a fixed 503 to the status of the error's kind (`app.rs`
   lines 601 to 612), so its configuration errors answer 500 too. Fastly and
   Axum log the reason, and Cloudflare and Spin log nothing until row 54, as
   the configuration guide says (`docs/guide/configuration.md` lines 784 to
   788), because EdgeZero's `init_logger` does nothing on either (`lib.rs`
   lines 39 to 41 in `edgezero-adapter-cloudflare`, and lines 78 to 80 in
   `edgezero-adapter-spin`), and row 54 gives each adapter a logger of its
   own. On Fastly the `GET /health` probe reads no settings and answers 200.
   Spin builds its state, the settings and the policy included, before it
   answers any request (`app.rs` lines 653 and 654), and its startup router
   still answers `GET /health` with 200.
8. **Changes to main's behavior.** Row 4 changes two behaviors that main has,
   and its entries in `CHANGELOG.md` list both. Cloudflare reads its blob from
   the config store in place of the `TRUSTED_SERVER_CONFIG` variable (on main,
   line 27 of the Cloudflare adapter's `src/lib.rs`). Spin's startup router
   answers with the status of the error's kind in place of a fixed 503, so its
   configuration errors answer 500 (on main, line 514 of the Spin adapter's
   `src/app.rs`).
9. **Kept once read.** A digest never changes what it names, so a parsed copy
   is safe to keep while the instance lives, and an instance that already
   holds the digest a blob names parses nothing more. Each instance keeps its
   copies in memory. Whether Fastly also keeps the parsed form in its value
   cache (`PlatformValueCache`, a store of bytes that every instance at a
   location shares), which arrives at row 24, is decided by a measurement on
   Fastly staging (§3.7).
10. **Seen from outside.** From row 9 (IABTechLab/trusted-server#1350),
    `/_ts/config` shows `permissions` as the policy's name and `sha256` only,
    never its `text`, and `/_ts/permissions` names the policy whose rules it
    resolved, in the field `policy` that `permissions_payload` adds
    (`inspect/permissions.rs`, and
    `2026-10-12-inspection-endpoints-design.md`). Every place that checks
    `[inspect]` patterns goes through `validate_patterns`
    (`inspect/config.rs`), being `ts config validate`, `ts config push`,
    `ts config diff`, `ts audit generate` and the load, and from row 51
    `ts config migrate`. `validate_patterns` refuses a pattern under
    `permissions.text`, which the view never shows, and judges every other
    pattern against the settings as the view shows a domain the push wrote
    and the loader loaded, with `text` gone and `sha256` present, because the
    push always writes it (`as_the_view_sees_a_pushed_domain`). So a pattern
    that names `permissions.sha256` is judged alike before the push and at the
    load. A name is a file's stem, which two versions of a file share, so the
    SHA-256 is what matches a domain to one exact copy of the file.

**What rows 9 and 11 add over row 4.**

- **Row 9** (IABTechLab/trusted-server#1350) adds the inspection endpoints.
  `permissions_payload` (`inspect/permissions.rs`) gives the permissions
  answer a field `policy`, which names the file whose rules the answer
  resolved, as `[permissions] policy` names it. The configuration view, which
  is the masked settings, shows `policy` and `sha256` and no `text`, because
  row 4's runtime loader takes the text out of the settings when it loads the
  policy (item 3). `validate_patterns` (`inspect/config.rs`) checks
  `[inspect]` patterns against that view (item 10).
- **Row 11** (IABTechLab/trusted-server#1352) adds a blob for each host on
  Fastly, the shared policy entries, and the 421 answer for a host with no
  blob, which stays 421 and is never 500. Only the Fastly adapter chooses a
  blob by host (`config_selection.rs`, and the keyed writer `keyed_push.rs`,
  which "Pushes an app-config blob to Fastly under a key other than the
  logical store ID"). The keyed writer writes a host's blob without `text`.
  For a blob at any key but the logical store ID, the Fastly adapter reads
  the policy from the host's policy entry (`blob_carries_its_policy` and
  `load_settings_from_config_store` in the adapter's `app.rs`).
  `FastlyPolicyEntries` (the adapter's `platform.rs`) implements
  `PlatformPolicySource` over that entry, and reads the entry through
  `FastlyPlatformConfigStore::get` and core's `resolve_fastly_chunk_pointer`
  (from row 49 the adapter's `config_chunks.rs`). Core's `policy_entry_text`
  takes the file out of the entry's envelope. A blob at
  the logical store ID, which EdgeZero's push writes, keeps `text`. One
  function in core, `policy_key` (`platform/policy_source.rs`), forms the key
  `permissions_<sha256>` that the keyed writer and the Fastly adapter use, so
  the command line tool and the adapter share one key format, and row 4 has
  no key function. Cloudflare and Spin keep the text in their one blob, and
  Axum reads the file from its folder when it starts.

**What later rows add, once each is merged forward over row 4.**

- **Row 13** adds the live keyed push. At row 11 the keyed writer writes
  only the local server's store and refuses a live push (`run` in
  `keyed_push.rs`), and from row 13 it writes the live store through the
  `fastly` command line tool.
- **Row 24** adds `PlatformValueCache`, the host's value cache, which only
  the Fastly adapter registers. The policy uses it only if the measurement
  on Fastly staging shows a need (§3.7), and core then uses it wherever an
  adapter registers one, through no branch that names a platform.
- **Row 49** adds the Fastly adapter's chunk reader (`config_chunks.rs`),
  which takes over from core's `resolve_fastly_chunk_pointer`
  (`settings_data.rs`), so from then on the chunks of a blob, and of a
  policy entry, are put back together in the Fastly adapter and not in core.
- **Row 51** adds `ts config migrate`. Its merge with row 4 adds the `to do:`
  line for a document that names no policy, points the `to do:` line on the
  consent lists at the publisher's policy file, and keeps the policy checks
  out of the command's validation, as they are kept out of `ts audit generate`
  (`2026-10-17-config-migrate-design.md`).
- **Row 54** gives the Cloudflare and Spin adapters loggers of their own
  (item 7).

**The shared policy entries, from row 11.** A host's blob carries `policy` and
`sha256` without `text`, and the file is stored once, as a policy entry under
the key `permissions_<sha256>`, however many hosts' blobs pin that digest. The
keyed writer loads and checks the settings through `prepare` in `policy.rs`,
as a push to the logical store ID does, `--no-env` included, and leaves the
text out of the blob. It validates the blob as settings and wraps it in
EdgeZero's envelope (the `envelope` function in `keyed_push.rs`). For each
policy file it builds the envelope itself, with `data` holding the policy's
`name` and `text` (`PolicyEntry` in core's `platform/policy_source.rs`), and
splits it into chunks and a pointer as it does a blob (`policy_entries`). The
entry is therefore in EdgeZero's own format, which EdgeZero's Fastly config
store would accept too, because it accepts any value it puts back together
that parses as an envelope and passes `verify` (`chunked_config.rs` lines 586
to 611). The Fastly adapter reads the entry as it reads a blob (see row 11
above) and hands the whole value to core's `policy_entry_text`, which parses
the envelope, checks it with `verify` and returns `text`. The runtime loader
then checks `text` against the digest (`load_policy`). Before the push skips
writing an entry that already exists, it reads the entry back and checks that
it holds the same file, and it rewrites one that does not (`holds_policy`).
The push writes an entry for each file `--permissions` supplies that the store
does not already hold, before the host's blob (`run`). At row 11 it writes
only the local server's store in `fastly.toml`, all in one write, so nothing
has to wait. From row 13 (IABTechLab/trusted-server#1354), a live push that
has just written a policy entry waits `POLICY_PROPAGATION_MARGIN_SECONDS`, 60
seconds, before it writes the blob (`push_remote` in `keyed_push.rs`).
Fastly documents no time for a change to reach every location (its guide
[Working with config stores](https://www.fastly.com/documentation/guides/compute/edge-data-storage/working-with-config-stores)
says only that changes "immediately impact all service versions"), and
EdgeZero notes that the store "is eventually consistent ACROSS keys"
(`config_store.rs` in `edgezero-adapter-fastly`), so the wait is a margin and
not a guarantee. A location that still sees a host's blob before its policy
entry fails to load that host, which answers 500 and logs the reason (item 7).
A hundred hosts that name three files store three policy entries, and each
file the size of the sample is a pointer and four chunks, so fifteen entries
in all. They share the 500 entries that a store of a paid Fastly account holds
(the same guide) with the hundred hosts' own blobs, which take one entry each
where a blob fits in one entry. A changed file is a new entry under a new key,
never an edit of an entry in use, so an instance still serving a previous blob
still finds the file that blob names. The underscore in the key keeps a `Host`
header from naming a policy entry under a selector that is `{host}` alone,
because a host name holds only letters, digits, hyphens and dots
(`2026-10-12-config-by-host-design.md`).

**While a blob propagates, at row 4.** At row 4 the policy's text changes with
its blob, under one key, on Fastly, Cloudflare and Spin, so it has no
propagation window of its own, and Axum reads its folder when it starts. On
Cloudflare and Spin the blob is one value. On Fastly a blob longer than an
entry is a pointer and chunks, and a location can see a new pointer before all
of its chunks. The Fastly adapter reads its blob itself, through
`FastlyPlatformConfigStore::get` (`platform.rs`) and core's
`resolve_fastly_chunk_pointer` (`settings_data.rs`), and not through
EdgeZero's Fastly config store, which answers a missing chunk with a 503 to
retry (`config_store.rs` in `edgezero-adapter-fastly`, lines 60 to 80 and 108
to 110). A chunk the store does not yet hold is "not found", which core
reports as `TrustedServerError::Configuration` (`read_config_entry` in
`settings_data.rs`), so the adapter's startup router answers 500, and not
EdgeZero's 503, with the reason logged (`app.rs` lines 1181 to 1183 and 1427).
A failed build is not kept, so the next request at that location tries again
(`main.rs` lines 390 to 395, where `initialize` keeps only a successful
build). From row 49 the adapter's own chunk reader reports a missing chunk as
"not found" through the same `read_config_entry`, so the answer stays 500.

**The push, further.**

- `--permissions` may be given once for each file. At row 4 a push writes
  one blob, which names one policy, so one file is enough, and any other
  file given is parsed and checked but not written. From row 11 a script
  that pushes many hosts' blobs can give each push every file the hosts use,
  and the keyed push writes each missing policy entry before that host's
  blob.
- A checkout that converts line endings changes a file's bytes and so its
  digest, and the repository has no `.gitattributes` to prevent it, so the
  file must reach the push, and Axum's folder, byte for byte.
- `ts config init` writes the repository's example settings
  (`trusted-server.example.toml`). The example names no policy and carries a
  comment telling the operator to name their own file in
  `[permissions] policy`, and a push of the example as written is refused
  with a message naming what is missing. No tool ever chooses a policy.
- No text in row 4's tree says that a build carries a policy. The sample's
  header (lines 17 to 23), the permission model guide, the example settings
  (`trusted-server.example.toml` lines 153 to 156 and 223 to 235) and the
  permissions inspector's `README.md` and `index.html` say that no build
  carries one and that a domain's settings name the publisher's own file. The
  inspector reads the sample with `include_str!` and parses it with
  `PermissionMaps::from_yaml` (`tools/permissions-inspector/wasm/src/lib.rs`
  lines 45 and 51).

**Removing an entry that no blob names is open.** EdgeZero's clean-up
command, `ts config gc` (`ConfigGcArgs` in
`crates/trusted-server-cli/src/run.rs`), does not do it. At EdgeZero
`v0.0.10`, the version the stack pins, the command runs on Fastly only, and
every other adapter answers that it does not implement the command. On
Fastly it removes only chunks, being entries whose key holds the reserved
`.__edgezero_chunks.` part, that no entry in the store points to and that
are older than the window `--older-than` asserts. It keeps every entry at an
ordinary key as one that may still be read, a policy entry among them,
because it cannot tell which entries a blob names (`classify_store_entries`
in EdgeZero's `crates/edgezero-adapter-fastly/src/cli.rs`). Removing a
policy entry needs a command that reads every blob in the store, collects
the digests the blobs name, and removes the other `permissions_` entries
once no instance can still be serving a blob that names them, which is the
assertion `--older-than` makes for chunks. No such command exists. Until one
does, an entry that no blob names stays in the store and changes no answer,
because no blob leads an instance to it.

The draft wanted policy in `[permissions]`, flowing through `ts config push`
with the settings so that it is validated, compared and pushed with them
(§11). Settings now name the policy in `[permissions]` and the push supplies
the file, so the policy gets all three, while the file keeps the YAML form a
policy owner reads. The draft's staged activation (§5.5) is still not built,
so a push reaches instances as any settings push does, and §5.5 says what
that means while a domain's policy changes.

The draft's specific complaints are answered:

- The parse runs once per instance for each digest, when a domain's settings
  load, and a file that does not parse fails that load (item 7). Fastly,
  Cloudflare and Spin load the settings for each request unless an instance
  keeps its application, so such a domain answers 500 on every request until
  a valid file is pushed, and no request is ever served under a policy that
  did not parse. `ts config push` runs the same parse first, as
  `ts config validate` does when `--permissions` is given, so a malformed
  file is refused before it reaches a store.
- The draft's complaint about unknown fields is only partly answered, because
  the parser does not reject unknown keys as such. Any key beside `group`,
  `jurisdiction` and `permissions` in a node of the `rules:` tree is read as a
  place code, since the tree has no other way to name a child. Under a region
  that is refused as a place written under a region. At the top and under a
  country, a misspelled key fails only when its value is neither the name of a
  defined group nor a block carrying `group:`, and otherwise it becomes a place
  that no request matches. A key outside `groups`, `rules` and `signals` is
  ignored, and so is a key inside `signals`, `signals.tcf` or
  `signals.us_opt_out` that the parser does not define. No test covers a
  misspelled key (Open questions, item 3).
- Two rule keys naming the same location in different case are rejected at
  parse, so one spelling cannot silently overwrite another.
- Auditability. The publisher keeps the file under version control or as
  they choose, and the SHA-256 in each blob ties what a domain runs to one
  exact version of it, which `/_ts/config` shows (item 10).

No build compiles the sample in, so core's library code reads no policy file
above the crate root, which settles the packaging concern the draft raised
about the `include_str!` path. Only test builds and core's `test-utils`
feature read the sample (`permissions.rs` lines 895 and 896, and §3.5), and
core's build script still reads the workspace's `Cargo.toml`, `Cargo.lock` and
`edgezero.toml` above the crate root (`WORKSPACE_INPUTS` in `build.rs`).

**Fallback posture.** A domain that serves requests always has a policy,
because a domain whose settings name no valid file never loads (item 7), so
there is no "no policy" state. Every location resolves to a group, because
the tree's top node is required and states the baseline for a request whose
country cannot be resolved (§3.2). A failed geo lookup is the separate state,
and it resolves every permission to the **requires-signal floor**, where
nothing is set without a signal that grants it. That floor also covers the
case where resolution finds no node at all, which should never happen.
Absence of an applicable node is always safe, and there is no fail-open
default. The draft's `regime = "gdpr"` component of the protective profile
has no implemented counterpart because no regime concept exists (§3.2).

### 3.2 Format

Named **groups** (baselines) and a **rules** tree that maps places to a group,
plus a **signals** section that declares how each session signal maps onto Data
Uses. Each permission resolves to an **acquisition rule**:

- `granted`, set without any signal,
- `requires_signal`, set only when a signal grants it (opt-in),
- `denied`, never set, even when a signal grants it.

```yaml
# permissions.yaml (abbreviated). The repository's sample lists every Data
# Use in every group so each group's meaning is fully explicit.
groups:
  gdpr-eu:
    necessary.operations.storage: requires_signal
    advertising_marketing.first_party.targeted: requires_signal
    # ... every remaining Data Use, requires_signal or denied
  gdpr-uk:
    necessary.operations.storage: granted
    # ... the other mapped purposes requires_signal, the rest denied
  us-opt-out:
    necessary.operations.storage: granted
    advertising_marketing.first_party.targeted: granted
    # ... the other mapped purposes granted, the rest denied

rules:
  group: gdpr-eu # baseline when no country can be resolved
  jurisdiction: gdpr # inherited by every node that states none
  FR: gdpr-eu
  GB: gdpr-uk
  AU: us-opt-out
  US:
    group: us-opt-out
    jurisdiction: non-regulated
    # A region node takes precedence over its country, and a state with a
    # comprehensive privacy law names itself with `jurisdiction: us-state`. A
    # node written as a mapping may also apply explicit per-permission
    # acquisitions on top of its group:
    # CA:
    #   group: us-opt-out
    #   jurisdiction: us-state
    #   permissions:
    #     advertising_marketing.first_party.targeted: requires_signal

signals:
  tcf:
    authoritative: true
  us_opt_out:
    sources: [gpc, gpp_sale_opt_out, us_privacy_opt_out]
    revokes:
      - necessary.operations.storage
      - disclosure.sale
      # ... sharing, third-party sale, targeted advertising and profiling
```

Format rules, as implemented:

- A group is a flat map of Data Use to acquisition flag, with an optional
  `default` key covering any permission the group omits. A group without
  `default` must list **every** modeled permission exactly once, or the
  parse fails (`IncompleteGroup`). The sample's groups list every Data Use.
- `rules` is one tree. Every node has a `group`, and children are optional.
  Child keys are place codes, so the first level holds ISO 3166-1 alpha-2
  country codes and the levels beneath a country hold ISO 3166-2 region codes
  with no country prefix. Codes are matched case-insensitively.
- A node whose value is a single string has that string as its `group` and no
  children. A node written as a mapping must contain `group:` and may contain
  child place codes beside it. Reserved keys cannot collide with place codes,
  because an ISO code is at most three characters.
- Any node may carry `jurisdiction:` beside `group:`, naming the consent
  handling for the places that node covers. A node that carries none inherits
  the nearest ancestor that does. The top node, meaning the `rules:` mapping
  itself, must carry both, so the inheritance always terminates and the
  unresolved-country case is simply the top node.
- The accepted `jurisdiction` values are the states of the `Jurisdiction` type
  in `crates/trusted-server-core/src/consent/jurisdiction.rs`, being `gdpr`,
  `us-state`, `non-regulated`, and `unknown`. Any other value is a
  configuration error. `us-state` carries **no** state code, because the region
  node it sits on already names the state, so the value is valid only on a
  region node and is rejected at the top and on a country node. The parser does
  not check the country above the region, so `us-state` is also accepted on a
  region under a country other than `US`, and names that region's code (Open
  questions, item 4).
- Matching is most specific wins with fallback to the parent, being region,
  then country, then the top node. `group` and `jurisdiction` fall back
  independently, so the two answers need not come from the same node.
- A mapping node may carry `permissions`, a map from a Data Use to an explicit
  acquisition (`granted`, `requires_signal`, or `denied`), overriding the group
  baseline for exactly that Data Use. This adopts the draft's requirement that
  overrides name explicit target states. The earlier `+`/`-` sigil scheme,
  which could not express `requires_signal`, is gone.
- The **signals** section is new relative to the draft. The opt-out source
  list and the opt-out revoke set are data in the file, and the
  `signals.tcf.authoritative` flag governs only whether a present TCF
  record's own grants and revokes apply, and not whether a record refusing
  storage withdraws (§4.2). The TCF purpose to Data Use mapping
  is not in the file. It lives in the TCF module crate
  (`crates/permission-signal/tcf/src/mapping.rs`), so a deployment that runs
  no TCF carries no table of another scheme's numbers. Nor is the order the
  signals are asked in, which is `[permission-signal] modules` in
  `trusted-server.toml` (§4). The `us_opt_out.revokes` value is `all` or an
  explicit list of Data Uses, so a deployer bounds what an opt-out drops.

The canonical shape of the tree, as written for policy owners in
`docs/guide/permission-model.md`:

```yaml
rules:
  group: gdpr-eu
  # Inherited by every country not overriding it, and the answer when no
  # country resolves.
  jurisdiction: gdpr
  GB: gdpr-uk # inherits gdpr
  US:
    group: us-notice
    jurisdiction: non-regulated
    CA:
      group: us-opt-out
      jurisdiction: us-state
    NY: us-notice # inherits non-regulated
```

A Manchester visitor gets `gdpr-uk` under GDPR handling, a Californian visitor
gets `us-opt-out` under that state's own opt-out handling, a Texas visitor gets
`us-notice` under `non-regulated` handling through the US fallback, and a
visitor with no resolvable country gets `gdpr-eu` under GDPR handling. The
nesting shows a reader the precedence rather than hiding it in slash-joined
keys, and the audience for the file is a policy owner rather than a developer.

The draft's required per-group **`regime`** class (`gdpr`, `us-privacy`,
`none`) is **not implemented**. Its intended consumer, server-side auction
dispatch, was not migrated to the permission model (§7.4), so the field would
be inert today. It returns with the dispatch migration.

### 3.3 Validation

Validation runs where the policy actually enters the system:

- **At parse**, meaning the unit tests and any `PermissionMaps::from_yaml`
  caller, the file is rejected for malformed YAML, a node that is neither a
  group name nor a block with a `group:` line, a rule referencing an
  undefined group, an unknown Data Use identifier anywhere (group flag,
  detailed-rule entry, or revoke list), an acquisition
  value outside `granted | requires_signal | denied`, a group without
  `default` that does not list every permission, duplicate rule keys under
  case-insensitive comparison (`us` and `US`), a `jurisdiction` value the
  `Jurisdiction` type cannot represent, `us-state` on the top node or a
  country node, a place written under a region, and a `revokes` keyword other
  than `all`. An unknown key is not rejected (§3.1).
- **At push and at load**, the same parse runs on the file a domain's settings
  name. `ts config push` always parses every file `--permissions` supplies and
  refuses settings whose `[permissions] policy` names no supplied file, and
  `ts config validate` and `ts config diff` do the same when `--permissions`
  is given, while `ts config diff` refuses to run without it when the settings
  name a policy (§3.1, item 2). `ts config migrate` and `ts audit generate`
  never run these checks. When a domain's settings load, core parses the file
  the adapter read for it, once its SHA-256 matches the blob. Either way the
  top node must carry both a `group` naming a defined group and a
  `jurisdiction`, and a file whose top node is missing either key is rejected
  exactly as a missing `[geo] default_country` was rejected before.
- **At startup**, the no-geo acknowledgment must be present where required
  (§5.3). A file refused at load and a missing acknowledgment fail when the
  settings are built, not while a request is handled, so where an adapter
  builds its settings for each request, every request fails the same way. A
  name in `[permission-signal] modules` that the adapter does not link, or a
  name given twice, fails when the adapter builds its application state (§4).
  Each of these failures is a configuration error, so after any of them every
  request for the domain answers 500, and the reason is logged at error level,
  so nothing is served from the origin. Row 4 moves Spin from a fixed 503 to
  the status of the error's kind, as the other adapters answer, and Cloudflare
  and Spin log nothing until row 54 (§3.1, item 7). Settings validation cannot
  check the module names, because only an adapter knows which crates it links,
  and the `ts` command line tool links none of them (§6.1).

Not implemented from the draft's list, and recorded as future hardening
(§11): checking rule-key country parts against the assigned ISO 3166-1 list,
checking region parts against assigned ISO 3166-2 subdivisions, and the group
identifier grammar. A mistyped country key (`DL` for `DK`) therefore still
parses. For the repository's sample the EU and EEA coverage test (§3.5)
closes the consequence the draft cared about, a member state silently
dropping to the fallback. A publisher's own file is checked by the parse
alone, and no test of its coverage runs.

### 3.4 One source of jurisdiction truth (adopted 2026-09-01)

The runtime lists `consent.gdpr.applies_in` and
`consent.us_states.privacy_states` retire into the `rules:` tree, so one
document now carries the permission baselines and the regime applicability
together. The repository's sample states the same 31 GDPR countries, being the
EU 27 plus Iceland, Liechtenstein, Norway and the United Kingdom, which inherit
`gdpr` from the top node, and the same 20 US states with a comprehensive
privacy law, written as region children of `US` carrying
`jurisdiction: us-state`. A visitor resolves to one node, and that node answers
both what is permitted and which regime applies, so the drift the draft named,
where a country added to one source had no effect on the other, has nothing
left to drift against and needs no consistency test between two lists.

What remains open is the consumer side. Auction dispatch still reads the
consent subsystem's gate rather than a policy regime class (§7.4), so unifying
the last consumer travels with the dispatch migration. Documentation that
pointed operators at the two `[consent]` lists points at the tree instead.

### 3.5 Sample coverage

Implemented as a unit test,
`every_eu_and_eea_member_requires_a_signal_for_storage` in `permissions.rs`,
which requires every one of the 27 EU member states plus the three EEA
members (IS, LI, NO), 30 codes in all, to have a rule, and each to resolve
`necessary.operations.storage` as `requires_signal`. The test reads the
repository's sample, so it guards that file and no publisher's own. A
mistyped member-state key fails this test rather than silently diverting the
country to the top node's group. The sample maps the EU 27 and EEA to
`gdpr-eu`, the UK to `gdpr-uk` (storage `granted`, a baseline that row 4 of the
rollout design's behavior-preservation matrix, §2, flags for the task force to
confirm with its citation, everything else opt-in), and the US and Australia to
`us-opt-out`. Countries with no node of their own fall to the tree's top
`group` (§5.4).

### 3.6 What the build carries

IABTechLab/trusted-server#1235 asks for every compile-time input of the
release binary to be listed. Each statement below names the row it describes,
being row 4 (IABTechLab/trusted-server#1345), or row 55
(IABTechLab/trusted-server#1423), which carries every row's changes, row 4's
included. At both rows a release binary of any adapter carries these, which
are the same for every publisher.

1. **The code.** The source of core, of the adapter and of every module crate
   the adapter links, at the dependency versions `Cargo.lock` pins, built by
   the toolchain `rust-toolchain.toml` names (1.98.1 at both rows) with the
   Cargo features the build turns on. The Fastly adapter's opt-in
   `reusable-sandbox` feature, which lets one Fastly instance (a sandbox)
   serve several requests, changes what the binary does at both rows, and so
   does core's `client-fixed-demo` at row 55. The crates an adapter links fix
   which modules a deployment can select, and the settings choose among them.
2. **The browser scripts.** The bundles that
   `crates/trusted-server-js/build.rs` builds from the TypeScript in
   `crates/trusted-server-js/lib/src`, and at row 55 in each module crate's
   `js/src` (`crate_script_folders`), with the bundler files the script
   watches, which its `main` names first. The script runs the `npm` it finds
   on the `PATH` (`build_bundles`), with whatever Node.js that `npm` runs.
   `.tool-versions` pins Node.js at 24.20.0 for a machine that reads that
   file, and the build does not check it. The script installs the packages
   `lib/package-lock.json` pins with `npm ci` only when `lib/node_modules` is
   missing (`install_dependencies_if_missing`), so a folder already there is
   used as it stands, unless `ensure_dependencies_fresh` finds its
   `node_modules/.package-lock.json` older than `package-lock.json` or
   missing, when the build stops. That check compares modification times, not
   contents. In place of a build it copies the bundles from the folder
   `TSJS_PREBUILT_DIR` names, which must hold the same set. The scripts that
   the crates a release links carry whole with `include_str!` are the Google
   ad tag bootstrap (`GPT_BOOTSTRAP_JS`), in core's `integrations/gpt.rs` at
   row 4 and in `crates/ad-tag/google` at row 55, and at row 55 also InMobi's
   `gpp_stub.js` and `tcf_stub.js` (`GPP_STUB_SCRIPT` and `TCF_STUB_SCRIPT` in
   `crates/cmp/inmobi/src/lib.rs`). At row 55 the seam probe carries its
   `js/probe.js` whole as well (`PROBE_JS` in
   `crates/testing/iabtl/src/lib.rs`), but each adapter lists that crate as a
   dev-dependency alone, so only test builds link it and the script never
   reaches a release.
3. **The Worker glue on Cloudflare.** Cloudflare's `build.sh` installs
   `worker-build` at the version of the `worker` crate that `Cargo.lock` pins
   and runs it (lines 21 to 39 at both rows). `worker-build` downloads the
   `wasm-bindgen` command line tool that matches the locked `wasm-bindgen`
   crate, which writes the JavaScript glue the Worker loads beside the WASM,
   so a Cloudflare release carries that glue as well. `worker-build --release`
   at the locked 0.8.5 also downloads binaryen version 130 and runs its
   `wasm-opt -O --all-features --debuginfo` over the WASM, which rewrites the
   WASM itself, and downloads esbuild 0.28.1 to bundle and minify the
   JavaScript shim. The sources are in workers-rs `v0.8.5`, being
   `worker-build/src/versions.rs` lines 11 and 12, `build/mod.rs` lines 309
   and 310, 455 to 470 and 558 and 559, `build/manifest.rs` lines 181 to 192
   and 299 to 305, and `main.rs` lines 123 and 124. The adapter's `Cargo.toml`
   turns none of this off. `worker-build` also reads `CUSTOM_SHIM`,
   `COREDUMP`, `RUN_TO_COMPLETION`, `COREDUMP_FLAGS` and `NO_MINIFY`
   (`main.rs` lines 88, 100, 176, 237 and 363), and a `<TOOL>_BIN` path in
   place of each tool it would download (`binary.rs` lines 22 to 24), and the
   `.env` files that `build.sh` exports could set any of them.
4. **The config store's default name.** Core's `build.rs` reads the default id
   of `[stores.config]` in `edgezero.toml` and passes it on (its `main`), and
   `config_payload.rs` compiles it in as `DEFAULT_CONFIG_STORE_ID`. An
   operator maps a store of their own to that name, so the repository's
   `edgezero.toml` is the same for every publisher.
5. **The git version.** `TRUSTED_SERVER__GIT_VERSION`, which the deploy
   pipeline sets or core's `build.rs` reads from local git
   (`emit_git_version`), compiled in as `TS_GIT_VERSION` in `constants.rs`.
6. **The template build digest.** A SHA-256 that core's `build.rs` takes over
   core's sources, its build script with the `build_support/git_version.rs` it
   includes, its `Cargo.toml`, and the workspace's `Cargo.toml`, `Cargo.lock`
   and `edgezero.toml` (`CRATE_INPUTS`, `WORKSPACE_INPUTS` and
   `template_build_digest`),
   compiled in as `TEMPLATE_BUILD_DIGEST` in `publisher.rs`. It is part of the
   fingerprint a stored page template is kept under, so a template stored by
   other code is never served.

No configuration, no policy and no key is among them at either row.

**No build-time read takes configuration or a policy.** At both rows no build
script reads configuration or a policy into the build. The sample reaches only
test builds and core's `test-utils` feature (`SAMPLE_POLICY_TEXT` in
`permissions.rs`), and the permissions inspector, a tool of its own, which
embeds it on purpose.

- Core's build script reads by name only `TRUSTED_SERVER__GIT_VERSION`
  (`emit_git_version`), at both rows, besides the directories Cargo sets, the
  files it hashes, `edgezero.toml` and local git.
- The JavaScript crate's build script reads by name only `TSJS_PREBUILT_DIR`,
  `TSJS_SKIP_BUILD` and `TSJS_TEST`, at both rows, besides the directories
  Cargo sets and the `PATH` it finds `npm` on.
- At row 55 the Fastly adapter has a build script as well, which writes the
  list of core's Rust sources that a test of the adapter reads to check that
  core never uses the Fastly SDK. Only a test build compiles that list in
  (`core_sdk_guard`, which the adapter's `lib.rs` declares for tests alone),
  and the script reads no variable by name beyond the directories Cargo sets.
- Vite runs with no configuration file and no `define` (`moduleBuildOptions`
  in `lib/build-module-options.mjs`, at both rows), and no browser source
  reads `import.meta.env` or `process.env`, the module crates' `js/src`
  included at row 55, so no environment file reaches a bundle.
- EdgeZero's runtime crates read nothing while they compile, and no adapter
  uses EdgeZero's `app!` macro, the one place where EdgeZero reads a file at
  compile time (`edgezero-macros/src/app.rs`).
- Cloudflare's `build.sh` exports the root `.env` and the adapter's
  `.env.cloudflare.dev`, neither of which is committed, into the build's
  environment (lines 8 to 15 at both rows). The committed `.env.dev` and
  `.env.example` hold `TRUSTED_SERVER__` settings, so such a file can carry
  settings, but no build-time read takes one of them.

**A check that no release WASM carries a policy.**
`scripts/check-no-compiled-policy.sh` reads each release WASM it is given and
fails when one carries text from a policy file in `config/permissions`, being
the file's first comment line or its `name:` value, which no code carries. It
skips either when it is shorter than 16 characters (`MIN_MARKER_LENGTH`),
because text that short, such as a bare `#`, could be in any WASM, and it
fails too when an artifact is missing or cannot be read, or when a policy file
has nothing long enough to look for. It reads the artifact, so it sees what
the linker kept rather than what the source says. At both rows CI runs it on
the Fastly and Spin release WASMs the test workflow builds, and on the
Cloudflare release WASM the integration workflow builds with `worker-build`,
in the steps named "Check that no permission policy is compiled into" each
WASM (`.github/workflows/test.yml` and
`.github/workflows/integration-tests.yml`). One more step of the test
workflow, "Check that the policy guard refuses a file that carries a policy",
runs the script on `config/permissions/sample.yaml` itself and fails the job
unless the script refuses that file with exit 1, so a guard that stops finding
the markers fails CI rather than passing every WASM.

### 3.7 Cost of the fetch

The policy is read when an instance loads a domain's settings, and what an
instance is differs by adapter.

| Adapter    | Where the policy is read from                                                                                                                                                                                                                                 | What an instance is                                                                                                                                                                       |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | Inside the blob at row 4, read with it, where the sample adds three to five reads (§3.1, item 3). From row 11, for a host's blob, the entry `permissions_<sha256>`, being one read for the pointer and one for each chunk, so five for the 24,271-byte sample | One request, unless the adapter is built with the opt-in `reusable-sandbox` feature, when a sandbox serves several requests and, from row 11, keeps the applications of up to eight hosts |
| Cloudflare | Inside the blob, which the Worker reads from the Workers KV namespace with one awaited read from row 4                                                                                                                                                        | A Worker instance, which builds its state for each request and serves requests until Cloudflare starts another                                                                            |
| Spin       | Inside the blob, in the `default` key-value store the settings come from, read with it                                                                                                                                                                        | A component instance, which builds its state for each request and serves one request or several as Spin decides                                                                           |
| Axum       | `<folder>/<name>.yaml`                                                                                                                                                                                                                                        | The process, which loads its settings once when it starts                                                                                                                                 |

An instance reads, hashes and parses a policy file the first time it meets
the file's digest. An instance that already holds the digest a blob names
reads nothing, hashes nothing and parses nothing, because the runtime loader
asks for the rules held for that digest before it asks its source for the
text (`load_domain_policy` in `config_payload.rs`, through `held_policy` in
`permissions.rs`). Every file that is read is still checked against the
digest, and a file that is refused leaves nothing held, so the next load
reads and checks that file again. Text that travels inside the blob is
always checked, because it is already in hand, and a blob that should carry
its policy's text and carries none is refused whatever the instance holds. So from row 11 a host's blob
costs an instance that already holds its digest no read of its policy entry,
and the reads of that entry, through `FastlyPolicyEntries::policy_text` (the
Fastly adapter's `platform.rs`), being the pointer and its chunks, five for
the sample, are paid once for each digest an instance meets.
The parsed copies stay until the instance ends, with no lifetime of their
own, because a digest never changes what it names. An instance keeps one
copy for each digest its domains' blobs pin, so the hundred hosts that name
three files in §3.1 cost an instance at most three copies while no file
changes, and each changed file adds a copy for the digest it brings.

At row 4 a settings load parses a policy file only the first time the
instance meets its digest (`parsed_policy` in `permissions.rs`), so on Fastly,
where an instance serves one request unless the opt-in feature is built, a
request parses its domain's file once, where before row 4 it parsed the
compiled sample twice, once to check it and once on first use. What is new on
Fastly is the digest, taken on every load of a blob that carries its policy,
and the three to five reads that such a blob adds, and from row 11, for a
host's blob, five reads of its policy entry the first time an instance meets
its digest.

On Cloudflare, at row 4 (IABTechLab/trusted-server#1345), the Worker's fetch
entry point `main` (the Cloudflare adapter's `lib.rs`) calls `build_app` for
every request, and `load_startup_settings` (the adapter's `app.rs`) opens the
config store's KV namespace and awaits one read of the blob, which the
runtime loader loads with the policy inside it. So every request reads the
blob and hashes the policy text (`load_policy`), and the parsed policy stays
in the Worker instance by digest, in the `PARSED` map of `parsed_policy`, for
as long as Cloudflare keeps that instance, so a request that meets a held
digest parses nothing. Before row 4, `main` built the application for every
request as well, through EdgeZero's `run_app`, from the settings in the
`TRUSTED_SERVER_CONFIG` variable, so what row 4 adds to each request is the
one KV read and the hash. The adapter's guide, `docs/guide/cloudflare.md`,
states the per-request read in its opening paragraph.

`template_fingerprint` serializes and hashes the typed settings on every
request that can use a shared page template, and leaves `permissions.text` out
of what it hashes, because `sha256` already names the file, so no request
hashes the file (`template_fingerprint` and its call in `publisher.rs`, lines
2116 to 2145 and 4881 at row 4).

The numbers are not measured yet. A measurement on Fastly staging will record,
for the sample, the time and the compute an instance spends from reading a
domain's blob to holding its policy, beside the same for a scratch build that
compiles the sample in. It will decide whether Fastly also keeps a serialized
copy of the parsed policy in its value cache (`PlatformValueCache`, from row
24, which only the Fastly adapter registers), shared by every instance at a
location, so that a new instance decodes one cached value in place of reading
and parsing the file. The value cache holds bytes, so each instance would
still decode its own copy. The numbers will be filled in here. Viceroy,
Fastly's local test server, serves its config store from `fastly.toml` on the
local machine, so a local run cannot show the cost of a read from Fastly's
store. The Fastly integration tests' Viceroy configuration stores the blob as
chunks of at most 7,000 bytes and a pointer, as
`ts config push --adapter fastly` does, because Viceroy, like Fastly, accepts
at most 8,000 characters in one config store value (`config_store_entries` in
`crates/trusted-server-integration-tests/src/bin/generate-viceroy-config.rs`,
lines 14 to 20 and 205 to 248). Cloudflare, Spin and Axum are not measured,
because the measurement decides only Fastly's value cache. From row 4 every
Cloudflare request waits for one Workers KV read, where main read a variable.

## 4. Signal precedence (normative, implemented)

Precedence is the **order a deployment configures**, not a rule in code. The
policy file decides what the shipped schemes mean for the deployment (§3.2),
each permission signal module decides what its own scheme says, and
`[permission-signal] modules` in `trusted-server.toml` decides the order the
modules are asked in. Each permission resolves in these steps.

1. **Policy `denied`** is never set, regardless of any signal. (Enforced in
   the resolver, `PermissionMaps::resolve_with`.)
2. **The configured modules are asked in order** (`combine` in
   `crates/trusted-server-core/src/permission_signal/mod.rs`). Each sees the
   baseline and what the modules before it settled on, and answers `Grant`,
   `Revoke` or `Neutral` for the permission. A `Neutral` answer leaves the
   prior value standing and any other answer replaces it, so **the last
   module with an opinion decides**. Every module is asked, because a
   later one may amend what an earlier one settled.
3. **No module with an opinion leaves the baseline standing**, so `granted`
   sets the permission and `requires_signal` leaves it unset. A `Revoke` drops
   a `granted` baseline, and only a `Grant` sets a `requires_signal` one.

**Core answers nothing ahead of the modules.** No step revokes every
permission because some consent record could not be decoded, and no part of
the order is fixed where a deployment cannot change it (`assemble_permissions`
in `crates/trusted-server-core/src/ec/consent.rs`, and the test
`core_answers_nothing_ahead_of_the_modules` beside it). A string that arrived
and could not be decoded is answered by the module for its own scheme. Each
shipped module reads such a string as the refusal or opt-out the string may
have carried, and only for the Data Uses its scheme covers.

- `tcf` answers `Revoke` for the 19 Data Uses a TCF purpose maps to, being the
  eleven named ones and eight further ones (§2), and only while
  `signals.tcf.authoritative` is true.
- `gpp` and `us-privacy` answer `Revoke` for each Data Use in the policy's
  opt-out revoke set (seven in the sample policy), and only while the policy
  lists their source in `signals.us_opt_out.sources`.
- `gpc` and `mtm` decode nothing. A `__mtm_pref` value that is not one of the
  three words `mtm` knows is read as absent.

A later module overrides an earlier one, as it does for any answer. Under the
default order a readable TC string that consents to storage therefore sets
storage over an unreadable GPP string, and a deployment that leaves `tcf` out
of the list removes the effect of an unreadable TC string. The Axum tests
`an_unreadable_tcf_record_is_the_tcf_modules_refusal` and
`an_unreadable_gpp_string_is_the_gpp_modules_opt_out_and_the_order_decides`
(`crates/trusted-server-adapter-axum/tests/permission_signals.rs`) run the real
modules for both. An unreadable string never withdraws (§4.2), and no module
vouches for it, so it reaches neither the page nor a bid request (§4.4). An
**expired** TCF record is deliberately a distinct state, not an unreadable one.
The pipeline clears the decoded record and sets the `expired` flag, the `tcf`
module answers `Neutral`, and acquisition proceeds as if the record were
absent, so the baseline applies.

The order is set by `[permission-signal] modules`, which takes a list rather
than a single name because several permission signal modules run, and the
list is the order they run in:

```toml
[permission-signal]
modules = ["gpc", "gpp", "us-privacy", "tcf", "mtm"]
```

- **Omitted**, every module the adapter links runs, in the order the adapter
  offers them, so a signal is never ignored because nobody listed it.
- **An empty list** runs no module, and every permission stays at its
  country and region baseline.
- **A name** is the module's crate folder below `crates/permission-signal`,
  such as `gpp`, and may also be written in full, as `permission-signal.gpp`.
- **A name the adapter does not link, or a name given twice**, fails when the
  adapter builds its application state (§3.3). The message for an unknown name
  lists the modules that are available.
- The modules acted on are written to the log once at startup, and a warning
  lists any module the list leaves out. Open questions, item 1, concerns how
  that warning compares names.

The section takes `modules` and nothing else. A block named for a module,
`[permission-signal.<name>]`, is refused today as an unknown field, because
none of the five modules has a setting to carry. A module that gained a
setting would take such a block, under the rule in the providers spec §2.1. The
earlier table name `[permission_signal]` and the earlier keys `sources` and
`module` are refused with a message that names `[permission-signal] modules`
(§6.1).

Every adapter offers the five shipped modules in the same default order,
being `gpc`, `gpp`, `us-privacy`, `tcf` and `mtm` (§4.7). Global Privacy
Control is a browser setting with no interface of its own, so it is asked
first, and the schemes that carry an answer a person gave through an interface
are asked after it. Under that default a TCF record consenting to a mapped
purpose sets a permission that a GPC header, a GPP sale opt-out or a US Privacy
opt-out revoked, and a deployment that wants the opt-out to stand lists it
after `tcf`. A PMP answer read by `mtm` is asked after `tcf`, so it answers
over `tcf` and over every opt-out for the eleven named Data Uses (§4.6). Which
scheme wins is a question about a jurisdiction and a publisher, so the
deployment decides it. The `signals.tcf.authoritative` flag governs only the
answers the `tcf` module gives about permissions. With the flag false the
module answers `Neutral` for every permission and declares nothing grantable,
but a record refusing storage still withdraws (§4.2).

A module is also given the whole ordered list and its own place in it, and
may ask a named peer what that peer makes of a permission. That makes a rule
such as "this answer supersedes a refusal only when Global Privacy Control
made it" expressible inside the module that wants it. A consultation goes
one level deep, so two modules asking each other cannot loop.

Removing `gpc` from the list stops the GPC module running, but the consent
pipeline can still synthesize a US Privacy opt-out from the same header for a
US privacy state (§4.4), which the `us-privacy` module then acts on. A
deployment that wants the header to have no effect also turns that synthesis
off with `[consent.us_privacy_defaults] gpc_implies_optout = false`.

Two simplifications against the draft's taxonomy, both recorded in §11:

- **Of the shipped modules, TCF and `mtm` are the grant-class signals.** The
  draft's grant class also admitted explicit GPP/USP non-opt-out values,
  regime-scoped, so a US rule could be `requires_signal` yet grant on
  signal-carrying traffic. The implementation instead expresses the US posture
  as a `granted` baseline that opt-outs revoke, so no-signal US traffic is
  allowed rather than blocked pending a signal. Explicit non-opt-out GPP/USP
  values grant nothing on their own.
- **An unreadable record is answered per scheme, not per section.** The draft
  blocked grants per record family and per mapped section of a GPP string. The
  implementation scopes the answer to the Data Uses of the scheme whose record
  could not be read, and does not look inside the sections of a GPP string
  (§4.5), so an unreadable record from one scheme leaves the other schemes'
  answers alone.

### 4.1 Decision matrix

For each permission, with baseline _B_ from the resolved rule, under the
default order and a request with no `__mtm_pref` cookie, so that `mtm` is
silent:

| Signal state                                                                         | B = granted | B = requires_signal | B = denied |
| ------------------------------------------------------------------------------------ | ----------- | ------------------- | ---------- |
| TC string present but undecodable, Data Use mapped by a TCF purpose                  | unset       | unset               | unset      |
| TCF present, consents to the mapped purpose, with or without an opt-out              | set         | set                 | unset      |
| TCF present, refuses the mapped purpose                                              | unset       | unset               | unset      |
| Opt-out present or its string undecodable, Data Use in the revoke set, no TCF answer | unset       | unset               | unset      |
| No signal (or neutral for this Data Use)                                             | set         | unset               | unset      |

With an opt-out listed after `tcf`, a request carrying both resolves unset
under every baseline for the Data Uses that opt-out revokes. An expired TCF
record resolves as the "no signal" row. A `__mtm_pref` cookie of `standard` or
`personalized` adds a last answer for the eleven named Data Uses under the
default order, which replaces the result in every row above for those Data
Uses (§4.6). A permission whose baseline is `requires_signal` and that no
module has answered is also listed as `awaiting` on the page when some
configured module could still grant it, and a permission that a signal refused
is never listed as awaiting (§13). Whether an unset outcome is also a
**withdrawal** is a separate, narrower question (§4.2).

### 4.2 Withdrawal vs. absence

Withdrawal (destructive, expiring the `ts-ec` cookie and writing the
identity-graph tombstone) and non-grant (the permission is simply unset, EC response
headers stripped, nothing egressed) are distinct outcomes, never conflated.
"Baseline" below means the resolved acquisition rule for
`necessary.operations.storage` in the request's jurisdiction, resolved once
at `EcContext` construction (`storage_acquisition`), never a group label.

The implemented trigger, exhaustively (nothing else withdraws):

1. **A TCF record refusing storage (Purpose 1) withdraws only when the
   baseline is not `granted`, and only when the refusal is carried by the live
   request.** Under a `requires_signal` (or `denied`) baseline the refusal
   is the visitor declining the very signal storage depends on, so it
   withdraws. Where the baseline is `granted`, storage never depended on
   the record, so the refusal suppresses use without destroying the
   identifier. Tombstones are irreversible, and PR #838 wrote them for
   visitors in unregulated jurisdictions whose global CMP emitted a
   purpose-refusing string. The consent pipeline reads only the live request
   (its cookies, its headers and the resolved location), so the withdrawal
   decision sees live request signals only, satisfying the draft's
   live-request constraint by construction.
2. **US-style opt-outs never withdraw.** GPC and sale opt-outs are use
   restrictions, which suppress the permissions the policy revokes (EC
   headers stripped, nothing egressed) but never trigger destruction, so
   lifting the opt-out restores the identity.
3. **An unreadable record never withdraws.** The `tcf` module reads the record
   the consent pipeline decoded, so a TC string that could not be decoded
   withdraws nothing, whatever the module answers for the permissions it maps
   (§4). Destruction requires an affirmative, decodable signal.
4. **Absence of signal never destroys identity.** A visitor who has not yet
   made a choice is never stripped of an existing identity.
5. **A policy change is not a user signal.** A domain's policy changes only
   when its settings are pushed again with another file or an edited one
   (§3.1), and a push that tightens a baseline does not itself tombstone,
   because withdrawal still requires the affirmative refusal above on a live
   request.

The draft's additional trigger, an explicit storage-withdrawal or
authenticated deletion request honored in every jurisdiction, has no
implemented carrier, as no such endpoint exists. It is recorded as deferred
(§11), and when it arrives it joins this list as a global trigger.

A module answers withdrawal separately from its signal, through
`withdraws` on `PermissionSignalModule`, and core scopes any answer to the
baseline (`withdrawn` in `permission_signal/mod.rs`), carrying the result as
`PermissionState::storage_withdrawn`. Of the shipped modules only TCF answers
it, and its answer does not read `signals.tcf.authoritative`, so with the flag
false a record refusing storage still withdraws while the module's answers
about permissions are silenced (the test
`a_non_authoritative_policy_silences_the_record_but_not_the_withdrawal` in
`crates/permission-signal/tcf/src/lib.rs`). The response path acts on the
answer only where `ec_allowed` is false (`ec_finalize_response` in
`ec/finalize.rs`), so a request on which a later module such as `mtm` sets
storage after a TCF refusal withdraws nothing. Arms 1 to 4 above have direct
coverage in `crates/trusted-server-adapter-axum/tests/permission_signals.rs`,
with the real modules assembled. Refusal under `requires_signal` withdraws,
refusal under `granted` does not, consent does not, GPC alone does not, sale
opt-outs do not, and no signal does not. An unreadable record does not
(`ec/consent.rs` and the same Axum file), and a deployment whose list leaves
out `tcf` cannot withdraw at all. No test covers a withdrawal beside an `mtm`
grant of storage.

### 4.3 Withdrawal durability (largely deferred)

Implemented behavior (`ec/finalize.rs`). When the request carries the
withdrawal signal and the client presented a cookie, the response expires
the EC cookie, and the identity-graph tombstone is written for each
presented identifier the module accepts (the incoming cookie value and
the active value). The tombstone is the authoritative revocation marker for
subsequent EC behavior. A tombstone write failure is logged at warn level,
saying the row may still be live with consent granted, and the request
completes, so the write is best effort.

The draft's durability protocol is **not implemented** and is recorded as
deferred follow-up in full: the family ID with deterministic derivation for
legacy rows, the family revocation record written before the cookie
expires, the permission-exempt suppression and authority-state records with
CAS fencing, evidence-recency comparison and anti-replay pinning, the
durable negative-intent outbox, the global identity safety breaker, and the
associated consistency and retention contracts. That machinery depends on
storage primitives (linearizable per-key CAS, independent durability
domains) the current adapters do not qualify. The 2026-07-31 draft remains
the reference design for that work. Until it lands, the known gaps the
draft called out stand. Cookie expiry is not fenced on the tombstone
commit, and revocation durability is bounded by the KV store's behavior.

### 4.4 Signal normalization

The consent subsystem (`consent/mod.rs`) remains the decoder and
normalizer. A module reads its output as the decoded consent record on
`SignalInput`, and a module for a scheme the pipeline does not decode (`mtm`)
reads the request evidence instead. The pipeline reads the live request only,
being its cookies, its headers and the resolved location. The implemented
pipeline:

1. Extract raw signals from cookies and headers, and decode TCF v2, GPP,
   and US Privacy. A decode failure keeps the raw string and leaves the
   decoded field empty, which the module for that scheme reads as an
   unreadable record (§4).
2. Resolve standalone-TCF vs GPP-embedded-TCF conflicts per the configured
   mode (`restrictive`, `permissive`, `newest`), preserving the pre-epic
   selection algorithm.
3. Apply the expiry check, where a TCF record older than the configured maximum
   age has its decoded form cleared, the `expired` flag set, and its raw
   string left in place. Expiry is its own state, excluded from the
   unreadable case, and resolves as absent for acquisition.
4. Construct a US Privacy string from GPC for US privacy states with no
   explicit USP cookie, so the opt-out also travels in transport fields. The
   setting `[consent.us_privacy_defaults] gpc_implies_optout`, true by default,
   controls it.

**Raw strings after assembly.** Once the permission state is assembled,
`EcContext` keeps only the raw consent strings that a module vouched for
(`ConsentContext::keep_only`, called when `EcContext` is built in
`ec/mod.rs`). A module vouches for a string by reporting it through
`valid_signal` as one it read and used, and the `tcf`, `gpp` and `us-privacy`
modules do so only for a string the pipeline decoded. An unreadable string, an
expired one and one whose module is not listed are therefore removed, and the
auction paths, the publisher paths and the identity-graph row (§1) see only the
raw strings that were read and used. The same list reaches the page as
`signals` (§13).

The draft's declared reordering, expiry filtering **before** conflict
resolution, was **not implemented**. Conflict resolution still runs first,
so the pre-epic order stands. Recorded in §11.

**Persisted-KV consent (not built).** The draft's fallback to the consent
persisted for an EC ID does not exist on this row. `ConsentPipelineInput`
takes no identity-graph input, `ConsentSource` has no persisted variant, and
`consent_store` is refused as an unknown key in `[consent]`. The plan that
removed the path is
`docs/superpowers/plans/2026-07-13-issue-883-remove-legacy-consent-store.md`
(IABTechLab/trusted-server#883). A request that carries no consent signals
therefore resolves from the place baseline and whatever the modules read from
the request. The identity graph keeps only the `consent.ok` flag and the raw
strings of the row (§1).

**Proxy mode.** Proxy mode still skips semantic decoding entirely. The
draft's minimal opt-out extraction was not implemented, and the fail-open
consequence the draft feared does not arise. A TC, GPP or US Privacy string in
proxy mode is present but undecoded, so the module for its scheme reads it as
unreadable (§4) and revokes the Data Uses that scheme covers. The GPC header
needs no decoding, so the GPC opt-out is honored directly. No grant is derived
from a TC, GPP or US Privacy string in proxy mode, and `mtm` still answers from
its cookie. Absent records resolve to the baseline. No module vouches for an
undecoded string, so `keep_only` removes the raw strings, and by reading the
code the OpenRTB consent fields on the auction and publisher paths carry none
in proxy mode. No test covers proxy mode through `EcContext`, and Open
questions, item 2, asks whether that is intended.

### 4.5 US signal field mapping

Implemented sources, as the repository's sample declares them in its
`signals` section:

| Source                                             | Effect                                   |
| -------------------------------------------------- | ---------------------------------------- |
| `Sec-GPC` request header                           | US-style opt-out                         |
| GPP US section sale opt-out                        | US-style opt-out                         |
| US Privacy `opt_out_sale = Y`                      | US-style opt-out                         |
| GPP or US Privacy string that could not be decoded | US-style opt-out for its own source (§4) |
| US Privacy `opt_out_sale = N`                      | Nothing (no grant class exists for USP)  |
| Any explicit Not Applicable value                  | Nothing                                  |
| Absent / unknown / reserved values                 | Nothing                                  |

An opt-out revokes the Data Uses the policy's `revokes` value names. The
sample, `config/permissions/sample.yaml`, lists device storage, sale,
sharing, third-party sale, first- and third-party targeted advertising, and
profiling, so an opted-out visitor gets no Edge Cookie written and no
identifier shared while contextual advertising and measurement continue. That
is broader than the draft's mapping, which scoped sale opt-outs to
personalized-ad selection only, and a deployer changes it by editing the list
or writing `revokes: all`. Each source is read by its own module (`gpc`,
`gpp`, `us-privacy`), which runs only when
`[permission-signal] modules` names it or the key is omitted, and whether an
opt-out stands over a TCF answer is the order (§4). No sale-family opt-out is
destructive (§4.2).

The remainder of the draft's §4.5 is **not implemented** and is recorded
as deferred: `SharingOptOut` and `TargetedAdvertisingOptOut` as distinct
inputs, grant-class non-opt-out values, embedded-GPC detection inside GPP
sections, the per-section applicability and aggregation algorithm with its
state-over-national precedence, the mapped-section malformed blocker at
per-section granularity, the derived OpenRTB `gpp_sid` construction with
the `__gpp_sid` consistency companion, and the complete pinned section
map. The current GPP decoder surfaces the EU TCF section, the section ID
list, and a US sale opt-out. Extending it to the full pinned registry is
its own project.

#### 4.5.1 GPP registry snapshot (deferred)

Not implemented. The vendored registry snapshot, the pinned per-section
accepted versions, the provenance manifest, and the fixture corpus travel
with the §4.5 decoder work. The 2026-07-31 draft's §4.5.1, including the
pinned upstream commit, remains the reference for that effort.

### 4.6 The Model Terms for Marketing module

The `mtm` crate (`crates/permission-signal/mtm`) reads the answer of the
Preference Management Platform (PMP). The PMP asks a visitor one question and
keeps one of three words, and its page script mirrors the word into the first
party cookie `__mtm_pref` (`COOKIE_MTM_PREF` in core) so that the server can
see it. A value that is not one of the three words is read as absent. The
module does not read the policy, so the `signals` section of `permissions.yaml`
cannot silence it, and it reads the cookie from the request evidence, so proxy
mode does not affect it.

| Word            | Answers `Grant` for                                | Answers `Revoke` for    |
| --------------- | -------------------------------------------------- | ----------------------- |
| `standard`      | Data Uses 1, 2, 7, 8, 9, 10 and 11 of the §2 table | Data Uses 3, 4, 5 and 6 |
| `personalized`  | All eleven                                         | None                    |
| `non-marketing` | None                                               | None                    |

The word `non-marketing` is a recorded answer that changes no permission, so
the baseline stands. Under the sample's `gdpr-eu` and `gdpr-uk` groups the
baseline grants no marketing Data Use, and under `us-opt-out` the baseline
grants them. The module also:

- declares the eleven Data Uses grantable (`grants`), so a page waits for them
  while the PMP question is open (§13),
- reports the word as a valid signal of scheme `mtm`,
- declares the versioned Model Terms for Marketing,
  `https://m4ow.uk/mtm/2.txt`, as the terms the data is available under
  whenever a word is present, `non-marketing` included, and
- never withdraws.

The cookie is written by script and is not `HttpOnly`, by design, because the
script that owns the question has to read and replace the cookie (the test
`the_preference_cookie_is_never_httponly` in the crate checks the PMP's own
`Set-Cookie` strings for `HttpOnly`). The server therefore cannot tell the
PMP's write from another script's. Under the default order `mtm` is asked
last, so a request carrying `standard` or `personalized` has `mtm` answer after
`tcf` and after every opt-out, and replace their results for the eleven named
Data Uses. A deployment that wants a TCF record or an opt-out to stand over a
PMP answer lists `mtm` before `tcf` and the opt-outs. The Axum test for `mtm`
(`a_pmp_answer_in_the_eu_settles_what_the_model_terms_cover_and_declares_them`)
gives `mtm` no TCF record or opt-out to answer over, so no test covers that
order (Open questions, item 5).

### 4.7 What each adapter runs

| Adapter    | Signal modules offered, in the default order | Geo when `[geo] module = "platform"`                                                                                                                                      |
| ---------- | -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | `gpc`, `gpp`, `us-privacy`, `tcf`, `mtm`     | `crates/geo/fastly` wraps the SDK's `geo_lookup`, which returns an `Option` and turns every failure into `None`                                                           |
| Cloudflare | The same five                                | Reads `cf-ipcountry` on every plan and `cf-region-code` only with an Enterprise plan and the visitor-location managed transform, so without them no region rule can match |
| Axum       | The same five                                | Resolves nothing                                                                                                                                                          |
| Spin       | The same five                                | Resolves nothing                                                                                                                                                          |

With `[geo] module` unset or `none`, every adapter uses `DisabledGeo`, which
resolves nothing (§5.2). Each adapter holds its own copy of the module list in
the `shipped_signal_modules()` function of its `app.rs`. No test compares the
copies with one another or with the order the Axum tests assume, and only the
Axum adapter's tests (`tests/permission_signals.rs`) assemble the real modules.
The permissions inspector (`tools/permissions-inspector`) holds a list of its
own that leaves out `mtm`.

## 5. Jurisdiction resolution

### 5.1 Order

Geo resolution runs **before** permission resolution. Jurisdiction is an
input to the permission set, which is why geo modules cannot themselves
be gated on it (providers spec §5). The selected geo module resolves a
country and optional region, and the tree is walked most specific first, being
the region node, then the country node, then the top node, matched
case-insensitively. Implemented in
`EcContext::read_from_request_resolving_geo` and
`PermissionMaps::rules_for`.

### 5.2 Lookup failure

Implemented, with the failure state carried explicitly. The type
`GeoStatus { Located, NoLocation, Failed }` (in `ec/consent.rs`) separates
"the module resolved no location" from "the lookup errored". A **failed**
lookup resolves every permission to the **requires-signal floor** and its
jurisdiction to `unknown`, never the tree's top node. The failure is logged at error
level so it is visible. The storage-withdrawal baseline follows the same
floor, so a failed lookup cannot widen destructive withdrawal either. The
rule is proven through the seam by
`a_geo_module_failure_resolves_permissions_at_the_requires_signal_floor`
in `ec/mod.rs`, which drives a `PlatformGeo` that returns an error rather
than constructing the status by hand.

**Which modules can reach it.** The floor is the contract for a geo
module that performs its own fallible lookup, and none of the modules
shipped in this workspace is one. Fastly's `geo_lookup` returns an
`Option` and the SDK collapses every hostcall, buffer and parse failure
into `None` before it reaches the caller. The Cloudflare module reads
request headers, which cannot error. The Axum and Spin modules resolve
nothing at all. `DisabledGeo`, the default whenever
`[geo] module` is not `"platform"`, returns nothing by construction. So
**a host geo outage today does not reach this floor.** It surfaces as
`Ok(None)`, which is `NoLocation`, and falls back to the tree's top `group`
like any other unmatched request. A deployer
choosing a permissive top `group` is therefore choosing what a geo
outage does, and should read §5.3 and the top-node guidance with
that in mind. The floor becomes reachable when a vendor geo crate under
`crates/geo/` does a real lookup that can fail.

At the floor, an explicit valid grant still counts. A TCF record consenting
to a mapped purpose sets that permission under `requires_signal`, exactly
the divergence-from-deny-all the draft declared for this row. Absent,
unreadable, or refusing evidence sets nothing.

Not implemented: a lookup-failure metric (the error log is the signal
today) and the draft's `regime = "gdpr"` component of the failure profile,
since no regime concept exists. The draft's capability check, that an
adapter whose geo implementation can never resolve anything must not accept
the selection, is part of the providers-spec module qualification rather
than this model.

### 5.3 No geo module selected

Every request resolves at the top of the rules tree, so jurisdiction becomes
a static constant, which is only honest when the operator can genuinely
assert single-jurisdiction traffic.

Constraint, implemented in
`GeoConfig::validate_jurisdiction_acknowledgment`. **Startup fails** when
an Edge Cookie module is configured and no geo module is selected,
unless the operator sets `[geo] assume_single_jurisdiction = true`. This
makes the dangerous migration config (a permissive top `group` with
geo unset) an explicit operator decision rather than an accident, closing
the highest-severity finding of the PR #838 review.

The guard's consumer list is narrower than the draft's. The draft enumerated
every jurisdiction consumer (EC module, regime-gated auction dispatch,
raw-EC and EID egress). In the implementation the EC module is the only
consumer whose behavior the policy gates, because auction dispatch was not
migrated (§7.4), so the guard fires on the EC module selection alone. When
dispatch joins the model, the guard's trigger list grows with it.

### 5.4 Defaults: the tree's top node plus a protective floor

The top node of the `rules:` tree is **required in every mode** and is
validated at startup, meaning it must carry a `group` naming a defined group
and a `jurisdiction`. Every node below may carry its own `jurisdiction`, and a
node that carries none inherits the nearest ancestor that does (§3.2). A no-geo
single-state deployment states that state's group at the top and nests nothing
beneath it, though `us-state` is not valid there, because the top node names no
state. The top node covers two states
the draft kept separate:

- the geo module resolved no location (or none is configured), and
- the resolved country or region has no node of its own.

`[geo] default_country` is retired. Treating an unknown visitor as being in a
chosen country was a fiction, and the tree states the unknown case explicitly
at its top instead, with the group and the consent handling side by side. The
draft's `rules.default` policy entry does not exist either, so
"resolved-but-unmatched" falls through the parent chain to the same top node
as "unresolved". The separation the draft treated as safety-critical is the
one the implementation does keep, where a **failed** lookup never reaches the
top node and resolves at the requires-signal floor instead (§5.2).
With no top `group` or no `jurisdiction` startup fails, and if resolution
still finds no node, which should never happen, the floor applies.

### 5.5 Policy revision activation (deferred)

Not implemented. A domain's policy changes when its blob is pushed with
another file or an edited one (§3.1), and the pushed blob, which carries the
file's name and SHA-256, is the policy identity. §3.1 also says what a
location answers while a blob, or from row 11 a policy entry, has not yet
reached it. Nothing coordinates the moment every instance has moved to a new
blob. While a push propagates, and for as long as an instance keeps a domain's
application, such as a Fastly sandbox built with `reusable-sandbox`, one
instance can run the previous policy while another runs the new one. The
draft's activation design, covering the JCS-canonical policy digest and
ordinal pair, the activation register and candidate protocol, fleet readiness
and quiescence, admission leases, the hash-linked activation journal with
store-clock retention, and the model-epoch transition, is the reference design
for ending that overlap, recorded in §11. Its guarantee that mixed-revision
irreversible behavior is prohibited is therefore not honored by construction.
Each instance runs one policy for a domain, the one its blob names, and
destructive withdrawal requires a live user signal (§4.2), never a policy
change. A push that changes a storage baseline can still let the same refusal
withdraw on an instance that runs one policy and not on an instance that runs
the other, until the overlap ends.

## 6. Failure-mode matrix (normative, implemented)

| Condition                                                    | Resolution behavior                                                                                                                                                                                                                                          |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Geo lookup reports a failure                                 | Requires-signal floor for every permission and an `unknown` jurisdiction, never the top-node baseline, with the error logged. No module shipped today can report one, so a host geo outage resolves as no location, at the top-node baseline, instead (§5.2) |
| No geo module configured                                     | Top-node baseline, guarded by `assume_single_jurisdiction` (§5.3)                                                                                                                                                                                            |
| Country resolved, no matching node                           | Top-node baseline (§5.4)                                                                                                                                                                                                                                     |
| Region resolved, no region node                              | Country node's group, and the nearest ancestor's `jurisdiction`                                                                                                                                                                                              |
| Top node missing `group` or `jurisdiction`                   | Startup failure (§3.3)                                                                                                                                                                                                                                       |
| EC module configured, no geo, no acknowledgment              | Startup failure (§5.3)                                                                                                                                                                                                                                       |
| Module name the adapter does not link, or a name given twice | Startup failure when the adapter builds its application state, and not at settings validation (§3.3)                                                                                                                                                         |
| Malformed `permissions.yaml`                                 | Refused by `ts config push`, and by `ts config validate` when `--permissions` is given, and again at load, where the domain fails to load and answers 500 (§3.1, §3.3)                                                                                       |
| Settings name no policy, or a policy no supplied file has    | Refused by `ts config push`, and by `ts config validate` when `--permissions` is given, and a blob pushed without a policy fails to load (§3.1)                                                                                                              |
| Policy file absent, or its SHA-256 differs from the blob     | The domain fails to load and answers 500, as it does when Axum cannot read the file from its folder (§3.1)                                                                                                                                                   |
| Blob names a policy with no `sha256`                         | The domain fails to load and answers 500 (§3.1)                                                                                                                                                                                                              |
| Axum with `TRUSTED_SERVER_PERMISSIONS_DIR` unset             | The domain fails to load and answers 500 (§3.1)                                                                                                                                                                                                              |
| Undecodable record present (TCF, GPP, or USP)                | Read by the module for that scheme as the refusal or opt-out it may have carried, on the Data Uses that scheme covers, and never a withdrawal. Core answers nothing ahead of the modules, so the order decides against the other schemes (§4)                |
| Expired TCF record                                           | Distinct state, not unreadable. The decoded record is cleared, the `tcf` module answers `Neutral`, and the baseline applies                                                                                                                                  |
| Signals contradict (opt-out plus consent)                    | The later module in the configured order decides, so under the default order a TCF consent amends an earlier opt-out and a PMP answer read by `mtm` amends both (§4)                                                                                         |
| No EC module selected                                        | Identity fails closed, with nothing created and an incoming cookie value never used or egressed (§7)                                                                                                                                                         |

A startup failure and a domain that fails to load have the same answer. The
startup router answers with the status of the error's kind, which is 500 for
every startup failure in the table, because each is a configuration error, as
a config store that cannot be opened or read is. The reason is logged at error
level, so nothing is served from the origin. Row 4 moves Spin from a fixed 503
to the same rule, and Cloudflare and Spin log nothing until row 54 (§3.1, item
7). The rows on the policy file state the code of row 4,
IABTechLab/trusted-server#1345.

The posture is fail-closed. Every ambiguous state resolves to the
configured baseline or more restrictive.

### 6.1 Upgrading from the earlier settings

A configuration written for the earlier model fails at startup on this row,
each time with a message that names what to write instead. The rollout design,
`2026-07-30-provider-migration-rollout-design.md` (§4 and §5), carries the full
migration for `[ec]`, `[device]` and `[geo]`. For the permission model:

1. Remove `[geo] default_country`. `[geo]` refuses any key other than `module`
   and `assume_single_jurisdiction`, and the top node of the `rules:` tree takes
   the place of the old key (§5.4).
2. Remove `[consent.gdpr]`, `[consent.us_states]` and `consent_store`.
   `[consent]` refuses a key it does not define, and the `rules:` tree holds the
   two lists (§3.4). The configuration guide, `docs/guide/configuration.md`,
   still lists the two lists and `consent_store`.
3. Set `[geo] assume_single_jurisdiction = true` where an Edge Cookie module
   runs and no geo module is selected (§5.3).
4. Write the table as `[permission-signal]`, with `modules` for the earlier
   `module` or `sources`. The earlier forms are refused with a message that
   names the new one.
5. Check the names in `modules` against the crates the adapter links. Only an
   adapter finds a wrong or repeated name, because settings validation and the
   `ts` command line tool link none of the module crates. When an adapter
   cannot build its state, every request answers with the status of the
   error's kind, 500 for a wrong or repeated name, which is a configuration
   error, and the reason is logged at error level, so nothing is served from
   the origin. Row 4 moves Spin from a fixed 503 to the same rule, and
   Cloudflare and Spin log the reason only from row 54 (§3.1, item 7).
6. Decide the order. The default order lets a TCF answer amend an opt-out,
   which is the opposite of the earlier fixed rule, and a deployment that wants
   the earlier outcome lists the opt-outs after `tcf` (§4).
7. Name the policy and supply its file. `[permissions] policy` names the
   publisher's file by its stem, and `ts config push --permissions <file>`
   supplies it, as `ts config validate` and `ts config diff` take it (§3.1).
   No policy ships with Trusted Server and no tool chooses one, so
   `ts config migrate` names no policy in a document that has none and reports
   the missing policy as an item left to do
   (`2026-10-17-config-migrate-design.md`). A deployment that skips this step
   does not start, with the reason "the configuration names no permission
   policy" (`docs/guide/error-reference.md`).

## 7. Enforcement points

Consumers of the resolved set in the implementation:

1. **EC module execution.** The module declares
   `required_permissions()`, core resolves a `PermissionState` once per
   request at `EcContext` construction, and the module executes only when
   every declared permission is set (`ec_allowed`). A module that
   requires nothing always runs. **Geo** is ungated because gating it is
   circular, jurisdiction being an input to permission resolution.
   **Device** is ungated by a separate, deliberate decision, because its
   security-classification role must run for traffic that has granted
   nothing, and operator selection is the recorded authorization (modules
   spec §5) for the built-in and host modules, which declare nothing. No code
   on this row reads the `required_permissions()` of a device or geo module. A
   device module that an integration module supplies arrives with row 7 of the
   stack (`split/7-integration-seam-impl`, IABTechLab/trusted-server#1348),
   where `resolve_device_module` in
   `crates/trusted-server-core/src/integrations/registry.rs` refuses at startup
   a device module that declares a permission, naming the module and the Data
   Uses, because no per-request device gate exists to honor the declaration.
   The built-in Edge Cookie modules declare `necessary.operations.storage`.

2. **EC lifecycle.** Creation requires the module's declared permissions
   through the gate above. Withdrawal follows §4.2. Recognition and
   revocation of an existing identifier are never permission-gated, and the
   withdrawal path runs precisely when `ec_allowed` is false, reading the
   raw cookie value kept for that purpose.

3. **Sharing beyond the edge.** One predicate,
   `EcContext::ec_sharing_allowed`, requires the module gate plus
   **both** `necessary.operations.storage` and
   `advertising_marketing.first_party.targeted`. A storage-only grant
   therefore keeps first-party use while withholding partner sharing. The
   implemented inventory:

   | Path                                         | Gate                                                          |
   | -------------------------------------------- | ------------------------------------------------------------- |
   | Bidstream EIDs (every auction path)          | `gate_eids_by_permissions`, storage plus personalized ads     |
   | OpenRTB `user.id` on the `/auction` endpoint | `ec_sharing_allowed`                                          |
   | Identify endpoint (partner-facing)           | `ec_sharing_allowed`                                          |
   | Pull sync (browser-request-scoped)           | `ec_sharing_allowed`, from the live request resolution        |
   | Batch sync (context-free S2S)                | Authenticated, and withdrawn or missing rows are ineligible   |
   | KV EID resolution for auctions               | `ec_allowed`, then the EID pair gate on the result            |
   | Publisher navigation and page-bids `user.id` | `ec_sharing_allowed` (the storage plus personalized-ads pair) |

   With **no EC module configured**, identity fails closed, meaning the gate is
   closed rather than open by default (`ec_allowed` is false), so a cookie
   value present on the request is treated as absent and never used or
   egressed. This replaces PR #838's vacuously-true `is_none_or` check.

   **Navigation and page-bids.** Both attach the EC-derived request ID and
   `user.id` under the sharing pair (`ec_sharing_allowed`), as the table above
   lists (`handle_publisher_request` and `handle_page_bids` in `publisher.rs`),
   so a storage-only grant keeps first-party use and places no EC in `user.id`
   on those paths. The draft's fuller inventory (proxy/click/Testlight
   forwarding gates, the observability denylist with typed redaction
   boundaries, the raw-regulatory-transport destination allowlist, integration
   response cookies) is deferred (§11). Today EC values are truncated
   (`log_id`) before logging as the observability mitigation.

4. **Server-side auction dispatch (not migrated).** Dispatch is still gated
   by the consent subsystem (`consent_allows_server_side_auction`), not by
   the permission model. When the jurisdiction is GDPR or unknown, or an EU
   TCF signal is present, dispatch requires an effective TCF record
   consenting to Purpose 1, and otherwise no bid request leaves (a no-bid
   response, with no PBS/APS call and no UA/IP/geo forwarding). Known
   non-GDPR jurisdictions without an EU TCF signal dispatch freely. The
   draft's regime-keyed dispatch table, and the `ContextualAuctionView`
   positive projection for dispatch with personalized-ad selection unset,
   are **not implemented**. When personalized-ad selection is unset today,
   EIDs and the pair-gated identifiers are stripped but dispatch is the
   ordinary request, not a contextual projection. Recorded as deferred
   (§11) together with §3.4.

The client-cycle resolve endpoint (`POST /_ts/api/v1/ec/resolve`) is a
further consumer, and this row does not build it. Row 5 of the stack builds it
(`split/4-client-resolve`, IABTechLab/trusted-server#1346), where
`handle_ec_resolve` in `ec/resolve.rs` answers 204 and accepts no identifier
while `ec_allowed` is false. Its design is
`2026-07-30-client-cycle-ec-resolve-design.md`.

### 7.1 Contextual OpenRTB v1 allowlist (deferred)

Not implemented. The machine-readable projection manifest, its path
grammar, cardinalities, cross-field rules, and derivation vocabulary, and
the conformance walker over final encoded bytes, belong to the dispatch
migration (§7.4) and remain specified by the 2026-07-31 draft for that
work.

## 8. Testing strategy

Implemented, in `permissions.rs`, `permission_signal/mod.rs`,
`ec/consent.rs`, `ec/mod.rs`, `ec/finalize.rs`, `consent/mod.rs`, the five
module crates, the page script, and
`crates/trusted-server-adapter-axum/tests/permission_signals.rs`:

- **Signal order.** The last module with an opinion decides, silence
  leaves an earlier answer standing, and silence from every module is not
  a refusal. One test per opt-out source shows a TCF answer applied over it
  under the default order (`a_prompt_answer_applies_over_a_gpc_signal` and
  companions), and `the_opt_out_wins_when_a_deployment_puts_it_last` shows
  the reverse. An omitted `modules` runs every module in the offered order,
  a configured list is the order they run in, an empty list is accepted, and
  an unknown or repeated name is refused. The tests in
  `permission_signal/mod.rs` use stand-in modules named `gpc` and `tcf`,
  whereas the real crates answer to `permission-signal.gpc` and
  `permission-signal.tcf`, and the Axum test
  `the_documented_names_select_every_shipped_module_in_order` is the one that
  selects the real crates by name.
- **Withdrawal scoping.** One test per §4.2 arm: refusal under
  `requires_signal` withdraws, refusal under `granted` suppresses without
  destroying, consent is not a withdrawal, GPC alone never withdraws, sale
  opt-outs never withdraw, no signal never withdraws, an unreadable record
  never withdraws.
- **Unreadable and expired records.** Each of the `tcf`, `gpp` and
  `us-privacy` crates tests that an unreadable string of its own scheme is read
  as that module's refusal or opt-out, and core is tested to answer nothing
  ahead of the modules
  (`core_answers_nothing_ahead_of_the_modules`). The Axum tests show the order
  deciding between a readable and an unreadable record, and an expired TC
  string is not treated as unreadable
  (`an_expired_tcf_record_is_not_treated_as_malformed`).
- **Geo status.** A failed lookup resolves at the requires-signal floor
  (permissions and the storage baseline both), driven through a
  `PlatformGeo` that returns an error rather than from a hand-built status
  (`a_geo_module_failure_resolves_permissions_at_the_requires_signal_floor`),
  and no location falls back to the top node of the `rules:` tree.
- **Policy parsing and validation.** Groups, rules, detailed-rule
  acquisition maps (including `requires_signal` per rule), and the
  signals-section round trip. One table of fifteen cases
  (`from_yaml_rejects_each_malformed_policy`) checks the refusals, being
  unknown groups, unknown permissions, unknown acquisitions, incomplete
  groups, case-insensitive duplicate rule keys, unknown revoke keywords, the
  jurisdiction rules and places written under a region. No test covers a
  misspelled key in a node, or `us-state` under a country other than `US`.
- **Sample coverage.** All 30 EU/EEA codes lock storage to
  requires-signal (§3.5).
- **Vocabulary breadth.** A TCF record grants or revokes every one of the
  eleven mapped purposes, not only storage and personalized ads.
- **Gates.** Module execution blocked without its required permissions,
  the sharing pair withheld on a storage-only grant, EIDs stripped when
  either permission of the pair is unset, and the no-module stateless
  posture.
- **The `mtm` crate.** Fourteen tests in the crate cover the three words, the
  Data Uses each answers for, the terms it declares, and that no word
  withdraws. The PMP's own `Set-Cookie` strings are parsed to the word they
  carry, and the cookie is pinned as never `HttpOnly`. One Axum test runs all
  five modules for a PMP answer in the EU.
- **Page script.** `crates/trusted-server-js/lib/test/core/permissions.test.ts`
  covers how the snapshot, with its signals and terms, reaches page code in
  every order of arrival.
- **The policy file (row 4).** The tests in `policy.rs` show that the push
  writes the digest and the exact text of the named file, that Axum and
  validation leave the text out, that the digest is taken after the
  environment overlay and that `--no-env` leaves the overlay out
  (`the_digest_is_taken_after_the_environment_overlay`), and that the
  settings handed to EdgeZero are removed when the command is done. They
  refuse a policy no supplied file has, a digest or text the settings set, a
  file that is not a `.yaml` policy in UTF-8 text, two files that supply one
  policy, the example settings as
  written, and a diff without the file when the settings name a policy, and
  they check every supplied file whether or not it is named. The command line
  tool's own tests show that `ts config validate` without the file says the
  policy was not checked and that a push of the example settings is refused
  (`crates/trusted-server-cli/tests/config_store_defaults.rs`). The tests in
  `config_payload.rs` show that a domain naming no policy, a policy with no
  digest, a blob without its file where the file travels in the blob, a file
  that is not the pushed file, a pushed file that is not a valid policy and a
  policy the source cannot read each fail to load, that only the policy's name
  and digest stay in the settings, that a source supplies the file and any
  text in the blob is unused, and that two loads that pin one digest parse the
  file once. Each adapter's tests show each request given the policy its
  domain loaded. Those of Fastly, Cloudflare and Spin show the startup router
  answering a policy load error with 500, Axum's show that startup without the
  pushed policy file answers 500 on every route, and Cloudflare's show that
  each manifest binds the config store as a namespace of its own and carries
  no settings in a variable. `the_loaded_policy_decides_and_not_the_sample` in
  `ec/consent.rs`, `the_policy_file_counts_by_its_digest_and_not_its_text` in
  `publisher.rs`, and the Viceroy configuration's tests, which load the
  generated chunks through the runtime loader, complete the set.
- **Not covered by a test.** The order between `mtm` and `tcf` or an opt-out in
  one request. A withdrawal beside an `mtm` grant of storage. The startup
  warning with the real module names. Proxy mode through `EcContext` and
  `keep_only`. A misspelled key in a policy node, and `us-state` under a
  country other than `US`. The module lists of the four adapters, which no test
  compares. The permissions inspector, whose CI job
  (`.github/workflows/inspector.yml`) builds the engine and runs nothing
  against it.

The draft's fuller matrices (the complete normalization matrix as
table-driven tests, the per-row egress inventory with a denylist check, the
dispatch regime-by-signal matrix, contextual-serializer poisoning, S2S
authority denial reasons, and the §4.3 fault-injection suite) travel with
their deferred features.

## 9. Out of scope

- The §4.3 durability protocol, §4.5 full field mapping and registry
  snapshot, §5.5 activation apparatus, §7.4 dispatch migration, and §7.1
  contextual projection are deferred follow-ups, recorded in §11 with the
  2026-07-31 draft as their reference design. Deferred, not rejected.
- Rules written into `[permissions]` itself are not adopted, because
  `[permissions]` names the policy file, which keeps the YAML form a policy
  owner reads (§3.1).
- Per-signal jurisdiction scoping (honoring GPC only where a law defines
  it) is rejected, as in the draft. The opt-out signal layer is
  jurisdiction-free in the implementation, and the country baseline decides
  only what a revoke has to drop.
- An authenticated deletion or explicit storage-withdrawal endpoint is
  deferred (§4.2).

**Open questions.** Each of these waits for a decision and for a code change,
and this revision changes no code.

1. Whether the startup warning that lists the modules a list leaves out should
   compare names the way selection does. `omitted` in
   `permission_signal/mod.rs` compares each written name with the full module
   name, and every shipped module is named `permission-signal.<folder>`. By
   reading, a list written as `["gpc", "tcf"]` therefore reports all five
   modules as not acted on, those listed included. The tests use stand-in
   names equal to the written ones, so they cannot show it.
2. Whether `proxy` mode should keep passing the raw consent strings on. By
   reading, it passes none, because the pipeline decodes nothing in that mode,
   no module vouches for an undecoded string, and `keep_only` removes the raw
   strings (§4.4). `docs/guide/configuration.md` and the comments on
   `ConsentContext::expired` and `build_consent_context` still say that proxy
   mode forwards them.
3. Whether the policy parser should refuse a key it does not define (§3.1).
   A key beside `group`, `jurisdiction` and `permissions` is read as a place
   code, a key outside `groups`, `rules` and `signals` is ignored (the sample
   file carries `name:`), and so is an unknown key inside `signals`,
   `signals.tcf` or `signals.us_opt_out`.
4. Whether `us-state` should be refused on a region under a country other than
   `US` (§3.2).
5. Whether the default order should keep `mtm` last. A script can write
   `__mtm_pref`, and under the default order a `standard` or `personalized`
   word replaces a TCF refusal and every opt-out for the eleven named Data
   Uses, and stops a TCF refusal of storage from withdrawing (§4.2 and §4.6).
6. Whether to correct, with the code, the text that now says otherwise.
   - `trusted-server.example.toml` lists four modules and says that an
     unreadable record revokes whichever modules are configured.
   - The comments in `config/permissions/sample.yaml` name
     `[permission_signal] module`.
   - `docs/guide/configuration.md` lists `gdpr.applies_in`,
     `us_states.privacy_states` and `consent_store`.
   - The comment on `request_jurisdiction` in `consent/mod.rs` mentions a KV
     read fallback that no longer exists.
   - The header of `permission_signals.rs` in the Axum tests counts four
     modules, and the comment on `shipped_signal_modules()` in each adapter
     counts three after the first.

## 10. Divergences from issue #779

This spec supersedes #779 on the following points, so there is one
acceptance contract, not two:

| #779 says                                                                                    | This spec says                                                                                                                                                                                                                     | Why                                                                                                                                                                        |
| -------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Unmatched countries fall to `default_country`                                                | Adopted, with a changed mechanism since 2026-09-01. Unmatched and unresolved requests both fall to the required top node of the `rules:` tree, and a **failed** lookup floors instead (§5, §12)                                    | The failure state is the one that must never reach a permissive default, and the draft's `rules.default` split was not kept                                                |
| The full TCF purpose vocabulary is modeled                                                   | Adopted and extended. All eleven purposes are signal-resolved, and the full Privacy Taxonomy is carried as declared baseline (§2)                                                                                                  | The joint taxonomy work made whole-taxonomy declaration the goal, and `denied` defaults keep undeclared uses inert                                                         |
| The legal policy is the deployer's to set and is not baked into Trusted Server               | Adopted. The policy is the publisher's file, selected and supplied when the service is deployed, and never compiled into a build (§3.1)                                                                                            | Agreed with IAB Tech Lab on 2026-10-23, so that one released binary serves every publisher and no domain starts without a valid policy file                                |
| A default deployment needs no policy interaction at all                                      | Not adopted. Every domain names a policy file, which the push supplies, because no policy ships with Trusted Server and a domain without a valid file does not start (§3.1)                                                        | Agreed with IAB Tech Lab on 2026-10-23, so that a binary carries no policy to fall back on                                                                                 |
| Permission sources are open-ended (#777: publisher interaction, external services may grant) | Adopted as a seam. A source is a permission signal module, a crate implementing `PermissionSignalModule`, listed by each adapter in `shipped_signal_modules()` and asked in the order `[permission-signal] modules` gives (§1, §4) | The five shipped schemes already use it from outside core, so the interface has real consumers, and publisher interaction or an external service arrives as another module |

## 11. Revision record vs the 2026-07-31 draft

One row per divergence between the draft and the implementation this revision
was verified against (branch `split/3-permissions` at `a028c00da`, row 4 of
the stack, which pins EdgeZero at `683202c`, where §3.1 cites
EdgeZero `v0.0.10`, the version the stack pins now). The rows on where the
policy lives state the code of row 4, IABTechLab/trusted-server#1345 (§3.1),
and the row on its activation records the deferral of §5.5.

| Draft position                                                                                                                                            | Implemented position                                                                                                                                                                                                                                                                  | Why                                                                                                                                                                                           |
| --------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Vocabulary is two TCF-purpose identifiers, enforced permissions only (§2)                                                                                 | IAB Privacy Taxonomy Data Uses, with eleven named purposes all signal-resolved, plus 53 further taxonomy Data Uses that no module gates on and that a signal reaches for eleven of them under the sample policy (§2)                                                                  | The joint taxonomy adoption postdates the draft, and whole-taxonomy declaration serves completeness and demonstration, with `denied` defaults keeping unenforced flags inert                  |
| Policy lives in `[permissions]` in `trusted-server.toml`, published via `ts config push` (§3.1)                                                           | Adopted in part. `[permissions] policy` names the publisher's `permissions.yaml`, which keeps its YAML form and is pinned in each blob by its SHA-256, and Fastly stores each file once from row 11. `ts config push`, `validate` and `diff` take it with `--permissions` (§3.1)      | The draft asked for `[permissions]` so that policy is validated, compared and pushed with the settings, and the file it names gets all three while keeping the YAML form a policy owner reads |
| Every group carries a required `regime` class read by auction dispatch (§3.2)                                                                             | No regime field exists                                                                                                                                                                                                                                                                | Its only consumer, regime-gated dispatch, was not migrated, and the field returns with that work                                                                                              |
| Overrides name explicit acquisition rules, replacing the `+`/`-` sigils (§3.2)                                                                            | Adopted. A detailed rule's `permissions` map assigns `granted`, `requires_signal`, or `denied` per Data Use                                                                                                                                                                           | The draft's requirement, expressed in the YAML schema. `requires_signal` is now expressible per rule                                                                                          |
| Two fallbacks: policy `rules.default` for unmatched countries, `default_country` only for the static no-geo mode (§5.4)                                   | One required fallback covers unmatched and unresolved requests in every mode, startup-validated, and a failed lookup floors separately. It was `[geo] default_country` until 2026-09-01 and is now the top node of the `rules:` tree (§12)                                            | One deployer knob is simpler, and the safety-critical separation kept is failure vs absence, carried by `GeoStatus`                                                                           |
| Validation checks assigned ISO 3166-1 codes, assigned subdivisions, and a group identifier grammar (§3.3)                                                 | Validation covers unknown groups, permissions, acquisitions, revoke rules, incomplete groups, case-duplicate rule keys, and the jurisdiction values. The parser does not reject an unknown key in a node (§3.1)                                                                       | A smaller surface shipped first, and the EU/EEA test guards the sample against typos. ISO checks are future hardening, and matter more now that a publisher's own file has no such test       |
| Three-class signal taxonomy with regime-scoped grant acceptance, where the US posture is `requires_signal` with GPP/USP non-opt-out values as grants (§4) | Of the shipped modules TCF and `mtm` are the grant sources, the US posture is a `granted` baseline that opt-outs revoke, and explicit non-opt-out values grant nothing                                                                                                                | A simpler two-signal model without regimes. The cost, no-signal US traffic is allowed by baseline rather than blocked pending a signal, is a deliberate policy choice in the sample           |
| Malformed-present blocks grants per record family and mapped section (§4.4, §4.5)                                                                         | Each module answers for its own scheme's unreadable record, as a refusal or opt-out on the Data Uses that scheme covers, and core answers nothing ahead of the modules (§4)                                                                                                           | The scope is the Data Uses of the scheme whose record could not be read, because per-section scoping needs the full §4.5 decoder work                                                         |
| Normalization runs expiry before conflict resolution, a declared change (§4.4)                                                                            | Conflict resolution still runs before the expiry check                                                                                                                                                                                                                                | The reordering was not implemented, though the expired state itself (distinct from unreadable, absent for acquisition) was adopted                                                            |
| Persisted-KV consent flows through the full normalization pipeline with an explicit TTL comparison (§4.4)                                                 | Not built. No persisted-KV consent exists on this row, the consent pipeline reads the live request only, and `consent_store` is refused (§4.4)                                                                                                                                        | The legacy consent store was removed (IABTechLab/trusted-server#883), so live consent is interpreted on each request                                                                          |
| Proxy mode gains minimal opt-out extraction (§4.4)                                                                                                        | Proxy mode still skips decoding, so a present TC, GPP or US Privacy string is unreadable to its module and revokes the Data Uses that scheme covers, and the GPC header opt-out is honored without decoding. By reading, no raw string reaches the auction and publisher paths (§4.4) | The permission-layer outcome is equally or more restrictive with no new decode paths, and this is revisited with the §4.5 decoder work                                                        |
| Withdrawal has four triggers including an explicit storage-withdrawal or authenticated deletion request (§4.2)                                            | The TCF Purpose 1 refusal under a non-granted baseline is the only trigger, and opt-outs, unreadable records, absence, and policy changes never withdraw (adopted)                                                                                                                    | No deletion endpoint exists to carry the extra trigger, so the narrowest destructive surface shipped first                                                                                    |
| §4.3 durability protocol: family records first, suppression and authority-state records, outbox, breaker, strong reads                                    | Cookie expiry plus best-effort identity-graph tombstones per presented identifier, with failures logged                                                                                                                                                                               | The protocol requires storage primitives (linearizable CAS, independent durability domains) the adapters do not yet qualify, so it is deferred with the providers-spec storage work           |
| §4.5 field mapping and §4.5.1 vendored registry snapshot (sharing/targeted opt-outs, embedded GPC, applicability, derived `gpp_sid`)                      | Opt-out sources are the GPC header, a GPP sale opt-out, and a USP sale opt-out. The revoke set is policy-declared, and the repository's sample lists device storage, sale, sharing, third-party sale, targeted advertising and profiling (§4.5)                                       | The full decoder and registry vendoring are their own project, and the policy-declared revoke set gives deployers the scoping lever meanwhile                                                 |
| §5.5 activation: JCS policy digests, ordinals, activation register, journal, drains, admission leases                                                     | None of it exists. Each blob names its policy file by name and SHA-256, so the pushed blob is the policy identity (§5.5)                                                                                                                                                              | A push reaches instances as any settings push does, and the draft remains the reference design for moving every instance at once                                                              |
| §3.4 single jurisdiction truth, and §7 dispatch gated on the policy regime with a contextual projection                                                   | Half adopted. Since 2026-09-01 the `rules:` tree carries regime applicability and the two `[consent]` lists retire into it (§3.4), while auction dispatch still keeps the consent-subsystem gate (effective TCF Purpose 1 for GDPR or unknown jurisdictions), with no contextual view | Dispatch migration is follow-up. The legacy-list drift risk the draft named still stands and is recorded rather than resolved                                                                 |
| Every raw-EC egress path is pair-gated, with per-row tests and a denylist check (§7)                                                                      | Pair gating is centralized in `ec_sharing_allowed` (auction endpoint `user.id`, publisher navigation and page-bids `user.id`, identify, pull sync) and `gate_eids_by_permissions` (EIDs everywhere). Batch sync checks row state only                                                 | Partial adoption. Aligning the remaining paths, the S2S stored-provenance authority, and the inventory tests is recorded follow-up                                                            |
| Identity rows never store raw consent strings, only normalized provenance and a digest (§1)                                                               | The identity-graph entry stores the raw TCF and GPP strings that a module vouched for with the row                                                                                                                                                                                    | The normalized provenance schema belongs to the providers-spec storage work, and until then rows carry the raw strings                                                                        |
| No signals block in policy, and the signal mapping is fixed in the spec                                                                                   | New. A `signals` section in `permissions.yaml` declares the opt-out sources and revoke set, with `tcf.authoritative` governing only TCF's own effect. The TCF purpose map has since moved into the TCF module crate (§13)                                                             | Moves signal policy from code into deployer-editable data, while what a scheme's own signal means stays with that scheme's module                                                             |
| The §5.3 no-geo guard covers every jurisdiction consumer                                                                                                  | The guard fires when an Edge Cookie module is configured with no geo module                                                                                                                                                                                                           | The EC module is the only policy-gated consumer today, and the trigger list grows when dispatch and further egress paths join the model                                                       |
| `default_country` is required only in the acknowledged static no-geo mode (§5.4)                                                                          | Required always and startup-validated. Since 2026-09-01 the requirement sits on the `rules:` tree's top node, which must carry a `group` and a `jurisdiction` (§12)                                                                                                                   | It is the baseline for unmatched requests in every mode, so it must always exist                                                                                                              |

## 12. Revision record: the rules tree (2026-09-01)

The flat rule keys and `[geo] default_country` are replaced by a single
`rules:` tree, following review discussion on PR #1084. The tree was extended
the same day, where `jurisdiction` became a per-node inherited attribute and
the two `[consent]` applicability lists retired into the tree. One row per
change.

| Before                                                                                                           | After                                                                                                                                                                                                   | Why                                                                                                                                                                                             |
| ---------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Flat rule keys, a bare country (`FR`) or a slash-joined pair (`US/CA`)                                           | One tree, with country codes at the first level and region codes nested beneath them, all matched case-insensitively                                                                                    | The nesting shows the reader the precedence instead of hiding it inside a joined key, and the file's audience is a policy owner rather than a developer                                         |
| A rule value is a group name, or a mapping of `{group, permissions}`                                             | Every node has a `group` and optional children. A single string is shorthand for that group with no children, and a mapping must carry `group:`                                                         | One node shape reads the same at every depth, and the shorthand keeps a one-line country rule to one line                                                                                       |
| `[geo] default_country` in `trusted-server.toml` names the rule for unmatched requests                           | The tree's top node carries the `group` for a request whose country cannot be resolved                                                                                                                  | Treating an unknown visitor as being in a chosen country was a fiction, and the tree states the unknown case explicitly where a reader will look for it                                         |
| The consent handling for an unresolved request follows from the chosen default country                           | The top node requires `jurisdiction:` beside `group:`, with the values of the `Jurisdiction` type (§3.2)                                                                                                | The unknown case deserves its own stated answer rather than one inherited from a stand-in country                                                                                               |
| Startup rejects a missing or unresolvable `default_country`                                                      | Startup rejects a top node missing `group` or `jurisdiction`, and reserved keys cannot collide with place codes because ISO codes are short                                                             | The same loud failure, moved to where the policy now lives                                                                                                                                      |
| `jurisdiction` is written at the top of the tree only                                                            | Any node may carry `jurisdiction:` beside `group:`, and a node carrying none inherits the nearest ancestor that does. The top node must carry both, so the inheritance always terminates                | A country or a state often needs its own regime, and inheritance states the common case once while leaving the exceptions visible where a reader meets them                                     |
| The US-state value carries the state code, as `us-state/<CODE>`                                                  | The value is bare `us-state`, valid only on a region node, and rejected at the top and on a country node. The parser does not check the country above the region (§3.2)                                 | The matched node already names the state, so repeating the code in the value invites the two disagreeing                                                                                        |
| `consent.gdpr.applies_in` and `consent.us_states.privacy_states` in the operator TOML carry regime applicability | Both retire into the tree, where the sample file holds the same 31 GDPR countries inheriting `gdpr` and the same 20 privacy-law states as region children of `US`, with `jurisdiction: us-state` (§3.4) | One document carries the baselines and the applicability together, so a policy owner keeps one list rather than two in step, and the drift risk §3.4 recorded has nothing left to drift against |

This revision leaves unchanged the group vocabulary and flags, the signal
precedence of §4, and the requires-signal floor for a failed geo lookup, which
stays distinct from having no location. Trusted Server still encodes no
jurisdiction's law, as the deployer states the policy and the software carries
it out.

## 13. Revision record: permission signal modules (2026-09-14)

Signal precedence moves from a fixed rule in code to the order a deployment
configures, and the shipped schemes move out of core into module crates. One
row per change. The rows describe the code on `split/3-permissions`, so they
include the changes that followed the first revision of this section on
2026-09-14, being an unreadable record answered by its own module, `awaiting`,
`signals` and `mtm`.

| Before                                                                                                                 | After                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Why                                                                                                                                                                                                                                                                                         |
| ---------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Precedence fixed in code, most restrictive first, so an opt-out always beat a consenting TCF record                    | The order in `[permission-signal] modules`, where the last module with an opinion decides, validated and logged at startup (§4)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | Which scheme wins is a question about a jurisdiction and a publisher, and a fixed rule answers it in code for every deployment                                                                                                                                                              |
| A GPC header suppressed a Data Use that a TCF record consented to                                                      | The default order is `gpc`, `gpp`, `us-privacy`, `tcf` and `mtm`, so a TCF answer amends an earlier opt-out, and listing the opt-out after `tcf` restores the previous outcome                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Global Privacy Control is a browser setting with no interface of its own, and the schemes after it carry an answer a person gave through an interface, so by default that answer amends the header the visitor arrived with                                                                 |
| Signals resolved inside core through a per-permission `ConsentSignal` closure, with further sources deferred (§1, §10) | A `PermissionSignalModule` trait in core, with GPC, the GPP sale opt-out, US Privacy, TCF and MTM each a crate under `crates/permission-signal/`, linked and listed by each adapter in `shipped_signal_modules()` and selected from `[permission-signal] modules`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | A scheme core has never heard of plugs in without a change to core, and none of the five is privileged by being built in                                                                                                                                                                    |
| The TCF purpose to Data Use map in the `signals` section of `permissions.yaml` (§3.2)                                  | The map in the TCF module crate, `crates/permission-signal/tcf/src/mapping.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | A deployment that runs no TCF carries no table of another scheme's numbers                                                                                                                                                                                                                  |
| Malformed-present as step 3 of the fixed order                                                                         | Each module answers for its own scheme's unreadable record, in the configured order, and core answers nothing ahead of the modules (§4)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Core no longer reads any scheme, so what an unreadable, absent or expired signal means is each module's decision, and the order decides between schemes                                                                                                                                     |
| Withdrawal decided in core from the TCF record                                                                         | A `withdraws` answer on the module trait, scoped by core to the storage baseline, and given today only by TCF (§4.2)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Revoking and withdrawing stay different questions, and a new scheme can carry its own withdrawal                                                                                                                                                                                            |
| The permission state carries the resolved set only                                                                     | It also carries `awaiting`, the permissions that need a signal no module has given and that some configured module could still grant (each module declares what it can grant through `grants` on the trait), `signals`, one entry for each signal a module read and found valid, as received (each entry carries the module, the scheme and the raw string, so the page receives the raw strings the modules used), and `tdls`, the terms documents the configured modules declare, in module order with duplicates removed. The page receives all four, and an empty state is `{"awaiting":[],"set":[],"signals":[],"tdls":[]}`. Each `tdls` entry must address a document never edited once published, which the `Tdl` type documents and cannot enforce | A page has to tell a prompt that has not run from a visitor who refused, a bid request has to carry only signals a module could read, and a recipient needs the exact terms the data is available under. An empty `tdls` says no terms were declared rather than that anything is permitted |
| Opt-out-beats-TCF pinning tests                                                                                        | Order tests in both directions, per-source tests, and selection tests (§8)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | The tests pin the configured behavior rather than the retired rule                                                                                                                                                                                                                          |
| No scheme read the PMP's answer                                                                                        | `mtm` reads the PMP's `__mtm_pref` cookie, answers for the eleven named Data Uses, and declares the versioned Model Terms for Marketing as the terms the data is available under (§4.6)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | The PMP keeps its own copy of the answer and mirrors it into the cookie, which is what lets the server see the answer at all                                                                                                                                                                |

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-08-25 | Revised against the implementation, with a row for each divergence from the 2026-07-31 draft (§11).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| 2026-09-01 | The `rules:` tree replaced the flat rule keys and retired `[geo] default_country` (§12).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 2026-09-14 | Permission signal modules replaced the fixed signal precedence with a configured order (§13).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 2026-10-06 | Module became the word for what an operator selects.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| 2026-10-07 | A module is named by its crate folder, so the selector reads `[permission-signal] modules`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 2026-10-15 | Fixes from review round 1. An unreadable record answered by its own module with nothing ahead of the modules, the fifth module `mtm` with its own section, registration through each adapter, the four members of the page state, the sharing pair on navigation and page-bids, the persisted consent, resolve endpoint and device module check named as built by other rows or not at all, what the policy parser does not reject, the Data Uses a signal reaches, the raw strings removed after assembly, a table of what each adapter runs, the upgrade list, the open questions and the review log. |
| 2026-10-16 | Review round 2, approved with one minor point recorded in the Review log.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 2026-10-27 | The policy file is never compiled into a build, as agreed with IAB Tech Lab at the Touchbase on 2026-10-23, and is selected and supplied when a service is deployed, as decided by 51Degrees after the Touchbase (§3.1). What row 4 builds and later rows add, Cloudflare reading its blob from the config store, the push, answers that follow the error's kind, and a blob's propagation (§3.1). What the build carries (§3.6), the cost of the fetch (§3.7), the failure modes (§6), the upgrade list (§6.1), the tests (§8), the records in §10 and §11, and the sample named as the sample.        |
| 2026-10-27 | A load asks for the rules held for a pinned digest before it reads a source, so an instance that already holds a host's digest reads and hashes nothing for it (§3.7, item 6 of §3.1), a fix found in review. The line numbers of `permissions.rs` and `config_payload.rs` follow the fix.                                                                                                                                                                                                                                                                                                              |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/3-permissions` at `a028c00da`. Issues found, nine of them blocking. The nine were the claim that an unreadable record revokes everything ahead of the modules, four shipped modules where five ship, registration through an integration builder, a page state described with two members where it has four, a follow-up note that gave `ec_allowed` for two paths that use `ec_sharing_allowed`, the persisted consent, the resolve endpoint and the device module check that this row does not build, unknown rule fields that the parser does not reject, the Data Uses said to have no signal, and raw strings said to be kept for forwarding. This revision states what the code does in each case, adds the fifth module `mtm`, a table of what each adapter runs, an upgrade list and the privacy effects the round found, names the row that builds each item this row lacks, and leaves the code changes the round asks for under Open questions.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/3-permissions` at `a028c00da`. Approved, none blocking. One minor point stays, that the `split/` branches the spec names are the fork's, while the IAB Tech Lab drafts carry the same commits on `feature/` branches.
3. **Round 3.** A review in a fresh session, against `split/3-permissions`. Issues found, six of them blocking. The six were that §3.1 rested on code that only later rows of the stack build and never said which row builds that code, that a Cloudflare Worker variable holds at most 5 KB while the repository's sample alone is 24,231 bytes, that nothing said what a location answers while a push is still reaching it, that the statuses a domain answers when it cannot start were partly untrue of today's code and left out Spin's fixed `503` and the `421` that Fastly answers from row 11 for a host with no blob, that fourteen places still called the repository's sample a policy that ships, and that the push could not be built from the text on any adapter. This revision says what row 4 builds by itself and what each later row adds once it is merged forward, has Cloudflare read its blob from its config store, a Workers KV namespace, in place of the variable, and says that each startup router answers with the status of the error's kind, so a domain without a valid policy file answers `500` on every adapter once row 4 moves Spin off its fixed `503`, while a host with no blob keeps its `421`. It also has the push, from row 11, write every policy entry before the blobs that name it and then wait, says that a location which still sees a blob before its policy entry answers `500`, sets out how `ts` adds `--permissions` to EdgeZero's commands, how the SHA-256 is taken and what the push refuses, and calls the sample the repository's sample throughout.
4. **Round 4.** A review in a fresh session, against `split/3-permissions`, of the round 3 fixes and the new text. Issues found, two of them blocking. The two were that the design cited row heads by commits that no published branch held, and that `ts config validate` takes no `--adapter`, so validation could not follow a rule under which the push adds the policy's text on every adapter but Axum. This revision cites commits that the published branches hold, and says that the configuration view never shows a policy's text, so a pattern under `permissions.text` is refused wherever patterns are checked and every check counts `permissions.sha256` as present, that `ts config validate` and `ts config diff` run the policy checks only when `--permissions` is given, with validate otherwise saying that the policy was not checked, and that `ts config migrate` and `ts audit generate` never run those checks. It also says how `ts` hands the settings to EdgeZero, which binding a Worker reads its blob from, what a Worker missing that binding or its blob answers, and how the tests of the `gpc`, `gpp`, `tcf` and `us-privacy` crates read the sample once `PermissionMaps::standard` is removed.
5. **Round 5.** A review in a fresh session, against `split/3-permissions`, with what rows 9, 11, 13, 51 and 55 add read on their own branches. Approved, none blocking. The round checked every file, function, line number and test that §3.1, §3.6, §3.7, §6 and §8 cite, against the code of those rows, EdgeZero `v0.0.10`, worker-build `v0.8.5` and Tech Lab main, and found each as the design states it, the order in which the runtime loader asks its source for a policy file before it looks for a parsed copy included. This revision says that the seam probe's script, which only test builds link, is carried whole as well and never reaches a release, names the adapter's test beside the Cloudflare manifests rather than the README's, and names the build-support file that the template build digest hashes with the build script. Before this round the text was brought into line with the code, so the wait between a new policy entry and its blob comes with row 13's live push, where rounds 3 and 4 read it at row 11.
6. **Round 6, 2026-10-27.** A review in a fresh session, against `split/3-permissions`, of the fix found in review that has a load ask for the rules held for a pinned digest before it reads a source, with the keyed push read on `split/11-config-by-host`. None blocking. The round checked §3.7 and item 6 of §3.1 against `load_domain_policy` and `held_policy`, every exit path of the loader, the line numbers in `permissions.rs` and `config_payload.rs`, and the tests that count a source's reads, and found each as the design states it. It noted that on Fastly a second load in one instance happens only when the opt-in reusable sandbox serves more than one request from it.
