# Design Spec: The Flight rewriter is a middleware of the Next.js module

**Status:** Proposed, 2026-10-14. Raised as draft pull request IABTechLab/trusted-server#1381 (from jwrosewell/trusted-server#35), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-09-07-850-parser-aware-body-hold-nextjs-streaming-design.md`, `2026-10-14-source-modules-design.md`
**Related PRs:** IABTechLab/trusted-server#1381, jwrosewell/trusted-server#35
**Issues:** IABTechLab/trusted-server#1340
**Last updated:** 2026-10-16

> **Why this spec exists.** Core rewrote the addresses inside a React Server Components Flight stream by its media type, in a module of its own, and the publisher path named that media type to call it. This change moves the rewriter into the Next.js module as the middleware `framework.nextjs.flight`, so the publisher path no longer names the media type. A deployment that fronts a Next.js site selects the module and names the middleware in a `[[fetch]]` entry, and section 2 says what happens to a deployment that does not.

## 1. Problem

The Next.js App Router answers a client side navigation with a Flight stream, `text/x-component`, the row format its `__next_f` scripts carry inside a page. A `T` row declares the byte length of its content, so moving an address inside one changes the length and the header has to be written again. Core's plain replacement of the origin's address, which runs on every other text body, would leave the old length and break the stream.

That is why core carried `rsc_flight`, a rewriter that reads the rows and writes each `T` row's length again, and why the publisher path checked for `text/x-component` to run it in place of the plain replacement. The entries for a media type other than HTML could not cover `text/x-component`, because the plain replacement would have run ahead of any middleware named there.

The Next.js crate carries the rest of the framework's handling. This piece stayed in core because it ran before a middleware could run on a media type other than HTML.

## 2. Design

**The rewriter is a middleware of the Next.js module.** `framework.nextjs.flight` is registered beside `framework.nextjs` whenever `[framework] module = "nextjs"` selects the module. It handles `text/x-component` alone, so an entry that names it under any other media type, HTML included, is refused when the registry is built, as any middleware named under a media type it does not handle is. It runs in the fetch phase, and its action carries one stream processor that reads the rows and rewrites them.

```toml
[framework]
module = "nextjs"

[[fetch]]
media_type = "text/html"
middleware = ["framework.nextjs"]

[[fetch]]
media_type = "text/x-component"
middleware = ["framework.nextjs.flight"]
```

**The publisher path names no framework.** The publisher path runs the entries the configuration lists on a text response and knows nothing of Flight. `text/x-component` is a media type an entry may cover, as every other `text/` type is.

**A middleware may move the origin's address itself.** `Middleware::moves_origin_address` is a new method of the public `Middleware` trait that answers `false` unless a middleware overrides it. A middleware that answers `true` says that it moves the origin's address to the reader's host itself, in a body of the media types it handles other than HTML, for a format whose rows count their bytes. `IntegrationRegistry::middleware_moves_origin_address` is true for a media type when any registered middleware both says so and handles that type, and a module registers its middleware only when a section selects the module. Where the answer is true for a media type, core leaves the addresses in a body of that type alone, whether or not an entry names the middleware, for every response of that type whatever path the request was for and whatever `path` an entry carries. So with the module selected and no entry, a Flight response passes through as the origin sent it, and with the entry the middleware rewrites the response row by row, writing each `T` row's length again.

**Why the middleware says so, and what else was possible.** The signal comes from the registered middleware rather than from the entry, because an entry that is missing must leave the stream whole rather than hand it to the plain replacement. Two other routes were not taken. Leaving the rewriter in core and naming its media type in the publisher path keeps in core the one piece of a framework's handling that this change moves out. Allowing an entry for `text/x-component` and adding no signal leaves core's plain replacement to run ahead of the middleware, which is the fault in section 1.

