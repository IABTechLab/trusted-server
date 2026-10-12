# Design Spec: A format can name the viewport widths at which the page asks for it

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#24, ready for review, answering IABTechLab/trusted-server#1287. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#24
**Last updated:** 2026-10-12

> **Why this spec exists.** A slot's formats are the sizes a page asks for, and the page asks for all of them at every width.

## 1. Problem

A slot's formats are the sizes a page asks for, and the page asks for all of them at every width. A page that shows a billboard on a wide screen and a small banner on a phone has one slot and several formats, so a bidder is offered a billboard the page cannot show, and the ad tag defines the slot with every size at every width.

## 2. Design

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

A format may name the viewport widths, in CSS pixels, at which the page asks for it, so a slot asks for a large size only where the page has room for one. Either bound may be left out, and a minimum above a maximum is refused when the settings load. When any format of a slot names a bound, the slot's page data carries `viewport_sizes`, one entry for each band of widths, widest first, and the Google ad tag's head bootstrap and the bundle's slot definition apply it as a size mapping when they define the slot. A slot whose formats name no bound asks for every format at every width, as before. The bounds apply in the browser only, and a server-side auction is still offered every format.

A binary released before these fields refuses a format carrying either of them, which fails every request, so the binary that reads them is promoted before any configuration uses them. The configuration guide says so.

**Why.** A page that shows a billboard on a wide screen and a small banner on a phone has one slot and several formats, and without bounds the slot asks for every format at every width, so a bidder is offered a billboard the page cannot show. The bounds are the publisher's statement of what fits where, and the ad tag passes them to the ad server as its size mapping.

## 3. What is not in this change

- Sizes by anything but viewport width. The bands are the only dimension a format names.

## 4. Acceptance

The pull request's description records how it was checked, with CI green on all 19 checks, and 4 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
