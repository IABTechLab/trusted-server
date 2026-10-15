# Design Spec: A module declares the caches it loads through

**Status:** Proposed, 2026-10-15. Written after the code, for draft pull request IABTechLab/trusted-server#1373. Every design document of the stack is in IABTechLab/trusted-server#1342.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-loading-cache-design.md`, `2026-10-12-robots-txt-from-modules-design.md`, `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** IABTechLab/trusted-server#1373, reviewed on jwrosewell/trusted-server#27
**Issues:** IABTechLab/trusted-server#1320
**Last updated:** 2026-10-15

> **Why this spec exists.** A module that holds its own answers asks core for the cache of the name it chose, finds no `[cache.<name>]` table for that name, and gets the cache that holds nothing, so every request loads the answer again.

## 1. Problem

A feature that holds answers in a loading cache selects the module that holds them in its own `[cache.<name>]` table, as `2026-10-12-loading-cache-design.md` describes. Before this change core read one such table. `CacheSettings::loading_caches` in `crates/trusted-server-core/src/settings.rs` returned an array of one entry, `robots-txt`, and `CacheSettings` refused every other key under `[cache]`.

A module outside core that holds its own answers, such as one service answer that every page of a deployment asks for, therefore had no table. The module asks `RuntimeServices::loading_cache_for` for the cache of the name it chose. `LoadingCaches::for_feature` in `crates/trusted-server-core/src/platform/loading_cache.rs` has no entry for that name and answers `NoLoadingCache`, which holds nothing and runs every load. So every request loaded the answer again. One deployment measured six service calls for each page request where one was needed, with a loading cache module linked.

Two things were missing. A deployment had no place in its document to select a cache for the module's answers. Core also had no way to learn the module's cache names, because core keeps every vendor's names out of its code and so cannot list the module's cache names itself.

## 2. Design

A builder declares the names of the caches its module loads through. Core reads a `[cache.<name>]` table for each declared name, with the same four settings as `[cache.robots-txt]`, and refuses a table whose name no linked module declares. No adapter source changes, because every adapter already passes its builders to `loading_caches_from`.

### 2.1 A builder declares its caches

```rust
impl IntegrationBuilder {
    pub const fn with_cache_features(mut self, features: &'static [&'static str]) -> Self;
    pub const fn cache_features(&self) -> &'static [&'static str];
}
```

Both methods are in `crates/trusted-server-core/src/integrations/mod.rs`. Both constructors of `IntegrationBuilder` start with an empty list, so a builder that declares nothing behaves as before. A fictional module from `crates/geo/example` declares its cache like this.

```rust
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::implementations("geo.example", SOURCE)
        .with_cache_features(&["example-answers"])
}
```

A declared name is the cache's name wherever the cache is used. The name is the `<name>` of the `[cache.<name>]` table. `RuntimeServices::load_cached` finds a cache by the `name` of the `CacheSpec` it is given, so a module that gives its `CacheSpec` the declared name is held by the cache that table selected. The name is also the middle part of every `StoreKey` the module builds with that name. Such a key reads `<publisher prefix>/<name>/<key>`.

`declared_caches(builders)` returns the names a deployment may hold answers in, being core's own `robots-txt` first and then each builder's names in builder order. `declared_caches` refuses two cases, and each refusal names the builders.

1. A builder declares `robots-txt`, which is core's own.
2. Two builders declare one name. The two modules would otherwise hold their answers in one cache under one set of keys without a word.

`declared_caches` compares a name only with the names already declared. The message of each refusal asks a module to name its caches under itself, as modules are named, but the check does not look at the spelling of a name or at whether the name starts with the module's own name.

### 2.2 A deployment selects each cache in its own table

`CacheSettings` gains `declared`, a `BTreeMap<String, LoadingCacheConfig>` read with `#[serde(flatten)]`. Every key under `[cache]` other than `asset_rules` and `robots-txt` is read as a table of the four `LoadingCacheConfig` settings. `CacheSettings::loading_caches` returns a `Vec` of name and settings pairs, being `robots-txt` first, always, and then the declared tables in name order. `loading_caches` returned an array of one entry before.

`LoadingCaches` is keyed by `String`, and `LoadingCaches::insert` takes any `&str` where it took a `&'static str`. `for_feature` and `RuntimeServices::loading_cache_for` keep their signatures, and a name with no entry still answers `NoLoadingCache`.

