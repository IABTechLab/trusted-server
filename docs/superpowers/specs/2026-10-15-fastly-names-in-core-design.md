# Design Spec: The last Fastly names leave core

**Status:** Proposed, 2026-10-15. Raised as pull request IABTechLab/trusted-server#1399 (from jwrosewell/trusted-server#44), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-last-names-in-core-design.md`, `2026-10-14-backend-naming-design.md`
**Related PRs:** IABTechLab/trusted-server#1399, jwrosewell/trusted-server#44
**Issues:** IABTechLab/trusted-server#777 (its Fastly row)
**Last updated:** 2026-10-26

> **Why this spec exists.** After the series before this one, core's shipped code still names one hosting platform, Fastly, in small pieces. They are the format of a long configuration document, a staging variable, three request headers, a device module selector and the names of two functions. Each is a place where a change by one platform is a change in core. Core's comments, tests and README files name Fastly, Cloudflare, Spin, Fermyon, Viceroy and Axum as well, and three tests use a host under Fastly's `edgecompute.app` domain. After this change no line of core names a hosting platform, and a guard outside core keeps it so (section 2.6). One Fastly header, `x-compress-hint`, whose name does not name the platform, is still set by core after this change (section 3).

## 1. Problem

These names remain in core, each with the files that hold it:

| What core names                                                        | Where                                                                             |
| ---------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| The Fastly config store's format for a document too long for one entry | `settings_data.rs`                                                                |
| Fastly's staging variable                                              | `constants.rs`, the DataDome module                                               |
| Fastly's TLS, client address and debug request headers                 | `http_util.rs`, `publisher.rs`, `settings.rs`, `constants.rs`                     |
| The `fastly` device module selector                                    | `ec/device.rs`, `integrations/registry.rs`, and the doc comments of `settings.rs` |
| Fastly in the names of the asset proxy's response conversions          | `proxy.rs`                                                                        |

Each name is small. Each one means that another platform, or a change in how Fastly stores, flags or forwards something, is an edit to core rather than to the adapter beside it.

Core's comments, doc comments, tests and README files named platforms too. Each told a reader that core knows which platform runs it or serves its origin. Section 2.6 counts them.

## 2. Design

Each name moves to the adapter that reads it, and core keeps a neutral declaration or interface in its place. The settings a deployment writes do not change, except the device selector.

### 2.1 A platform's config store returns a long document whole

A Fastly config store entry holds at most 8,000 characters, so a longer settings document is written as chunks, with a pointer at its key naming each chunk's key, length and hash. Core's settings loader recognized the pointer and read the chunks itself.

**The interface.** `PlatformConfigStore::get` returns an entry whole. The Fastly adapter's store follows a pointer, reads each chunk and checks every length and hash before it returns the document, so core reads what any platform's store returns.

**What each adapter does.** The Fastly adapter's store is the only one that follows a pointer. The Axum adapter loads its settings through the same core function (`get_settings_from_config_store_with`) from a store that reads an environment variable, so it followed a pointer before this change and now reads the entry as it finds it. The Cloudflare and Spin adapters load their settings from the blob they read (`settings_from_config_blob_with`) and never followed a pointer.

**What changes for an operator.** A long document reads as before on Fastly, and one whose chunks disagree with the pointer is still refused. The detail of the refusal moves. Core's loader put the detail in the message of its own `Configuration` error. The Fastly store now attaches the detail to a config store error, which the loader wraps in a general `Configuration` error saying that the key could not be read. The detail therefore shows when the error is printed with `{:?}`, as the Fastly adapter does when it logs that it failed to build its state, and not in the error's `to_string()`, which the removed core test read. A refused document stops the service from building its application, so every route that reads the document answers 500 with `An internal error occurred`, apart from `/health`, which is answered before any settings are read.

### 2.2 The platform says whether it runs in a staging environment

Core named Fastly's `FASTLY_IS_STAGING` variable, and the DataDome module read it so that its test bypass works only on a staging version.

**The interface.** An adapter declares whether its platform runs the service in a staging environment with `platform::declare_platform_staging`, and a module asks `platform::platform_is_staging`, which answers false until an adapter declares otherwise. `ENV_FASTLY_IS_STAGING` is gone from core, and a crate outside core that used it stops building.

