# Design Spec: A middleware may run on a media type other than HTML

**Status:** Proposed, 2026-10-12. Raised as pull request IABTechLab/trusted-server#1360, row 19 of the stack in IABTechLab/trusted-server#777, for Tech Lab review. The two earlier pull requests for this change, IABTechLab/trusted-server#1306 and jwrosewell/trusted-server#14, are closed.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-10-12-page-middleware-design.md`, `2026-10-12-ads-txt-sellers-design.md`, `2026-10-12-middleware-creates-a-missing-file-design.md`, `2026-10-12-robots-txt-middleware-design.md` and `2026-10-14-rsc-flight-middleware-design.md`, all in IABTechLab/trusted-server#1342, which holds every design document of the stack
**Related PRs:** IABTechLab/trusted-server#1360, IABTechLab/trusted-server#1306 (closed), jwrosewell/trusted-server#14 (closed)
**Issues:** IABTechLab/trusted-server#1277
**Last updated:** 2026-10-16

> **Why this spec exists.** Before this change the `[[fetch]]` and `[[serve]]` entries covered HTML alone, so a module could not change a publisher's `ads.txt`, a script or a stylesheet. This change lets an entry cover those text files, and this spec says which media types, what runs on a body, and what stays unchanged.

## 1. Problem

Every middleware in the stack changes HTML, and the entries that place a middleware refused any `media_type` but `text/html`. A publisher's `ads.txt`, a script and a stylesheet are text that passes through the same publisher path as a page, with the origin's addresses already moved to the reader's host, and the entries already say where a change goes and in what order. A module with something to add to one of those files had nowhere to put the addition. The page middleware design (`2026-10-12-page-middleware-design.md`) introduced the entries and refused every media type but `text/html`, and this change replaces that refusal with the rule in section 2.

## 2. Design

```toml
[[fetch]]
media_type = "text/plain"
path = "/ads.txt"
middleware = ["example.sellers"]
```

An entry keeps the three fields it had. Only the rule for `media_type` changes.

| Field        | Type             | Required | Meaning                                                                                                                                   |
| ------------ | ---------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `media_type` | String           | Yes      | The media type the entry covers, a bare `type/subtype` in lower case                                                                      |
| `path`       | String           | No       | A prefix of the request path, compared as written, that starts with `/` and holds no `*`, `?` or `#`. Absent, the entry covers every path |
| `middleware` | Array of strings | Yes      | The middleware to run, in the order written                                                                                               |

**What an entry may cover.** An entry's `media_type` is one of these.

- `text/html`
- `text/plain`, `text/css` or any other `text/` type except `text/x-component`
- `application/javascript`
- `application/json`

Nothing else is accepted. Every image is refused, `image/svg+xml` among them, because core sends an image to the reader without reading its body and no middleware could run on one. `text/x-component` is refused too. It is the React Server Components stream, whose rows state their own length, and core moves the origin's addresses in the stream with a rewriter of its own that writes each length again. Row 40 (the Flight rewriter as a middleware of the Next.js module, IABTechLab/trusted-server#1381) lets an entry cover `text/x-component`.

**How a response is matched.** A response takes the entry for the type and subtype of its `Content-Type`, in lower case and without the parameters, so `text/plain; charset=utf-8` takes the entry for `text/plain`. Each phase takes the first entry that covers the response and runs that entry's middleware alone, in the order written. An entry that an earlier entry for the same media type already covers is refused, so the entry with the longer path is written first, and entries for different media types never hide one another. The `path` is a prefix of the request path without its query string, so `/ads.txt` also covers `/ads.txt.bak`. An entry looks at neither the request method nor the response status.

**What no entry can change.** Five kinds of response reach the reader with no middleware run on them.

1. A response of a type outside the list above, such as an image. Core does not read its body.
2. A `text/x-component` stream, which goes to core's own rewriter.
3. A response with a `Content-Encoding` other than `identity`, gzip, deflate or br, which core cannot decode. A response with no `Content-Encoding` is read as it is.
4. A response that declares a charset other than UTF-8, because a processor that adds UTF-8 text to a body in another charset would corrupt the body. A response that declares no charset is handed on as UTF-8. Core still moves the origin's addresses in the body of a response that declares another charset, as it does in every text response, and that replacement reads every text body as UTF-8 and drops everything from the first byte that is not valid UTF-8 (`streaming_replacer.rs`, lines 114 to 141). A body in another charset therefore does not reach the reader intact either. That fault is in core already and this change leaves it as it was.
5. A response with no body, being the reply to a `HEAD` request, a `204`, a `205` or a `304`.

```rust
pub trait Middleware: Send + Sync {
    fn middleware_id(&self) -> &'static str;
    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == HTML_MEDIA_TYPE
    }
    fn phases(&self) -> &[MiddlewarePhase];
    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction;
}
```