`loading_caches_from(settings, builders)` makes the caches. The function applies one rule to each table, `robots-txt` and declared tables alike.

1. A `module` that names a loading cache module selects that module.
2. No `module` selects the one loading cache module the build links, or nothing when the build links none or several.
3. A module's cache is made once however many tables select it.

The new step comes after the tables. A declared cache that has no table at all follows the rule for a table with no `module`, and shares the cache already made for that module when a table made one. A stock build therefore holds a module's answers without a line in the document. The table below shows the result for a declared cache named `answers`.

| The document                       | Loading cache modules linked | The `answers` cache                             |
| ---------------------------------- | ---------------------------- | ----------------------------------------------- |
| No `[cache.answers]` table         | One                          | Held by that module                             |
| No `[cache.answers]` table         | None, or two or more         | Holds nothing, and loads every time             |
| `[cache.answers]` with no `module` | One                          | Held by that module                             |
| `[cache.answers]` with no `module` | None, or two or more         | Holds nothing, and loads every time             |
| `[cache.answers] module = "x"`     | `x` is among them            | Held by `x`                                     |
| `[cache.answers] module = "x"`     | `x` is not among them        | Refused when the state is built                 |
| `[cache.answres]`, not declared    | Any                          | Refused by the deploy check and the state build |

```toml
[cache.example-answers]
module = "51degrees"
store_seconds = 3600
```

### 2.3 Configuration, defaults and validation

Each `[cache.<name>]` table takes the four settings that `[cache.robots-txt]` takes.

| Setting              | Default                        | Range                                                     | Meaning                                                                           |
| -------------------- | ------------------------------ | --------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `module`             | Absent                         | The short name or the full name of a loading cache module | The module whose loading cache holds the copies. Absent, the rule in 2.2 applies. |
| `memory_seconds`     | The declaring module's default | 0 to 31536000                                             | How long the brief copy in process memory is used. Zero keeps none.               |
| `store_seconds`      | The declaring module's default | 1 to 31536000                                             | The longest a copy in the shared store is used.                                   |
| `store_idle_seconds` | The declaring module's default | 1 to 31536000                                             | How long a copy in the shared store lasts unused.                                 |

A module supplies its defaults when it calls `LoadingCacheConfig::lifetimes`. For `[cache.robots-txt]` the three defaults are 60, 604800 and 2592000 seconds, as `docs/guide/configuration.md` lists. This change defines no default for a declared cache, because no module in this repository loads through one.

Validation is in three places.

1. `LoadingCacheConfig` keeps `#[serde(deny_unknown_fields)]`, so a table with a setting other than the four is refused when the document is parsed.
2. `CacheSettings::prepare_runtime` runs `LoadingCacheConfig::check` on every declared table, as it does for `cache.robots-txt`. A value out of range is refused naming the table, as in `[cache.answers] store_seconds is 0, and must be from 1 to 31536000`. Loading the document and the deploy check both reach `prepare_runtime`.
3. The name of each table is checked against the declared names, see 2.4.

`CacheSettings` no longer carries `#[serde(deny_unknown_fields)]`, because a key under `[cache]` is now read as a declared table. The refusal of an unknown name moves from parsing to `check_cache_tables`.

### 2.4 Where the names are checked

`check_cache_tables(settings, builders)` calls `declared_caches` and then refuses the first table, in the order `loading_caches` lists them, whose name is not declared. The message names the table and lists the declared names, core's own first.

```text
[cache.answres] names a cache no linked module loads through. The caches this build's modules declare are: robots-txt, answers
```

The check runs in two places.

1. **The deploy check.** `validate_settings_for_deploy_with` in `crates/trusted-server-core/src/config.rs` runs the check after `prepare_runtime`, over core's built-in builders followed by the builders the caller passes. `TrustedServerAppConfig::validate` passes the builders a tool registered with `register_deploy_integrations`. A misspelled table therefore fails `ts config validate` and not the deployment. The `ts` command registers the stock modules through `register_stock_modules` in `crates/trusted-server-cli/src/app_config.rs`, so the command knows the caches the stock modules declare and no others. A deployment whose own module declares a cache validates with a tool that registers that module's builder, as the deployment already must for the module's other settings. `validate_settings_for_deploy` passes no builders, so the function refuses every table except `robots-txt`.
2. **The state build.** `loading_caches_from` runs the check first, so a service started with a misspelled table fails when its state is built and not when a request first needs the cache.