**What each adapter declares.** The Fastly adapter declares staging when `FASTLY_IS_STAGING` is `1`. The Axum, Cloudflare and Spin adapters declare nothing.

**What changes for an operator.** Nothing on Fastly. On the Axum, Cloudflare and Spin adapters the DataDome bypass no longer follows a `FASTLY_IS_STAGING` variable and stays inactive, because none of them declares a staging environment. The warning DataDome logs when a bypass is configured and inactive now says the platform does not run the service in its staging environment, where it said `FASTLY_IS_STAGING` is not 1. The DataDome guide (`docs/guide/integrations/datadome.md`) names the condition in the same terms.

### 2.3 The platform names the request headers it sets

Core read Fastly's `fastly-ssl` header for the scheme and kept it from the origin, stripped `fastly-ssl` and `fastly-client-ip` as headers a client could send, accepted `fastly-client-ip` by name as a trusted client address header, and listed `x-debug-fastly-pop` among the headers never passed to a third party.

**The interface.** An adapter declares three lists before it serves.

- `platform::declare_platform_tls_headers` names a header its platform sets on a request that arrived over TLS. Core reads it for the scheme after the TLS status the adapter reports and the standard forwarding headers, and removes it from the request to the publisher's origin (`rewrite_origin_request` and `handle_publisher_request` in `publisher.rs`). The asset proxy and the first-party proxy build their origin requests from fixed lists of headers to copy (`ASSET_PROXY_FORWARD_HEADERS` and `PROXY_FORWARD_HEADERS` in `proxy.rs`), and neither list holds a TLS header.
- `platform::declare_platform_spoofable_headers` names the platform's other headers a client could also send. `sanitize_forwarded_headers` strips them, and the TLS headers, with the three standard forwarding headers.
- `platform::declare_platform_internal_headers` names headers never passed to a third party. `constants::is_internal_header` answers for core's fixed list and these together, and the trusted client address settings refuse any of them.

The Fastly adapter makes its declarations in `app::declare_platform()`. `run()` calls the function before the adapter serves, which is before the first settings load and the validation that refuses a declared internal header. Every state build calls the function again, so a state built without `run()` is declared the same, and declaring twice changes nothing.

A trusted client address header must start with `x-` or end with `-client-ip`. That is the shape of Fastly's `fastly-client-ip`. Another platform's own client address header can take another shape, as Cloudflare's `cf-connecting-ip` does, and was refused before this change and is refused now. The rule is a pattern because `ts config validate` checks the settings where no adapter has declared anything. The pattern accepts any name ending `-client-ip`, where only `fastly-client-ip` was accepted before.

**What reads the declarations.** Each declared list has these readers in production.

| Declared list             | What reads it                                                                                                                                                | Runs on               |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------- |
| TLS headers               | `detect_request_scheme` in `http_util.rs`, for the scheme                                                                                                    | Every adapter         |
| TLS headers               | `remove_platform_tls_headers` in `publisher.rs`, for the request to the publisher's origin                                                                   | Every adapter         |
| TLS and spoofable headers | `sanitize_forwarded_headers` in `http_util.rs`, for the strip at entry                                                                                       | The Spin adapter only |
| Internal headers          | `constants::is_internal_header`, for the copy of a client's `x-` headers to a third party by Permutive and Lockr, and for the check of `[trusted_client_ip]` | Every adapter         |

The Spin adapter is the only adapter that calls `sanitize_forwarded_headers` (`normalize_spin_request` in its `app.rs`), and it declares nothing. The Fastly adapter does not call the function. Its entry point strips with `sanitize_fastly_forwarded_headers` in its `compat.rs`, which removes the three standard headers and its own `fastly-ssl` and `fastly-client-ip` by name and does not read the declared lists. The Axum and Cloudflare adapters call neither function. So the one production call that reads the spoofable list is on an adapter that declares nothing, and the Fastly adapter's declaration of `fastly-client-ip` as spoofable is read by nothing. An adapter that declares a TLS or spoofable header gets the header stripped at entry only if the adapter calls `sanitize_forwarded_headers`, so declaring is not enough on its own.

