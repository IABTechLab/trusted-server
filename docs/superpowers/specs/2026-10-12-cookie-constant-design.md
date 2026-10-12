# Design Spec: The Model Terms cookie name is declared by the module that reads it

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#12, ready for review, answering IABTechLab/trusted-server#1275. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-07-30-permission-model-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#12
**Last updated:** 2026-10-12

> **Why this spec exists.** `COOKIE_MTM_PREF`, the name of the cookie a preference platform writes the Model Terms for Marketing preference into, sits in core's constants.

## 1. Problem

`COOKIE_MTM_PREF`, the name of the cookie a preference platform writes the Model Terms for Marketing preference into, sits in core's constants. It is not in `main` today. It arrived with the stack's own Model Terms signal module, in the pull request that adds the module documentation set (#1047), so this corrects a slip the stack made earlier in itself rather than anything in `main`, and the reason is elegance alone, that core should name no one implementation's cookie. The Model Terms permission signal module, `crates/permission-signal/mtm`, is the only code that reads it. The cookie is particular to that one implementation of a permission signal. It is not a standard core supports, and nothing in core should know its name.

## 2. Design

| **The commit.** | Commit                                                            | What it does |
| --------------- | ----------------------------------------------------------------- | ------------ |
| f2086639f       | Moves the constant from `trusted-server-core` to the signal crate |

The whole is 3 files, 8 lines added and 9 removed.

## 3. What is not in this change

Nothing beyond the design above. A deployment whose settings do not select the change behaves as before.

## 4. Acceptance

The pull request's description records how it was checked, with CI green on all 19 checks.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
