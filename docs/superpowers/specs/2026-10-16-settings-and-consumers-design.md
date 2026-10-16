# Design Spec: Every settings section has a consumer, and every consumer has its settings

**Status:** Proposed, 2026-10-16
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-page-middleware-design.md` (row 10, entries that name middleware), `2026-10-12-shared-vendor-tables-design.md` (row 26), `2026-10-13-middleware-settings-design.md` (row 38), `2026-10-14-no-modules-crate-design.md` (row 43, each adapter lists the modules it links) and `2026-10-15-module-cache-declarations-design.md` (row 32), all in IABTechLab/trusted-server#1342, which holds every design document of the stack
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1403 (settings without a consumer and consumers without settings), with the related IABTechLab/trusted-server#1125, #1132, #1201, #1097, #1142, #1148, #1134, #1102, #1140, #1141 and #1139
**Last updated:** 2026-10-16

> **Why this spec exists.** A settings section and the consumer that acts on it are checked against each other by a dozen separate checks that run at different times and stop at the first gap, and several pairs are not checked at all, so this spec proposes one declaration model in core and one check that reports every gap at startup and at push.

Line references are at `3aaf720c0`, the head of `split/50-cookies-by-path`, which is the head of IABTechLab/trusted-server#1400 and the top of the stack. A file named without a folder is in `crates/trusted-server-core/src`, and `registry.rs` and `mod.rs` are `integrations/registry.rs` and `integrations/mod.rs`.

## 1. Problem

Four words are used as the code uses them. A builder (`IntegrationBuilder`, `mod.rs` line 452) is what a crate hands to core to say what the crate offers. The registry is what core builds at startup from every builder (`IntegrationRegistry::with_plan_and_registrations`, `registry.rs` lines 2033 to 2400). Startup is an adapter loading the settings and building its state, which each adapter does through that function (Fastly `app.rs` line 360, Axum `app.rs` line 240, Cloudflare `app.rs` line 295, Spin `app.rs` line 219). Push is `ts config validate`, `ts config diff` and `ts config push`, which validate through `validate_settings_for_deploy_with` (`config.rs` lines 311 to 341).

A consumer is whatever acts on a settings section, being a middleware an entry names, a module a section selects, a request filter, a page request preparer or a part of the adapter. Many features need both halves, and today the two halves are matched in four different ways.

**Checks in the registry, at startup only.** Push builds no registry. `validate_phase_entries` checks the shape of each entry and says that whether a name is supplied "is only knowable where the registry is built" (`settings.rs` lines 4776 to 4791), and the section selection check is left out of push deliberately, "because a vendor crate the CLI never links may supply the name" (`registry.rs` lines 2163 to 2168). The registry's checks are these.

- An entry naming a middleware no running module supplies is refused (`registry.rs` lines 1669 to 1685). The note that follows names a module no section selects (lines 1118 to 1134) and never names an absent shared table.
- A `[middleware.<name>]` table at a name no running module supplies is refused (lines 1702 to 1723).
- A middleware no entry names is a warning, that it "changes no page" (lines 1724 to 1731).
- A `[shared.<vendor>]` table no builder reads is refused (lines 2044 to 2069).
- A section selecting a module no builder supplies is refused, with a note naming absent shared tables (lines 1774 to 1825 and 1162 to 1178).
- A geo, Edge Cookie, device or analytics module that runs and is not selected is a warning (lines 1199 to 1212).

**Checks a feature wrote for itself.** The InMobi prompt refuses `[cmp]` selecting it with no `text/html` entry naming it, whatever the entry's `path` (`crates/cmp/inmobi/src/lib.rs` lines 141 to 158). The ads.txt sellers do the same for `text/plain` (`crates/ads-txt/iabtl/src/lib.rs` lines 152 to 167). Core's robots.txt check requires `path = "/robots.txt"` exactly (`settings.rs` lines 4414 to 4462). Each of the three runs at push and at startup, because a builder's validate function runs at both (`config.rs` lines 394 to 406) and `prepare_runtime` runs at both (`config.rs` line 323 and `settings.rs` lines 4357 to 4406). The cache table check is the one core check that runs at push as well (`mod.rs` lines 1008 to 1025, called from `config.rs` line 330).

**No check.** These pairs are not matched at all.

- The 51Degrees client hints crate registers a request filter and a middleware whenever `[shared.51degrees]` is present (`crates/middleware/51degrees/src/lib.rs` lines 110 to 132). Core runs every registered filter (`registry.rs` lines 2640 to 2674), so with no entry naming the middleware the filter still asks the service on every page request (`lib.rs` lines 352 to 381), and a request carrying the filter's state skips the shared page template and is sent `no-store, private` (`publisher.rs` lines 5536 to 5540 and 6396 to 6402).
- The DataDome page tag writes nothing when `client_side_key` is blank (`crates/bot-protection/datadome/src/lib.rs` lines 1016 to 1026), and the startup checks look only at the tag's URL (lines 509 to 511), so an entry naming `bot-protection.datadome.tag` with no key is accepted silently.
- Only the Fastly adapter runs request filters (Fastly `app.rs` lines 836 to 867), dispatches asset routes (lines 1191 to 1194 and 1310 to 1313), reads the analytics sink (lines 389 to 392) and configures origin certificates from `certificate_check` (Fastly `backend.rs` lines 492 to 496). The Axum adapter's origin client takes no certificate setting (Axum `platform.rs` lines 321 to 333), and the Cloudflare and Spin adapters use the setting only in a backend's name (Cloudflare `platform.rs` lines 70 to 78, Spin `platform.rs` lines 92 to 100). With `[geo] module = "platform"` the registry leaves the adapter's own lookup in place (`registry.rs` lines 1480 to 1482), and the Axum and Spin lookups answer no location for every request (Axum `platform.rs` lines 210 to 218, Spin `platform.rs` lines 809 to 817). Each of these is accepted on the adapters without the part, with no log.
- `cluster_trust_threshold` and `cluster_recheck_secs` in `[ec]` are read into the settings (`settings.rs` lines 783 to 795) and read by no crate, which IABTechLab/trusted-server#1097 records.

**One gap at a time.** Every check returns its first error (for example `registry.rs` lines 1679, 1716, 1815 and 2063), so a document with two gaps fails twice before it starts.

**One-off notices.** Where a setting is known to do nothing, the code that knows says so in its own way, being the Fastly warning when `certificate_check` is off (Fastly `app.rs` lines 484 to 490) and the DataDome warning for its reserved GraphQL flag (`lib.rs` lines 513 to 515).

## 2. Design

One model in core, in four points.

1. Every consumer declares the settings it needs.
2. Every settings section has a consumer.
3. Each adapter declares the parts it has.
4. Startup, `ts config validate` and `ts config push` give one report listing every gap.

### 2.1 The declaration types in core

Core already holds one declaration a builder makes about a consumer, `SuppliedMiddleware`, which names a middleware, its phase and its media type (`mod.rs` lines 376 to 385), and the registry already checks that declaration against the middleware the builder really supplies (`check_described_middleware`, `registry.rs` lines 1591 to 1630). The new types extend that. They name no vendor, standard or platform, and every name in them is a string the declaring crate supplies.

```rust
/// What one consumer a builder supplies needs before it acts.
pub struct ConsumerNeeds {
    pub consumer: Consumer,
    /// The settings the consumer reads, each required or optional.
    pub settings: &'static [SettingsNeed],
    /// What places the consumer in a deployment.
    pub placed_by: Placement,
    /// The adapter parts the consumer cannot act without.
    pub parts: &'static [AdapterPart],
}