**What each adapter declares.** The Fastly adapter declares `fastly-ssl` as a TLS header, `fastly-client-ip` as a spoofable header and `x-debug-fastly-pop` as an internal header. The Axum, Cloudflare and Spin adapters declare nothing.

**What changes for an operator.** On Fastly the scheme, the strip at entry, the removal from the origin request and the keeping of `x-debug-fastly-pop` from third parties all behave as before, and `ip_header = "fastly-client-ip"` stays valid. One refusal moves. `ts config validate` no longer refuses `x-debug-fastly-pop` as an `ip_header` or `auth_header` name, and the Fastly service refuses it when it loads the document (see "Where a refusal happens" below). On the Axum, Cloudflare and Spin adapters, which declare nothing, these change.

1. On the Axum and Cloudflare adapters a client's `fastly-ssl` is no longer removed from the request to the publisher's origin.
2. On the Axum and Cloudflare adapters a request that carries `fastly-ssl: 1` and no scheme in `Forwarded` or `X-Forwarded-Proto` is no longer taken to be HTTPS. Neither adapter reports a TLS status (the `ClientInfo` each builds in its `platform.rs` carries none) and neither strips a client's forwarding headers, so core read the header there as the scheme and a client could send it. The scheme is now `http` unless one of those two headers says otherwise. The Spin adapter's scheme is unchanged, because Spin removed the header at entry and takes the scheme from the runtime URL it trusts.
3. On the Spin adapter a client's `fastly-ssl` and `fastly-client-ip` are no longer stripped at entry, so both now reach the publisher's origin with the rest of the client's headers, except that `fastly-client-ip` is still removed where `[trusted_client_ip]` names it as `ip_header`. The request middleware of each of the Axum, Cloudflare and Spin adapters removes the configured `ip_header` and `auth_header` (`middleware.rs`), before this change and after, so nothing changes for `fastly-client-ip` on the Axum and Cloudflare adapters.
4. On the Axum, Cloudflare and Spin adapters a client's `x-debug-fastly-pop` is no longer kept from third parties. Permutive and Lockr copy a client's `x-` headers that are not internal, so both now pass a client-sent copy on, and the header is no longer refused as an `ip_header` or `auth_header` name. Only a client can send the header on these adapters, so the effect is small.

**Where a refusal happens.** The settings check refuses a declared internal header named as `ip_header` or `auth_header`, and an `ip_header` that neither starts with `x-` nor ends with `-client-ip`. The second refusal needs no declaration, so `ts config validate` applies it. The first depends on what the running adapter declared, and `ts config validate` runs with no adapter's declarations, so a document that names `x-debug-fastly-pop` passes `ts config validate` and is refused when the Fastly service loads the document. Before this change `ts config validate` refused the name, because the fixed list held it. A refused document stops the service from building its application, so every route that reads the document answers 500 with `An internal error occurred`, apart from `/health`, and the log carries the reason.

**For crates outside core.** `SPOOFABLE_FORWARDED_HEADERS` holds the three standard forwarding headers and no longer holds `fastly-ssl` and `fastly-client-ip`, and `INTERNAL_HEADERS` no longer holds `x-debug-fastly-pop`. A crate that reads either constant still builds and now sees the shorter list. To see what the running platform added, the crate reads `platform::platform_tls_headers` and `platform::platform_spoofable_headers`, or calls `constants::is_internal_header`. `HEADER_X_DEBUG_FASTLY_POP` is gone, and a crate that used it stops building.

**Open questions.** Both ask for a code change, and this design chooses neither.

1. Should the Fastly adapter's `sanitize_fastly_forwarded_headers` read the declared TLS and spoofable lists, so that what an adapter declares and what it strips cannot differ? The other choice is for the spoofable list to leave the declarations, so that each adapter strips its own headers by name as the Fastly adapter does today.
2. Is it acceptable that the Axum, Cloudflare and Spin adapters pass a client-sent `x-debug-fastly-pop` to third parties? The other choice is to keep it from third parties on every adapter, as before this change.

### 2.4 The platform's own device module is `platform`

`[device] module = "fastly"` selected the module the Fastly adapter builds from its TLS and HTTP/2 signals, so core's device selection and the registry both named Fastly.

**The interface.** `[device] module = "platform"` selects the platform's own module, as `[geo] module = "platform"` selects the platform's geo lookup, and `build_device_module` builds it through the `build_platform` factory the adapter passes.