**What a deployment does.** A deployment that fronts a Next.js site selects the module and names the middleware in a `[[fetch]]` entry for `text/x-component`, as it names the rest of the framework's middleware for `text/html`. `trusted-server.example.toml` shows the entry, the module's recorded page settings carry it, and the stock middleware list in `trusted-server-modules` lists it with its media type, so the configuration guide's table and the template are checked against it. `ts audit` writes entries for `text/html` alone, because `trusted_server_modules::middleware_for` leaves out every other media type. `ts audit` also does not detect Next.js and so does not select the module, which means a configuration it writes carries no Flight entry, and a deployment adds the entry by hand.

**A `path` on the Flight entry.** Settings validation accepts a `path` on this entry, as it does on any entry, and the entry then covers only the Flight responses fetched for a request path under it. Core still leaves the addresses of every Flight response to the middleware, so a Flight response for a path no entry covers passes through as the origin sent it. A `[[fetch]]` entry for a media type other than HTML that carries a `path` and names a middleware can also make a file the origin does not have. Where more than one such entry covers the request, the one with the longest `path` makes it, and the first written wins between two as long. With a `path` on the Flight entry, an origin `404` to a `GET` or `HEAD` request for a path under it, an HTML page's included, is therefore answered `200` with an empty Flight document, and the middleware runs on that document, unless another such entry wins under that rule. That entry makes an empty document of its own media type instead. The template's entry carries no `path`, and no test covers an entry with one.

**Configuration.** The change adds no key and no default. It adds one middleware name, `framework.nextjs.flight`, and lets an entry cover `text/x-component`. The registry refuses an entry that names a middleware no running module supplies, one that names a middleware in a phase it does not run in, and one that names a middleware under a media type it does not handle, each by naming the entry and the middleware. The Flight middleware is therefore refused in a `[[serve]]` entry, under any media type but `text/x-component`, and where no section selects the module.

**What the middleware is told.** The origin host, the reader's host and the reader's scheme from its context, which is what the module's HTML middleware works from too. The origin host is the `Host` the source sends, being the source's host and port unless the source sets another `Host`. A page names itself by that `Host`, so core tells every middleware that name as the origin host, and a middleware is told no source address.

**What the middleware moves.** From the origin host, the middleware moves an `https://` or `http://` address to the reader's scheme and host, a protocol relative address to the reader's host, and a bare host at hostname boundaries to the reader's host. It moves each address once, in one pass from the start, where `rsc_flight` read each replacement's output again and wrote a request host that begins with the origin host more than once. What the middleware moves is not exactly what core's plain replacement moves, and not exactly what `rsc_flight` moved, the rewriter in core that this change moves out. Core's replacement and `rsc_flight` build the `https://` and `http://` forms from the source's address as written, where the middleware builds them from the host alone, and `rsc_flight` already respected hostname boundaries for a bare host, as the middleware does. The middleware differs from core's replacement in four ways and from `rsc_flight` in the first three.

1. **A folder in the source's address.** For a source at `https://origin.example.com/shop`, core's replacement and `rsc_flight` turn `https://origin.example.com/shop/item` into `https://proxy.example.com/item`. The middleware turns the same text into `https://proxy.example.com/shop/item`, so the folder stays in a rewritten address, as it does in the links of a page.
2. **A `Host` that differs from the address.** Where the source sends another `Host` than its address names, core's replacement and `rsc_flight` look for the address as written and for the `Host`, and the middleware looks for the `Host` alone. An address in a Flight response that names only the source's own address is therefore no longer moved.
3. **The scheme of the address.** Core's replacement and `rsc_flight` add the `http://` form only where the source's address is written `https://`. The middleware always moves both forms to the reader's scheme, so for a source written `http://`, an `https://` address in the body takes the reader's scheme, where core's replacement left the `https` the body had.
4. **A bare host inside a longer name.** Core's replacement treats the bare host as plain text, so `notorigin.example.com` becomes `notproxy.example.com`. The middleware moves a bare host only at hostname boundaries, as `rsc_flight` did, and leaves a bare `notorigin.example.com`, `cdn.origin.example.com` and `origin.example.com.uk` alone. Core's replacement, `rsc_flight` and the middleware all match the `https://`, `http://` and `//` forms as plain text, so a longer name that begins with the origin host after one of them, such as `https://origin.example.com.uk`, becomes `https://proxy.example.com.uk` in each.