The two lists of builders differ in one respect. The deploy check chains core's built-in builders (`all_builders`) ahead of the registered ones. Each adapter passes `trusted_server_modules::builders_with(integrations)` to `loading_caches_from` and does not chain core's built-in list. The two lists of declared names agree today, because the one built-in builder outside core's tests, `js_asset_proxy`, declares no cache. A built-in builder that did declare one would be seen by the deploy check and not by the adapters.

### 2.5 The adapters

All four adapters make the same call in the function that builds their application state, and none of their source changed.

```rust
let loading_caches =
    trusted_server_core::integrations::loading_caches_from(&settings, integrations)?;
```

- **Fastly.** `build_state_with_registrations` in `crates/trusted-server-adapter-fastly/src/app.rs` makes the call and keeps the result in `AppState::loading_caches`. The adapter hands the result to the `RuntimeServices` builder of each request and to `build_finalize_services`.
- **Axum.** `build_state_with_registrations_and_services` in `crates/trusted-server-adapter-axum/src/app.rs` makes the call and keeps the result in `AppState::loading_caches`. `services_for_request` adds the result to each request's services with `with_loading_caches`.
- **Cloudflare.** `build_state_with_registrations_and_services` in `crates/trusted-server-adapter-cloudflare/src/app.rs` does the same as the Axum function.
- **Spin.** `build_state_with_registrations_and_services` in `crates/trusted-server-adapter-spin/src/app.rs` does the same as the Axum function.

Each adapter passes `trusted_server_modules::builders_with(integrations)`, being the stock modules followed by the deployment's own, so a deployment's own module can declare a cache. The Axum, Cloudflare and Spin adapters accept `RuntimeServices` from the caller. Where a caller supplies `RuntimeServices`, those services carry their own loading caches and `services_for_request` does not add the state's. The Fastly adapter builds its services itself and has no such parameter.

The adapters differ in how a failed state build answers, see 2.7. Only the Axum adapter has a test that asserts on this change, see section 4.

### 2.6 Consent and privacy

This change stores nothing new and reads no request data. The change decides which loading cache module holds a module's copies and how long a copy lasts. Five facts follow.

1. **What core knows about a copy.** Core sees a cache's name, its lifetimes and the key a module builds with `StoreKey::new(prefix, cache, key)`. Core does not read a key or a value, and the change adds no consent check on this path. A module that holds an answer which depends on the visitor has to put what the answer depends on into its key, or not hold the answer. The change gives the module a cache and does not settle that question for the module.
2. **Who can read a copy.** A key starts with the publisher's key prefix and the cache's name, so two publishers sharing a store never share a copy. Because a declared name cannot repeat or be `robots-txt`, two modules of one publisher never share a copy either. Within one publisher, the copy in the shared store is the one every instance reads.
3. **How long a copy lasts.** Each declared table sets its own lifetimes, so a deployment can shorten them. The shortest the document can ask for is no copy in process memory (`memory_seconds = 0`) and one second in the shared store (`store_seconds = 1`).
4. **Whether the document can turn a cache off.** The document cannot turn a declared cache off in a build that links one loading cache module. Such a build holds a declared cache's answers whether or not the document has a table, and a table naming `none`, or any other name that no linked crate offers a cache from, is refused when the state is built.
5. **What changes today.** No module that ships declares a cache, so no deployment holds a new kind of answer because of this change.

### 2.7 Failure handling