**What each adapter does.** The Fastly adapter is the only one that calls `build_device_module`, and its factory builds the Fastly device module from the request's TLS and HTTP/2 signals. The Axum, Cloudflare and Spin adapters never call the function, so `module = "platform"` is accepted there and selects nothing, as `module = "fastly"` was before.

**What changes for an operator.** A document that wrote `module = "fastly"` now writes `module = "platform"`. The registry refuses the old value as a device module no running module supplies, when the service builds its modules as it starts. `ts config validate` does not check the device selector, so it passes a document that still writes `fastly`, and the service then answers 500 on every route that reads the document, apart from `/health`, as it does for any refused document.

**Rollout.** The two values do not overlap. A package from before this change refuses `platform` and a package from this change refuses `fastly`, each at startup, and `ts config validate` passes either value. The package and the document therefore have to change together, and the live change needs the same pairing because the active version reads one document. A deployment can try the pair first on a staged Fastly version, since `ts deploy --staging` and `ts config push --staging` give the staged version a document of its own that the active version does not read (`docs/guide/cli.md`). A deployment that selects no device module, or `builtin`, changes nothing.

### 2.5 Names that were not Fastly's

`platform_response_to_fastly` returned a buffered response and `platform_response_to_fastly_asset` an asset response that keeps a stream, and neither touched Fastly. They are `platform_response_to_buffered` and `platform_response_to_asset`.

**What changes for an operator.** One error message changes with the names. The `Proxy` error for a streaming body in the buffered conversion (`proxy.rs`) now reads `streaming platform response body is not supported by buffered response conversion`, where it named a Fastly response conversion. A `Proxy` error answers 502 with `An internal error occurred`, so the message reaches the log and not the response. Both functions are internal to core.

### 2.6 Core names no hosting platform

Core is built on EdgeZero and names no hosting platform in any line, whether code, comment, test, document or file name. EdgeZero's own names stay, because EdgeZero is the platform layer core is built on. An adapter may name its own platform.

**What changes.**

- Comments and doc comments say what they meant without the platform's name, for example a streaming adapter, an adapter whose platform has a shared cache, or a platform whose `send_async` runs each request eagerly. Where one cited a platform SDK's source, the adapter that maps core's type onto that SDK already documents it (`apply_fastly_cache_intent` in the Fastly adapter's `platform.rs`).
- The `Surrogate-Control` tests in `publisher.rs` test the standard header core reads for every platform, so their names and assertion messages say `Surrogate-Control`, or edge freshness, where they said Fastly. The wildcard exclusion test in `creative.rs` excludes `*.cdn.example.com` where it excluded a platform's domain. A registry test name, a publisher test name and a stub's assertion message drop the platform they named.
- The test that the largest `KvMetadata` serializes within 2048 bytes moves to the tests of the Fastly adapter's store (`ec_kv.rs`), because the limit is the Fastly KV Store's.
- The test that core's code never uses the Fastly SDK moves from core's `migration_guards.rs` to the Fastly adapter's `core_sdk_guard.rs`. A build script in the adapter embeds every Rust source under core's `src` by path, so a file added to core is checked with no edit to the test. Core's build script no longer lists its own sources.
- Core's README and the auction README show the command that runs core's tests on the host and point at `TESTING.md` for the adapter aliases, where they listed each alias by its platform.
- Three HTML processor tests use `test-publisher-ts.edge.example` as the request host, where they used a host under `edgecompute.app`.
- `scripts/lint-core-platform-names.py` fails on any line or path under `crates/trusted-server-core` that names a hosting platform, a tool named for one, or a domain a platform serves deployments from. It splits text into words at every character that is not a letter and at each change from lower to upper case, so `FASTLY_IS_STAGING` and `edgezero_adapter_cloudflare` each name a platform. Six names, among them `spin`, `axum` and `aws`, count only as words of their own, because ordinary words such as `spinner` and `withdraws` contain them. Eighteen, among them `fastly`, `cloudflare`, `akamai`, `vercel` and `netlify`, also count at the start of a word, so `akamaized` counts. Sixteen domains, among them `edgecompute.app`, `workers.dev` and `fermyon.app`, count where they are not part of a longer host name. It runs its matcher on every name and domain and on known non-names before it reads core, and stops with exit 2 when the matcher is wrong. The Run Format workflow runs it as a job of its own, and `AGENTS.md` lists it among the CI gates.

