# Design Spec: A file the origin does not have is made by the middleware named for its path

**Status:** Proposed, 2026-10-12. Raised as pull request [IABTechLab/trusted-server#1363](https://github.com/IABTechLab/trusted-server/pull/1363), row 22 of the stack in [IABTechLab/trusted-server#777](https://github.com/IABTechLab/trusted-server/issues/777), for Tech Lab review. The two earlier pull requests for the same commits, [jwrosewell/trusted-server#17](https://github.com/jwrosewell/trusted-server/pull/17) and [IABTechLab/trusted-server#1309](https://github.com/IABTechLab/trusted-server/pull/1309), are closed. [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-10-12-page-middleware-design.md`, `2026-10-12-middleware-media-types-design.md`, `2026-10-12-ads-txt-sellers-design.md` and `2026-10-12-robots-txt-middleware-design.md` (row 25, built after this row), all in IABTechLab/trusted-server#1342
**Related PRs:** IABTechLab/trusted-server#1363 builds on the consent prompt phases in [IABTechLab/trusted-server#1362](https://github.com/IABTechLab/trusted-server/pull/1362) (row 21). The `[[fetch]]` and `[[serve]]` entries come from [IABTechLab/trusted-server#1351](https://github.com/IABTechLab/trusted-server/pull/1351) (row 10, page changes as middleware), the entries for a media type other than HTML from [IABTechLab/trusted-server#1360](https://github.com/IABTechLab/trusted-server/pull/1360) (row 19) and the sellers module from [IABTechLab/trusted-server#1361](https://github.com/IABTechLab/trusted-server/pull/1361) (row 20). [IABTechLab/trusted-server#1366](https://github.com/IABTechLab/trusted-server/pull/1366) (row 25) makes robots.txt a middleware and comes after [IABTechLab/trusted-server#1364](https://github.com/IABTechLab/trusted-server/pull/1364) (row 23) and [IABTechLab/trusted-server#1365](https://github.com/IABTechLab/trusted-server/pull/1365) (row 24)
**Issues:** [IABTechLab/trusted-server#1280](https://github.com/IABTechLab/trusted-server/issues/1280). [IABTechLab/trusted-server#777](https://github.com/IABTechLab/trusted-server/issues/777) lists the stack
**Last updated:** 2026-10-16

> **Why this spec exists.** A publisher whose origin has no `ads.txt` gets the origin's `404`, however many sellers the settings give, because a middleware named for the path runs on the origin's file and the origin has none. This change makes the file from the middleware instead. The sections below give when core makes the file, which entry makes the file, what the reader gets and what the change leaves as it was.

## 1. Problem

A publisher whose origin has no `ads.txt` gets the origin's `404`, however many sellers the settings give, because a middleware named for the path runs on the origin's file and the origin has none. Every well known file a module can add to has the same gap. Without this change each of those files would need a route of its own in core and on every adapter, as `/robots.txt` has when the settings carry `[robots-txt]` (the table of adapters in section 2 gives the line where each adapter registers that route).

Line numbers in this document are those of commit `7723c5e56`, the head of IABTechLab/trusted-server#1363, unless a row is named. A file named without a folder is in `crates/trusted-server-core/src`, so `publisher.rs`, `middleware.rs`, `middleware/tests.rs`, `http_util.rs` and `error.rs` are there. The sellers module is `crates/ads-txt/sellers/src/lib.rs`.

## 2. Design

**What happens.** When the origin answers `404 Not Found` to a `GET` or a `HEAD`, and a `[[fetch]]` entry for a media type other than HTML has a `path` that begins the request path, core drops the origin's answer and runs the entry's middleware on an empty document of the entry's media type. The middleware's output is the file, which the reader gets with status `200`. So a publisher whose origin has no `ads.txt` gets one made of the sellers lines alone.

```toml
[ads-txt]
modules = ["sellers"]

[ads-txt.sellers]
lines = ["ssp.example, 12345, DIRECT, f08c47fec0942fa0"]

[[fetch]]
media_type = "text/plain"
path = "/ads.txt"
middleware = ["ads-txt.sellers"]
```

**What the change touches.** The change adds 448 lines and removes 5 across 8 files, and 367 of the added lines are tests. The change touches no adapter and adds no setting.

| File                                | Added | Removed | What changes                                                                                                   |
| ----------------------------------- | ----- | ------- | -------------------------------------------------------------------------------------------------------------- |
| `publisher.rs`                      | 295   | 0       | The block that makes the file (lines 5610 to 5621), `empty_document` (lines 1922 to 1935) and 7 tests          |
| `middleware.rs`                     | 26    | 0       | `PhaseEntries::making` (lines 470 to 494), which picks the entry and is the one new public item                |
| `middleware/tests.rs`               | 102   | 0       | 4 tests                                                                                                        |
| `crates/ads-txt/sellers/src/lib.rs` | 2     | 1       | The module comment (lines 3 and 4), with no change to code                                                     |
| `crates/ads-txt/sellers/Cargo.toml` | 1     | 1       | The crate description                                                                                          |
| `docs/guide/configuration.md`       | 14    | 2       | The ads.txt section (lines 1112 to 1116) and the entries for a media type other than HTML (lines 1821 to 1829) |
| `docs/guide/integration-guide.md`   | 6     | 0       | The section on a middleware for a media type other than HTML (lines 211 to 215)                                |
| `CHANGELOG.md`                      | 2     | 1       | A new entry for the rule (line 86) and a reworded sellers entry (line 87)                                      |

**The rule.** A well known file is made where middleware is configured for its path, and is the origin's answer where none is.

| The origin's answer to the request                                         | The reader gets                                    | Lines in `publisher.rs` |
| -------------------------------------------------------------------------- | -------------------------------------------------- | ----------------------- |
| `404` to a `GET` or a `HEAD`, where an entry covers the path (next table)  | The made file                                      | 5610 to 5621            |
| `404` to a `GET` or a `HEAD`, where no entry covers the path               | The origin's answer, handled as before this change | 5613                    |
| `404` to any other method                                                  | The origin's answer, handled as before this change | 5612                    |
| Any other status, among them `5xx` errors, redirects and a `200` HTML page | The origin's answer, handled as before this change | 5611                    |
| No answer, because the request to the origin failed                        | An error response, and no file                     | 5588 to 5601            |

Only `[[fetch]]` entries are asked (line 5613), so a `[[serve]]` entry never makes a file. Which `[[fetch]]` entry makes the file is decided by `PhaseEntries::making`.

| The entry                             | Makes the file                                                                                                                                                                            | Lines                                                          |
| ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| `media_type = "text/html"`            | No, and an HTML entry written first does not hide a later entry for another type                                                                                                          | `middleware.rs` 479                                            |
| No `path`                             | No                                                                                                                                                                                        | `middleware.rs` 481 to 484                                     |
| A `path` that begins the request path | Yes. The `path` is compared as written, so case counts, against the request path without its query string. So `/ads.txt` also makes `/ads.txt.bak`, and a `path` of `/` covers every path | `middleware.rs` 396 to 400 and 481 to 484, `publisher.rs` 4949 |
| Two or more entries cover the path    | The entry with the longest `path` makes the file, and the first written wins between two of one length                                                                                    | `middleware.rs` 470 to 473 and 487 to 493                      |

**What the reader gets.** The made response is the same on every adapter, except where the table names an adapter.

| Aspect         | What the code does                                                                                                                                                                                                    | Lines in `publisher.rs`                     |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- |
| Status         | `200`                                                                                                                                                                                                                 | 1929                                        |
| `Content-Type` | The entry's media type, with `; charset=utf-8` added for a `text/` type and nothing added for `application/javascript` or `application/json`                                                                          | 1923 to 1930                                |
| Headers        | Core builds the response with `Content-Type` alone. The status, headers and body the origin sent are dropped, a `Set-Cookie` among them                                                                               | 1922 to 1935 and 5620                       |
| Body           | What the entry's middleware writes on an empty document, as the next paragraph says                                                                                                                                   | 707 to 734                                  |
| Stored         | No. The file is made again on every request                                                                                                                                                                           | 7101 to 7107                                |
| A `GET`        | The body, with the headers a serve middleware writes where a serve entry covers the file, and with a `Content-Length` on Axum, Cloudflare and Spin, which buffer the body, and none on Fastly, which streams the body | 2121 to 2131, 2249 to 2252 and 2925 to 2947 |
| A `HEAD`       | The status and `Content-Type` core built, with no body, no `Content-Length`, no header a serve middleware writes and no middleware run                                                                                | 2098 to 2119 and 2901 to 2923               |

**The empty document and the middleware.** The made response goes through the code that a response from the origin goes through. Core first moves the origin's addresses to the reader's host, which changes nothing in an empty body. The middleware of the `[[fetch]]` entry that made the file runs next, and after that middleware the middleware of the `[[serve]]` entry for the same media type and path, if one exists, because the template cache stores HTML alone, so core runs both phases on every response that is not HTML (`publisher.rs` 707 to 734). The entry that made the file is also the first fetch entry that covers the made file's media type and path, because the checks when the settings load refuse an entry that an earlier entry for the same media type already covers (`middleware.rs` 555 to 578). The registry refuses an entry that names a middleware under a media type the middleware does not handle, as the media types design says, so a made file never reaches a middleware that cannot work on its type.

A stream processor that adds text at the end of a body adds the text on the last call. An empty body reaches the last call as every body does, on the buffered adapters (`publisher.rs` 1101 to 1133) and on Fastly (3076 to 3096). The sellers module starts from the state of a file that already ends with a line break, so on an empty document the sellers lines come out with nothing before the first (`sellers/src/lib.rs` 245 to 253 and 264 to 277, tested by `the_lines_follow_the_publishers_file_on_a_line_of_their_own` at 491 to 495). A middleware that adds to a file therefore writes the same on an empty one, and the code of the sellers module is not changed.

**Caching.** Core stores nothing of the made file. The template cache accepts HTML alone (`publisher.rs` 7101 to 7107), so the file is made again on every request, after core has asked the origin again. Core builds the made response with no cache header. A `[[cache.asset_rules]]` rule that matches the request path applies to the made file as to any other response that is not HTML (`publisher.rs` 1941 to 1970 and 5789 to 5795). Core has two rules that send a page `private, no-store`, one for a page the ad stack changes and one for a page built from values that modules left on the request. Both rules apply to HTML alone (`publisher.rs` 5759 to 5788 and 3151 to 3163), so core's rule for the request's state does not apply to a made file. The media types design leaves open whether a file that is not HTML should be sent `private, no-store` in that case.

**Configuration and rollout.** No setting is added, no default changes and no setting switches the rule off. Each entry is checked as the settings load, for the entry's media type, `path` and names and for being hidden behind an earlier entry (`middleware.rs` 502 to 581, with the media type rule of row 19). A deployment stops a file being made by removing the entry. Removing the entry also stops the middleware running on the origin's file, because no setting lets an entry run the middleware on the origin's file without making a file. A document that selects the sellers module and names the module in no `[[fetch]]` entry for `text/plain` is refused as the settings load, and the check does not look at the entry's `path` (`sellers/src/lib.rs` 144 to 159). The entries for a media type other than HTML (`CHANGELOG.md` line 88) and the sellers module (line 87) are new in the same unreleased block as this change (line 86), so no released configuration changes meaning.

The configuration guide (`docs/guide/configuration.md` lines 1112 to 1116 and 1821 to 1829), the integration guide (`docs/guide/integration-guide.md` lines 211 to 215), the changelog and the sellers module's comment all state the rule. None of the guides says that the origin's `404` headers and body are dropped or that the file is made again on every request.

**Adapters.** All four adapters call one core function and none names the rule. Fastly streams the made file and the other three adapters buffer it.

| Adapter    | Calls `handle_publisher_request`                                | Finishes the answer by                                                   | Registers the `/robots.txt` route |
| ---------- | --------------------------------------------------------------- | ------------------------------------------------------------------------ | --------------------------------- |
| Axum       | `crates/trusted-server-adapter-axum/src/app.rs`, line 391       | Buffering, with `buffer_publisher_response_async` (line 406)             | Line 543                          |
| Cloudflare | `crates/trusted-server-adapter-cloudflare/src/app.rs`, line 700 | Buffering, with `resolve_publisher_response` (line 715)                  | Line 890                          |
| Fastly     | `crates/trusted-server-adapter-fastly/src/app.rs`, line 1096    | Streaming, with `publisher_response_into_streaming_response` (line 1117) | Line 1419                         |
| Spin       | `crates/trusted-server-adapter-spin/src/app.rs`, line 987       | Buffering, with `resolve_publisher_response` (line 1002)                 | Line 1109                         |

Each adapter registers the `/robots.txt` route only when the settings carry `[robots-txt]`. Each adapter also sends seven methods, `GET`, `POST`, `HEAD`, `OPTIONS`, `PUT`, `PATCH` and `DELETE`, to the publisher path (`publisher_fallback_methods` in the adapter's `src/app.rs`, at line 645 on Axum, 503 on Cloudflare, 504 on Fastly and 348 on Spin), so a request with a method other than `GET` and `HEAD` reaches the rule and keeps the origin's `404`.

**Consent and privacy.** Core reads no consent or permission state when it makes the file, and the context a middleware is handed carries none (`MiddlewareContext`, `middleware.rs` 123 to 141). The origin's headers are dropped, so a cookie the origin sent with the origin's `404` never reaches the reader. The sellers lines come from the settings and hold no reader data. A serve middleware is handed the values the modules' request hooks left on the request (`publisher.rs` 718 to 720), so a module can write text for one reader into a made file, and nothing marks that file private (see Caching).

**When it fails.**

1. **The request to the origin fails.** The reader gets an error response, as for any page, and no file is made (`publisher.rs` 5588 to 5601).
2. **A middleware refuses the made file.** Core plans the chain for a made file as for any covered response, and a refusal, such as an action that decides about HTML on a file that is not HTML, fails the request with `500` (`publisher.rs` 731 and `error.rs` 113) before a byte is sent. The reader then gets `500` where the origin said `404`.
3. **The request names no host.** A request with no `Forwarded` host, `X-Forwarded-Host` or `Host` header has an empty host (`http_util.rs` 178 to 195). When the host is empty, core sends content of a type that core processes as it stands and runs no middleware on the content (`publisher.rs` 1909 to 1911 and 5849 to 5885). The made document is empty, so the reader gets an empty `200`.

Core writes one debug log line when it makes a file (`publisher.rs` 5615 to 5619) and sets no header that says the file was made.

## 3. What is not in this change

- **robots.txt as a middleware.** Row 25 ([IABTechLab/trusted-server#1366](https://github.com/IABTechLab/trusted-server/pull/1366)) makes `/robots.txt` from a middleware core supplies. Row 23 (a middleware may prepare with the request's services, [IABTechLab/trusted-server#1364](https://github.com/IABTechLab/trusted-server/pull/1364)) and row 24 (the loading cache core declares, [IABTechLab/trusted-server#1365](https://github.com/IABTechLab/trusted-server/pull/1365)) come before row 25. At this row, with `[robots-txt]` set, a `GET` or a `HEAD` for `/robots.txt` is answered by the route each adapter registers and never reaches this rule, and without that section an entry for `/robots.txt` makes the file as an entry for any other path does. Row 25 also widens the rule for the robots.txt middleware alone, so that a `200` HTML page the origin sends for `/robots.txt` is replaced as well (`answers_robots_txt_with_a_page` in `publisher.rs` at row 25). No later row through row 50 changes `PhaseEntries::making` or the empty document.
- **A page the origin sends with a status other than `404`.** An origin that answers every unknown path with its home page and a `200` keeps its answer, and so does any other status, though a middleware still runs on a body of a covered type whatever the status of the response, as the media types design says. The entry for `text/plain` covers no response of type `text/html`, so the sellers lines are not added to such a page.
- **A `404` the origin sends by mistake.** The reader gets a file made of the sellers lines alone, with status `200`, for as long as the origin answers `404`, and the publisher's own lines are missing from that file. The configuration guide calls the publisher's own file authoritative (`configuration.md` lines 1096 to 1098), and core holds no copy of the last file the origin sent.
- **A run through an adapter.** No file has been made through any of the four adapters against a real origin, as the description of IABTechLab/trusted-server#1363 says, and the manual test through `fastly compute serve` is not ticked.

**Open questions.** Each of these is a question about the code. This spec records what the code does and leaves the questions open.

1. Should an entry whose `path` covers every path make a file for every path the origin answers `404` for, which is what such an entry does today, or nothing, which is what an entry with no `path` does? An entry at `/` covers every path because `/` begins every path (`middleware.rs` 481 to 484) and the checks accept `/` (523 to 537). A `text/css` entry at `/` turns a missing page into an empty stylesheet with status `200`, and the test of the longest path relies on an entry at `/` making `/site.css` (`middleware/tests.rs` 1729 to 1735).
2. Should a request that names no host keep the origin's `404`? Today core sends the made empty document as it stands, because core processes content only when the request names a host, so the reader gets an empty `200`, and the same request for a file the origin has gets the origin's file without the middleware's change.
3. Should the check on `[ads-txt] modules = ["sellers"]` also require that an entry for `text/plain` has a `path` that covers `/ads.txt`? Today any entry for `text/plain` that names the module is accepted (`sellers/src/lib.rs` 144 to 159). An entry with no `path` loads, runs on every `text/plain` response and makes no `ads.txt` where the origin has none, and an entry for another path, `/robots.txt` for example, loads and makes the lines into that file. The message for a missing entry already tells the deployer to write `path = "/ads.txt"`.
4. Should the example settings file say that the sellers module also makes the file where the origin has none? The comment at `trusted-server.example.toml` lines 206 to 209 still says that the origin's file stays authoritative and the lines are written at the end of the origin's file, while the guides, the changelog and the module's comment say the file is made where the origin has none.

## 4. Tests

The change adds 11 tests. The tests are in `crates/trusted-server-core/src`.

| Behavior                                                                                                                                                                         | Test                                                                                               |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| A `GET` for `/ads.txt` that the origin answers `404` is made as `200` with `Content-Type: text/plain; charset=utf-8` and what the entry's middleware writes on an empty document | `a_file_the_origin_does_not_have_is_made_by_the_middleware_named_for_its_path` in `publisher.rs`   |
| A `HEAD` for the same path gets `200`, the same `Content-Type` and no body                                                                                                       | `a_head_request_for_a_made_file_gets_the_same_answer` in `publisher.rs`                            |
| An HTML entry with a `path` makes nothing, so the origin's `404` for `/news/missing` stands                                                                                      | `an_html_entry_with_a_path_makes_no_missing_page` in `publisher.rs`                                |
| An HTML entry for `/` written ahead of the `text/plain` entry for `/ads.txt` does not hide that entry                                                                            | `an_html_entry_ahead_does_not_shadow_the_entry_for_a_missing_file` in `publisher.rs`               |
| An entry with no `path` makes nothing                                                                                                                                            | `an_entry_covering_every_path_makes_no_missing_page` in `publisher.rs`                             |
| A request path that the entry's `path` does not begin keeps the origin's `404`                                                                                                   | `a_missing_file_outside_the_entry_path_keeps_the_origin_answer` in `publisher.rs`                  |
| A `500` from the origin on the entry's path stands                                                                                                                               | `an_origin_error_on_the_entry_path_keeps_the_origin_answer` in `publisher.rs`                      |
| `making` passes an entry that names no middleware, takes the first that does, and returns nothing where no entry with a `path` covers the request                                | `the_making_entry_has_a_path_covering_the_request_and_names_a_middleware` in `middleware/tests.rs` |
| An HTML entry with a `path` makes nothing                                                                                                                                        | `an_html_entry_with_a_path_makes_nothing` in `middleware/tests.rs`                                 |
| The entry with the longest covering path makes the file, and a shorter path still makes a file that only the shorter path covers                                                 | `the_entry_with_the_longest_covering_path_makes_the_file` in `middleware/tests.rs`                 |
| An HTML entry written ahead does not hide the entry that makes the file                                                                                                          | `an_html_entry_ahead_does_not_shadow_the_entry_that_makes_the_file` in `middleware/tests.rs`       |

Ten things have no test.

1. A method other than `GET` and `HEAD` for a path the origin answers `404` for (`publisher.rs` 5612).
2. The origin's headers. The stub origin's `404` carries a `Content-Type` alone (`publisher.rs` 21447 to 21451), so no test shows that a `Set-Cookie` or a `Cache-Control` on the `404` is dropped.
3. The real sellers module on a `404`. The tests in core use the fixture middleware in `integrations/registry.rs` that writes one comment line, and the sellers crate's tests run the module on an empty body outside the publisher path.
4. An adapter. No adapter test makes a file, and no test runs Fastly's `publisher_response_into_streaming_response` on one. The `GET` test calls `stream_publisher_body_async` and the `HEAD` test calls `buffer_publisher_response_async`.
5. A serve entry on a made file, and the headers a serve middleware writes on one. The tests set `[[fetch]]` entries alone (`publisher.rs` 21159 to 21166).
6. A request that names no host (`publisher.rs` 1909 to 1911).
7. An entry whose `path` covers every path and a missing page, and two entries of one path length for different media types (`middleware.rs` 481 to 484 and 487 to 493).
8. A made file of a type that is not `text/`, whose `Content-Type` carries no charset (`publisher.rs` 1923 to 1927).
9. A made file under a `[[cache.asset_rules]]` rule (`publisher.rs` 5789 to 5795).
10. A middleware that refuses a made file when the chain is planned (`publisher.rs` 731).

## 5. Acceptance

**CI.** Every check passed at `7723c5e56`. IABTechLab/trusted-server#1363 has 25 check runs at that commit, all successful, and jwrosewell/trusted-server#17, now closed, has 19, all successful. The test plan in the description of IABTechLab/trusted-server#1363 gives the 19 of 19 as the fork's CI on `7723c5e56` and does not give the 25. Under "How it was verified" the description says CI passed on `82c1a196e` on the fork, which is the last commit of the series, and four merges of row 21 follow that commit.

**Local checks.** The description of IABTechLab/trusted-server#1363 ticks `cargo test-fastly` and `cargo test-axum`, `cargo clippy-fastly` and `cargo clippy-axum`, `cargo fmt --all -- --check`, the JavaScript tests, the JavaScript format check, the docs format check, the WebAssembly build of the Fastly adapter and "the full workflow set of this repository, run on the pull request named above". The line above the ticks names the fork's pull request, jwrosewell/trusted-server#17, as the place CI ran. The description leaves the manual test through `fastly compute serve` unticked. The description says that no file has been made through an adapter against a real origin, and that the rule is checked through the publisher path with a stub origin answering `404`.

**Checks that can be repeated.** In a clone of IABTechLab/trusted-server, this diff prints `8 files changed, 448 insertions(+), 5 deletions(-)`, which is the footprint in section 2.

```text
git diff --shortstat 938be5d3b...7723c5e56
```

This search prints seven lines, six in `middleware/tests.rs` and one in `publisher.rs` (line 5613), so no adapter and no other code applies the rule.

```text
git grep -n "\.making(" 7723c5e56 -- crates
```

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand.                                                                                                                                                                                                                                                                                                                                                     |
| 2026-10-15 | Fixes from review round 1. The header and the pull requests the header names, what the change touches, the rule as tables for the trigger, the entry and the response, the empty document, caching, configuration and rollout, the adapters, consent and privacy, failure, the rows that hold what the change leaves out, the open questions, the tests and the cases with none, the acceptance and the review log. |
| 2026-10-16 | Fixes from review round 2. The acceptance gives the 25 check runs on the pull request and the 19 on the fork's closed pull request separately, says that the description records only the 19, and quotes the description for where the full workflow set ran.                                                                                                                                                       |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/22-creating` at `7723c5e56`. Issues found, four of them blocking. The four were that the spec said the made file is stored when core makes the file again on every request, that the rule left out four behaviors (any path under the prefix makes a file, the origin's `404` body and headers are dropped, an HTML entry makes nothing and the longest path wins), that the header named pull requests that are closed and never the series pull request, and that the acceptance said the pull request description records checks, 6 deliberate faults among them, which neither pull request description records. This revision states what the code does at that head for the trigger, the entry that makes the file, the response, the phases, caching, configuration, the adapters, consent and privacy, failure and the tests, replaces the acceptance claims with what the pull request description records, and leaves four questions about the code under Open questions until the code changes, being an entry whose `path` covers every path, a request with no host, the sellers check that accepts an entry that cannot make `/ads.txt` and the example settings file.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/22-creating` at `7723c5e56`. Approved, none blocking. This revision gives the 25 passing check runs on the pull request and the 19 on the fork's closed pull request separately, says that the description records only the 19, and quotes the description for where the full workflow set ran.
