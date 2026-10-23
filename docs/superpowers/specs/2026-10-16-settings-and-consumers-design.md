# Design Spec: Every settings section has a consumer, and every consumer has its settings

**Status:** Proposed, 2026-10-16
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-page-middleware-design.md` (row 10, entries that name middleware), `2026-10-12-analytics-module-design.md` (row 16, telemetry as a module `[analytics]` selects), `2026-10-12-inmobi-choice-module-design.md` (row 18, the InMobi consent prompt), `2026-10-12-ads-txt-sellers-design.md` (row 20, the ads.txt sellers), `2026-10-12-robots-txt-middleware-design.md` (row 25, robots.txt as a middleware), `2026-10-12-shared-vendor-tables-design.md` (row 26), `2026-10-15-module-cache-declarations-design.md` (row 32), `2026-10-13-middleware-settings-design.md` (row 38), `2026-10-14-no-modules-crate-design.md` (row 43, each adapter lists the modules it links) and `2026-10-14-51degrees-client-hints-design.md` (row 44, the 51Degrees client hints), all in the pull request that holds every design document of the stack (IABTechLab/trusted-server#1342)
**Related PRs:** none yet
**Issues:** Settings without a consumer and consumers without settings (IABTechLab/trusted-server#1403), whose body lists the related issues of the same shape
**Last updated:** 2026-10-16

> **Why this spec exists.** A settings section and the consumer that acts on it are checked against each other by a dozen separate checks that run at different times and stop at the first gap, and several pairs are not checked at all, so this spec proposes one declaration model in core and one check that reports every gap at startup and at push.

Line references are at `3aaf720c0`, the head of `split/50-cookies-by-path`, which is the head of the cookies by path change (IABTechLab/trusted-server#1400) and the top of the stack. A file named without a folder is in `crates/trusted-server-core/src`, and `registry.rs` and `mod.rs` are `integrations/registry.rs` and `integrations/mod.rs`. A file named with an adapter is in that adapter's `src` folder, so Fastly `app.rs` is `crates/trusted-server-adapter-fastly/src/app.rs`. `edgezero.toml`, `trusted-server.example.toml` and the workspace `Cargo.toml` are at the repository root, and every other file is named in full.

## 1. Problem

Four words are used as the code uses them. A builder (`IntegrationBuilder`, `mod.rs` line 452) is what a crate hands to core to say what the crate offers. The registry is what core builds at startup from every builder (`IntegrationRegistry::with_plan_and_registrations`, `registry.rs` lines 2033 to 2400). Startup is an adapter loading the settings and building its state, which each adapter does through that function (Fastly `app.rs` line 360, Axum `app.rs` line 240, Cloudflare `app.rs` line 295, Spin `app.rs` line 219). Push is `ts config validate`, `ts config diff` and `ts config push`, which validate through `validate_settings_for_deploy_with` (`config.rs` lines 311 to 341).

A consumer is whatever acts on a settings section, being a middleware an entry names, a module a section selects, a request filter, a page request preparer, code in core or a part of the adapter. Many features need both halves, and today the two halves are matched in four different ways.

**Checks in the registry, at startup only.** Push builds no registry. `validate_phase_entries` checks the shape of each entry and says that whether a name is supplied "is only knowable where the registry is built" (`settings.rs` lines 4776 to 4791), and the section selection check is left out of push deliberately, "because a vendor crate the CLI never links may supply the name" (`registry.rs` lines 2163 to 2168). The registry's checks are these.

- An entry naming a middleware no running module supplies is refused (`registry.rs` lines 1669 to 1685). The note that follows names a module no section selects (lines 1118 to 1134) and never names an absent shared table.
- A `[middleware.<name>]` table at a name no running module supplies is refused (lines 1702 to 1723).
- A middleware no entry names is a warning, that it "changes no page" (lines 1724 to 1731).
- A `[shared.<vendor>]` table no builder reads is refused (lines 2044 to 2069).
- A section selecting a module no builder supplies is refused, with a note naming absent shared tables (lines 1774 to 1825 and 1162 to 1178).
- A geo, Edge Cookie, device, prefix or analytics module on offer that its section does not select is a warning (lines 1199 to 1212, which the prefix selection calls at line 1400 as the other four do).

**Checks a feature wrote for itself.** The InMobi prompt refuses `[cmp]` selecting it with no `text/html` entry naming it, whatever the entry's `path` (`crates/cmp/inmobi/src/lib.rs` lines 141 to 158). The ads.txt sellers do the same for `text/plain` (`crates/ads-txt/iabtl/src/lib.rs` lines 152 to 167). Core's robots.txt check requires `path = "/robots.txt"` exactly (`settings.rs` lines 4414 to 4462). Each of the three runs at push and at startup, because a builder's validate function runs at both (`config.rs` lines 394 to 406) and `prepare_runtime` runs at both (`config.rs` line 323 at push, and `settings.rs` line 4290 as the settings load). Core's cache table check runs at both as well (`mod.rs` lines 1008 to 1025, called from `config.rs` line 330 at push and from `mod.rs` line 1041 as an adapter builds its state).

**No check.** These pairs are not matched at all.

- The 51Degrees client hints crate registers a request filter and a middleware whenever `[shared.51degrees]` is present (`crates/middleware/51degrees/src/lib.rs` lines 110 to 132). Core runs every registered filter (`registry.rs` lines 2640 to 2674), so on Fastly, with no entry naming the middleware, the filter still asks the service on every page request (`crates/middleware/51degrees/src/lib.rs` lines 352 to 381).
- The DataDome page tag writes nothing when `client_side_key` is blank (`crates/bot-protection/datadome/src/lib.rs` lines 1016 to 1026), and the startup checks look only at the tag's URL (lines 509 to 511), so an entry naming `bot-protection.datadome.tag` with no key is accepted silently.
- Only the Fastly adapter runs request filters (Fastly `app.rs` lines 836 to 867), dispatches asset routes (lines 1191 to 1194 and 1310 to 1313), reads the analytics sink (lines 389 to 392) and configures origin certificates from `certificate_check` (Fastly `backend.rs` lines 492 to 496). The Axum adapter's origin client takes no certificate setting (Axum `platform.rs` lines 321 to 333), and the Cloudflare and Spin adapters use the setting only in a backend's name (Cloudflare `platform.rs` lines 70 to 78, Spin `platform.rs` lines 92 to 100). With `[geo] module = "platform"` the registry leaves the adapter's own lookup in place (`registry.rs` lines 1480 to 1482), and the Axum and Spin lookups answer no location for every request (Axum `platform.rs` lines 210 to 218, Spin `platform.rs` lines 809 to 817). With `[device] module = "platform"` the registry supplies no module either (`registry.rs` line 1323), and only the Fastly adapter builds a host device module (Fastly `lib.rs` lines 1256 to 1263), from the one host device crate, which only the Fastly adapter links (`crates/trusted-server-adapter-fastly/Cargo.toml` line 65, `crates/device/fastly/src/lib.rs` line 69). Each of these is accepted on the adapters without the part, with no log.
- `cluster_trust_threshold` and `cluster_recheck_secs` in `[ec]` are read into the settings (`settings.rs` lines 783 to 795) and read by no crate, which the issue on the two unread Edge Cookie settings records (IABTechLab/trusted-server#1097).

**One gap at a time.** The first check that fails stops the start. Most checks also stop at their first gap (for example `registry.rs` lines 1679, 1716 and 1815), and the shared table check, which names every unread table in one error (`registry.rs` lines 2046 to 2066), still stops the start before any later check runs. So a document with two gaps of different kinds fails twice before it starts.

**One-off notices.** Where a setting is known to do nothing, the code that knows says so in its own way, as the DataDome warning for its reserved GraphQL flag does (`crates/bot-protection/datadome/src/lib.rs` lines 513 to 515). The Fastly warning when `certificate_check` is off (Fastly `app.rs` lines 484 to 490) is not one of these, because Fastly honors the setting (Fastly `backend.rs` lines 492 to 496) and the warning says that certificate checks are off.

## 2. Design

One model in core, in four points.

1. Every consumer declares the settings it needs.
2. Every settings section has a consumer.
3. Each adapter declares the parts it has.
4. Startup, `ts config validate` and `ts config push` give one report listing every gap.

### 2.1 The declaration types in core

Core already holds one declaration a builder makes about a consumer, `SuppliedMiddleware`, which names a middleware, its phase and its media type (`mod.rs` lines 376 to 385), and the registry already checks that declaration against the middleware the builder really supplies (`check_described_middleware`, `registry.rs` lines 1591 to 1630). The new types extend that. They name no vendor, standard or platform. Every name in them is a string the declaring crate supplies, apart from the adapter parts, which core names by what each part does.

```rust
/// One consumer a builder or core supplies, and what it needs before it acts.
pub struct ConsumerNeeds {
    pub consumer: Consumer,
    /// Tables the consumer reads, whatever they hold, besides the tables its
    /// builder's own declarations name.
    pub reads: &'static [&'static str],
    /// What the settings, as written, have to say for the consumer to run,
    /// besides the selection or the shared table that runs its builder.
    pub runs_when: &'static [SettingsNeed],
    /// What places the consumer in a deployment.
    pub placed_by: Placement,
    /// The adapter parts the consumer cannot act without.
    pub parts: &'static [AdapterPart],
}