**The count.** Lines of `crates/trusted-server-core` that name each platform on this branch before and after this change, counted as a whole word with `git grep -i -w`, and with the guard, which also counts a name inside an identifier such as `FASTLY_IS_STAGING`.

| Name                       | `git grep -i -w` before (files) | Guard before | After, either count |
| -------------------------- | ------------------------------- | ------------ | ------------------- |
| Fastly                     | 100 (32)                        | 110          | 0                   |
| Cloudflare                 | 26 (14)                         | 28           | 0                   |
| Axum                       | 27 (14)                         | 27           | 0                   |
| Spin                       | 21 (10)                         | 23           | 0                   |
| `edgecompute.app`          | 5 (1)                           | 5            | 0                   |
| Viceroy                    | 4 (3)                           | 4            | 0                   |
| Fermyon                    | 1 (1)                           | 1            | 0                   |
| Every other name or domain | 0                               | 0            | 0                   |

At `13d57b03f`, this row's last commit before its comments, documents and tests were changed, the guard finds 173 lines. One is the `_vercel` entry of the attestation prefixes, which core no longer has, and the other 172, some of them naming two platforms, are comments, documents or test code. At this row's head the guard finds none.

**What changes for an operator.** Nothing that core does when it runs changes.

**For crates outside core.** Nothing in core's API changes. A change that adds a hosting platform's name to core, in a comment or a test as much as in code, fails the guard. The Fastly adapter gains a build script and a `regex` dev-dependency for the test of core's sources.

## 3. What is not in this change

- The `x-compress-hint` header, a Fastly compression hint. Core sets it to `on` on the script bundle responses it serves (`HEADER_X_COMPRESS_HINT` in `constants.rs`, used in `publisher.rs`) and lists it among the headers never passed to a third party (`constants.rs`). The header's name does not name the platform, so the guard of 2.6 passes it.
- The comment at the top of `platform/request_headers.rs`, which says a TLS or spoofable header is removed at entry, without the precondition in 2.3 that the adapter calls `sanitize_forwarded_headers`. It is a code comment and stays for a later change.
- The Axum, Cloudflare and Spin adapters' comments comparing their behavior with the Fastly adapter's, and their two errors that suggest the Fastly adapter for parallel bidder fan-out.
- The `ts` development proxy's removal of `fastly-ssl`.

## 4. Tests

- The Fastly adapter's `whole_entry` returns a document that fits, and other JSON at the key, as they are, reads a chunked document back whole, and refuses a pointer whose lengths disagree with the document and a chunk whose hash disagrees.
- The DataDome test bypass applies while the platform declares staging and not otherwise.
- A declared TLS header sets the scheme and an undeclared one does not, the declared TLS and spoofable headers are stripped with the standard ones, the auction sees the client's declared TLS header where the origin request does not, a declared internal header is refused in the `auth_header` slot of `[trusted_client_ip]`, and a bare `client-ip` is refused where `example-client-ip` is accepted.
- The `platform` device selector builds the module the adapter's factory supplies, and no selector builds the built-in one.
- The asset proxy's tests pass unchanged through the renamed conversions.
- The guard of 2.6 passes on core after this change and fails on core before it. Each of its 40 names and domains, planted once on a line of its own in a file git does not yet track, fails it on that line, and so does the `edgecompute.app` test host put back where it was. It passes `spinner`, `SpinyAI`, `steadfastly`, `denominator`, `withdraws`, `EdgeZero`, `notworkers.dev` and `butterfly.io`. With its word pattern or its domain pattern broken to match nothing, it stops with exit 2 before it reads core.
- The Fastly adapter checks that core's code never uses the Fastly SDK, that the list of core's sources it embeds covers core, and that the largest `KvMetadata` fits the Fastly KV Store's 2048-byte metadata limit.

Not tested:

