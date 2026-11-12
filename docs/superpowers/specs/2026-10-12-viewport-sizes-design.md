# Design Spec: A format can name the viewport widths at which the page asks for it

**Status:** Proposed, 2026-10-12. Implemented as the pull request [IABTechLab/trusted-server#1370](https://github.com/IABTechLab/trusted-server/pull/1370), row 29 of the stack, answering [IABTechLab/trusted-server#1287](https://github.com/IABTechLab/trusted-server/issues/1287). [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-07-24-prebid-refresh-gam-path-opt-out-design.md` (the wrapper on `refresh()` that runs the refresh auction, whose sizes this change takes from the band) and `2026-07-24-prevent-duplicate-gpt-slot-requests-design.md` (how the head bootstrap and the bundle define a slot on the publisher's div), both on `main` of IABTechLab/trusted-server
**Related PRs:** [IABTechLab/trusted-server#1370](https://github.com/IABTechLab/trusted-server/pull/1370) builds on the device signals in [IABTechLab/trusted-server#1369](https://github.com/IABTechLab/trusted-server/pull/1369) (row 28), replaces [IABTechLab/trusted-server#1317](https://github.com/IABTechLab/trusted-server/pull/1317) (closed, an earlier pull request for this change against `main`) and was reviewed on [jwrosewell/trusted-server#24](https://github.com/jwrosewell/trusted-server/pull/24) (closed)
**Issues:** [IABTechLab/trusted-server#1287](https://github.com/IABTechLab/trusted-server/issues/1287). [IABTechLab/trusted-server#777](https://github.com/IABTechLab/trusted-server/issues/777) lists the stack
**Last updated:** 2026-10-16

> **Why this spec exists.** A slot lists the sizes a page asks for, and does not record the widths at which the page shows each size. This spec says what the two viewport bounds on a format add, which requests for sizes the bounds reach, and which requests are still offered every format.

## 1. Problem

A slot's formats are the sizes a page asks for, and the page asks for all of them at every width. A page that shows a billboard on a wide screen and a small banner on a phone has one slot and several formats, so a bidder is offered a billboard the page cannot show, and the ad tag defines the slot with every size at every width.

Two scripts define the slot in the browser. The head bootstrap is the script that the Google ad tag writes into the head of the page, and the bundle is the script bundle that the server adds to the page. Three requests then ask for the slot's sizes. The first is the ad server request that the Google Publisher Tag (GPT) makes for the slot. The second is the refresh auction, which the bundle runs in the browser when a publisher refreshes a slot. The third is a pair of server-side auctions, being the initial-page auction that the server runs when it serves the document and the page-bids auction that the server runs when a single-page application moves to a new path and the bundle asks for bids.

## 2. Design

Line numbers are those of `054f3a2a0`, the head of the pull request. Paths are under `crates/`, where `core/` is `trusted-server-core/src/`, `js/` is `trusted-server-js/lib/src/` and `tag/` is `ad-tag/google/src/`.

```toml
[[creative_opportunities.slot]]
id = "billboard"
page_patterns = ["/**"]
formats = [
  { width = 970, height = 250, min_viewport_width = 1040 },
  { width = 728, height = 90, min_viewport_width = 800, max_viewport_width = 1039 },
  { width = 300, height = 250, min_viewport_width = 600, max_viewport_width = 799 },
  { width = 320, height = 50, max_viewport_width = 599 },
]
```

**The bounds.** A format may name `min_viewport_width` and `max_viewport_width`, the narrowest and the widest viewport at which the page asks for the format, so a slot asks for a large size only where the page has room for one. Each bound is a whole number of CSS pixels and is inclusive, and either may be left out to leave that side open. Neither bound has a default and neither is written out when unset, so a format that names neither is read and written as it was (`core/creative_opportunities.rs`, lines 1037 to 1042). Equal bounds name one width, and formats whose ranges overlap are all asked for where the ranges overlap. A minimum above a maximum is refused (lines 1055 to 1063). The refusal comes when the settings load from a file or from the stored configuration that every adapter reads (`Settings::prepare_runtime`, `core/settings.rs` line 4518, reached from `core/config_payload.rs` line 99), and again in the validation that runs before settings are pushed (`core/config.rs` line 360).

**The bands.** `CreativeOpportunitySlot::viewport_sizes` (`core/creative_opportunities.rs`, lines 954 to 982) turns the formats of a slot into bands. A band is a range of viewport widths over which the slot asks for the same sizes. Each band is recorded as the narrowest width at which it applies and the `[width, height]` sizes asked for from that width up to the width of the next band, in the order the formats are written. The bands start at zero and at every width where the formats on offer change, being each minimum and the width one above each maximum, and neighbouring bands that offer the same sizes become one band. A band can be empty, which means the slot asks for nothing at those widths, as when the only format of a slot has a minimum, which leaves every narrower width empty. A slot whose formats name no bound has no bands and asks for every format at every width, as before. For the slot above the bands are, widest first:

| From viewport width | Sizes asked for |
| ------------------- | --------------- |
| 1040                | 970x250         |
| 800                 | 728x90          |
| 600                 | 300x250         |
| 0                   | 320x50          |

**The page data.** When a slot has bands, its page data carries them as `viewport_sizes`, a list with one `[[width, 0], sizes]` entry for each band, widest first. The pair `[width, 0]` is the viewport pair that GPT's size mapping takes, written with a height of zero because the bands are by width alone, so the browser passes each entry to the mapping as it stands. For the slot above the list is:

```json
[
  [[1040, 0], [[970, 250]]],
  [[800, 0], [[728, 90]]],
  [[600, 0], [[300, 250]]],
  [[0, 0], [[320, 50]]]
]
```

The slot's `formats` still lists every size. `build_slot_json` (`core/publisher.rs`, lines 6821 to 6854) builds the page data for each of the three ways the page data reaches the browser, being the head script that sets `window.tsjs.adSlots` (`build_ad_slots_script`), the list of slots that a shared page receives with each request (`seam_ad_slots_json`) and the answer to the page-bids request (`handle_page_bids`).

## 3. Which requests the bands reach

The bands reach the ad server request for a slot that Trusted Server defines and the refresh auction, and they do not reach the server-side auctions.

| Request                                                           | Sizes offered                                                                                                        |
| ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| The ad server request for a slot that Trusted Server defined      | The sizes of the band that GPT finds for the viewport, through a size mapping                                        |
| The ad server request for a slot that the publisher defined first | The sizes the publisher defined, with no mapping added                                                               |
| The refresh auction                                               | The sizes of the band the viewport is in when the refresh runs, and no auction at all for a slot whose band is empty |
| The initial-page auction and the page-bids auction                | Every format                                                                                                         |

**The ad server request.** The head bootstrap and the bundle each define the slot with every size and then, when the slot has bands, apply the bands as a size mapping, which is GPT's way of giving a slot different sizes at different viewport widths. Each does so in a function named `defineWithViewportSizes` (`tag/gpt_bootstrap.js`, lines 361 to 383, and `js/integrations/gpt/index.ts`, lines 858 to 877). Each calls the function in two places, being the `adInit` pass (`tag/gpt_bootstrap.js` line 855, `js/integrations/gpt/index.ts` line 1382) and the fallback that defines the slot later when a publisher claimed the placement but the claim ran out before the publisher made an ad request (`tag/gpt_bootstrap.js` line 707, `js/integrations/gpt/index.ts` line 1186). The mapping is set once, when Trusted Server defines the slot, and no code sets it again when the window is resized. When GPT has no `googletag.sizeMapping` or the slot has no `defineSizeMapping`, nothing is mapped and nothing is logged, so the slot is requested with every size (`tag/gpt_bootstrap.js` lines 369 to 375, `js/integrations/gpt/index.ts` line 869).

**The refresh auction.** The refresh auction compares `window.innerWidth` with the start of each band every time a publisher's refresh runs, and offers the slot the sizes of the widest band that the width reaches (`injectedSlotBandSizes`, `js/integrations/prebid/index.ts`, lines 1137 to 1146). Those sizes become the banner sizes of the auction's ad unit (same file, lines 2762 to 2768). The bundle's Prebid adapter posts the ad unit, with its banner sizes, to the auction endpoint, which is `/auction` unless the Prebid configuration names another path (same file, lines 2357 and 2375 to 2395, with `buildAdRequest` in `js/core/auction.ts`, lines 76 to 114), and the server reads the sizes as the formats of the slot (`convert_tsjs_to_auction_request`, `core/auction/formats.rs`, lines 168 to 217). The ad unit also lists the client-side bidders that the publisher configured for the slot (same file, line 2784), so Prebid asks those bidders about the same sizes. A slot whose band is empty at the current width is left out of the auction (`js/integrations/prebid/index.ts`, lines 1154 to 1157 and 2720 to 2724), its stale targeting is cleared as for every slot left out, and GPT still refreshes the slot. A slot with no bands, or a slot that the bundle cannot match to a slot of the page data, is offered the sizes it was offered before.

**The server-side auctions.** Both server-side auctions build their slots with `to_ad_slot`, which lists every format whatever the bounds (`build_auction_request`, `core/publisher.rs`, lines 6308 to 6312, called for the initial page at line 5521 in `handle_publisher_request` and for page-bids at line 7906 in `handle_page_bids`). The server holds the bands but reads no viewport width from either request. The page-bids request carries no viewport width, as it sends the path, the cookies and the `X-TSJS-Page-Bids` header and nothing more (`js/integrations/gpt/index.ts`, lines 1564 to 1573), and a search of the crates and the guides for the `Viewport-Width` and `Sec-CH-Viewport-Width` headers (section 6) finds none. So the server cannot choose a band for a reader, and a bidder in either auction is still offered a billboard on a phone, which is the problem section 1 describes. A bid for a size outside the band can therefore win. With the slot above, a 970x250 bid can win on a phone 375 pixels wide whose mapping allows only 320x50. Nothing compares the size of a winning bid with the band, and a winning bid that carries no size of its own is sized as the slot's first configured format, not as the band's (`js/integrations/gpt/index.ts`, lines 2182 to 2184). Open question 2 asks about both. What the ad server does with a winner whose size the mapping leaves out was not exercised.

**Slots the publisher defines.** Whoever defines the GPT slot first decides whether the slot has a mapping. Where the publisher defined the slot before `adInit` looked for the slot, Trusted Server reuses the slot and adds no mapping (`tag/gpt_bootstrap.js` lines 700 to 705 and 845 to 851, `js/integrations/gpt/index.ts` lines 1180 to 1184 and 1370 to 1376). The refresh auction does not tell the two cases apart, because `findInjectedSlotForRefresh` (`js/integrations/prebid/index.ts`, line 1173) finds the page data of a slot by element id whoever defined the slot. So a slot that the publisher defined is offered only the band's sizes in the refresh auction, while the ad server request of that slot carries the sizes the publisher defined. Open question 3 asks about this.

**When the band takes effect, and the space for the slot.** The sizes a slot asks for are fixed when its first ad request is made. The first `adInit` of a document runs after the window's `load` event and then two animation frames (`ts.scheduleInitialAdInit`, `tag/gpt_bootstrap.js` lines 329 to 350, and `installScheduleInitialAdInit`, `js/integrations/gpt/index.ts` lines 809 to 833), so the band takes effect after the first paint. No code in this row sets, reserves or removes the space of a slot's container before the advert arrives, so the page's own markup and style decide that space. Where the one container matching a prefix of the slot's id is hidden at the viewport, `adInit` skips the slot and logs a debug message (`tag/gpt_bootstrap.js` lines 816 to 830). A page that has not reserved the space for the band's size therefore moves when the advert arrives, and a container whose band is empty keeps whatever space the page gave it. Open question 1 asks about this.

## 4. Failure, rollout, adapters and privacy

**Failure.** A minimum above a maximum stops the settings from loading, with a message that names the slot, the format and both bounds. A GPT without the size mapping interface maps nothing and says nothing (section 3). The mapping calls have no `try` and `catch` (open question 5).

**Rollout.** A binary released before these fields refuses a format carrying either of them, because `CreativeOpportunityFormat` denies unknown fields (`core/creative_opportunities.rs`, line 1028), and the binary then cannot load its settings. So the binary that reads the fields goes out before any configuration uses them, and the configuration guide says so (`docs/guide/configuration.md`, lines 2792 to 2795). Rolling back reverses the order. A configuration that still names a bound has to be pushed again without the bound before the binary is rolled back, and the guide does not say so. A bundle from before this row ignores `viewport_sizes`, and a bundle from this row that is given page data without `viewport_sizes` asks for every format, so the page data, the head bootstrap and the bundle can be mixed in either order.

A binary that refuses the settings answers every route with the startup error, which is status 500 on the Axum, Cloudflare and Fastly adapters and status 503 on Spin (`startup_error_router` in each adapter's `src/app.rs`). The route `/health` is the exception on two of the four. The Fastly adapter answers `/health` before it reads any settings (`crates/trusted-server-adapter-fastly/src/lib.rs`, line 267) and the Spin adapter registers `/health` on its startup error router (`crates/trusted-server-adapter-spin/src/app.rs`, line 649), so a health check on those two adapters stays green while every other route fails.

**Adapters.** No adapter file changes in this row. All four adapters load their settings through `settings_from_config_blob_with` (`core/config_payload.rs`). In each adapter's own `src/app.rs`, the Cloudflare adapter calls the function at line 191 and the Spin adapter at line 140, and the Fastly adapter at line 238 and the Axum adapter at line 111 call `get_settings_from_config_store_with`, which calls the function at `core/settings_data.rs` line 120. The refusal, the page data and the browser code are therefore the same on each adapter.

**Privacy.** The bands in a slot's page data come from the configuration and the request path alone, so the bands are the same for every reader. The browser reads the viewport width with `window.innerWidth` (`js/integrations/prebid/index.ts`, line 1144) and does not send the width. What goes out is the sizes of the band in the refresh auction, which tells the server and the bidders roughly how wide the viewport is. This row changes no consent check and adds no request, cookie or storage.

## 5. What is not in this change

- **Sizes by anything but viewport width.** The bands are the only dimension a format names, and the viewport of each entry is `[width, 0]`, so the viewport's height, the device's pixel ratio, its orientation and the width of the container take no part.
- **Narrowing the server-side auctions.** Section 3 gives the reason the server-side auctions are offered every format, and open question 2 asks about the difference from the refresh auction.
- **Space for a slot's container.** Section 3 says that the page's own markup and style decide the space, and open question 1 asks whether that is intended.
- **A mapping on a slot the publisher defined.** Open question 3 asks whether the refresh auction should narrow the sizes of such a slot.
- **Choosing a band again when the window is resized.** The refresh auction reads the width each time it runs, the mapping is set once, when Trusted Server defines the slot, and no code of the ad stack refreshes a slot or sets a mapping again when the window is resized.
- **A change to any adapter or to the command line tool.** Open question 6 lists what the command line tool prints.

## 6. Acceptance

**Checks that can be repeated.** In a clone of IABTechLab/trusted-server, this search prints six lines. Three of the six read the bands, being `tag/gpt_bootstrap.js` line 368, `js/integrations/gpt/index.ts` line 868 and `js/integrations/prebid/index.ts` line 1142, and the other three are the type declaration (`js/core/types.ts` line 34) and two comments. So the bands are read in three places in the browser, to define a mapping in the head bootstrap, to define a mapping in the bundle and to choose the refresh auction's sizes, and nothing compares a winning bid's size with a band.

```text
git grep -n viewport_sizes 054f3a2a0 -- crates/trusted-server-js/lib/src crates/ad-tag/google/src/gpt_bootstrap.js
```

In core, this search prints five lines in the tests of `core/creative_opportunities.rs` and one line in `core/publisher.rs` (line 6847), so `build_slot_json` is the only caller of `viewport_sizes()` outside the tests.

```text
git grep -n "viewport_sizes()" 054f3a2a0 -- crates/trusted-server-core/src
```

This search prints nothing, which is why section 3 says that the server reads no viewport width.

```text
git grep -n -i "viewport-width" 054f3a2a0 -- crates docs
```

**CI.** Every check passed at `054f3a2a0`, the head of the pull request, being 25 of 25 on [IABTechLab/trusted-server#1370](https://github.com/IABTechLab/trusted-server/pull/1370) and 19 of 19 on the fork's closed pull request [jwrosewell/trusted-server#24](https://github.com/jwrosewell/trusted-server/pull/24).

**Local checks.** The description of IABTechLab/trusted-server#1370 records `cargo test-fastly` and `cargo test-axum`, `cargo clippy-fastly` and `cargo clippy-axum`, `cargo fmt --all -- --check`, the JavaScript tests and format check, the docs format check and the WebAssembly build of the Fastly adapter as run. The description marks a manual run with `fastly compute serve` as not done. The description also says that the ad server's own handling of a size mapping was not exercised, the tests proving only that the mapping is defined with the bands that the page data carries.

**Tests that exist.**

- Core (`core/creative_opportunities.rs`, lines 1550 to 1673). Five tests of `viewport_sizes` check that a slot with no bound has no bands, that each band holds only the formats that fit it (with the four formats of section 2), that an unbounded format stays in every band, that a band is empty where no format fits, and that neighbouring bands offering the same formats become one. One test checks that a minimum above a maximum is refused, and the test calls the slot's `validate_runtime`. Two tests of the format check that an unbounded format is written without the fields and that both bounds are read as written.
- The page data (`core/publisher.rs`, line 25595). One test checks that a slot without bounds carries no `viewport_sizes`, that a slot with bounds carries them widest first, and that `formats` still lists every size.
- The head bootstrap and the bundle. The test `fallback adInit defines, targets, and displays a TS slot through the command queue` in `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts` and the test `displays TS-defined slots and does not include them in refresh` in `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts` each run with and without bands. With bands, each checks that the mapping is built from the bands in order and applied to the slot, and without bands that no mapping is built. A test in the ad tag crate (`tag/lib.rs`, line 1355) checks that the bootstrap carries the call at the `adInit` site.
- The refresh auction (`crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`, the group `a slot with viewport bands`). Three tests check that a width of 375 pixels offers `[[320, 50]]`, that a width of 1280 pixels offers `[[970, 250]]`, and that a width of 700 pixels, where the band is empty, leaves the slot out of the auction and refreshes the slot through GPT.

**What no test covers.**

1. The two fallback sites that define a slot (`tag/gpt_bootstrap.js` line 707, `js/integrations/gpt/index.ts` line 1186), because the tests of the head bootstrap and the bundle drive `adInit` alone.
2. GPT without `sizeMapping` or `defineSizeMapping`, where a slot with bands is defined with no mapping. Every test that gives bands gives GPT both.
3. The refusal of an inverted range through `Settings::from_toml` or `Settings::from_json_value`. The one test calls the slot's `validate_runtime`, and the loaders reach that function through `prepare_runtime` (section 2).
4. The server-side auctions for a slot with bounds. No test builds an auction request for a slot with a bounded format, so no test pins that every format is offered.
5. Equal bounds, and two bounded formats whose ranges partly overlap.
6. GPT and the ad server acting on a size mapping, an empty size list included. The tests use a stand-in for GPT, and the description records that the ad server's own handling of a size mapping was not exercised.

## 7. Open questions

The questions below are for the reviewer. This revision records what the code does and decides none of the questions. Line numbers and paths follow section 2.

1. **A size applied after the first paint.** Should the size of a slot be settled before the first paint, so that the page does not shift when an advert arrives? The band takes effect when the first ad request is made, after the window's `load` event and two animation frames, and nothing in this row reserves the space of the slot's container for the band's size or removes the container where the band asks for nothing (section 3). No test of this row runs GPT itself, so GPT's response to an empty size list, which is what an empty band becomes in the mapping, was not exercised.
2. **A winning bid outside the band.** Is it intended that the initial-page auction and the page-bids auction are offered every format while the refresh auction is offered only the band's sizes? On a phone a 970x250 bid can win while the mapping allows 320x50, nothing compares the size of a winning bid with the band, and a winner that carries no size of its own is sized as the slot's first configured format (`js/integrations/gpt/index.ts`, lines 2182 to 2184). The server reads no viewport width from either request (section 3).
3. **A slot the publisher defined.** Should the refresh auction narrow the sizes of a slot that carries no mapping because the publisher defined the slot first? Today the bidders are offered only the band's sizes (`js/integrations/prebid/index.ts`, lines 2720 to 2724 and 2762 to 2768) while the ad server request carries the publisher's sizes.
4. **Paths no test covers.** Should tests cover the two fallback sites that define a slot, GPT without the size mapping interface, and the refusal of an inverted range through the settings loaders (section 6)?
5. **Mapping calls with no `try` and `catch`.** The calls that follow `defineSlot` in `defineWithViewportSizes` have no `try` and `catch` (`tag/gpt_bootstrap.js` lines 361 to 383, `js/integrations/gpt/index.ts` lines 858 to 877). An exception from them would skip `addService` and the release of the claim on the placement (`tag/gpt_bootstrap.js` lines 706 to 713, `js/integrations/gpt/index.ts` lines 1185 to 1192), so the slot would be defined and not added to the ad service. Should a failure in the mapping calls leave the slot defined with every size instead? Nothing in this row shows GPT throwing from these calls.
6. **Four places that do not describe the row.** First, the comment on `build_ad_slots_script` (`core/publisher.rs`, lines 7526 to 7529) lists the property names of the page data without `viewport_sizes`. Second, `ts config ad-templates --details` prints each format as its size and media type, without its bounds (`crates/trusted-server-cli/src/commands/config/ad_templates.rs`, lines 490 to 497). Third, in the configuration guide the new section sits between the example of `[creative_opportunities]` and the paragraph that begins "The same switch can be overridden", so that paragraph no longer follows the switch it names (`docs/guide/configuration.md`, lines 2772 to 2797). Fourth, the new section of the guide does not say that the server-side auctions are still offered every format. Should each be corrected?

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| 2026-10-16 | Fixes from review round 1. The header, the pull requests it names and the related specs, the three requests that ask for a slot's sizes and which of them the bands reach, the refresh auction that the second commit of the row (`9c2e6e802`) added after the first draft, what a band is and what an empty band means, the bounds and the page data, slots the publisher defines, when the band takes effect and who holds the space, failure, rollout, rollback, the adapters and privacy, the checks recorded with the claim of deliberate faults removed, the tests that exist and those that do not, the open questions and the review log. |
| 2026-10-16 | Fixes from review round 2. What the page-bids request sends and the line of the Spin health route.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |

## Review log

1. **Round 1, 2026-10-16.** A review in a fresh session, against `split/29-viewport-sizes` at `054f3a2a0`. Issues found, two of them blocking. The blocking issues were that the spec never named the refresh auction, which offers a bidder only the sizes of the band, called the bounds browser only and gave no reason why the server-side auctions are still offered every format, and that its last paragraph claimed four deliberate faults that no description or commit message records. This revision states which requests the bands reach and why the server-side auctions are not among them, what a band is and what an empty band means, which slots get a mapping, when the band takes effect, what a wrong order of rollout and rollback does, what each adapter runs, what the refresh auction tells a bidder about the viewport, the checks recorded and the tests that exist, while six open questions wait for code changes, being the size applied after the first paint, the winning bid outside the band, the slot the publisher defined, the paths no test covers, the mapping calls with no `try` and `catch`, and four places that do not describe the row.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/29-viewport-sizes` at `054f3a2a0`. Issues found, none blocking. This revision says what the page-bids request sends and corrects the line where the Spin adapter registers its health route.
