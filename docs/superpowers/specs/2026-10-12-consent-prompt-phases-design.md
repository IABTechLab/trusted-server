# Design Spec: A consent prompt runs in the phase the publisher chooses

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#16, ready for review, answering IABTechLab/trusted-server#1279. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#16
**Last updated:** 2026-10-12

> **Why this spec exists.** The InMobi, Didomi and Sourcepoint consent prompt modules run in the fetch phase alone, and an entry of the serve phase naming one is refused when the settings load.

## 1. Problem

The InMobi, Didomi and Sourcepoint consent prompt modules run in the fetch phase alone, and an entry of the serve phase naming one is refused when the settings load. What a prompt writes is the same for every reader, so the fetch phase stores it in the page every reader is served from, and that is the right default. A publisher who wants the stored page without it, so that one stored copy can serve readers with and without the prompt, has no way to say so. The modules make the choice for the publisher.

## 2. Design

`cmp.inmobi`, `cmp.didomi` and `cmp.sourcepoint` may be named in a `[[fetch]]` or a `[[serve]]` entry. Each ran in the fetch phase alone, and an entry of the other phase naming one was refused when the settings loaded.

```toml
[[serve]]
media_type = "text/html"
middleware = ["cmp.inmobi"]
```

**Why.** What a consent prompt writes is the same for every reader, so the fetch phase stores it in the page every reader is served from, and that is the phase `ts audit generate` places it in. A publisher who wants the stored page without it, so that one stored copy can serve readers with and without the prompt, places it in the serve phase instead, where it is written on each reader's copy. Which phase is the publisher's choice, and the modules no longer make it for them.

## 3. What is not in this change

- A prompt that answers differently per reader. Each of the three writes the same tag or settings for every reader, which is what lets the fetch phase store it.

## 4. Acceptance

- Each of the three modules declares both phases.
- The InMobi module selected with no entry of either phase naming it is refused, as before, and an entry of the serve phase alone is accepted and registers the prompt for that phase.
- The stock list keeps each prompt in the fetch phase, and its test allows a middleware that runs in several phases to be listed in one of them, checking that what is listed for a phase is registered for it and that everything registered is listed somewhere.

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 4 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
