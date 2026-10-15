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

### 2.2 The platform says whether it runs in a staging environment

Core named Fastly's `FASTLY_IS_STAGING` variable, and the DataDome module read it so that its test bypass works only on a staging version.

**The interface.** An adapter declares whether its platform runs the service in a staging environment with `platform::declare_platform_staging`, and a module asks `platform::platform_is_staging`, which answers false until an adapter declares otherwise.

**What each adapter declares.** The Fastly adapter declares staging when `FASTLY_IS_STAGING` is `1`. The Axum, Cloudflare and Spin adapters declare nothing.

**What changes for an operator.** Nothing on Fastly. On the other adapters the DataDome bypass no longer follows a `FASTLY_IS_STAGING` variable.

### 2.3 The platform names the request headers it sets

Core read Fastly's `fastly-ssl` header for the scheme and kept it from the origin, stripped `fastly-ssl` and `fastly-client-ip` as headers a client could send, accepted `fastly-client-ip` by name as a trusted client address header, and listed `x-debug-fastly-pop` among the headers never passed to a third party.

**The interface.** An adapter declares three lists before it serves.

- `platform::declare_platform_tls_headers` names a header its platform sets on a request that arrived over TLS. Core reads it for the scheme after the TLS status the adapter reports and the standard forwarding headers, and removes it from every request to an origin.
- `platform::declare_platform_spoofable_headers` names the platform's other headers a client could also send. `sanitize_forwarded_headers` strips them, and the TLS headers, with the three standard forwarding headers.
- `platform::declare_platform_internal_headers` names headers never passed to a third party. `constants::is_internal_header` answers for core's fixed list and these together, and the trusted client address settings refuse any of them.

The Fastly adapter makes its declarations in `app::declare_platform()`, which `run()` calls before it serves and every state build also calls, because settings are validated as they load.

A trusted client address header must start with `x-` or end with `-client-ip`, which is the shape a platform's own client address header takes, because `ts config validate` checks the settings where no adapter has declared anything.

**What each adapter declares.** The Fastly adapter declares `fastly-ssl`, `fastly-client-ip` and `x-debug-fastly-pop`, and strips its own two at entry by name. The Axum, Cloudflare and Spin adapters declare nothing.

**What changes for an operator.** Nothing on Fastly, where `ip_header = "fastly-client-ip"` stays valid. On the Axum, Cloudflare and Spin adapters a client's `fastly-ssl` is no longer removed from the request to the publisher's origin, and on Spin a client's `fastly-ssl` and `fastly-client-ip` are no longer stripped at entry. None of these adapters reads either header.

### 2.4 The platform's own device module is `platform`

`[device] module = "fastly"` selected the module the Fastly adapter builds from its TLS and HTTP/2 signals, so core's device selection and the registry both named Fastly.

**The interface.** `[device] module = "platform"` selects the platform's own module, as `[geo] module = "platform"` selects the platform's geo lookup, and `build_device_module` builds it through the `build_platform` factory the adapter passes.

**What changes for an operator.** A document that wrote `module = "fastly"` now writes `module = "platform"`. The old value is refused at startup as a device module no running module supplies.

### 2.5 Names that were not Fastly's

`platform_response_to_fastly` returned a buffered response and `platform_response_to_fastly_asset` an asset response that keeps a stream, and neither touched Fastly. They are `platform_response_to_buffered` and `platform_response_to_asset`.

**What changes for an operator.** Nothing. Both are internal to core.

## 3. What is not in this change

- Comments that mention Fastly, in about 25 of core's files, and the names and assertion messages of some tests, the publisher's surrogate cache tests among them. None is a shipped line of core.
- The Axum, Cloudflare and Spin adapters' comments comparing their behavior with the Fastly adapter's, and their two errors that suggest the Fastly adapter for parallel bidder fan-out.
- The `ts` development proxy's removal of `fastly-ssl`.

## 4. Tests

- The Fastly store returns a document that fits, and other JSON at the key, as they are, reads a chunked document back whole, and refuses a pointer whose lengths disagree with the document and a chunk whose hash disagrees.
- The DataDome test bypass applies while the platform declares staging and not otherwise.
- A declared TLS header sets the scheme and an undeclared one does not, the declared TLS and spoofable headers are stripped with the standard ones, the auction sees the client's declared TLS header where the origin request does not, a declared internal header is refused as a trusted client address header, and a bare `client-ip` is refused where `example-client-ip` is accepted.
- The `platform` device selector builds the module the adapter's factory supplies, and no selector builds the built-in one.
- The asset proxy's tests pass unchanged through the renamed conversions.

## Review log

No review rounds yet.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-15 | First draft. |