- `FastlyPlatformConfigStore::get`. The tests call `whole_entry` with a reader of their own, and `get` needs Fastly's config store, so nothing proves `get` passes each entry through `whole_entry`.
- That the Fastly adapter makes its declarations in `declare_platform()`. No test reads the lists back from the Fastly adapter, and core's tests declare example names of their own, so deleting a declaration passes every test. Deleting the TLS declaration sends `fastly-ssl` to the publisher's origin and stops core reading it for the scheme, deleting the internal declaration sends a client's `x-debug-fastly-pop` to third parties, and deleting the staging declaration switches the DataDome bypass off on Fastly staging.
- A declared internal header in the `ip_header` slot. The one test uses the `auth_header` slot, and one loop checks both slots.
- Any change on the Axum, Cloudflare and Spin adapters listed in 2.1 to 2.4. No adapter test sends a client's `fastly-ssl`, `fastly-client-ip` or `x-debug-fastly-pop` to those adapters, or selects `module = "platform"` on them.
- The strip at entry of a declared header through an adapter. Core's test declares example headers and calls `sanitize_forwarded_headers` directly, and the one adapter that calls the function declares none.
- The guard's job in the Run Format workflow, which only a pull request's checks run.
- That the guard's list of names is complete. It holds the platforms the stack has adapters or tools for and other common edge, CDN and hosting platforms, so a platform outside the list has to be added to it.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-15 | First draft.                                                                                                                                                                                                                                                                                                          |
| 2026-10-15 | Fixes from review round 1. The readers of each declared list and the open questions on them, `x-debug-fastly-pop`, the scheme change, the effects on the other adapters, where each refusal happens, the rollout, the narrowed constants, `x-compress-hint`, the file count, the untested paths and the review log.   |
| 2026-10-15 | Corrections from review round 2. The Fastly sentence of 2.3 and the changelog on the `x-debug-fastly-pop` refusal, the changelog on the spoofable strip and the scheme change, when the Axum, Cloudflare and Spin adapters strip `fastly-client-ip`, and the comment in `request_headers.rs` left for a later change. |
| 2026-10-16 | Core names no hosting platform or platform domain in any line. Section 2.6 with the counts, the `_vercel` prefix and the test host, the guard and the tests that moved to the Fastly adapter, and section 3 no longer leaves core's comments and tests for later.                                                     |
| 2026-10-26 | Row 12 no longer adds `ORIGIN_ASSET_PREFIXES`, so core holds no `_vercel` entry before this change. Section 2.6 names the commit it counts at, `13d57b03f`, where the guard finds 173 lines, that entry and 172 comments, documents or test code, and no other section names the prefix.                              |
| 2026-10-26 | The header no longer calls the pull request a draft.                                                                                                                                                                                                                                                                  |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/49-host-features` at `69ba03219`. Issues found, two of them blocking. The page left out the change to `x-debug-fastly-pop` on the Axum, Cloudflare and Spin adapters, and described the spoofable header list as stripping headers although the one adapter that calls the function reading it declares nothing. This revision states both as the code has them today, adds the effects on the other adapters, the scheme change, where each refusal happens, the order for changing the package and the document, the name that stays and the untested paths, and corrects the partly true claims, whereas the two code fixes wait on the open questions in section 2.3.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `b319bf039`, of the round 1 fixes and the new text. No blocking issues. This revision names in the Fastly sentence of 2.3 that `ts config validate` no longer refuses `x-debug-fastly-pop`, corrects when the Axum, Cloudflare and Spin adapters strip `fastly-client-ip`, lists the comment at the top of `platform/request_headers.rs` among the comments left for a later change, and corrects the changelog entry on the spoofable strip while adding the scheme change and the `x-debug-fastly-pop` change on Fastly to it.
3. **Round 3.** A review in a fresh session, against `split/49-host-features`, of the changes that follow attestation leaving row 12. No issues found. The round ran the guard's own matcher on core at the commit before this row's guard commits and found 173 lines, one of them the `_vercel` entry of `ORIGIN_ASSET_PREFIXES`, which row 12 no longer adds, so the count of 172 lines before this change, all of them comments, documents or test code, holds. This revision also stops calling the pull request a draft, as IABTechLab/trusted-server#1399 is not one.
4. **Round 4.** A review in a fresh session, checking the round 3 fixes. No issues found. The round found the header right, because IABTechLab/trusted-server#1399 is not a draft, and found nothing else to change in this design.