| What goes wrong                                                                | Refused by                                | What the operator sees                                                                                                                                        |
| ------------------------------------------------------------------------------ | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A table names a cache no linked module declares                                | The deploy check and the state build      | The message in 2.4, naming the first such table                                                                                                               |
| A builder declares `robots-txt`                                                | The same two, with or without a table     | A message saying the builder declares the cache `robots-txt`, which is core's own, and naming the builder                                                     |
| Two builders declare one name                                                  | The same two, with or without a table     | A message naming both builders and the cache, so linking the two modules fails every deployment that does                                                     |
| A table's `module` names a crate that offers no loading cache                  | The state build only                      | A message naming the table and the module written, saying no linked crate offers a loading cache from it, and listing the loading cache modules the build has |
| A lifetime out of range                                                        | Loading the document and the deploy check | `[cache.answers] store_seconds is 0, and must be from 1 to 31536000`                                                                                          |
| A declared cache with no table, and no or several loading cache modules linked | Nothing                                   | The cache holds nothing and loads every time, with no log line                                                                                                |
| A module loads through a name it did not declare                               | Nothing                                   | `LoadingCaches::for_feature` answers `NoLoadingCache`, with no error                                                                                          |

When the state build fails, each adapter logs the full report and serves its startup error router. The Fastly, Axum and Cloudflare routers answer every path for GET, POST, HEAD, OPTIONS, PUT, PATCH and DELETE with the status and body that `TrustedServerError::Configuration` gives, being 500 and `An internal error occurred`. The Spin router answers every path with 503 and `Service Unavailable`, and still answers `/health` with 200. The names in the message reach the log and the deploy check's output, and never a response.

A load that fails is handled as `2026-10-12-loading-cache-design.md` describes. Every request waiting on the load receives the error, and nothing is stored.

### 2.8 Rolling out to an existing deployment

1. **No document has to change.** `[cache.robots-txt]` is untouched and no other table is needed. A build that links one loading cache module holds a declared cache's answers with no table.
2. **No running deployment gains a cache.** No module that ships declares one, so a deployment holds a new kind of answer only when it links a module that declares a cache.
3. **A document with another `[cache.<name>]` table did not load before.** `CacheSettings` refused the key at parse time, so no running deployment has one. Now the parse accepts the table, and `check_cache_tables` refuses the table at the deploy check and when the state is built unless a linked module declares the name. A tool that parses a document without running the deploy check no longer refuses the table.
4. **A module that adds a declaration changes nothing until a deployment links the module.** A deployment then writes a table only to choose a loading cache module other than the one linked, to set lifetimes other than the module's defaults, or because its build links two or more loading cache modules.
5. **Crates outside core meet three changes.** `CacheSettings::loading_caches` returns a `Vec` where it returned an array, `LoadingCaches::insert` takes `&str` where it took `&'static str`, and `CacheSettings` has a public `declared` field, so code that builds the struct by listing every field needs the new field. No code in this repository builds the struct that way. The changelog entry under Changed names the three and does not mark them as breaking.
6. **The documentation.** `docs/guide/configuration.md` describes the declaration under the loading cache settings, and the guide's example module is fictional.

### 2.9 The seam probe

The seam probe, `crates/testing/seam-probe`, is a fixture crate that uses the extension points core offers to modules, so that tests can drive those extension points from outside core. The stock list does not include the probe. This change gives the probe four things.

1. `builder()` declares `seam-probe-answers`, the constant `SEAM_PROBE_ANSWERS_CACHE`.
2. `loading_cache_builder()` offers a loading cache module, `loading-cache.seam-probe` (`SEAM_PROBE_LOADING_CACHE_ID`), whose `MemoryLoadingCache` keeps every value in process memory until the process ends.
3. The report route's JSON gains `answers_cache_keeps_values`, read with `services.loading_cache_for(SEAM_PROBE_ANSWERS_CACHE).keeps_values()`.
4. The crate depends on `bytes`, in its `Cargo.toml` and in `Cargo.lock`.

The probe asks only whether the cache keeps values. The probe never loads a value through the cache.

## 3. What is not in this change

- **A module that loads through a declared cache.** No module in this repository does. The probe declares one and reports whether the cache keeps values. Moving a module's own lookups onto its declared cache is each module's change.
- **A way to hold nothing in a build that links one loading cache module.** A table names a module that offers a cache, and a table naming anything else, `none` included, is refused when the state is built.
- **A helper that hands a module its own table's settings.** `CacheSettings::declared` is public and `LoadingCacheConfig::lifetimes` takes the module's defaults, but the only caller of `lifetimes` is the robots.txt cache in `crates/trusted-server-core/src/robots_txt/holding.rs`.
- **A check that a module calls only the names it declared.** `LoadingCaches::for_feature` answers `NoLoadingCache` for any name without an entry.
- **A check of a declared name's spelling,** or of whether the name starts with the module's own name.
- **A warning when a declared cache holds nothing** because the build links several loading cache modules and no table names one.
- **A `module` check in the deploy check.** A table whose `module` names a crate that offers no loading cache is still refused only when the state is built.
- **A row in the integration guide.** The table of builder methods in `docs/guide/integration-guide.md` lists `.with_loading_cache(...)` and does not list `.with_cache_features(...)`.

