# Design Spec: The Model Terms cookie name is declared by the module that reads it

**Status:** Proposed, 2026-10-12. Implemented as the draft pull request [IABTechLab/trusted-server#1358](https://github.com/IABTechLab/trusted-server/pull/1358), answering [IABTechLab/trusted-server#1275](https://github.com/IABTechLab/trusted-server/issues/1275). [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-07-30-permission-model-design.md`
**Related PRs:** IABTechLab/trusted-server#1358, which is row 17 of the stack, builds on auction telemetry as a module in [IABTechLab/trusted-server#1357](https://github.com/IABTechLab/trusted-server/pull/1357) and was reviewed on [jwrosewell/trusted-server#12](https://github.com/jwrosewell/trusted-server/pull/12) (closed). [IABTechLab/trusted-server#1304](https://github.com/IABTechLab/trusted-server/pull/1304), the earlier pull request on this repository for the same commit, is closed without being merged, and the review continues on IABTechLab/trusted-server#1358. The permission model in [IABTechLab/trusted-server#1345](https://github.com/IABTechLab/trusted-server/pull/1345) added the constant this change moves, and the integration seam is in [IABTechLab/trusted-server#1348](https://github.com/IABTechLab/trusted-server/pull/1348)
**Issues:** [IABTechLab/trusted-server#1275](https://github.com/IABTechLab/trusted-server/issues/1275). [IABTechLab/trusted-server#777](https://github.com/IABTechLab/trusted-server/issues/777) lists the stack
**Last updated:** 2026-10-16

> **Why this spec exists.** `COOKIE_MTM_PREF`, the name of the cookie that the Preference Management Platform (PMP) writes the Model Terms for Marketing preference into, is declared in core's constants on the rows below this one, although the Model Terms signal module is the only code that reads the cookie. This change declares the name in that module and changes no behavior.

## 1. Problem

`COOKIE_MTM_PREF` is the constant for `__mtm_pref`, the first party cookie that the Preference Management Platform (PMP) writes the Model Terms for Marketing preference into. The cookie holds one of three words, `standard`, `personalized` or `non-marketing`. On the rows of the stack below this one, core (`trusted-server-core`) declares the constant in `crates/trusted-server-core/src/constants.rs`, at lines 16 to 22 on row 16.

`main` at `fa9837f2b` has neither the constant nor the Model Terms crate. Both first appear in the permission model pull request (IABTechLab/trusted-server#1345, row 4 of the stack), in commit `7554c3e7b`. That pull request is still an open draft at this revision, and this change does not edit that pull request.

The Model Terms permission signal module, `crates/permission-signal/mtm` (package `trusted-server-permission-signal-mtm`), is the only code that reads the cookie, in `preference()` at line 143 of its `src/lib.rs`. The cookie belongs to that one implementation of a permission signal and is not a standard that core supports. Core's `RequestInfo::cookie` already hands a module any cookie by name, and the comment on that function says that core keeps no list of which cookies belong to which scheme (`crates/trusted-server-core/src/evidence.rs`, lines 94 to 109). The only reason for the change is that core should name no one implementation's cookie.

Line numbers in this document are those of commit `af8af38f5`, the head of IABTechLab/trusted-server#1358, unless a row is named.

## 2. Design

**The move.** The constant leaves core and is declared in the Model Terms crate, as `pub const COOKIE_MTM_PREF: &str = "__mtm_pref"` at line 74 of `crates/permission-signal/mtm/src/lib.rs`. The name and the value are the same as before, byte for byte. The change is one commit of its own, `f2086639f`, on top of row 16 (the other commits on the branch merge the row below), and the commit touches three files, with 8 lines added and 9 removed.

| File                                                             | Added | Removed | What changes                                                                                                                                           |
| ---------------------------------------------------------------- | ----- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/permission-signal/mtm/src/lib.rs`                        | 7     | 1       | Declares the constant with its doc comment (lines 69 to 74) and drops the `use` of the constant from core                                              |
| `crates/trusted-server-adapter-axum/tests/permission_signals.rs` | 1     | 1       | The test of the signal imports the constant from `trusted_server_permission_signal_mtm` and no longer from `trusted_server_core::constants` (line 614) |
| `crates/trusted-server-core/src/constants.rs`                    | 0     | 7       | Loses the constant and its doc comment                                                                                                                 |

**The doc comment is reworded, not copied.** The comment keeps its meaning, being the three words, a first party cookie written by whatever preference platform asked the visitor, and a name that is not a Trusted Server name so that anyone can build a platform that sets the cookie. The comment drops the words that said the Model Terms signal module reads the cookie, because the constant now sits in that module, and now says that this module reads the cookie and never writes it, where the old comment said the same of Trusted Server. No code in this repository sets a cookie of that name.

**What the module does with the cookie does not change.** The diff changes no other line of the module. `preference()` reads the first pair named `__mtm_pref` in the request's first `Cookie` header, through `RequestInfo::cookie`. A cookie that is absent, or whose value is not one of the three words once the spaces around it are removed, is read as no answer, and the module then answers `Neutral` for every permission, so the country and region rules stand (lines 117 to 127 and 189 to 194). What each word grants or revokes, and the terms the module declares, are as on row 16.

**Why a constant and not a setting.** The name is what the module and whatever platform writes the cookie have to agree on, and the constant's comment says that the name is "not a Trusted Server name", so that anyone can build a platform that sets the cookie. A deployment that renamed the cookie would stop reading the answer of every platform that keeps to the agreed name. A setting would also be the first setting of any permission signal module, and the `[permission-signal]` section accepts only `modules` today and refuses any other key by name (`crates/trusted-server-core/src/settings.rs`, lines 1334 to 1340).

## 3. What changes for a deployment

**Nothing a deployment can observe.** This change adds no setting, so no setting selects the change and every deployment gets it. The cookie's name and the way the module reads the cookie are the same, so no deployment behaves differently and the PMP needs no change. The `[permission-signal]` section accepts the same key, `modules`, with the same default and the same checks. A deployment that lists `mtm`, or leaves `modules` out so that every module an adapter offers runs, reads the same cookie as before.

**What breaks for code.** The constant moves from `trusted_server_core::constants::COOKIE_MTM_PREF` to `trusted_server_permission_signal_mtm::COOKIE_MTM_PREF`, and stays public. Only two files in this repository imported the old path, being the module itself and the Axum adapter's test, and the change updates both. `main` does not declare the constant, so no crate built against `main` imports it. A crate built against rows 4 to 16 that imports the old path stops compiling until the crate changes the path. No row after 17 imports the constant from core (checked through row 50).

**Adapters.** All four adapters register the module in their own `src/app.rs`, and none of them names the constant except the Axum adapter's test.

| Adapter    | Registers `MtmModule`                                           | Names the constant                                                 |
| ---------- | --------------------------------------------------------------- | ------------------------------------------------------------------ |
| Axum       | `crates/trusted-server-adapter-axum/src/app.rs`, line 93        | Only in its test, `tests/permission_signals.rs`, lines 614 and 630 |
| Cloudflare | `crates/trusted-server-adapter-cloudflare/src/app.rs`, line 339 | No                                                                 |
| Fastly     | `crates/trusted-server-adapter-fastly/src/app.rs`, line 370     | No                                                                 |
| Spin       | `crates/trusted-server-adapter-spin/src/app.rs`, line 268       | No                                                                 |

Only the Axum adapter's tests send the cookie through the module, in `permission_signals.rs` (lines 613 to 685). Those tests call core's `assemble_permissions` with a module list that the test builds (`all_five`, line 51), so they exercise the module and not the adapter's own list.

## 4. What is not in this change

- **Other cookie names in core's constants.** `COOKIE_TS_EIDS` (line 14), `COOKIE_SHAREDID` (line 16) and the four names of the IAB consent cookies, `euconsent-v2`, `__gpp`, `__gpp_sid` and `us_privacy` (lines 151 to 154), each name the cookie of one scheme or one vendor and stay in core's constants on this row. The Fastly adapter reads `COOKIE_SHAREDID` at lines 636 and 955 of its `src/app.rs`. The EIDs cookie's name changes at row 27 and leaves core at row 41. Row 27 ([IABTechLab/trusted-server#1368](https://github.com/IABTechLab/trusted-server/pull/1368), which lets a module name the browser's cookies) replaces `COOKIE_TS_EIDS` with `COOKIE_EIDS`, which holds `eids` and stays at line 17 of core's `constants.rs` through row 40, and core writes the cookie's whole name after the deployment's public prefix, so the name is `ts-eids` under core's own prefix. Row 41 ([IABTechLab/trusted-server#1382](https://github.com/IABTechLab/trusted-server/pull/1382)) deletes `COOKIE_EIDS` and the Prebid crate declares the name instead, as `EID_COOKIE` (`crates/auction/prebid/src/eids.rs`, line 24), so core names the EIDs cookie nowhere from row 41. Row 42 ([IABTechLab/trusted-server#1383](https://github.com/IABTechLab/trusted-server/pull/1383)) moves the consent cookie names. `COOKIE_SHAREDID` is still in core on row 50.
- **The README and the guide.** The README of core's permission signal module (`crates/trusted-server-core/src/permission_signal/README.md`) names `__mtm_pref` in a sentence on lines 39 and 40, and the Edge Cookies guide (`docs/guide/edge-cookies.md`, line 96) lists the cookie among the signals that the permission signal modules read. Neither changes here. The README is the only file in `crates/trusted-server-core` that names the cookie, and it still does on row 50. Open question 2 asks about the README.
- **The permission model design.** `2026-07-30-permission-model-design.md`, section 4.6, still says that the cookie's constant is `COOKIE_MTM_PREF` in core, and this change does not amend that document.
- **What reaches Prebid Server.** The cookie reaches Prebid Server in every `consent_forwarding` mode, as it did before this change. Only the `openrtb_only` mode removes cookies from the forwarded `Cookie` header (line 1145 of `crates/auction/prebid-server/src/lib.rs`), and the mode removes the four in `CONSENT_COOKIE_NAMES` (`crates/trusted-server-core/src/cookies.rs`, lines 22 to 27), which do not include `__mtm_pref`. Open question 4 asks whether that is intended.
- **A setting or a new test.** The change adds neither. Section 5 says what no test covers.

## 5. Acceptance

**Checks that can be repeated.** In a clone of IABTechLab/trusted-server, this search prints one line, which is in the README of core's permission signal module, so no Rust file in `crates/trusted-server-core` names the constant or the cookie.

```text
git grep -n -e COOKIE_MTM_PREF -e __mtm_pref af8af38f5 -- crates/trusted-server-core
```

Because no test fails if the literal changes (see below), this diff must also print two lines that are the same apart from their `-` and `+`, being the removed and the added declaration, `pub const COOKIE_MTM_PREF: &str = "__mtm_pref";`.

```text
git diff 7b19336ec...af8af38f5 | grep __mtm_pref
```

**CI.** Every check passed at `af8af38f5`, being 19 of 19 on [jwrosewell/trusted-server#12](https://github.com/jwrosewell/trusted-server/pull/12) and 25 of 25 on IABTechLab/trusted-server#1358.

**Local checks.** The description of IABTechLab/trusted-server#1358 records `cargo test-fastly` and `cargo test-axum`, `cargo clippy-fastly` and `cargo clippy-axum`, `cargo fmt --all -- --check`, the JavaScript tests and format check, the docs format check and the WebAssembly build of the Fastly adapter as run. The description marks a manual run with `fastly compute serve` as not done.

**What no test covers.**

1. **The cookie's name.** No test compares the name the PMP writes with `COOKIE_MTM_PREF`. The module's tests build the cookie header from the constant (`with_cookie`, line 261) and so does the Axum test (line 630). The fixtures of the PMP's `Set-Cookie` strings (`PMP_SET_COOKIE`, lines 292 to 313, which the comment above them calls the PMP's own output) carry the literal `__mtm_pref`, but the tests that use them read only the value and the absence of `HttpOnly` (lines 316 to 319 and 323 to 357), never the name. From reading the tests, a change to the literal at line 74 would move the code and the tests together and fail none of them.
2. **The adapters' own lists.** No test refers to `shipped_signal_modules`, the private function in each adapter's `src/app.rs` that lists the modules, so no test checks that an adapter registers the Model Terms module. The Fastly, Cloudflare and Spin adapters also have no test that reads the cookie.

## 6. Open questions

These are for the reviewer. This revision records what the code does and decides none of them.

1. Should a test assert that the cookie name in each `PMP_SET_COOKIE` fixture equals `COOKIE_MTM_PREF`, so that a changed literal fails a test? The name matters because a module that reads the wrong name sees no answer from any visitor, and a visitor's `standard` choice then no longer revokes the four uses that depend on browsing history (lines 94 to 103 and 217 to 224) where the country baseline grants them.
2. Should the README of core's permission signal module stop naming `__mtm_pref`? The README says on its lines 21 and 22 that core "holds no scheme's wire format and no scheme's meaning", and the identifier modules design (`2026-10-14-identifier-modules-design.md`, row 41) says that core names the Model Terms cookie nowhere.
3. Should the doc comment on `valid_signal` (lines 239 and 240 of the module), which says that a value that is not "one of the two words" is not an answer, say three words, as the module reads three? The row for `mtm` in the Permission Signals guide (`docs/guide/permission-signals.md`, line 123) also says "one of two words".
4. Is it intended that `__mtm_pref` reaches Prebid Server when the four consent cookies are removed in `openrtb_only` mode? With the name out of core, removing the cookie would need a list that core can read without naming the cookie. On row 42 the list moves into the Prebid Server crate, built from the constants of the TCF, GPP and US Privacy crates, and still does not hold the Model Terms cookie. Neither the issue (IABTechLab/trusted-server#1275) nor the pull request description (IABTechLab/trusted-server#1358) says whether that is intended.

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/17-cookie-constant` at `af8af38f5`. Issues found, three of them blocking. The blocking issues were that the spec said the constant arrived in the module documentation set pull request when the permission model pull request added it, that the header sent reviewers to pull requests that are now closed and never named the open one, and that section 3 implied that a deployment's settings can select the change when no setting does. This revision names the pull requests that exist now, says where the constant arrived, lists the three files and what each changes, states that no setting is involved and that every deployment behaves as before, and adds the adapters, the names left in core, the acceptance checks and the untested paths, while four open questions wait for code changes, being the missing test of the cookie's name, the README's mention of the cookie, one stale comment and the cookie reaching Prebid Server.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/17-cookie-constant` at `af8af38f5`. Approved, none blocking. This revision names IABTechLab/trusted-server#1304, the earlier pull request on this repository, as closed without being merged, and says that the EIDs cookie's constant is renamed `COOKIE_EIDS` and stays in core's constants from row 27 until row 41 deletes it, where the earlier text said that row 41 alone moves the name out of core.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                               |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand.                                                                                                                                                                                                                                                                                                      |
| 2026-10-15 | Fixes from review round 1. The header and the pull requests it names, where the constant arrived, the three files and what each changes, the reworded comment, why a constant and not a setting, what changes for a deployment and for code, the adapters, the names left in core, the acceptance checks, the untested paths, the open questions and the review log. |
| 2026-10-16 | Fixes from review round 2. The earlier pull request on this repository, closed without being merged, named in the header, the rows at which the EIDs cookie's constant is renamed and then deleted from core (section 4), and the review log.                                                                                                                        |
