# Design Spec: Each adapter lists the modules it links, each module describes its middleware, and the project's own modules are renamed iabtl

**Status:** Proposed, 2026-10-14. Raised as draft pull request IABTechLab/trusted-server#1384 (from jwrosewell/trusted-server#38), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-source-modules-design.md`, in IABTechLab/trusted-server#1342, which replaced IABTechLab/trusted-server#1084 as the one place for every design document of the stack of pull requests listed in IABTechLab/trusted-server#777
**Related PRs:** IABTechLab/trusted-server#1384, jwrosewell/trusted-server#38
**Issues:** IABTechLab/trusted-server#1334
**Last updated:** 2026-10-26

> **Why this spec exists.** One crate held the list of the modules a stock build ships on every adapter's behalf, with a table saying which middleware each supplies, so a deployment that wanted a different set edited a crate that was not its own, and a module's description of itself lived in a crate that was not the module's. The same change renames the three modules the project owns itself, and those renames are the only part that changes what an operator writes in settings.

## 1. Problem

`crates/trusted-server-modules` listed the twenty stock module builders in hook order, and the Axum, Cloudflare, Fastly and Spin adapters, the `ts` tool and the integration tests crate each linked the whole of it. A deployment that wanted a different set of modules than the stock list had no composition of its own to write. The crate also held the table `ts audit` read to write `[[fetch]]` and `[[serve]]` entries for HTML pages, with every stock middleware's phase and media type, and the section and name each stock module is selected by. A module's page changes were therefore described twice, once by the module's registration and once by a table in another crate. One test in that crate (`the_listed_middleware_are_the_ones_the_modules_register`) kept the table in step with the stock modules' registrations, and a module a deployment added had no row in the table at all.

Three modules the project owns itself also sat under names that said nothing about who maintains them, being `crates/ad-server/mock`, `crates/testing/seam-probe` and `crates/ads-txt/sellers`. A crate sits at `crates/<type>/<vendor>/`, and these three have no vendor to take a folder name from, because each names the Trusted Server maintainers as its owner, with the status `project owned`, in its manifest.

## 2. Design

**Each adapter lists the builders it links.** The Axum, Cloudflare, Fastly and Spin adapters each hold the list in their own `app.rs`, beside the function that lists the permission signal schemes the adapter links, and the schemes stay a separate list. `shipped_module_builders()` gives the twenty builders in the order their request hooks run, which is the order the old crate listed them, and `module_builders_with(extra)` gives the same builders followed by a deployment's own. Each adapter builds its state and loads its settings from its list in the places where it took the old crate's list. The Axum, Cloudflare and Spin adapters export both functions from their public `app` module, and the Fastly adapter, whose `app` module is private, re-exports them from the crate root. A deployment that wants a different set links a different set, and one that only adds a module hands its builder to the adapter (`run_with` on Fastly, `routes_with_registrations` on the others), which runs the builder after the listed ones.

The `ts` tool lists the builders it links the same way, because it validates a deployment's settings against each module's own rules. The list is the function `linked_builders()` in `crates/trusted-server-cli/src/app_config.rs`, and the crate exports the function from its root. The integration tests crate keeps no list of its own and validates its fixture configurations against `trusted_server_adapter_axum::app::shipped_module_builders()`, on the understanding that every adapter links the same modules.

A deployment with a different set also needs a `ts` tool with a matching list. No call hands `ts audit generate` another list, because the command reads `linked_builders()` directly, in `build_page_entries` and `module_section` (`crates/trusted-server-cli/src/commands/audit/generate/mod.rs`), and `run_from_env()` takes none. A deployment with a different set therefore changes `linked_builders()` in its own copy of the crate. A list registered first with `trusted_server_core::config::register_deploy_integrations`, where the first call wins, changes what the tool validates against and leaves what `ts audit generate` writes as it was.

**Each module describes the middleware it supplies.** `IntegrationBuilder::with_middleware` takes the page changes a module's registration supplies, each as a `SuppliedMiddleware` naming the middleware, the phase an entry names it in and the media type the entry covers. A middleware that runs in either phase is described in the one a generated configuration places it in. The registry refuses a registration whose middleware contradicts the description, in a phase it does not run in or for a media type it does not work on, and a per-crate test proves each stock module's description is exactly what it registers. The registry leaves alone a described middleware that the registration does not supply, because a module may supply one only where its settings call for it. Every stock module registers each middleware it describes whenever the module runs.

```rust
const SUPPLIED_MIDDLEWARE: &[SuppliedMiddleware] = &[
    SuppliedMiddleware::on_html(MODULE, MiddlewarePhase::Fetch),
    SuppliedMiddleware::on_html(TAG_MIDDLEWARE, MiddlewarePhase::Serve),
];

pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(ID, env!("CARGO_PKG_NAME"), register, validate)
        .with_module_name(MODULE)
        .with_middleware(SUPPLIED_MIDDLEWARE)
}
```