- **A middleware says what it works on.** `handles_media_type` answers for HTML alone unless a middleware overrides it, so a module keeps its behavior with no change to its production code. An entry naming a middleware under a media type the middleware does not handle is refused when the registry is built, naming the entry, the media type and the middleware.
- **A chain carries its media type.** `MiddlewareChain::new` takes the media type of the responses the chain runs on. A chain's plan is what each middleware in the chain says it will do with a response, collected by asking each one in turn. On a media type other than HTML the plan refuses, by the middleware's name, an action that carries head markup, markup for after the script bundle, an element handler or a text handler, because each is a decision about HTML. An action there may carry a stream processor, and a serve middleware's action may also carry response headers.
- **A serve middleware's headers reach the response.** The headers a serve middleware writes for a body that is not HTML are written on the response, each replacing a header of the same name, on Fastly and on the three adapters that buffer, as they are for a page. A fetch middleware that writes a header is refused by name on every media type, and was before this change.

| Configuration                                                                                                                                                        | When it is refused                              | Refused with                                                                                              |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| An entry for `image/png`, `image/svg+xml`, `application/octet-stream` or `text/x-component`                                                                          | When the settings load and at deploy validation | "is not a media type a middleware runs on", the entry, and the types to write                             |
| An entry with capital letters (`TEXT/HTML`), a parameter (`text/html; charset=utf-8`), a `*` (`*/*`, `text/*`), white space, no subtype (`text/`) or no `/` (`text`) | When the settings load and at deploy validation | The same                                                                                                  |
| An entry that an earlier entry for the same media type already covers, or that covers the same pages                                                                 | When the settings load and at deploy validation | The entry and the earlier entry by position, and whether to write the longer path first or share one list |
| An entry naming a middleware under a media type it does not handle                                                                                                   | When the registry is built                      | The entry, the media type and the middleware's name                                                       |
| A middleware's head markup, markup for after the script bundle, element handler or text handler on a body that is not HTML                                           | When the chain is planned for a response        | The middleware's name and the media type                                                                  |

**What runs on the body.** Core moves the origin's addresses to the reader's host first, with the replacement it runs on every text response. The stream processors the entries supply then run on the result, the fetch entry's and then the serve entry's, each handed what the one before it produced. A stream processor is code that core hands a body one piece at a time and that returns the piece to send on. The template cache refuses anything that is not HTML, so no copy of such a body is stored, and both phases run on every response as core processes it. A fetch middleware is handed nothing a request left, and a serve middleware is handed the values the modules' request hooks left on the request, as on a page.

**What a stream processor is handed.** The first processor is handed what core's replacement produced, in pieces with no fixed size. The replacement holds back the last few bytes of each piece in case an address is split between two pieces, so a piece can be empty, and every piece the replacement produces is made of whole UTF-8 characters. The last call has `is_last` set and can carry no bytes, because the call carries only what the replacement held back, which can be nothing. A processor that adds text at the end of a body adds the text on the call with `is_last` set.

**Status codes.** Core checks no status, apart from skipping the responses with no body listed above. A response an entry covers runs the processors whatever its status, so a `206` slice of a file and an error page of a covered type are changed like a `200`. Core strips a reader's `Range` and conditional request headers only from a request for an HTML document that it expects to change for the reader, so a request for a text file normally reaches the origin with them, and the origin may answer `206` or `304`.

**Caching and validators.** This change sets no cache header and removes none. A response whose body a middleware changed keeps the origin's `Cache-Control`, `ETag` and `Last-Modified`, and a `[[cache.asset_rules]]` rule that matches the path applies to the response as to any other response that is not HTML. `Content-Length` is not kept, because Fastly drops the header and the three adapters that buffer write the length of the buffered body.

**Consent and privacy.** The context a middleware is handed carries no consent signal and no permission state, on a page or on any other body. A page for a request that modules left values on is sent `private, no-store` and loses its validators, so no shared cache replays the page to another reader. A response that is not HTML gets neither the header nor the removal (open question 1). The comment on `IntegrationRequestState` in `integrations/registry.rs` describes an HTML document alone.

