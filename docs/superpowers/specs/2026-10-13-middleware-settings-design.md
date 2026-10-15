# Design Spec: A middleware reads its settings from a table of its own

**Status:** Proposed, 2026-10-13. Raised as draft pull request jwrosewell/trusted-server#33, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-10-12-page-middleware-design.md`, `2026-10-12-shared-vendor-tables-design.md`, `2026-10-14-51degrees-client-hints-design.md`
**Related PRs:** jwrosewell/trusted-server#33, IABTechLab/trusted-server#1379, IABTechLab/trusted-server#1342
**Issues:** IABTechLab/trusted-server#1331
**Last updated:** 2026-10-15

> **Why this spec exists.** A middleware has no table of its own for its settings, so the options of one page change share a table with the module's other settings or, for a vendor, sit beside the endpoint and license key that every module of the vendor shares.

## 1. Problem

A middleware is a page change that runs where a `[[fetch]]` or `[[serve]]` entry names it. An entry places and orders middleware and carries no settings. Section 3.7 of `2026-08-27-integration-provider-seam-design.md` says a middleware reads its settings from `[middleware.<name>]`. The page middleware change (`2026-10-12-page-middleware-design.md`) took them from the module's own table instead, because reading the module's table needed nothing new, and no table named for a middleware was added.