**When a description is wrong.** The check runs where the registry is built, which is each adapter's state build. Deploy validation (`ts config validate` and `ts config push`) never builds the registry (`validate_settings_for_deploy_with` in `crates/trusted-server-core/src/config.rs`), so the per-crate tests are what find a description the registration contradicts before a service starts, and a running service finds it only at startup. A refusal fails the adapter's state build, so the service serves no page. Fastly, Axum and Cloudflare answer with a 500 and a generic body, Spin answers with a 503, and the refusal, naming the module, the middleware and the difference, goes to the log.

**Core lists what a build supplies.** `trusted_server_core::integrations::supplied_middleware(builders)` gives every described middleware of the built-in modules and `builders`, with the integration that supplies it, in the order the registry registers them: what registers from the auction plan first, then the built-in modules, then the linked builders in their order, then core's robots.txt and service version middleware, which no builder supplies. `ts audit` asks this of the builders it links and holds no table, and finds the section a module is selected in and the name it is written under from the builder itself. The entries `ts audit` writes are for `text/html` only, so a middleware described for another media type, such as the Flight rewriter's `text/x-component` or the ads.txt module's `text/plain`, is left out.

**The project's own modules take the name `iabtl`.** `iabtl` is short for IAB Tech Lab, which hosts this repository, and is the vendor folder for a module the project owns itself, as `didomi` is the vendor folder in `crates/cmp/didomi`. `crates/ad-server/mock` is `crates/ad-server/iabtl`, `crates/testing/seam-probe` is `crates/testing/iabtl` and `crates/ads-txt/sellers` is `crates/ads-txt/iabtl`, each with its maintainers named as every crate outside core names them. The packages change with the folders, from `trusted-server-ad-server-mock`, `trusted-server-integration-seam-probe` and `trusted-server-ads-txt-sellers` to `trusted-server-ad-server-iabtl`, `trusted-server-testing-iabtl` and `trusted-server-ads-txt-iabtl`. A module's name follows its folder, so the selectors a deployment writes change:

| Was                                                       | Is                                                     |
| --------------------------------------------------------- | ------------------------------------------------------ |
| `[ad-server] module = "mock"` and `[ad-server.mock]`      | `[ad-server] module = "iabtl"` and `[ad-server.iabtl]` |
| `implementation = "ad-server.mock"`                       | `implementation = "ad-server.iabtl"`                   |
| `[ads-txt] modules = ["sellers"]` and `[ads-txt.sellers]` | `[ads-txt] modules = ["iabtl"]` and `[ads-txt.iabtl]`  |
| `middleware = ["ads-txt.sellers"]`                        | `middleware = ["ads-txt.iabtl"]`                       |

The seam probe is a test fixture that no deployment ships, so its renames reach only the tests and fixtures that drive it. They are:

- the module `testing.seam-probe`, now `testing.iabtl`, and its serve middleware `testing.seam-probe.reader`, now `testing.iabtl.reader`
- the shared table `[shared.seam-probe]`, now `[shared.iabtl]`
- the declared cache `seam-probe-answers`, now `iabtl-answers`
- the loading cache `loading-cache.seam-probe`, selected as `module = "seam-probe"`, now `loading-cache.iabtl`, selected as `module = "iabtl"`

**What does not change.** A deployment that keeps the stock set and selects none of the renamed modules runs the same modules in the same order with the same settings, because the twenty builders are the old crate's in its order, apart from the ad server and ads.txt crates, which carry their new names. The permission signal schemes stay in their own list in each adapter. A list that leaves a module out makes startup refuse any section that selects it, naming the modules on offer (`check_section_selections` in `crates/trusted-server-core/src/integrations/registry.rs`). The change adds no setting and no default, and the only names that move in settings are the renames above.

**For crates outside core.** `trusted-server-modules` is removed, with `builders()`, `builders_with(extra)`, `selection_of(id)`, `middleware()`, `middleware_for(ids, phase)` and `StockMiddleware`. A crate that took its list from `builders()` or `builders_with(extra)` names the crates it links instead, or takes an adapter's `shipped_module_builders()`. A crate that read the table of middleware reads `supplied_middleware(builders)` in core, and one that used `selection_of(id)` asks the builder for its `section()` and `module_name()`. The removal and the renames are listed as breaking in `CHANGELOG.md`.

**Rollout.** A module's name is its folder's and has no second name, so no alias accepts both the old and the new name, and a name in settings that no linked module answers to is refused when the service starts. A deployment that selects `mock` or `sellers`, or names one of them in an `implementation` line or an entry, changes its settings and its build together. On Fastly the settings sit in a config store apart from the compiled service (`load_settings_from_config_store` in `crates/trusted-server-adapter-fastly/src/app.rs`), and `ts config push` and a deploy are separate steps. If the new settings are pushed first, the active version refuses the new names, and if the new version is activated first, that version refuses the old names, so the service serves no page until the second step is done. A deployment in that position chooses the moment, and can rehearse the pair on a staged version first with `ts deploy --staging` and `ts config push --staging` (`docs/guide/cli.md`).