The middleware builds from the host because it works from what its context carries, as the module's HTML middleware does, and its context carries the origin host and no address. Core's own rewriting of the links in a page also builds from the host.

**Adapters.** This change touches no adapter. The Axum, Cloudflare, Fastly and Spin adapters each build their registry from the stock builders of `trusted_server_modules`, which include the Next.js module, and each reaches the body path through core's publisher functions, so the middleware, and core leaving a Flight body to it, apply on all four.

**Consent and privacy.** The middleware reads no consent state and no identifier and sets no header, so the change has no effect on either. Where no entry names the middleware, the origin's own addresses reach the reader inside the Flight response, as the origin sent them.

**When it fails.** The row reader is the code core had, moved without change. Content that is not valid UTF-8 is written as it came, and so is a row that the end of the stream cuts short. A byte in a row's length that is not a hexadecimal digit counts as zero. A row is held in memory until its newline or its whole length has arrived, and no limit applies to the size held, so a row that never ends is held to the end of the response. A body that declares a charset other than UTF-8, on a media type whose addresses a middleware moves itself, passes through whole, because core skips its replacement for the type and no middleware runs on a charset other than UTF-8. Before this change core rewrote a Flight response whatever charset the response declared.

**Rollout.** The `CHANGELOG.md` entry for this change is marked breaking. It says that a deployment that fronts a Next.js site selects the module and adds the entry, and that `trusted_server_core::rsc_flight` is removed for crates outside core. The build goes first and the entry second. A build from before this change refuses a configuration that carries the entry, and so does `ts config validate` from before this change, because neither accepts `text/x-component` as the media type of an entry. A build with this change, with the module selected and no entry, starts and logs a warning that no entry names the middleware, and it sends a Flight response as the origin sent it, so the origin's address reaches the reader in Flight responses until the entry is added. Going back to a build from before this change needs the entry removed first, because that build refuses a configuration that carries the entry.

**Open question for the reviewer.** A deployment that fronts a Next.js site and does not select `[framework] module = "nextjs"` now gets core's plain replacement on a Flight response, because no registered middleware says it moves the addresses of `text/x-component`. Before this change core rewrote Flight for every deployment. The plain replacement changes the text inside a `T` row and leaves the row's length as it was, which is the fault in section 1, so the stream breaks wherever the replacement changes a row's length. Nothing in the code warns about this case at startup or on a request, and the `CHANGELOG.md` entry says only that a deployment that fronts a Next.js site selects the module and adds the entry. The question for the reviewer is whether core should keep a default rewrite of Flight for such a deployment or whether the breaking note is enough. This spec makes no decision on it.

## 3. What is not in this change

- A default for a deployment that serves Flight responses without selecting the Next.js module. Core treats `text/x-component` as it treats any other text body for such a deployment, so its Flight responses meet the plain replacement, as the open question in section 2 says.
- An adapter change. The four adapters take the middleware through the stock builders and need none.
- Writing the Flight entry from `ts audit`, which writes `text/html` entries alone and does not detect Next.js.
- Reading JSON escapes inside a Flight row. A navigation response carries plain addresses, and the escaped form inside a page's `__next_f` scripts is handled by the HTML middleware.
- Running the middleware in the serve phase. A Flight response is not stored, and the fetch phase runs on each one as it is fetched.

## 4. Tests

