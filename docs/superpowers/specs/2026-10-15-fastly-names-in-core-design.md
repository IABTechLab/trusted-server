# Design Spec: The last Fastly names leave core

**Status:** Proposed, 2026-10-15. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-last-names-in-core-design.md`, `2026-10-14-backend-naming-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#777 (its Fastly row)
**Last updated:** 2026-10-15

> **Why this spec exists.** After the series before this one, core still names one hosting platform, Fastly, in small pieces. They are the format of a long configuration document, a staging variable, three request headers, a device module selector and the names of two functions. Each is a place where a change by one platform is a change in core.

## 1. Problem

These names remain in core, each with the code that reads it:

| What core names                                                        | Where                                                         |
| ---------------------------------------------------------------------- | ------------------------------------------------------------- |
| The Fastly config store's format for a document too long for one entry | `settings_data.rs`                                            |
| Fastly's staging variable                                              | `constants.rs`, the DataDome module                           |
| Fastly's TLS, client address and debug request headers                 | `http_util.rs`, `publisher.rs`, `settings.rs`, `constants.rs` |
| The `fastly` device module selector                                    | `ec/device.rs`, `integrations/registry.rs`, `settings.rs`     |
| Fastly in the names of the asset proxy's response conversions          | `proxy.rs`                                                    |

Each name is small. Each one means that another platform, or a change in how Fastly stores, flags or forwards something, is an edit to core rather than to the adapter beside it.

## 2. Design

Each name moves to the adapter that reads it, and core keeps a neutral declaration or interface in its place. The settings a deployment writes do not change, except the device selector.

### 2.1 A platform's config store returns a long document whole

A Fastly config store entry holds at most 8,000 characters, so a longer settings document is written as chunks, with a pointer at its key naming each chunk's key, length and hash. Core's settings loader recognized the pointer and read the chunks itself.

**The interface.** `PlatformConfigStore::get` returns an entry whole. The Fastly adapter's store follows a pointer, reads each chunk and checks every length and hash before it returns the document, so core reads what any platform's store returns.

**What changes for an operator.** Nothing. A long document reads as before, and one whose chunks disagree with the pointer is refused as before.

### 2.2 Later rows

The staging variable, the request headers, the device selector and the conversion names follow in the same series, each with its own section here as it lands.

## 3. What is not in this change

- Comments that mention Fastly, in about 25 of core's files, and the names and assertion messages of some tests, the publisher's surrogate cache tests among them. None is a shipped line of core.
- The Axum, Cloudflare and Spin adapters' comments comparing their behavior with the Fastly adapter's, and their two errors that suggest the Fastly adapter for parallel bidder fan-out.
- The `ts` development proxy's removal of `fastly-ssl`.

## 4. Tests

- The Fastly store returns a document that fits, and other JSON at the key, as they are, reads a chunked document back whole, and refuses a pointer whose lengths disagree with the document and a chunk whose hash disagrees.

## Review log

No review rounds yet.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-15 | First draft. |