## 3. What is not in this change

- A registry refusal of a registered middleware its builder does not describe. A description is optional, so a vendor crate that describes none is still linked and run, and `ts audit` places only what a builder describes.
- A shared list any two adapters read. Nothing is shared by being listed, so each adapter and each deployment names the crates it links itself.
- A way to hand `ts audit generate` a deployment's list. Section 2 gives what a deployment with a different set does instead.
- An alias for the old module names. The rollout in section 2 gives the order of work instead.
- The crates' own type names and log labels, which say what a module does rather than where it lives.
- Three small changes left for a later change: different names for `IntegrationBuilder::with_middleware` and `IntegrationRegistrationBuilder::with_middleware`, the doc comments now above the wrong items (`check_described_middleware` in the registry and `SUPPLIED_MIDDLEWARE` in the seam probe), and the mention of `trusted-server-modules` in `2026-10-14-rsc-flight-middleware-design.md`.

## 4. Tests

- A builder describes no middleware until told, and the description is read back as given.
- `supplied_middleware` lists what the plan registers first, then core's own, then the linked builders in order, then core's file and version middleware, with each row's phase and media type.
- Core's robots.txt row is what the registry registers for a `[robots-txt]` section, in the listed phase and for the listed media type.
- A description the registration contradicts is refused at startup, naming the module, the middleware and the difference, for a wrong phase and for a wrong media type, and a description the registration bears out is accepted.
- Each stock module that supplies middleware, core's script proxy and the seam probe describe exactly what they register under their own fixture settings, in registration order.
- Each adapter lists its builders in the order their hooks run, and `module_builders_with` puts a deployment's builders after them.
- The Axum adapter's linked builders give what the stock list gave: no loading cache module is linked, so every feature loads through `NoLoadingCache` unless a deployment links one (`2026-10-12-loading-cache-design.md`), every page module refuses a setting it does not know, the diagnostics module's request preparer is attached, the template's entries and the guide's table name the middleware the linked modules supply in order, the documented tables validate when uncommented, deploy validation runs a module's own rules and lists its secret settings, the ad server keeps an APS winner's identities, and every crate outside core declares its maintainers.
- `ts audit` finds a module's selection by its integration id from the builders the tool links, and writes `text/html` entries that run the selected modules' middleware in registration order.

Three things have no test. The first is the five lists agreeing. Each adapter's order test pins its own list against a copy of the same twenty names, so the four lists agree today, but nothing compares one list with another, and the `ts` list has no order test of its own. Its order is checked only through `build_draft_config_places_the_page_changes_of_the_modules_it_selects`, for the modules that test selects, and the integration tests crate validates its fixtures against the Axum adapter's list alone. Adding a module takes five edits and a missed one passes every test, so a test that gives the same ids in the same order from all five lists is missing. The second is the registry accepting a described middleware that the registration leaves out, because no test has a registration omit a middleware its builder describes. The third is `ts audit` leaving out a middleware described for another media type, because the draft test that checks the entries selects `gpt`, `datadome`, `didomi` and `google_tag_manager`, whose middleware are all described for `text/html`, and `build_page_entries` has no direct test.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-14 | First draft.                                                                                                                                                                                                                                                                                                                                                                                                          |
| 2026-10-15 | Fixes from review round 1. The other users of the old crate, the test that kept its table in step, the renamed packages and the seam probe's names, the adapters and the list functions, what does not change, the startup refusal, the rollout of the renamed selectors, what `ts audit` reads and writes, the missing tests, `iabtl` and the stack defined, the renames in the title, and the pull requests raised. |
| 2026-10-15 | Fixes from review round 2. The claimed test for the media types `ts audit` writes, the three changes left for a later change, and the changelog entries that still named the removed crate.                                                                                                                                                                                                                           |
| 2026-10-26 | The Axum adapter's tests in section 4 say that its linked builders include no loading cache module, so every feature loads through `NoLoadingCache` unless a deployment links one (`2026-10-12-loading-cache-design.md`), where they said a feature selects the stock loading cache by name.                                                                                                                          |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/43-no-modules-crate` at `3ad378928`. Approved with no blocking issues. This revision closes the coverage gaps by naming the four adapters and the three list functions with where each is exported, saying what stays the same for a deployment that keeps the stock set, saying that deploy validation never builds the registry so a wrong description is refused only at startup and then stops the whole service, giving the order of work for the renamed selectors, and recording the missing test that ties the five lists together.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `0fdbaf5dc`, of the round 1 fixes and the new text. No blocking issues. This revision stops claiming a test for `ts audit` leaving out a middleware described for another media type and lists the gap as a third untested item, records the three small changes left for a later change, and corrects the two older changelog entries that still named the removed crate.