**Adapters.** This change touches no adapter, and all four reach a body through core's publisher functions. Fastly plans the chain before it answers and then streams the body, so a refusal gives an error response, and a failure while the body is processed ends the response part way, after the status has gone out. Axum, Cloudflare and Spin hold the processed body in memory, up to `[publisher] max_buffered_body_bytes` (16 MiB by default), and answer with the body's length. With `[robots-txt]` set, a `GET` or `HEAD` request for `/robots.txt` is answered by a route of its own on all four adapters and never reaches an entry. Without that section the file comes from the origin and takes an entry as any text file does. Row 25 (robots.txt as a middleware core supplies, IABTechLab/trusted-server#1366) moves the file into the entries.

**When it fails.** A chain is planned for each response an entry covers. A refusal found then fails the request with a `500` before a byte is sent. A page fails the same way when its middleware asks for a selector that does not parse. The refusals are an action that decides about HTML on a body that is not HTML, and a fetch middleware that writes a header. The registry cannot find these when it is built, because a middleware gives its action only when core asks about a response. A processor that returns an error while the body is processed ends a Fastly response part way, with the status already sent, and gives an error response with no partial body on the three adapters that buffer. A body larger than `max_buffered_body_bytes` ends a Fastly response part way and gives an error response on the other three.

**Compatibility.** A module that overrides nothing runs on HTML alone, as before, so no module's production code changes. `MiddlewareChain::new`, a public function, takes the media type as a new required argument, so code outside core that builds a chain has to pass a media type. The callers in this repository are the registry, core's tests and the InMobi module's tests, and each now passes a media type. The public surface also gains `is_dispatched_media_type`, `media_type_of`, `is_utf8_or_undeclared_charset`, `MiddlewareAction::is_stream_only` and `MiddlewareChain::media_type`. A configuration with no entry for a media type other than HTML behaves as before. A build from before this change refuses a configuration that carries such an entry, when the settings load and at deploy validation, so a deployment moves to this build first and adds the entry afterwards, and going back needs the entry removed first. The configuration guide, the integration guide and the changelog describe the change.

## 3. What is not in this change

- A middleware that uses the change. The ads.txt sellers module, which adds a publisher's sellers to the origin's file, is the first, in row 20 (IABTechLab/trusted-server#1361, `2026-10-12-ads-txt-sellers-design.md`). At this row no module changes a body that is not HTML. Of the fixture middleware core's tests register, one changes such a body through the publisher path, and a second writes a header on such a response and leaves its body alone.
- A file the origin does not have. An origin `404` is not turned into a file here. Row 22 (IABTechLab/trusted-server#1363, `2026-10-12-middleware-creates-a-missing-file-design.md`) makes the file from the middleware an entry names.
- An entry for `text/x-component`, which row 40 adds.
- A rule for the status, the cache headers or the validators of a text response a middleware changed, which the open questions raise.

**Open questions.** Each of these is a question about the code. This spec records what the code does and leaves the questions open.

1. Should a text response that a serve middleware built from the request's state be sent `private, no-store`, as a page is? Only HTML gets the header today, so such a response keeps the origin's caching directives. No test covers a text response built from the request's state.
2. Should a middleware run on a response whose status is not `200`? Every covered response with a body runs the processors today, whatever its status, so a `206` slice, whose `Content-Range` stays as the origin sent it, and an error page of a covered type are both changed. No test runs a response with such a status.
3. Should the origin's `ETag` and `Last-Modified` stay on a body a middleware changed? They stay today, so a reader who asks again with the `ETag` held gets a `304` while the origin's file is unchanged, and keeps the body held, whatever the middleware would add now.
4. Should the check on an entry's `media_type` accept only a well formed `type/subtype`? The check refuses a list of faults today, so `text/plain,x` starts with `text/`, holds none of them and is accepted, though no ordinary response has that type.
5. Should the refusal for an HTML decision on another media type say that a serve action may carry headers too? The text says an action there "carries a stream processor alone".

## 4. Tests

The tests are in `crates/trusted-server-core/src`.

| Behavior                                                                                                                                                                                                                                    | Test                                                                                                                                                                     |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| An entry may cover `text/plain`, `text/css`, `application/javascript` and `application/json`                                                                                                                                                | `an_entry_may_cover_a_text_media_type_that_is_not_html` in `middleware/tests.rs`                                                                                         |
| An entry with capital letters, a parameter, a `*`, white space, no subtype or no `/`, or one for `image/png`, `image/svg+xml`, `application/octet-stream` or `text/x-component`, is refused with the entry, the type and the types to write | `an_entry_for_a_media_type_a_middleware_does_not_run_on_is_refused` in `middleware/tests.rs`                                                                             |
| A settings document with an entry for `image/png` is refused when it loads                                                                                                                                                                  | `settings_with_a_misshapen_entry_are_refused` in `middleware/tests.rs`                                                                                                   |
| A content type gives its type and subtype in lower case, and `image/svg+xml`, `text/x-component` and `image/png` are not media types a middleware runs on                                                                                   | `a_media_type_is_the_type_and_subtype_of_a_content_type_in_lower_case` in `middleware/tests.rs`                                                                          |
| A body goes to a middleware only in UTF-8 or with no declared charset                                                                                                                                                                       | `a_body_is_handed_to_a_middleware_only_in_utf8_or_an_undeclared_charset` in `middleware/tests.rs`                                                                        |
| A chain on another media type refuses head markup by the middleware's name and plans a stream processor                                                                                                                                     | `a_chain_on_another_media_type_runs_a_stream_processor_alone` in `middleware/tests.rs`                                                                                   |
| A serve chain on another media type keeps a header it is given                                                                                                                                                                              | `a_serve_chain_on_another_media_type_may_write_a_header` in `middleware/tests.rs`                                                                                        |
| The registry gives `text/plain` the text middleware and a page the page middleware                                                                                                                                                          | `an_entry_runs_a_middleware_on_the_media_type_it_handles` in `integrations/registry.rs`                                                                                  |
| The registry refuses a middleware named under a media type it does not handle, in both directions                                                                                                                                           | `an_entry_naming_a_middleware_under_a_media_type_it_does_not_handle_is_refused` in `integrations/registry.rs`                                                            |
| A buffered text body has the origin's address moved and then the entry's line added                                                                                                                                                         | `a_text_body_passes_through_the_middleware_of_the_entry_covering_it` in `publisher.rs`                                                                                   |
| The same for a streamed body                                                                                                                                                                                                                | `a_streamed_text_body_passes_through_the_middleware_of_the_entry_covering_it` in `publisher.rs`                                                                          |
| A serve middleware's header is handed back for a buffered body and for a streamed one                                                                                                                                                       | `a_text_body_s_serve_middleware_write_their_headers_on_the_response` and `a_streamed_text_body_s_serve_middleware_write_their_headers_on_the_response` in `publisher.rs` |
| A body at a path the entry does not cover is left as the origin sent it                                                                                                                                                                     | `a_text_body_no_entry_covers_is_left_as_it_was` in `publisher.rs`                                                                                                        |
| A body that declares `utf-16` gets no middleware                                                                                                                                                                                            | `a_text_body_in_a_charset_other_than_utf8_is_left_as_it_was` in `publisher.rs`                                                                                           |

Eight things have no test.

1. Fastly's streaming conversion of a text body, which is where the headers are written for Fastly and where a failure ends the response part way. The async test reaches the processor constructor, `PublisherBodyProcessor::new`, that the conversion also uses. Tests run the conversion on `text/css` with no entry, and none runs it on a body an entry covers.
2. A stream processor in the serve phase. The serve fixture writes a header and no processor.
3. A fetch entry and a serve entry on one body, to show the fetch entry's processors run first.
4. A plan refusal through the publisher path. The chain test refuses head markup alone, so markup for after the script bundle, an element handler and a text handler on a body that is not HTML have no test.
5. `application/json` and `application/javascript` through the publisher path. Both appear only in the test of which types an entry may cover, and `application/json` also in the test of the media type a content type gives.
6. A compressed text body with an entry. The text tests send identity encoding.
7. A response whose status is not `200`, a request with `Range` or a conditional header, and a serve middleware that reads the request's state on a text body.
8. An adapter. No adapter test sends a text body through an entry.

## 5. Acceptance

The fork's CI passed on the series head, `149280a65`, all nineteen checks, as the description of IABTechLab/trusted-server#1360 records. That description ticks the Fastly and Axum tests, the Fastly and Axum Clippy runs, the format check, the JavaScript tests and format, the docs format and the WASM build, and leaves the manual test through `fastly compute serve` unticked. It records that no module of the stack changes a body that is not HTML yet, and that the fixture middleware in core's tests does, through the publisher's own path.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand.                                                                                                                                                                                                                                                                                                                                                                                                  |
| 2026-10-15 | Fixes from review round 1. The types an entry may cover and the refusals, the match, what no entry can change, the charset, the headers, what a processor is handed, status codes, caching and validators, consent and privacy, the adapters, failure, compatibility, the rows that hold what this change leaves out, the tests and the cases with none, the open questions, the header and the acceptance lines brought to the series head, and the review log. |
| 2026-10-16 | Fixes from review round 2. The identity encoding, the one fixture that changes a body, the tests that run Fastly's conversion and the tests that name `application/json` are stated as the code has them, and core's address replacement is said to drop a body's bytes from the first one that is not valid UTF-8.                                                                                                                                              |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/19-media-types` at `149280a65`. Issues found, four of them blocking. The design listed `image/svg+xml` as a type an entry may cover when the code refuses it, described stored copies that a body that is not HTML never has, said a middleware changes the body alone when a serve middleware's headers also reach the response, and left out that a response declaring a charset other than UTF-8 gets no middleware. This revision states what the code does at that head for the types, the match, the charset, the status codes, caching, the headers, the adapters, failure and compatibility, replaces the acceptance claims with what the pull request description records, and lists five questions that wait for code changes.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/19-media-types` at `149280a65`. Approved, none blocking. This revision qualifies the identity encoding, the one fixture that changes a body, the tests that run Fastly's conversion and the tests that name `application/json`, and says that core's address replacement drops a body's bytes from the first one that is not valid UTF-8.