- The rewriter moves an address in a newline row, moves an `http://` address to the reader's scheme, writes a `T` row's length again, handles a `T` row split across chunks, and leaves an unrelated bare hostname alone.
- The rewriter writes a request host that begins with the origin host once, as a `.localhost` name or with a port, in a `T` row and a newline row over both schemes, writes a hyphenated host and a `www.` host over an apex origin as before, and moves each address once from the start, leaving a row alone when the origin host is empty.
- The middleware handles `text/x-component` alone, runs in the fetch phase, says it moves the origin's address itself, and its plan carries one stream processor that rewrites a Flight body.
- The module registers the middleware, core leaves `text/x-component` to it and moves the addresses in every other body, a Flight response takes the entry naming it and runs nothing where no entry covers it, and an entry for HTML naming it is refused by name.
- In core, a body whose registered middleware moves the address itself is handed to that middleware as the origin sent it, on the buffered path and the streamed path. The same body passes through untouched where no entry names the middleware, tested on the buffered path alone. A `text/plain` body still has its addresses moved before the entry's middleware runs.
- `text/x-component` is accepted as an entry's media type, and the stock middleware list, the configuration guide's table and the template's entries agree on the new middleware.

Seven cases have no test.

1. The real middleware through the publisher path. The tests in core use a middleware of their own, on a media type of its own, and the tests in the Next.js crate stop at the registry and the middleware's own plan, so no test sends a `text/x-component` body through the publisher path with `framework.nextjs.flight` registered.
2. The streamed path where no entry names a middleware that moves the address itself.
3. A body that declares a charset other than UTF-8, on a media type whose addresses a middleware moves itself. The existing charset test uses `text/plain` and a middleware that does not move addresses.
4. A source address with a folder, and a source that sends another `Host`, through the middleware. The rewriter's tests give it a host and nothing else.
5. A Flight entry with a `path`.
6. Any adapter. No adapter route test and no parity test sends a `text/x-component` response, and the shared Next.js fixture they use (`nextjs_auction` in `crates/trusted-server-core/src/test_support.rs`) selects the module with an entry for `text/html` alone.
7. The integration scenario `NextJsRscFlight` (`crates/trusted-server-integration-tests/tests/frameworks/scenarios.rs`), which sends a Flight request to the Next.js test origin. Before this change core rewrote every Flight response. The integration test configuration selects no `[framework]` module and has no `[[fetch]]` entry, so a Flight response in that scenario now meets core's plain replacement and not the middleware. The scenario checks only that the content type is not HTML and that the body holds no injected script, so it would pass with a broken `T` row.

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/40-rsc-flight` at `b2124875a`. Issues found, one of them blocking. Section 2 said the middleware rewrites a Flight response as it was rewritten before and as core's plain replacement does, when the middleware builds its addresses from the origin host and so differs from both for a folder in the source's address, for another `Host` and for the `http://` form, and from core's replacement for a bare host inside a longer name. This revision states each difference and why the middleware builds from the host, and it adds the adapters, the configuration refusals, consent and privacy, failure, the rollout order with an open question for deployments that do not select the module, the cases no test covers and the advice the round gave.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `303981e49`, of the round 1 fixes and the new text. Approved, with no blocking issues. This revision corrects the hostname boundary difference, which holds only for a bare host, the `path` paragraph, which now names the validation that checks a `path` and the rule that picks the entry making an empty document, and the template comment, whose order now covers the HTML entries alone.

## Revision record

| Date       | Change                                                                                                                                                                                                                                        |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-14 | First draft.                                                                                                                                                                                                                                  |
| 2026-10-15 | Fixes from review round 1. The differences from core's plain replacement and why, the alternatives to the new method, adapters, configuration, consent, failure, rollout with its open question, the cases no test covers and the review log. |
| 2026-10-15 | Fixes from review round 2. The hostname boundary difference for a bare host alone, the `path` paragraph, the template comment and the review log.                                                                                             |
| 2026-10-16 | The rewriter moves each address once, in one pass, and the tests that show it.                                                                                                                                                                |