pub enum Consumer {
    /// A middleware, by the name an entry writes.
    Middleware(&'static str),
    /// A module, by the name a section selects it by.
    Module(&'static str),
    /// A request filter, by the name it is registered with.
    RequestFilter(&'static str),
    /// A page request preparer, by the name it is registered with.
    Preparer(&'static str),
    /// Code in core that acts on a section of core's own, by the section.
    Core(&'static str),
    /// A part of the adapter that acts on a setting core reads.
    AdapterPart(AdapterPart),
    /// Nothing. Its `runs_when` names a setting that is read into the
    /// settings and acted on by no code, and is kept so that documents
    /// carrying it still load.
    Nothing,
}

/// What one table, as written, has to say.
pub struct SettingsNeed {
    /// The table, written as in the document, such as `shared.<vendor>`,
    /// `middleware.<name>` or `proxy`.
    pub table: &'static str,
    pub condition: Condition,
}

pub enum Condition {
    /// The table is written, or for an array of tables, at least one is.
    TableWritten,
    /// The key is written, whatever its value.
    KeyWritten(&'static str),
    /// The key is written with this value.
    KeyIs(&'static str, Literal),
    /// The key is absent, or written with another value.
    KeyIsNot(&'static str, Literal),
    /// The key is written and is not blank.
    KeyNotBlank(&'static str),
}

pub enum Literal {
    Bool(bool),
    Str(&'static str),
}

pub enum Placement {
    /// An entry naming the middleware, for the media type its description
    /// gives, in `phase` when one is given or else in any phase the
    /// middleware runs in, and with this `path` when one is given. An entry
    /// naming it for another media type or path is refused.
    Entry {
        phase: Option<MiddlewarePhase>,
        path: Option<&'static str>,
    },
    /// The section that selects the module.
    Section,
    /// Wherever the named middleware is placed, for a request filter or a
    /// preparer that only feeds it.
    Feeds(&'static str),
    /// Wherever the consumer runs, for one nothing else places, such as a
    /// request filter that feeds no middleware, code in core or an adapter
    /// part.
    WhenRunning,
    /// Wherever its crate is linked, whatever the settings select, for a
    /// preparer that strips its module's reserved query or cookie.
    WhenLinked,
}

/// A part an adapter may have, named by what it does and never by a
/// platform.
pub enum AdapterPart {
    /// Runs the registered request filters before routing.
    RequestFilters,
    /// Dispatches `[[proxy.asset_routes]]`.
    AssetRoutes,
    /// Hands an auction's rows to the sink `[analytics]` selects.
    TelemetrySink,
    /// Sets its origin client's certificate checks from `certificate_check`.
    OriginCertificateSetting,
    /// Answers `[geo] module = "platform"` with a host geo lookup.
    HostGeoLookup,
    /// Answers `[device] module = "platform"` with a host device module.
    HostDeviceModule,
}
```

An `Entry` placement takes the media type from the middleware's description, so the two cannot disagree. A builder's middleware is described by its `SuppliedMiddleware` (`mod.rs` lines 376 to 385). Core's own middleware, which no builder supplies and which the registry adds itself (`registry.rs` lines 2376 to 2392), is described in `supplied_middleware` (`mod.rs` lines 939 to 952). An entry has a phase of its own because a middleware that runs in both phases may still have to be placed in one, as a middleware that makes a file the origin lacks has to be, since only a `[[fetch]]` entry makes one (`settings.rs` lines 4408 to 4413).

Reading and running are declared apart. A consumer reads a table whatever the table's keys hold, and a key's value only decides, through `runs_when`, whether the consumer runs. So `certificate_check = true`, the default (`trusted-server.example.toml` lines 436 to 438), is a key in a table core reads, and it runs no adapter part. A module reads its own table, a builder its shared table and a module the tables of the caches it declares, as `with_module_name`, `with_shared_settings` and `with_cache_features` already say, and core reads its own sections. So `reads` lists only other tables, such as a middleware's `[middleware.<name>]`.

A condition is data rather than a function such as `ModuleSecretSetting::in_use` (`mod.rs` lines 359 to 367), so a line of the report can say what the setting has to be. Like `in_use`, it is judged on the table as written (2.3).

A consumer is matched to what a builder registers by its name. A request filter and a preparer have no name today, because a filter carries only its integration id (`registry.rs` lines 532 to 541), which every filter of one builder shares, and a preparer is a bare function (`mod.rs` lines 323 to 324). So `with_request_filter` (`registry.rs` lines 753 to 756) and `with_request_preparer` (`mod.rs` lines 597 to 600) each take the name the declaration uses, beside the filter or the function.

### 2.2 How each crate and each adapter declares

**A vendor crate** declares on its builder, next to the declarations a builder already makes (`with_shared_settings`, `with_secret_settings`, `with_middleware` and `with_cache_features`, `mod.rs` lines 655 to 741). A new `with_needs(&'static [ConsumerNeeds])` takes one entry for each middleware, request filter and preparer the builder supplies. A module gets an entry only when it runs under a condition of its own, reads a table besides its own or needs a part, because `with_module_name` already declares the module, and with it the module's table and the section that selects it (`mod.rs` lines 684 to 698 and 772 to 789). Each vendor declares in its own folder, so core gains no vendor name.

**Core** declares its own consumers in the same types. Its robots.txt middleware is one. A section that core reads and the adapter acts on is declared too. Its consumer is code in core where core acts on the section as well. Core checks the asset routes as the settings load and matches a request's path to one (`settings.rs` line 2803), and the adapter then dispatches the request. Its consumer is an adapter part where core only carries the value, as for `certificate_check`. A setting that nothing reads, kept so that old documents still load, is declared with `Nothing`, and so is a vendor's reserved setting.

The declarations for the cases in the issue this design answers (IABTechLab/trusted-server#1403), with the host device case added, would be these.

| Case                         | Declared in                      | Consumer                                      | Reads                                               | Runs when                                                                                                      | Placement                               | Parts            |
| ---------------------------- | -------------------------------- | --------------------------------------------- | --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | --------------------------------------- | ---------------- |
| 51Degrees client hints       | `crates/middleware/51degrees`    | `Middleware("51degrees.client_hints")`        | `middleware.51degrees.client_hints` (2.5)           | `endpoint` and `license_key` in `shared.51degrees` not blank                                                   | `Entry`                                 | none             |
| 51Degrees client hints       | `crates/middleware/51degrees`    | `RequestFilter("51degrees.client_hints")`     | none                                                | no condition of its own                                                                                        | `Feeds("51degrees.client_hints")`       | `RequestFilters` |
| DataDome protection          | `crates/bot-protection/datadome` | `RequestFilter("bot-protection.datadome")`    | none                                                | `enable_protection = true` in `bot-protection.datadome`                                                        | `WhenRunning`                           | `RequestFilters` |
| DataDome page tag            | `crates/bot-protection/datadome` | `Middleware("bot-protection.datadome.tag")`   | none                                                | `client_side_key` in `bot-protection.datadome` not blank, and `inject_client_side_tag` there not `false` (2.5) | `Entry`                                 | none             |
| DataDome GraphQL flag        | `crates/bot-protection/datadome` | `Nothing`                                     | none                                                | `enable_graphql_support = true` in `bot-protection.datadome`                                                   | not used                                | none             |
| InMobi consent prompt        | `crates/cmp/inmobi`              | `Middleware("cmp.inmobi")`                    | none                                                | `script_url` in `cmp.inmobi` not blank                                                                         | `Entry`                                 | none             |
| ads.txt sellers              | `crates/ads-txt/iabtl`           | `Middleware("ads-txt.iabtl")`                 | none                                                | `lines` in `ads-txt.iabtl` written                                                                             | `Entry` (open question 3)               | none             |
| robots.txt                   | core                             | `Middleware("robots_txt")`                    | none                                                | `robots-txt` written                                                                                           | `Entry` in `[[fetch]]` on `/robots.txt` | none             |
| Middleware settings          | the middleware's crate           | that middleware                               | `middleware.<name>`                                 | that middleware's                                                                                              | that middleware's                       | none             |
| Shared vendor tables         | the vendor's crate               | each consumer the vendor's builder supplies   | `shared.<vendor>`, through `with_shared_settings`   | what each consumer needs in `shared.<vendor>`                                                                  | that consumer's                         | that consumer's  |
| Section selections           | the module's crate               | the module, declared by `with_module_name`    | its own table, through `with_module_name`           | no condition of its own                                                                                        | `Section`                               | none             |
| Loading cache tables         | the module's crate               | the module, declared by `with_cache_features` | `cache.<name>`, through `with_cache_features` (2.5) | the module's                                                                                                   | the module's                            | none             |
| Auction analytics            | `crates/analytics/tinybird`      | `Module("analytics.tinybird")`                | its own table, through `with_module_name`           | no condition of its own                                                                                        | `Section`                               | `TelemetrySink`  |
| Asset routes                 | core                             | `Core("proxy.asset_routes")`                  | none                                                | `proxy.asset_routes` written                                                                                   | `WhenRunning`                           | `AssetRoutes`    |
| Origin certificate check     | core                             | `AdapterPart(OriginCertificateSetting)`       | none                                                | `certificate_check = false` in `proxy`                                                                         | `WhenRunning`                           | its own          |
| Host geo lookup              | core                             | `AdapterPart(HostGeoLookup)`                  | none                                                | `module = "platform"` in `geo`                                                                                 | `WhenRunning`                           | its own          |
| Host device module           | core                             | `AdapterPart(HostDeviceModule)`               | none                                                | `module = "platform"` in `device`                                                                              | `WhenRunning`                           | its own          |
| Edge Cookie cluster settings | core                             | `Nothing`, one for each setting               | none                                                | `cluster_trust_threshold` or `cluster_recheck_secs` written in `ec`, one in each declaration                   | not used                                | none             |

Core's sections need no `reads`, because core always reads them. Under these declarations a setting written at its default, or a switch written off, gives no line. `certificate_check = true` runs no adapter part, `enable_protection = false` (`trusted-server.example.toml` line 936) registers no filter, and `inject_client_side_tag = true` (line 945) lets the tag run. A DataDome tag entry with a blank `client_side_key`, or with `inject_client_side_tag = false`, gets one line, that the tag is placed and does not run (2.3), naming the key.

A builder declares every middleware, request filter and preparer it supplies, not only those in the table. The Google ad tag diagnostics builder, for example, declares its preparer as `Preparer("ad-tag.google.diagnostics")` placed by `WhenLinked` (`crates/ad-tag/google/src/diagnostics.rs` lines 46 to 58), because the preparer removes the reserved `ts_console` query and cookie whether or not the module runs (lines 317 to 320).

**An adapter** declares the parts it has in `parts.toml` in its own crate, by the names core gives them in lower case with hyphens, for example `parts = ["host-geo-lookup"]`. The adapter reads the file as it is built (`include_str!`) and hands the parts to the check with its builders, before it builds the registry. The `ts` tool reads the same file at push (2.4), so startup and push cannot disagree about an adapter. The list cannot go in the adapter's block in `edgezero.toml`, because EdgeZero refuses a table it does not know in that block and a key it does not know in the block's `adapter` table (`crates/edgezero-core/src/manifest.rs` lines 814 to 861 in EdgeZero at `683202c66`, the revision the workspace `Cargo.toml` pins at lines 94 to 99), and the block's other tables hold EdgeZero's own build, command and logging settings. A list of adapters and their parts kept in core or in the tool would name every platform and repeat what each adapter declares. At `3aaf720c0` the four files would hold these parts.

| Part                       | Fastly                                            | Axum                               | Cloudflare                          | Spin                               |
| -------------------------- | ------------------------------------------------- | ---------------------------------- | ----------------------------------- | ---------------------------------- |
| `RequestFilters`           | yes, `app.rs` lines 836 to 867                    | no                                 | no                                  | no                                 |
| `AssetRoutes`              | yes, `app.rs` lines 1191 to 1194 and 1310 to 1313 | no                                 | no                                  | no                                 |
| `TelemetrySink`            | yes, `app.rs` lines 389 to 392                    | no                                 | no                                  | no                                 |
| `OriginCertificateSetting` | yes, `backend.rs` lines 492 to 496                | no, `platform.rs` lines 321 to 333 | no, `platform.rs` lines 70 to 78    | no, `platform.rs` lines 92 to 100  |
| `HostGeoLookup`            | yes, `app.rs` line 621                            | no, `platform.rs` lines 210 to 218 | yes, `platform.rs` lines 724 to 733 | no, `platform.rs` lines 809 to 817 |
| `HostDeviceModule`         | yes, `lib.rs` lines 1256 to 1263                  | no                                 | no                                  | no                                 |

### 2.3 The one check and the one report

The check runs once, before any builder builds. Every declaration is static and the check reads the document as written, so it needs nothing a build makes. Run after the builds, it would come too late, because the shared table refusal runs before any builder (`registry.rs` lines 2044 to 2069, ahead of the loop at line 2077) and a build can refuse first (line 2152, as the InMobi build does at `crates/cmp/inmobi/src/lib.rs` line 190).

The check reads each table as the document writes it, before serde fills a default, so a default can neither hide a missing key nor make an unwritten one look written. Module, shared and middleware tables are kept as written already, which is how `Settings::module_config` reads a module's table (`settings.rs` lines 4653 to 4680). Core's typed sections and a vendor's typed table fill defaults, for example for the two Edge Cookie cluster settings (`settings.rs` lines 786 and 794) and for DataDome's `client_side_key` (`crates/bot-protection/datadome/src/lib.rs` lines 342 to 343 and 456), so the load keeps the document's own tables beside the typed settings for the check to read.

A consumer runs when its builder runs, which is when a section selects its module, when its shared table is written, or always for core's own, and when every condition in its `runs_when` holds. It is placed as its placement says, in the deployment as a whole, so a middleware is placed when any entry of the document names it, whatever the path of a given request. A gap is one of five kinds, and each line names the setting, the consumer, the missing half and what to add.

1. A written table that no consumer reads, or that only consumers whose builders do not run read, and an entry or a selection that names nothing a linked crate declares, or that names a middleware for a media type or path its placement does not allow. A module's table that its section does not select is one, which core refuses today (`provider_table.rs` lines 564 to 567). This kind looks at tables and names and never at a single key, so a key whose value fails a condition only decides whether a consumer runs.
2. A placed consumer that does not run, naming the selection, shared table or condition it lacks.
3. A consumer that a written setting runs, being its builder's selection or shared table or a condition of its own, and that nothing places. A consumer that nothing written runs, such as core's service version middleware, is never one, as today (`registry.rs` lines 1735 to 1746).
4. A placed consumer that runs and needs a part the adapter lacks, naming the adapter and the part. An `AdapterPart` consumer needs its own part.
5. A setting that a `Nothing` declaration names, written so that the declaration's `runs_when` holds, which the line names as accepted and unused. A switch written off gives no line, as for any other consumer.

A request filter or preparer placed by `Feeds` gets no line of kind 2 or 3 of its own, because the line for its middleware covers it, so one state gives one line.

Each line has a level. Kinds 1, 2 and 4 refuse. Kind 3 refuses when the consumer runs under a condition of its own, or a table in its `reads` is written, because that setting then does nothing, as the InMobi, ads.txt and robots.txt checks refuse today. It warns when only its builder's selection or shared table runs it, as the warnings for a middleware no entry names and for a module on offer that its section does not select do today. Kind 5 warns, and so do the lines in 2.4 for a name or for parts that the tool cannot check at push. Startup and push refuse when any line refuses, and give every line either way, so a document with two gaps gets both lines. The existing refusals and warnings in section 1 become lines of this report, with their wording kept where it already names what to add. The registry's checks after the build stay only to catch a registration that contradicts its builder's declarations, as `check_described_middleware` does today.

A request filter or preparer placed by `Feeds` is kept only where its middleware is placed, which the registry decides once as it is built, so it runs on every request wherever its middleware is placed, as filters run today. A preparer placed by `WhenLinked` is kept for every linked builder, as every preparer is collected today (`registry.rs` lines 2103 to 2108).

### 2.4 The same check at validate and at push

`TrustedServerAppConfig` validates through a trait that takes no arguments, so the tool registers the builders it validates against once, before validating (`config.rs` lines 87 to 106). The `ts` tool registers the builders it links (`crates/trusted-server-cli/src/app_config.rs` lines 35 to 81) at the start of every command (`crates/trusted-server-cli/src/run.rs` line 126). Push would run the check in 2.3 over those builders' declarations, so every gap a linked builder can cause is caught before the document is written to the config store. A kind 1 line for a name that no crate the tool links declares, being a selection, an entry, a shared table, a middleware table or a cache table, warns at push and says that the tool cannot check the name, because a vendor crate the tool never links may supply it. That is why push leaves a selection unchecked on purpose today (`registry.rs` lines 2163 to 2168). The cache table check refuses such a name at push today (`config.rs` line 330), so this would change it (open question 2).

The parts reach push from the adapter's own `parts.toml` (2.2). `ts config push` and `ts config diff` already take `--adapter` (the tool's tests parse it at `crates/trusted-server-cli/src/run.rs` lines 626 to 647), and `ts config validate` takes none (lines 785 to 799), so the tool adds an optional `--adapter` to `ts config validate`, beside the arguments it takes from EdgeZero (lines 4 to 7 and 75 to 76). Given an adapter, the tool finds the adapter's block in `edgezero.toml` at the repository root through the manifest loader it already uses (`crates/trusted-server-cli/src/app_config.rs` line 12), reads `parts.toml` in the folder the block's `crate` key names (for example `edgezero.toml` line 56 for Axum), and registers the parts as it registers the builders, before EdgeZero validates. Neither core nor the tool names a platform, because the adapter's name comes from the command line and its folder from the manifest. With no adapter given, `ts config validate` checks everything but the parts, and a line that warns says so. A block with no `crate` key, or a folder with no `parts.toml`, gets the same warning, so the parts are never passed over in silence.

### 2.5 A consumer that runs on defaults

A table a consumer only reads goes in its `reads` and not in its `runs_when`, so leaving the table out is not a gap. The 51Degrees client hints middleware reads `[middleware.51degrees.client_hints]` and writes `Critical-CH` unless the table turns it off (`crates/middleware/51degrees/src/lib.rs` lines 191 to 202), so that table is in its `reads`. A key whose default lets a consumer run is declared with `KeyIsNot`, as the DataDome tag declares `inject_client_side_tag`, which is on unless it is written `false` (`crates/bot-protection/datadome/src/lib.rs` lines 346 to 347, 405 to 407 and 1024). A module's `[cache.<name>]` table is read in the same way, through `with_cache_features`, because a declared cache with no table takes the one loading cache module the build links (`mod.rs` lines 1083 to 1104), and every adapter's stock list links one (for example Axum `app.rs` line 140). Only a build that links no loading cache module, or several, loads a cache with no table through no cache at all (`platform/loading_cache.rs` lines 240 to 242 and 269 to 272). Neither is a stock build, and this model leaves the case as it is.

## 3. What is not in this change

- **Unknown keys and integration IDs.** The issue on unknown integration IDs and fields (IABTechLab/trusted-server#1201) covers keys a table does not know, which serde refuses or drops before any consumer sees them. This model starts from tables that read correctly.
- **The health route and the startup failure response.** The issue on one health and startup failure contract across the adapters (IABTechLab/trusted-server#1142) decides what an adapter answers once startup refuses, and this model only decides what startup refuses.
- **Adding the missing adapter parts.** Dispatching asset routes on every adapter (IABTechLab/trusted-server#1125) or honoring `certificate_check` on Axum (IABTechLab/trusted-server#1132) remain separate changes. This model makes their absence a named gap until then, and an adapter that gains a part adds it to its `parts.toml`.
- **Deciding the Edge Cookie cluster settings.** This model declares them read by nothing, which is true today, and the issue on the two unread settings (IABTechLab/trusted-server#1097) decides whether they gain a reader, which then replaces the declaration, or leave with it.
- **Running a request filter only on the paths its middleware covers.** A filter that feeds a middleware runs on every request wherever the middleware is placed, as filters run today.
- **The Fastly warning when `certificate_check` is off.** It warns that certificate checks are off on an adapter that honors the setting (section 1), so it is not a notice that the setting does nothing, and it stays.

## 4. Acceptance

1. For each row of the cases table in the issue this design answers (IABTechLab/trusted-server#1403), and for the host device case in section 1, one test with the settings and no consumer and, where the consumer cannot act without its settings, one test with the consumer and no settings. Each runs through the real settings load and state build of each adapter, as the Axum adapter's tests already build the adapter's routes from settings (`crates/trusted-server-adapter-axum/tests/seam_probe.rs` line 94), and through the Fastly, Cloudflare and Spin builds, not through the check called directly.
2. Each of those tests is seen to fail with its declaration removed before the test is relied on.
3. A document with two gaps of different kinds gets both lines in one report, and a document whose only lines warn starts and is pushed. A document that writes settings at their defaults, such as `certificate_check = true`, `enable_protection = false` and `inject_client_side_tag = true`, gets no line for them.
4. `ts config validate --adapter <name>` and `ts config push --adapter <name>`, given the builders the tool links and the parts in that adapter's `parts.toml`, report the same lines as that adapter's startup for every gap a linked builder or the adapter's parts can cause. `ts config validate` with no adapter reports every other gap, with one line that warns that the parts were not checked.
5. A test over every builder each adapter links (for example Axum `app.rs` lines 109 to 145) checks both ways. Every middleware, module, request filter and page request preparer a builder registers has a declaration of the same name, and every declaration is registered by its builder when the builder runs and the declaration's `runs_when` holds, so a declaration cannot outlive its consumer. `check_described_middleware` (`registry.rs` lines 1591 to 1630) is the model only for matching by name, because it checks a registered middleware against its description and skips a described one that the registration leaves out (lines 1596 to 1603).
6. On each adapter, for each part its `parts.toml` names, a test sees the part act, and for each part the file does not name, a test sees a document that needs the part refused, so the file cannot claim a part the adapter lacks.

## 5. Open questions

1. Should the lines that refuse under this model, where today there is only a warning or nothing at all, warn for one release first? They are the kind 3 lines for a consumer that runs under a condition of its own or has a table in its `reads` written, which today get the warning for a middleware no entry names (`registry.rs` lines 1724 to 1731), the kind 1 lines for a table that is accepted today although no running consumer reads it, such as a `[middleware.<name>]` table at a middleware that reads its module's table instead (`settings.rs` lines 4696 to 4698) or a `[cache.<name>]` table whose module does not run, and the kind 2 and kind 4 lines for gaps that are accepted today with no log. A deployment that carries `[shared.51degrees]` with no entry starts today and would stop, and so would one on Axum with `[geo] module = "platform"`.
2. Should a name that no crate the tool links declares refuse at push, as the cache table check does today (`config.rs` line 330), or warn, as section 2.4 proposes and as push leaves a selection unchecked on purpose today (`registry.rs` lines 2163 to 2168)? Refusing catches a misspelt name before the document is written, and warning lets a deployment push, with the stock tool, names that its own crates supply.
3. Should the ads.txt sellers' placement require `path = "/ads.txt"`, as the robots.txt placement requires its path? An entry with no path appends the lines to every `text/plain` file it covers (`crates/ads-txt/iabtl/src/lib.rs` lines 254 to 262), and the module's check counts any `text/plain` entry today (lines 152 to 167), so the declaration keeps that placement until this is decided.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 2026-10-16 | First draft, from the code at `3aaf720c0` and the issue that states the problem (IABTechLab/trusted-server#1403).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 2026-10-16 | Round 1 fixes. The types gain consumers for code in core, adapter parts and settings nothing reads, conditions on a key's value, named filters and preparers, and placements for a consumer nothing else places and for a preparer that runs whenever its crate is linked. Each adapter declares its parts in `parts.toml`, which push, diff and a new `ts config validate --adapter` read. Lines have levels, the check runs before any build on the settings as written, the host device case is added, and the claims on the push-time checks, the loading cache default and the one-off notices are corrected. |
| 2026-10-16 | Round 2 fixes. A consumer declares the tables it reads apart from the conditions it runs under, and the first kind of gap looks only at tables and names, so a default such as `certificate_check = true` is no longer refused. `KeyIsNot` holds for a key that is absent or has another value, core's own middleware takes its media type from `supplied_middleware`, the doc comment on `SettingsNeed` names no vendor, and the files at the repository root are named as such.                                                                                                                                  |
| 2026-10-16 | Round 3 fix. The fifth kind of gap applies only when the declaration's run conditions hold.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |

## Review log

1. **Round 1, 2026-10-16.** A review in a fresh session, against `split/50-cookies-by-path` at `3aaf720c0`. Issues found, two of them blocking. The declaration types could not say that a consumer is code in core, an adapter part, a request filter that feeds no middleware or a preparer that runs without its module, nor that a setting must have a given value or must not be blank, and push and validate had no way to see the parts of the adapter a document is for. This revision gives the types those consumers and conditions, has each adapter declare its parts in a `parts.toml` in its own crate that the tool reads for the adapter `--adapter` names, and applies the other findings.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/50-cookies-by-path` at `3aaf720c0`. Issues found, two of them blocking. A written key whose value failed a consumer's condition counted as a setting no running consumer declares, so defaults such as `certificate_check = true` and `enable_protection = false` were refused, and the doc comment on a core type named two vendors. This revision declares the tables a consumer reads apart from the conditions it runs under, so that the first kind of gap looks only at tables and names, adds a condition for a key that is absent or has another value, takes the media type of core's own middleware from `supplied_middleware`, places the files at the repository root, and gives the doc comment neutral examples.

3. **Round 3, 2026-10-16.** A short check of the round 2 fixes in a fresh session, against `split/50-cookies-by-path` at `3aaf720c0`. All six were fixed truthfully and nothing blocking remains. This revision makes the fifth kind of gap apply only when the declaration's run conditions hold, so a switch written off gives no line.
