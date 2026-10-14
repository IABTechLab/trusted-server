# Design Spec: The Flight rewriter is a middleware of the Next.js module

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-09-07-850-parser-aware-body-hold-nextjs-streaming-design.md`, `2026-10-14-source-modules-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1340
**Last updated:** 2026-10-14

> **Why this spec exists.** Core rewrites the addresses inside a React Server Components Flight stream by its media type, in a module of its own, and the publisher path names that media type to call it. Every other piece of a framework's handling lives in the framework's crate.

## 1. Problem

The Next.js App Router answers a client side navigation with a Flight stream, `text/x-component`, the row format its `__next_f` scripts carry inside a page. A `T` row declares the byte length of its content, so moving an address inside one changes the length and the header has to be written again. Core's plain replacement of the origin's address, which runs on every other text body, would leave the old length and break the stream.

That is why core carried `rsc_flight`, a rewriter that reads the rows and writes each `T` row's length again, and why the publisher path checked for `text/x-component` to run it in place of the plain replacement. The entries for a media type other than HTML could not cover `text/x-component`, because the plain replacement would have run ahead of any middleware named there.

The Next.js crate carries the rest of the framework's handling. This piece stayed in core because it ran before a middleware could run on a media type other than HTML.

## 2. Design

**The rewriter is a middleware of the Next.js module.** `framework.nextjs.flight` is registered beside `framework.nextjs` whenever `[framework] module = "nextjs"` selects the module. It handles `text/x-component` alone, runs in the fetch phase, and its action carries the stream processor that reads the rows and rewrites them, so an entry naming it for HTML is refused when the registry is built, as any middleware named under a media type it does not handle is.

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

**Core names no framework.** The publisher path runs the entries the configuration lists on a text response and knows nothing of Flight. `text/x-component` is a media type an entry may cover, as every other `text/` type is.

**A middleware may move the origin's address itself.** `Middleware::moves_origin_address` says that a middleware moves the origin's address to the reader's host itself in a body of the media types it handles, for a format whose rows count their bytes. Where a registered middleware says so for a media type, core leaves the addresses in a body of that type alone, whether or not an entry names the middleware. So with the module selected and no entry, a Flight response passes through as the origin sent it, and with the entry it is rewritten as it was before, row by row with each `T` row's length written again. The signal comes from the registered middleware rather than from the entry, because an entry that is missing must leave the stream whole rather than hand it to the plain replacement.

**What a deployment does.** A deployment that fronts a Next.js site selects the module and names the middleware in a `[[fetch]]` entry for `text/x-component`, as it names the rest of the framework's middleware for `text/html`. `trusted-server.example.toml` shows the entry, the module's recorded page settings carry it, and the stock middleware list in `trusted-server-modules` lists it with its media type, so the configuration guide's table and the template are checked against it.

**What the middleware is told.** The origin host, the reader's host and the reader's scheme from its context, which is what the module's HTML middleware works from too. The origin's `https://` and `http://` addresses go to the reader's scheme and host, a protocol relative address to the reader's host, and a bare host at hostname boundaries to the reader's host, as core's plain replacement does them.

## 3. What is not in this change

- A deployment that serves Flight responses without selecting the Next.js module. Core treats `text/x-component` as it treats any other text body, so such a deployment selects the module.
- Reading JSON escapes inside a Flight row. A navigation response carries plain addresses, and the escaped form inside a page's `__next_f` scripts is handled by the HTML middleware.
- Running the middleware in the serve phase. A Flight response is not stored, and the fetch phase runs on each one as it is fetched.

## 4. Tests

- The rewriter moves an address in a newline row, moves an `http://` address to the reader's scheme, writes a `T` row's length again, handles a `T` row split across chunks, and leaves an unrelated hostname alone.
- The middleware handles `text/x-component` alone, runs in the fetch phase, says it moves the origin's address itself, and its plan carries one stream processor that rewrites a Flight body.
- The module registers the middleware, core leaves `text/x-component` to it and moves the addresses in every other body, a Flight response takes the entry naming it and runs nothing where no entry covers it, and an entry for HTML naming it is refused by name.
- In core, a body whose registered middleware moves the address itself is handed to that middleware as the origin sent it, on the buffered path and the streamed path, and passes through untouched where no entry names the middleware, while a `text/plain` body still has its addresses moved before the entry's middleware runs.
- `text/x-component` is accepted as an entry's media type, and the stock middleware list, the configuration guide's table and the template's entries agree on the new middleware.