`Settings` has two functions that read the tables of modules. `Settings::module_config` reads the table of a selected module, and `Settings::shared_config` reads a vendor's `[shared.<vendor>]` table. Neither is named for a middleware. A module that supplies several middleware has one table for all of them. The issue records that a vendor whose module supplies several middleware had to put their settings in the vendor's shared table (the vendor shared tables change, IABTechLab/trusted-server#1284), where the options of one page change sit beside the endpoint and license key that every module of the vendor reads. An entry names a middleware by its full name, such as `example.cleanup`, and no table is named for that middleware.

The specs named in the header that are not in this tree are in [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342), which carries every design document of the stack in one place.

## 2. Design

`Settings` gains one section, `[middleware]`, with one `[middleware.<name>]` table for each middleware that has settings of its own. `<name>` is the middleware's full name, which is what a `[[fetch]]` or `[[serve]]` entry writes and what `Middleware::middleware_id` returns. Core declares the section and reads none of a table. The middleware reads its own through `Settings::middleware_config`. A table switches nothing on. The section that selects a module decides whether the module runs, and an entry decides where its middleware runs.

```toml
[middleware.example.cleanup]
selectors = [".promo", "#newsletter"]
```

```rust
pub struct Settings {
    // ...
    pub middleware: MiddlewareTables,
}

impl Settings {
    pub fn middleware_config<T>(&self, name: &str) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: DeserializeOwned + Validate;
}
```

| What                                                    | Where                                                                                                                                         |
| ------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| The section, its tables and the function that reads one | `MiddlewareTables`, `Settings::middleware` and `Settings::middleware_config` in `crates/trusted-server-core/src/settings.rs`                  |
| The name rules                                          | `MiddlewareTables::validate_names`, called by `Settings::prepare_runtime`                                                                     |
| The refusal at startup                                  | `check_middleware` in `crates/trusted-server-core/src/integrations/registry.rs`, called by `IntegrationRegistry::with_plan_and_registrations` |
| The guide and the changelog                             | "Middleware Settings" and "Placing page changes" in `docs/guide/configuration.md`, and `CHANGELOG.md`                                         |

**What a table is.**

- **The name is the path.** `[middleware.example.cleanup]` is the table `cleanup` inside the table `example`, and `MiddlewareTables::table` finds a name by walking down its parts in the same way.
- **A table holds values and arrays.** `MiddlewareTables::table` returns the entries of a table that are not tables. An array is a value whatever it holds, so a list of tables is a setting.
- **A table inside a table belongs to a longer name and is never a setting.** The middleware with the shorter name does not see the inner table. Two stock middleware show the case, `ad-tag.google` in `crates/ad-tag/google/src/lib.rs` and `ad-tag.google.diagnostics` in `crates/ad-tag/google/src/diagnostics.rs`. `[middleware.ad-tag.google.diagnostics]` is the table of the second, and the first does not see the keys in that table. A setting written as an inline table, such as `options = { depth = 1 }` in `[middleware.example.cleanup]`, is the table `example.cleanup.options` and not a setting of `example.cleanup`.
- **A name that only begins longer names has a table with no values.** With only `[middleware.example.cleanup]` written, `MiddlewareTables::table("example")` returns an empty table. `Settings::middleware_config` for `example` then reads an empty table, so it returns `Some` for a settings type that needs no keys and refuses a settings type that needs a key, and it does not return `None`.

**How a middleware reads its table.** A module's build function, the `IntegrationBuilderFn` its `IntegrationBuilder` carries, calls `settings.middleware_config::<T>(name)?` and gives the values to the middleware it builds. `MiddlewareContext` in `crates/trusted-server-core/src/middleware.rs` carries no `Settings`, so a middleware is not handed its table while it works on a document. The table is read once, when the registry is built, which is the integration registry that `IntegrationRegistry::with_plan_and_registrations` makes from the modules that run.

`Settings::middleware_config` returns `Ok(None)` when no table stands at the name, `Ok(Some(value))` when the table reads as `T` and passes `validator::Validate`, and a `TrustedServerError::Configuration` that names the table otherwise. The work is done by `MiddlewareTables::get_typed`. The two messages are `[middleware.<name>] could not be read: ...` for a table that `T` cannot read and `[middleware.<name>] failed validation: ...` for a value that `Validate` refuses. Core hands over the values and does not know the keys. A key that `T` does not know is therefore refused only when `T` is declared with `#[serde(deny_unknown_fields)]`, as `LabeledSettings` in the test module is.

**Configuration.**

| What                               | Default                                               | Refused when                                                                                                                            | Refused                                                                   |
| ---------------------------------- | ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| The `[middleware]` section         | Absent, which reads as no tables                      | A value stands in it where only tables stand, as in `enabled = true`                                                                    | When the settings load, and when the configuration is validated or pushed |
| A table's name                     | Absent, which `middleware_config` returns as `None`   | The name is not parts joined by `.`, each of lower case letters, digits, `_` or `-`, with at most 255 bytes in all                      | When the settings load, and when the configuration is validated or pushed |
| A table at a name nothing supplies | Not applicable                                        | No middleware that a running module supplies has the name, and none continues it after a `.`                                            | When the registry is built, which is when the service starts              |
| A key inside a table               | The settings type's own default (`#[serde(default)]`) | The settings type does not know the key (when it denies unknown fields), the value cannot be read as the type, or `Validate` refuses it | When the module's build function runs, as the registry is built           |

Core has no default inside a table. `Settings::middleware` is `#[serde(default)]`, so a document without `[middleware]` reads an empty `MiddlewareTables`, and `skip_serializing_if` leaves the section out of the serialized settings when it is empty, so the settings that `ts config push` serializes for such a document are the same as before. A name is valid by `module_name::is_valid` in `crates/trusted-server-core/src/module_name.rs`, the rule that `check_middleware` also applies to the name of a middleware.

`ts config validate`, `ts config diff` and `ts config push` run deploy validation, which is `validate_settings_for_deploy_with` in `crates/trusted-server-core/src/config.rs`. `validate_settings_for_deploy_with` calls `Settings::prepare_runtime` on a copy of the settings, so deploy validation applies the first two rows of the table. It builds no registry, so it does not apply the third row. It runs each module's own `validate` function and not its build function, so it does not apply the fourth row to the test module, whose `validate` function reads the module's table alone.

**Which names are supplied.** A name is supplied when it is the `Middleware::middleware_id` of a middleware that a running module supplies, or of a middleware core supplies itself, which are `service_version` and, when `[robots-txt]` is set, `robots_txt`. `check_middleware` goes through every name that `MiddlewareTables::names` gives, which includes the names of tables that only contain other tables. `check_middleware` accepts a name that is supplied and a name that a supplied name continues after a `.`, so with `testing.middleware-fixture.labeled` supplied, `testing` passes for that reason. `check_middleware` refuses any other name with `[middleware.<name>] names <name>, which no module that runs supplies` and the list of supplied names, which is the wording an entry naming a middleware that nothing supplies gets. When the name is that of a module no section selects, or begins with one, `IntegrationRegistryInner::unselected_module_note` adds a sentence saying that nothing the module supplies is running.

**The test module.** Core's tests use a module that exists only for them, `testing.middleware-fixture` (`test_support::middleware_fixture` in `registry.rs`). The test module gains the fetch middleware `testing.middleware-fixture.labeled`, whose settings type is `LabeledSettings { text }`. The module's build function reads `settings.middleware_config::<LabeledSettings>(LABELED)?` and gives the text to `Labeled`. `Labeled::create` writes `labeled_marker(text)` into the head when `Labeled` has a text and writes nothing when the module has no table.

**Each adapter.** None of the four adapter crates changes. Each loads its settings through `settings_from_config_blob_with` in `crates/trusted-server-core/src/config_payload.rs`, directly or through `get_settings_from_config_store_with`. `settings_from_config_blob_with` ends in `Settings::from_json_value`, so the name rules run for each adapter as the settings load. Each builds its registry with `IntegrationRegistry::with_plan_and_registrations`, so the typed reads and `check_middleware` run for each adapter as the registry is built.

| Adapter    | Settings come from                                                                                                                                                           | The registry is built                                                                                                                                                                      | A refusal is answered with                                                                                         |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| Fastly     | `load_settings_from_config_store` in `crates/trusted-server-adapter-fastly/src/app.rs`, through `get_settings_from_config_store_with`, from the blob chosen for the request  | By `build_state_with_registrations` in the same file, once for each Wasm instance                                                                                                          | `startup_error_router` answers every route with `500` and `An internal error occurred`                             |
| Axum       | `build_state` in `crates/trusted-server-adapter-axum/src/app.rs`, through `get_settings_from_config_store_with`                                                              | By `build_state_with_registrations_and_services` in the same file, once, when `main` in `crates/trusted-server-adapter-axum/src/main.rs` calls `TrustedServerApp::routes` before it serves | The same `500` from its `startup_error_router`, with the process still running                                     |
| Cloudflare | `load_startup_settings` in `crates/trusted-server-adapter-cloudflare/src/app.rs`, which calls `settings_from_cloudflare_config_json` and so `settings_from_config_blob_with` | By `build_state_with_registrations_and_services` in the same file, inside the per-request entry point                                                                                      | The same `500` from its `startup_error_router`                                                                     |
| Spin       | `load_startup_settings` in `crates/trusted-server-adapter-spin/src/app.rs`, which calls `settings_from_config_blob_with`                                                     | By `build_state_with_registrations_and_services` in the same file, inside the per-request entry point                                                                                      | `startup_error_router` answers every route with `503` and `Service Unavailable`, and `/health` still answers `200` |

The comment on the SHA-256 check of a registration's browser module in `with_plan_and_registrations` records that Cloudflare and Spin build the registry inside the per-request entry point and that Axum builds it once. The module comment of the Fastly `app.rs` records one build for each Wasm instance. The Fastly adapter chooses the blob in `crates/trusted-server-adapter-fastly/src/config_selection.rs`, by the request's host or by its request addressing. Where the selector names a blob for each host, each host's blob carries its own `[middleware.<name>]` tables.

**Consent and privacy.** No code in this change reads a request, a cookie, a header or a consent signal. `MiddlewareTables`, `Settings::middleware_config` and `check_middleware` are given settings and the list of middleware and nothing from a reader. A table is the publisher's configuration, the same for every reader, and a middleware takes its values once, when the registry is built, so a table does not vary with a reader or with consent. A middleware that must act differently for a reader makes that choice in its own code while it works on a document.

The values are not hidden. The configuration page and its JSON form, `/_ts/config` and `/_ts/config.json` (`handle_config` in `crates/trusted-server-core/src/inspect/config.rs`), are served to anyone while `[inspect] config` is on, which is the default. They mask a secret the loader filled in, a leaf declared as a secret, a value serialized as sensitive, and whatever `[inspect] hide` names. A value in a `[middleware.<name>]` table is none of the first three, so the page shows the value as written unless the publisher names its path in `[inspect] hide`. `MiddlewareTables` prints only the names of its tables when it is printed for debugging (`Debug`), so a log line that prints the settings does not print their values.

A secret does not belong in a middleware's table. `written_forms` in `crates/trusted-server-core/src/module_secrets.rs` finds the secrets that a builder declares through a module's section or through `[shared.<vendor>]` and through no other table, so a name written in a `[middleware.<name>]` table is not looked up in the secret store and is not masked. A middleware that needs a secret keeps the secret in its module's table or its vendor's shared table, where the builder declares it.

**Failure handling.** Every refusal is a `TrustedServerError::Configuration` that names the table or the key at fault. No refusal comes from working on a page, because the table is read when the registry is built, before any document is worked on.

1. A `[middleware]` that holds a value, and a table whose name is not valid, are refused as the settings load.
2. A table that the middleware's settings type cannot read, or that fails `Validate`, is refused as the module's build function runs.
3. A table at a name that no running module supplies is refused as the registry is built. A build function that fails is reported before the check in this item, because the check runs after every module has built.

All four adapters log the full error with `log::error!` and serve `startup_error_router`, which answers every route. The reader of the site gets a generic text, because `TrustedServerError::user_message` returns `An internal error occurred` for a configuration error and Spin returns `Service Unavailable`. The table's name and the error text reach the log only.

A deployment that stops selecting a module has to remove that module's middleware tables as well. The names stop being supplied, so the startup check refuses the tables and says that the module is not selected.

**Rollout.** A document without `[middleware]` reads as before and serializes as before. No module in this repository reads a table (section 3), so a deployment changes nothing when it takes this change.

A build from before this change reads a top-level `[middleware]` as the section of a module type called `middleware`, because `TypeSections` in `settings.rs` takes every top-level table that core does not read itself. `TypeSections::validate` then refuses the section, saying `[middleware] selects no module`. A deployment that adds a `[middleware.<name>]` table therefore has to push it only after every instance that reads the configuration runs a build with this change, and has to restore a document without the table before it goes back to an older build. The comment on `trusted_client_ip` in `Settings` records the same rule for a field that older builds do not know.

**Why in core.** The registry is the one place that knows every middleware a deployment runs, and `Settings` is where every table is read from, so the section and its two checks are in core. Core names no vendor and reads no table. Core checks that a name could be a middleware's and that a running module supplies the name.

## 3. What is not in this change

- **A stock middleware reading a table.** No code in this repository calls `Settings::middleware_config` except the build function of the test module, so each stock middleware keeps reading its module's table. A table written at the name of a middleware that runs, whether a stock module's or core's own, passes the startup check and nothing reads the table.
- **A vendor's middleware leaving the vendor's shared table.** That is a change in the vendor's crates. `2026-10-14-51degrees-client-hints-design.md` describes one middleware that reads a table of its own.
- **A middleware saying that it reads its table.** `IntegrationBuilder::with_shared_settings` ties a shared table to the builder that reads it, and `with_plan_and_registrations` refuses a shared table that no builder reads. A middleware makes no such statement, so the startup check cannot refuse a table whose middleware never reads it.
- **Values in a table that only begins a supplied name.** `[middleware.testing]` holding `text = "x"` passes the startup check when only `testing.middleware-fixture.labeled` is supplied, and no middleware reads the value.
- **A setting that is a table.** A table inside a table is the table of a longer name (section 2), so a middleware that needs structure inside a setting writes a list of tables, which stays a value.
- **A secret in a table.** A name written in a middleware's table is not looked up in the secret store and is not masked on the configuration page. Declaring a secret in this section would need a rule in `crates/trusted-server-core/src/module_secrets.rs`.
- **Checking supplied names when the configuration is validated or pushed.** Deploy validation builds no registry, so a table at a name nothing supplies is found when the service starts.
- **Documentation for module authors.** `docs/guide/integration-guide.md` still says that a middleware reads its settings from the module's own table and has no section on `Settings::middleware_config`, and `trusted-server.example.toml` names `[shared.<vendor>]` and not `[middleware.<name>]`. This change updates `docs/guide/configuration.md` and `CHANGELOG.md` only.
- **A module type named `middleware`.** Core now reads `[middleware]`, so a module type cannot use that name. No crate in this tree sits under a `crates/middleware` folder. A crate that did would, by default, be selected by `[middleware] modules = [...]`, and `MiddlewareTables::validate_names` refuses that value.

## 4. Acceptance

The new tests are in `crates/trusted-server-core/src/integrations/registry.rs` and use the test module `testing.middleware-fixture`.

- `a_middleware_reads_its_own_table_and_not_a_longer_names`. With `testing.middleware-fixture.labeled` holding `text = "hello"`, `MiddlewareTables::names` gives `testing`, `testing.middleware-fixture` and `testing.middleware-fixture.labeled`. `Settings::middleware_config` reads `text` from the third. `MiddlewareTables::table` gives the second a table with no values. A name with no table, `testing.nothing`, reads as `None`. The registry builds with the table written.
- `a_middleware_table_at_a_name_nothing_supplies_is_refused`. Two tables are refused when the registry is built, `testing.nothing` and a table inside the labeled table, `testing.middleware-fixture.labeled.deeper`. Each message names the table and lists the supplied names.
- `a_middleware_table_with_a_key_it_does_not_know_is_refused_as_its_own`. A key `colour` in the labeled table is refused when the registry is built, and the message names the table and the key.
- `the_middleware_section_refuses_a_value_and_a_name_no_middleware_could_have`. `MiddlewareTables::validate_names` refuses a value `enabled` where only tables stand, and refuses a table named `Testing Fixture`.

Four existing tests list every middleware that the test module supplies, and each now lists the labeled middleware too. They are `a_selected_module_s_middleware_run_where_an_entry_names_them`, `an_entry_naming_a_middleware_nothing_supplies_is_refused_with_what_is_supplied`, `a_middleware_no_entry_names_is_listed_for_the_startup_warning` and `each_phase_has_entries_of_its_own`.

**Joins with no test.** A join is a place where two parts of the system meet. The joins below have no test.

- **A document written in TOML.** The four tests build the tables with `serde_json::from_value`. No test loads a `[middleware.<name>]` table through `Settings::from_toml` or `Settings::from_json_value`. The `[shared]` section has such tests in `settings.rs`.
- **The paths into `validate_names`.** The fourth test calls `MiddlewareTables::validate_names` directly. No test gives `Settings::prepare_runtime`, `Settings::from_toml`, `Settings::from_json_value` or `validate_settings_for_deploy_with` a document with a `[middleware]` section, so no test shows that any of them refuses one.
- **Writing the settings out, and the debug print.** No test shows that an empty `[middleware]` is left out of the serialized settings, that a table survives `serde_json::to_value`, or that `Debug` prints names and no values. The `[shared]` section has all three.
- **The refusals inside `MiddlewareTables::get_typed`.** Only an unknown key is tested. A value of the wrong type and a value that `Validate` refuses are not, so the `failed validation` message has no test.
- **A name that only begins a supplied name.** No test writes values in such a table, and none calls `Settings::middleware_config` for such a name.
- **The note for a module no section selects.** `an_entry_naming_the_middleware_of_a_module_no_section_selects_says_so` covers the note for an entry. No test covers the note for a table, or a table at the name of a middleware that core supplies.
- **A value reaching a page.** `Labeled::create` writes the table's text into the head through `labeled_marker`, and no test runs `Labeled::create`, because no test has an entry naming `testing.middleware-fixture.labeled`. The tests show that the table is read and that the registry builds with the table.
- **The adapters.** No test in the Fastly, Axum, Cloudflare or Spin crate sets `Settings::middleware`, so no test shows an adapter answering for a refused table. The Fastly and Spin crates test their `startup_error_router` directly.
- **The operator's tools.** No test shows `/_ts/config` showing a middleware table, `[inspect] hide` masking one, or `ts config validate` or `ts config push` taking a document with a `[middleware]` table.

The results of the tests are shown by the checks on the pull request.

## Review log

No review rounds yet.

## Revision record

| Date       | Change                               |
| ---------- | ------------------------------------ |
| 2026-10-15 | First draft, written after the code. |