pub enum Consumer {
    /// A middleware, by the name an entry writes.
    Middleware(&'static str),
    /// A module a section selects, by its full name.
    Module(&'static str),
    /// A request filter or a page request preparer that only feeds the
    /// named middleware, so it runs only where that middleware is placed.
    Feeds(&'static str),
}

pub struct SettingsNeed {
    /// The table, written as in the document, such as `shared.<vendor>`.
    pub table: &'static str,
    /// Keys inside the table the consumer cannot act without.
    pub keys: &'static [&'static str],
    pub required: bool,
}

pub enum Placement {
    /// An entry in the phase, for the media type, and on the path when one
    /// is given.
    Entry {
        phase: MiddlewarePhase,
        media_type: &'static str,
        path: Option<&'static str>,
    },
    /// The section's `module` or `modules` naming the consumer.
    Section(&'static str),
    /// Present whenever its settings are, such as an adapter part.
    WithSettings,
}

/// A capability an adapter may have, named by core in neutral terms.
pub struct AdapterPart(pub &'static str);
```

Core names the parts it knows of as constants, for example running request filters, dispatching asset routes, reading the telemetry sink, setting origin certificate checks and a host geo lookup. A part is a capability and never a platform.

### 2.2 How each crate and each adapter declares

**A vendor crate** declares on its builder, next to the declarations a builder already makes (`with_shared_settings`, `with_secret_settings`, `with_middleware` and `with_cache_features`, `mod.rs` lines 655 to 741). A new `with_needs(&'static [ConsumerNeeds])` takes one entry for each consumer the builder supplies. The 51Degrees client hints builder (`crates/middleware/51degrees/src/lib.rs` lines 94 to 100) would declare its middleware as needing `[shared.51degrees]` with `endpoint` and `license_key`, placed by a `[[serve]]` entry for `text/html`, and its request filter as `Feeds("51degrees.client_hints")` needing the part that runs request filters. Each vendor declares in its own folder, so core gains no vendor name.

**A middleware or module core supplies** declares in core in the same types. The robots.txt middleware would declare `[robots-txt]` as required and a `[[fetch]]` entry for `text/plain` on `/robots.txt` as its placement, which is what `require_robots_txt_placed` checks by hand today (`settings.rs` lines 4414 to 4462).

**A section core reads directly** declares its consumer as a part. `[[proxy.asset_routes]]` would need the part that dispatches asset routes, `certificate_check = false` the part that sets origin certificate checks, and `[geo] module = "platform"` the host geo lookup. A field kept only so that old documents still load, as IABTechLab/trusted-server#1097 may decide for `cluster_recheck_secs`, is declared as having no consumer, and the report names it as accepted and unused.

**An adapter** declares the parts it has as one constant list in its own crate, which it hands to the registry build beside its builders. The registry build takes builders alone today (`registry.rs` lines 2033 to 2037), and each adapter already passes its own list of builders (for example Axum `app.rs` lines 109 to 145, from row 43), so the parts travel the same way.

### 2.3 The one check and the one report

After every builder has registered, and before any check above returns, core goes through the declarations once. A gap is one of four kinds.

1. A settings section that no linked and running consumer declares, or that only consumers nothing places declare.
2. A placed consumer whose required settings are absent, with the table and keys named.
3. A consumer whose settings are present and which nothing places. Its feeding request filters and preparers do not run.
4. A section or a consumer that needs a part the adapter lacks, with the adapter and the part named.

Every gap is collected, and startup refuses with one report in which each line names the section, the consumer, the missing half and what to add. A document with two gaps gets both lines. The existing refusals in section 1 become lines of this report, with their wording kept where it already names what to add.

A request filter or preparer declared with `Feeds` is run only when its middleware is placed. Preparers that sanitize a reserved query or cookie in a deployment that does not run their module are collected for every builder on purpose (`registry.rs` lines 2103 to 2108), so those declare no `Feeds` and keep running.

### 2.4 The same check at validate and at push

`TrustedServerAppConfig` validates through a trait that takes no arguments, so the tool registers the builders it validates against once, before validating (`config.rs` lines 87 to 106). The `ts` tool registers the builders it links (`crates/trusted-server-cli/src/app_config.rs` lines 35 to 81) at the start of every command (`run.rs` line 126). Push would run the check in 2.3 over those builders' declarations, so every gap a stock builder can cause is caught before the document is written to the config store. A selection that names a module the tool does not link is reported as a line saying that the name is not one this tool can check, rather than passed in silence. Adapter parts reach push the same way as the builders, registered once for the adapter the document is pushed for (open question 2).

### 2.5 A consumer that runs on defaults

A consumer whose settings are optional declares `required: false`, and leaving its table out is then not a gap. The 51Degrees client hints middleware reads `[middleware.51degrees.client_hints]` and writes `Critical-CH` unless the table turns it off (`crates/middleware/51degrees/src/lib.rs` lines 191 to 202), so it declares that table optional. A loading cache with no `[cache.<name>]` table loads through no cache at all (`platform/loading_cache.rs` lines 240 to 242 and 269 to 272), so a module that declares a cache says in its declaration whether running with no cache is intended, and the report names a missing cache table only where it is not.

## 3. What is not in this change

- **Unknown keys and integration IDs.** IABTechLab/trusted-server#1201 covers keys a table does not know, which serde refuses or drops before any consumer sees them. This model starts from tables that read correctly.
- **The health route and the startup failure response.** IABTechLab/trusted-server#1142 decides what an adapter answers once startup refuses, and this model only decides what startup refuses.
- **Adding the missing adapter parts.** Dispatching asset routes on every adapter (IABTechLab/trusted-server#1125) or honoring `certificate_check` on Axum (IABTechLab/trusted-server#1132) remain separate changes. This model makes their absence a named gap until then.
- **Deciding the Edge Cookie cluster settings.** IABTechLab/trusted-server#1097 decides whether they gain a reader or leave.

## 4. Acceptance

1. For each row of the cases table in IABTechLab/trusted-server#1403, one test with the settings and no consumer and, where the consumer cannot act without its settings, one test with the consumer and no settings. Each runs through the real settings load and state build of each adapter, as the Axum adapter's seam probe tests already build the adapter's routes from settings (`crates/trusted-server-adapter-axum/tests/seam_probe.rs` line 94), and through the Fastly, Cloudflare and Spin builds, not through the check called directly.
2. Each of those tests is seen to fail with its declaration removed before the test is relied on.
3. A document with two gaps of different kinds gets both lines in one report.
4. `ts config validate`, given the builders the tool links, reports the same lines as startup for every gap a linked builder can cause.
5. A test over every builder each adapter links (for example Axum `app.rs` lines 109 to 145) finds a declaration for each middleware, module, request filter and page request preparer the builder registers, so a new consumer cannot leave one out. `check_described_middleware` (`registry.rs` lines 1591 to 1630) is the model for matching a declaration to what is registered.

## 5. Open questions

1. Should every gap refuse at once, or should the two kinds that only warn today, a middleware no entry names (`registry.rs` lines 1724 to 1731) and a module that runs and is not selected (lines 1199 to 1212), warn for one release first? A deployment that carries `[shared.51degrees]` with no entry starts today and would stop.
2. How does push learn which adapter a document is for, and where does push read that adapter's parts from, when the `ts` tool does not link the adapter crates?
3. Should a selection naming a module the tool does not link fail push, or be reported and allowed, as section 2.4 proposes?
4. Should the InMobi and ads.txt sellers placements carry a path, as robots.txt does, or should all three count any entry for the media type?
5. Should the Edge Cookie cluster settings be declared as having no consumer, or wait for the decision in IABTechLab/trusted-server#1097?

## Review log

No review rounds yet.

## Revision record

| Date       | Change                                                                                                            |
| ---------- | ----------------------------------------------------------------------------------------------------------------- |
| 2026-10-16 | First draft, from the code at `3aaf720c0` and the issue that states the problem (IABTechLab/trusted-server#1403). |