## 4. Acceptance

These tests exist.

In `crates/trusted-server-core/src/integrations/mod.rs`, module `loading_caches_from_tests`:

- `a_cache_a_module_declares_selects_a_module_as_core_s_own_does` shows a declared table naming a module selecting that module, and `robots-txt` with no table taking the one module offered.
- `a_declared_cache_with_no_table_takes_the_one_module_offered` shows a declared cache with no table taking the one loading cache module.
- `a_declared_cache_with_no_table_keeps_nothing_when_two_are_offered` shows the declared cache keeping nothing, and `LoadingCaches::is_empty` true, when two modules are offered and no table names one.
- `a_cache_no_module_declares_is_refused_naming_the_declared_ones` shows the state build refusing `[cache.answres]` and listing `robots-txt, answers`.
- `the_deploy_check_refuses_the_table_the_cache_build_refuses` shows `check_cache_tables` and `loading_caches_from` giving the same message, and a declared name passing the check.
- `two_builders_declaring_one_cache_are_refused_naming_both` shows two builders declaring `answers` refused with no table in the document, and both named.
- `a_builder_declaring_core_s_own_cache_is_refused` shows a builder declaring `robots-txt` refused, with a message that calls the name core's own.

In `crates/trusted-server-core/src/settings.rs`:

- `a_cache_a_module_declares_is_read_from_its_own_table` shows `[cache.answers]` read with its `module` and `store_seconds`, listed second by `loading_caches`, and written back with its `module`.
- `a_loading_cache_module_is_read_and_written_back` is an earlier test, changed only to compare with a `Vec`.

In `crates/trusted-server-adapter-axum/tests/seam_probe.rs`, through the Axum router:

- `a_cache_the_probe_declares_is_selected_in_its_own_table` shows three cases. A table naming `seam-probe` makes the cache keep values. Two loading cache modules and no table make the cache keep nothing. The stock loading cache module alone, with no table, is taken and keeps values.
- `a_cache_table_no_module_declares_is_refused_by_the_deploy_check_and_at_startup` shows `validate_settings_for_deploy_with` and `routes_with_registrations` refusing a misspelled table with the same message, listing `robots-txt, seam-probe-answers`.

These joins have no test.

1. **The Fastly, Cloudflare and Spin adapters.** No test in them writes a `[cache.<name>]` table for a declared cache or asserts on one. The Fastly adapter's probe tests in `crates/trusted-server-adapter-fastly/src/app/seam_probe_tests.rs` now build their state with a builder that declares a cache, so the declaration check runs there, but no assertion in them is about caches. The Cloudflare and Spin test files do not use the probe at all.
2. **`ts config validate` and `TrustedServerAppConfig::validate`.** The Axum test calls `validate_settings_for_deploy_with`, the function that `ts config validate` and `TrustedServerAppConfig::validate` both reach. No test registers a builder that declares a cache through `register_deploy_integrations`.
3. **A range error on a declared table.** The range test covers `cache.robots-txt` only, so the loop over `CacheSettings::declared` in `prepare_runtime` has no test.
4. **A declared table's lifetimes holding a copy.** No module loads through a declared cache, so no test shows `memory_seconds`, `store_seconds` or `store_idle_seconds` of a declared table reaching a cache.
5. **The stock loading cache module selected for a declared cache by name.** No test in `crates/loading-cache/51degrees` or `crates/trusted-server-modules` does this. The stock module is reached for a declared cache only as the one module linked, in the third case of the Axum test.
6. **The deploy check's builder list against the adapters'.** No test covers a built-in builder that declares a cache. Only the deploy check would see that cache.
7. **A module loading through a name it did not declare.** Nothing refuses the call and no test shows what happens.

## Review log

No review rounds yet.

## Revision record

| Date       | Change                               |
| ---------- | ------------------------------------ |
| 2026-10-15 | First draft, written after the code. |
