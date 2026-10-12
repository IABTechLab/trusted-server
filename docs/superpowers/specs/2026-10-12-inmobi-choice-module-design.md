# Design Spec: The InMobi Choice consent prompt as a module

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#13, ready for review, answering IABTechLab/trusted-server#1276. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#13
**Last updated:** 2026-10-12

> **Why this spec exists.** The repository carries consent prompt modules for Didomi, Sourcepoint and Osano, each changing a page in its own way, and none for InMobi Choice, the TCF consent prompt a publisher loads from their own InMobi account.

## 1. Problem

The repository carries consent prompt modules for Didomi, Sourcepoint and Osano, each changing a page in its own way, and none for InMobi Choice, the TCF consent prompt a publisher loads from their own InMobi account. Nothing states what a consent prompt module is held to, so each one decides for itself where its tag goes, whether the IAB interfaces are stubbed before its loader runs, and how a reader's answer reaches the permission model.

## 2. Design

`crates/cmp/inmobi`, a consent management platform module for InMobi Choice, the TCF consent prompt a publisher loads from their own InMobi account.

```toml
[cmp]
module = "inmobi"

[cmp.inmobi]
script_url = "https://cmp.inmobi.com/choice/<account>/<site>/choice.js?tag_version=V3"
# cmp_id = 10

[[fetch]]
media_type = "text/html"
middleware = ["cmp.inmobi", "tag.google-tag-manager"]
```

Where a `[[fetch]]` entry names `cmp.inmobi`, the module's middleware writes three things into the head, in this order, and the stored page every reader is served from carries them.

1. The IAB TCF v2 stub, which creates the `__tcfapiLocator` frame, queues every `__tcfapi` call made before the prompt arrives, answers `ping` as a stub, and relays calls posted from frames.
2. InMobi's GPP stub, which answers `ping` and the other generic `__gpp` commands at once with the CMP id, queues every other call for the prompt, creates the `__gppLocator` frame, and relays calls posted from frames.
3. The account's Choice loader, deferred, so a slow consent vendor never holds up the page and the loader never runs ahead of a stub the publisher's own head carries.

Each stub defines nothing where the page already has its interface. The module serves no browser module, because the stubs and the Choice script are the whole client side, and the prompt writes the reader's answer to the cookie the TCF permission signal reads, so the answer reaches the permission model on the next request the way every signal does.

- A `script_url` that is empty, is not a URL, is not `https`, is on a host other than `cmp.inmobi.com` or `cdn.inmobi.com`, or carries a user name or password. The value becomes a `<script src>` on every page, and a settings document reaches the service from a config store rather than from a person reading it. A host that merely contains an allowed name is refused by what it is rather than by what it looks like.
- A document that selects the prompt and names it in no entry, because the prompt would never be written.
- A field in `[cmp.inmobi]` the module does not read.

**What a consent prompt module keeps to.** The guide page states six things this module keeps to, and they are what every `[cmp]` module is held to: the prompt is written before any vendor's tag, the IAB interfaces are stubbed ahead of the loader, the loader is deferred, the reader's answer reaches the permission model through the permission signal modules and no other path, a script address written into every page is checked at load, and a selected prompt that no entry places is refused.

**Maintainers.** The crate's manifest names 51Degrees as its maintainers, with the status `seeking vendor owner`, until InMobi adopt it. With it the stock build has 1 crate `vendor owned`, 19 `seeking vendor owner` and 4 `project owned`.

**CI.** The two stubs are tested in a stand-in for the browser with Node's test runner and no packages, as `tests/*.test.mjs` beside the module. The vitest job now runs every module's such tests after the browser bundle's.

## 3. What is not in this change

- The other consent management platform modules brought to what the guide page states. Didomi, Sourcepoint and Osano each change a page in their own way today, and bringing them to the same rules is a change to each of them, after this one.
- A refresh of the page's permission state when the reader answers. The answer is read on the next request, as every permission signal is.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 17 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
