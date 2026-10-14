# Design Spec: Each adapter lists the modules it links, and each module describes its middleware

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-source-modules-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1334
**Last updated:** 2026-10-14

> **Why this spec exists.** One crate held the list of the modules a stock build ships on every adapter's behalf, with a table saying which middleware each supplies, so a deployment that wanted a different set edited a crate that was not its own, and a module's description of itself lived in a crate that was not the module's.

## 1. Problem

`crates/trusted-server-modules` listed the twenty stock module builders in hook order, and every adapter and the `ts` tool linked the whole of it. A deployment that wanted a different set of modules than the stock list had no composition of its own to write. The crate also held the table `ts audit` read to write `[[fetch]]` and `[[serve]]` entries, with every stock middleware's phase and media type, and the section and name each stock module is selected by, so a module's page changes were described twice, once by the module's registration and once by a table in another crate, and the two could drift.

The stack's own modules also sat under names that said nothing about who maintains them, being `crates/ad-server/mock`, `crates/testing/seam-probe` and `crates/ads-txt/sellers`.

## 2. Design

**Each adapter lists the builders it links.** Each adapter's `app.rs` holds the list in its own composition, the way it already lists the permission signal schemes it links, with a composition function that puts a deployment's builders after them. The order is the order the crate listed them, which is the order their request hooks run. A deployment that wants a different set links a different set. The `ts` tool lists the builders it links the same way, because it validates a deployment's settings against each module's own rules.

**Each module describes the middleware it supplies.** `IntegrationBuilder::with_middleware` takes the page changes a module's registration supplies, each as a `SuppliedMiddleware` naming the middleware, the phase an entry names it in and the media type the entry covers. A middleware that runs in either phase is described in the one a generated configuration places it in. The registry refuses a registration whose middleware contradicts the description, in a phase it does not run in or for a media type it does not work on, and a per-crate test proves each stock module's description is exactly what it registers.

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

**Core lists what a build supplies.** `trusted_server_core::integrations::supplied_middleware(builders)` gives every described middleware of the built-in modules and `builders`, with the integration that supplies it, in the order the registry registers them: what registers from the auction plan first, then the built-in modules, then the linked builders in their order, then core's robots.txt and service version middleware, which no builder supplies. `ts audit` asks this of the builders it links and holds no table, and finds the section a module is selected in and the name it is written under from the builder itself.

**The stack's own modules take the stack's name.** `crates/ad-server/mock` is `crates/ad-server/iabtl`, `crates/testing/seam-probe` is `crates/testing/iabtl` and `crates/ads-txt/sellers` is `crates/ads-txt/iabtl`, each with its maintainers named as every crate outside core names them. A module's name follows its folder, so the selectors a deployment writes change:

| Was                                                       | Is                                                     |
| --------------------------------------------------------- | ------------------------------------------------------ |
| `[ad-server] module = "mock"` and `[ad-server.mock]`      | `[ad-server] module = "iabtl"` and `[ad-server.iabtl]` |
| `implementation = "ad-server.mock"`                       | `implementation = "ad-server.iabtl"`                   |
| `[ads-txt] modules = ["sellers"]` and `[ads-txt.sellers]` | `[ads-txt] modules = ["iabtl"]` and `[ads-txt.iabtl]`  |
| `middleware = ["ads-txt.sellers"]`                        | `middleware = ["ads-txt.iabtl"]`                       |

The seam probe is a test fixture, so its selectors change in the tests that drive it, from `testing.seam-probe` to `testing.iabtl`.

## 3. What is not in this change

- A registry refusal of a registered middleware its builder does not describe. A module may register one only where its settings call for it, and a vendor crate that describes none is still linked and run. A described middleware is checked against what is registered whenever the module runs.
- A shared list any two adapters read. Nothing is shared by being listed, so each adapter and each deployment names the crates it links itself.
- The crates' own type names and log labels, which say what a module does rather than where it lives.

## 4. Tests

- A builder describes no middleware until told, and the description is read back as given.
- `supplied_middleware` lists what the plan registers first, then core's own, then the linked builders in order, then core's file and version middleware, with each row's phase and media type.
- Core's robots.txt row is what the registry registers for a `[robots-txt]` section, in the listed phase and for the listed media type.
- A description the registration contradicts is refused at startup, naming the module, the middleware and the difference, for a wrong phase and for a wrong media type, and a description the registration bears out is accepted.
- Each stock module that supplies middleware, core's script proxy and the seam probe describe exactly what they register under their own fixture settings, in registration order.
- Each adapter lists its builders in the order their hooks run, and its composition puts a deployment's builders after them.
- The Axum adapter's linked builders give what the stock list gave: a feature selects the stock loading cache by name, every page module refuses a setting it does not know, the diagnostics module's request preparer is attached, the template's entries and the guide's table name the middleware the linked modules supply in order, the documented tables validate when uncommented, deploy validation runs a module's own rules and lists its secret settings, the ad server keeps an APS winner's identities, and every crate outside core declares its maintainers.
- `ts audit` finds a module's selection by its integration id from the builders the tool links, and writes the entries that run the selected modules' middleware in registration order.
