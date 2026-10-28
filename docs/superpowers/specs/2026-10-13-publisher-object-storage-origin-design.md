# Design Spec: A publisher's site served from a folder of object storage

**Status:** Proposed, 2026-10-13. Raised as pull request IABTechLab/trusted-server#1377 (from jwrosewell/trusted-server#31, which is closed), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-page-middleware-design.md`, `2026-10-12-middleware-creates-a-missing-file-design.md`, `2026-04-28-multi-backend-asset-proxy-design.md` and `2026-10-14-origin-signing-design.md`, with every design document of the stack held in IABTechLab/trusted-server#1342
**Related PRs:** IABTechLab/trusted-server#1377, jwrosewell/trusted-server#31 (closed)
**Issues:** IABTechLab/trusted-server#1329
**Last updated:** 2026-10-16

## 1. Problem

A publisher whose site is static files in object storage can only be served by an asset route, and an asset route skips the page pipeline, so none of the publisher's `[[fetch]]` or `[[serve]]` middleware runs on its pages. The publisher proxy does run the pipeline, and three things keep it from reaching such a site. It sends the request path to the origin's host and discards any path in `origin_url`, so a site kept in a folder of a bucket has no address. It sends its requests unsigned, and only an asset route can sign. And object storage answers `folder/` with nothing, where a web server answers with `folder/index.html`.

## 2. Design

Two optional settings in `[publisher]`, `origin_auth` and `directory_index`, and a new meaning for the path of `origin_url`, which is required and so is no setting. Each setting is off unless set, and a document that sets neither serializes to the same bytes as before, so a build that predates them still reads that document. A document that sets one is refused by such a build, because `[publisher]` refuses a field it does not know, so the build is deployed before the document is pushed. Two changes also reach a deployment that sets neither setting. A path already in `origin_url` now decides where every request goes, and the check that answers `404` to the administration paths now closes more paths on every adapter. Each change is breaking for the requests it reaches (see "Rollout").

```toml
[publisher]
domain = "publisher.example"
cookie_domain = ".publisher.example"
origin_url = "https://storage.example/bucket/site"
proxy_secret = "publisher_proxy_secret"
directory_index = "index.html"

[publisher.origin_auth]
type = "s3_sigv4"
region = "example-region-1"
access_key_id = "publisher_origin_access_key_id"
secret_access_key = "publisher_origin_secret_access_key"
```

| A request for                                | Is sent to the origin as                         |
| -------------------------------------------- | ------------------------------------------------ |
| `/`                                          | `/bucket/site/index.html`, signed                |
| `/news/`                                     | `/bucket/site/news/index.html`, signed           |
| `/news`                                      | `/bucket/site/news`, signed                      |
| `/style.css?v=2`                             | `/bucket/site/style.css?v=2`, signed             |
| `/news/../../other-site/page`                | Not sent. Answered `400`                         |
| A `POST`, or any method but `GET` and `HEAD` | Not sent. Answered `405` with `Allow: GET, HEAD` |

The page that comes back is a publisher page like any other, so it is fetched, changed by the middleware named for it, stored and served as one from a web server is.

**The origin path.** A path in `origin_url` is kept, and the request path is added after it with exactly one `/` between them, whichever side carries one. The request's query is kept unless `origin_auth` sets `origin_query = "strip"` (see "The query"). An `origin_url` with no path sends the request path as it arrived, apart from the rebuilding of a signed address (see "Signing"). A query in `origin_url` is ignored, as it was. The path is no setting, because `origin_url` is required, and before this change the path was discarded. A deployment whose existing document has a path in `origin_url` therefore sends different requests to its origin, whether or not the document sets the two settings, because each request now goes to the folder the path names and a request path with a `.` or `..` segment is answered `400`. The path is a breaking change for such a deployment.

The publisher proxy builds the origin address in one place, `handle_publisher_request` in `publisher.rs`, and that address now comes from `publisher_origin_path_and_query`. The streamed page, the buffered page, the template cache key, a `HEAD` and every other method take the address built there. The publisher path follows no redirect. A URL in the folder in a response header, a redirect's `Location` among them, reaches the reader without the folder, and one outside the folder is left as the origin wrote it, as `2026-10-13-origin-redirect-design.md` sets out.

**Signing.** `origin_auth` is the table an asset route's `auth` takes, read as the same type, with the same fields, defaults and secret store key references, and it signs through the same function, `apply_asset_origin_auth`. There is one signing path. The request is signed last, once its address and `Host` are final. Signing rebuilds the address through the URL parser, `url::Url`, signs the rebuilt address and sends the request to it, so the two cannot differ. The parser writes the address in its own form. It percent-encodes an apostrophe or a non-ASCII character in the query, and a double quote, a curly brace or a non-ASCII character in the path, all of which `http::Uri` accepts as they are, and it resolves dot segments in the path, which are refused with `400` first when `origin_url` has a path. The table still accepts the deprecated `secret_store` field that an asset route's `auth` accepts. The field is ignored with a logged warning and is never written back.

**The query.** By default a signed read passes the reader's query to storage and signs it, in the parser's form described under "Signing". AWS's documentation of the S3 `GetObject` request lists query parameters that select a version of an object or set headers of the answer, `versionId` and `response-content-type` among them. A reader's query reaches storage with the publisher's signature on it. No test here calls S3, so what S3 does with those parameters rests on AWS's documentation. `origin_query` is a field of the `origin_auth` table that takes `preserve`, the default, or `strip`. The value `strip` leaves the query off before the request is signed, and a site that has no use for a query string sets `strip`. The cost is that `/style.css?v=2` and `/style.css?v=3` are sent to storage as the same address, so a version number in the query no longer changes the request.

**The Host that is signed.** The signer reads the `Host` header of the request, which is `origin_host_header_override` when that setting is set and the host of `origin_url` otherwise, with its port when the port is not the default. The signer lists `host` among the headers it signs, so the signature belongs to that value. Every adapter but Spin passes that value on (see "The adapters").

What keeps a signed read safe:

| The risk                                                                | What prevents it                                                                                                                                                                                                       |
| ----------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| The reader's credentials reach the storage service                      | `Cookie`, `Authorization` and `Proxy-Authorization` are removed before signing                                                                                                                                         |
| The reader's browsing history reaches the storage service               | `Referer` is removed                                                                                                                                                                                                   |
| A reader's cache decides what the publisher's credentials ask for       | `If-None-Match`, `If-Modified-Since`, `If-Match`, `If-Unmodified-Since`, `If-Range` and `Range` are removed, so every signed read asks for the whole object                                                            |
| The reader sets a header the signing scheme reads                       | Every header in the scheme's own namespace is removed, and the signer writes its own                                                                                                                                   |
| The publisher's credentials are used to write or delete                 | Only `GET` and `HEAD` are signed. Any other method is answered `405` and nothing is sent                                                                                                                               |
| A reader climbs out of the folder `origin_url` names                    | With a path in `origin_url`, a request path with a `.` or `..` segment, as written, after up to four rounds of percent-decoding, or while still changing after the fourth round, is answered `400` and nothing is sent |
| The reader's query reaches storage with the publisher's signature on it | Nothing by default, because the query is kept and signed. `origin_query = "strip"` leaves the query off                                                                                                                |
| The signed request, with any session token, travels in the clear        | `origin_url` must use `https` when `origin_auth` is set                                                                                                                                                                |

An origin with no `origin_auth` is sent the reader's headers and methods as before. The `400` for a dot segment does not depend on `origin_auth`. Whenever `origin_url` has a path, an unsigned origin answers `400` for every method, and a signed origin answers `400` for `GET` and `HEAD`, because any other method is answered `405` first.

**One decoding function for two checks.** The dot segment check and the closed path check in `closed_paths.rs` share one function, `matches_any_decoding`. The function tests a path as written and after each of four rounds of percent-decoding, which turns `%2e` into `.`, and counts a path that is still changing after the fourth round as a match, so each check fails closed. A match is a `400` for the dot segment check and a `404` for the closed path check, and every adapter runs the closed path check on every deployment (see "Rollout").

**What storage is sent, and what the reader gets back.** A signed read leaves off the ten reader headers the table above lists and every `x-amz-` header. The other headers of the request are sent, with `Host` set to the origin's and `Accept-Encoding` limited to what the page pipeline can decode, as for any origin. `User-Agent` therefore stays, and so does any client address header still on the request, such as `X-Forwarded-For`, because the list names none. What storage sends back is handled as any origin's answer is. Core has no rule that removes a storage service's response headers, so they reach the reader as any origin's do. AWS's documentation lists `x-amz-request-id` and `x-amz-id-2` among the headers S3 adds to a response, and no test here sends a storage response that carries them. This series changes no consent code, so consent is handled as before.

**When a read is refused.** The `405` and the `400` are plain text answers with `Cache-Control: no-store, private`, and the `405` carries `Allow: GET, HEAD`. Nothing is sent to storage for either. A read that storage refuses reaches the reader with the status, headers and body storage sent. A body that is not text, JavaScript or JSON is returned unchanged, and a text body goes through the page pipeline like any other error page the origin sends. AWS's documentation describes an error response from S3 as an XML document. No test here sends an S3 error response, so what a reader sees for one follows from the rule above and has not been tested. A `404` from storage can be made into a file as for any origin, and a `403` is not (see section 3). If signing fails, for example because the session token holds a character a header cannot carry, nothing is sent and the failure leaves `handle_publisher_request` as an error. The adapter answers that error as it answers any failed request, with `400` when the signer cannot write a header value and `502` for the other failures.

**The index file.** `directory_index` names one file. A request path ending in `/`, the root among them, has the name added for the origin request only, ahead of the query. A path without the trailing `/` is sent as asked, so `/news` is not guessed to be a folder. The reader's address does not change, and neither does the path the middleware and the creative opportunity rules see. The key is written in the document, because an environment override such as `TRUSTED_SERVER__PUBLISHER__DIRECTORY_INDEX` replaces a key the document already has and is ignored when the key is missing (see "Environment Variable Overrides (Typed CLI)" in `docs/guide/configuration.md`).

**What is refused when the settings load.**

- An `origin_auth` with an empty or malformed `region`, with credentials that are empty once resolved, or beside an `origin_url` that is not `https`. The message names `publisher.origin_auth`.
- An `origin_auth` with a `type` or a field its table does not know, or that is not a table. The parser refuses the document with its own message, to which this change adds nothing.
- An `origin_auth` with an empty secret key reference, by `ts config validate` and `ts config push`, naming the reference, such as `publisher.origin_auth.access_key_id`.
- A `directory_index` that is not one file name. A name may hold only ASCII letters and digits and the characters `-._~!$&'()*+,;=:@`, may not be `.` or `..`, and may not be longer than 255 bytes. An empty name, or one with a `/`, a `%`, a space, a query, a fragment, a backslash or a letter outside ASCII, is refused. The message names `publisher.directory_index`.

An asset route's `auth` is refused in the words it was, naming the asset route.

**The adapters.** All four adapters reach the origin through `handle_publisher_request` in core, and no adapter file changed, so the two settings and the path in `origin_url` work the same way on each. The adapters differ in what they do with the `Host` header of the outbound request. The Fastly adapter sets the `Host` it sends through the backend it registers for the origin, from `origin_host_header_override` when that is set. The Axum and Cloudflare adapters copy the request's headers, `Host` among them, onto the outbound request. The Spin adapter drops `Host`, because WASI HTTP forbids it on an outbound request, and takes the host from the request address. On Spin an `origin_host_header_override` that differs from the host of `origin_url` therefore makes the signature cover one `Host` while the request carries another, so a Spin deployment that signs sets no override.

**Rollout.** Two changes reach a deployment that sets neither `origin_auth` nor `directory_index`, and each is breaking for the requests it reaches.

1. A path in `origin_url` was discarded before and is now kept. A deployment whose `origin_url` has one sends each request to the folder the path names, and answers `400` to a request path with a `.` or `..` segment, with or without `origin_auth`. A deployment whose `origin_url` has no path sends its requests as before.
2. The closed path check in `closed_paths.rs` closes `/_ts/admin` and `/admin/keys`, with the paths beneath them, by answering `404`. All four adapters run it before their publisher fallback, so no setting turns it off and no adapter file changed. A request path still changing after four rounds of percent-decoding is now answered `404`, where the publisher received it before. A path with an escape that is not valid UTF-8 is now read with the bad bytes replaced and checked again, so it is answered `404` when it reaches a closed path, where the check stopped at that escape and the publisher received the request before.

The build is deployed before a document that sets `origin_auth` or `directory_index` is pushed, because a build that predates them refuses the document. A deployment whose `origin_url` already has a path first checks that its origin serves the site at that path, or removes the path. Rolling back is the reverse. A build that predates this change refuses a document that sets either setting, so the document is changed first, and that build also discards the path in `origin_url` and sends unsigned requests, so it cannot serve a site kept in a folder of storage.

**Why the publisher proxy and not a route of its own.** The page pipeline, the template cache, the making of a file the origin does not have and the cache policy for a path all hang off the publisher path. A second path for a site kept in storage would carry each of them again, and a page would behave differently according to where its bytes are kept.

**Why the settings and not a module.** Each change acts on the request that core already makes to the one origin `[publisher]` names, and the signer is the one core already has for asset routes. The path in `origin_url` and `directory_index` name no vendor. `origin_auth` does, because AWS signing is a vendor's and does not belong in core. Core already kept AWS signing for asset routes. This series adds the `origin_auth` setting to `[publisher]` and, in `s3_sigv4.rs`, the `x-amz-` header rule that removes a reader's headers from the signing namespace. A later series, whose design is `2026-10-14-origin-signing-design.md` (IABTechLab/trusted-server#1391), moves the signer, its settings type and that header rule out of core into a shared crate.

## 3. What is not in this change

- **Validators and ranges on a signed origin.** A browser revalidating a file through a signed publisher origin is sent the file again, and a range request is answered with the whole file. An asset route forwards both, and stays the place for large media.
- **A missing object read as a missing file.** A file the origin does not have is made when the origin answers `404`. Some object storage answers a missing object with `403`, and with `404` only where the credentials may also list the bucket. No storage service was called in testing this change, so a deployment that wants a file made checks what its storage answers for an object it does not have.
- **Addresses in a page that name the storage host.** A site that links to itself by relative or root-relative addresses is unaffected. An absolute address naming the storage host is rewritten to the reader's host as before. In a style sheet or a script the folder is removed with it, because the whole of `origin_url` is matched there. In an HTML document only the host is matched, so the folder stays in the path, and the address then names nothing beneath the folder.
- **Media types.** The page pipeline runs on what the origin says a file is, so each object is stored with its media type.
- **A trailing slash on `origin_url`** is refused, as it was. The join gives one `/` whether or not the path ends in one.
- **A second signing scheme.** `origin_auth` takes the one type an asset route takes.

## 4. Acceptance

Tests in `trusted-server-core`, where the code is:

- A publisher with neither `origin_auth` nor `directory_index`, and an `origin_url` with no path, sends each origin address it sent before, a path with a dot segment and a doubled slash among them, with the reader's cookies, `Authorization` and referrer, and a `POST` as a `POST`.
- A path in `origin_url` is kept and joined with one `/`, with and without a trailing `/` on either side, and the query is kept.
- `directory_index` is added to `/` and to a path ending in `/`, and to no other path, and the page pipeline is given the reader's own path.
- A signed request carries the signature the signer gives for the address, `Host` and method that were sent, and none of the reader's headers the table above lists, with one assertion for each. A `HEAD` is signed as a `HEAD`, and `origin_query = "strip"` leaves the query off before signing.
- A signed origin answers `POST`, `PUT`, `DELETE` and `OPTIONS` with `405` and `Allow: GET, HEAD`, and a request path with a dot segment under a path in `origin_url` is answered `400` in each of eight spellings, and neither is sent.
- A path still changing after the last round of percent-decoding is closed by the closed path check in `crates/trusted-server-core/src/closed_paths.rs`.
- Each malformed `origin_auth` and `directory_index` is refused when the settings load, with the setting named, and the documented example in `trusted-server.example.toml` loads when uncommented.
- The credentials of `origin_auth` are resolved from the secret store as an asset route's are, masked in the configuration view, never served by it, and kept out of debug output.

The tests of the publisher path are in the `object_storage_origin_tests` module of `crates/trusted-server-core/src/publisher.rs`, and each behavior in the table is asserted by the test beside it.

| Behavior                                        | Test                                                                 |
| ----------------------------------------------- | -------------------------------------------------------------------- |
| The reader's headers are left off a signed read | `signed_origin_request_carries_none_of_the_reader_s_private_headers` |
| Headers in the signing namespace are left off   | `signed_origin_request_carries_none_of_the_reader_s_private_headers` |
| A `HEAD` is signed as a `HEAD`                  | `signed_head_request_is_signed_as_a_head`                            |
| The path of `origin_url` is kept                | `origin_path_is_kept_and_joined_with_one_slash`                      |
| `directory_index` is added                      | `directory_index_changes_the_origin_address_and_not_the_reader_s`    |
| A method that is not a read is answered `405`   | `signed_origin_refuses_a_method_that_is_not_a_read`                  |
| A dot segment is answered `400`                 | `origin_path_refuses_a_reader_path_that_climbs_out_of_it`            |

**Tests that are missing.** Four gaps have no test.

1. No test sends a page from a signed folder origin through `[[fetch]]`, `[[serve]]` or the template cache. The signed tests answer with a plain `200` and check the request sent to storage, and the test that streams a page through the pipeline sets `directory_index` and no `origin_auth`.
2. The signed tests set `origin_auth` on the settings struct, so none loads a document that carries `[publisher.origin_auth]` and then sends a request through it.
3. The refusals of a `type` or a field the table does not know, and of an `origin_auth` that is not a table, are checked only for the text `origin_auth`, which the parser's own echo of the document line satisfies, so no test checks the full name `publisher.origin_auth` for them.
4. No test combines a dot segment with `origin_auth`, sets `origin_host_header_override` on a signed origin, gives the closed path check an escape that is not valid UTF-8, or sends a signed read whose query or path holds a character the URL parser encodes.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                               |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-13 | First draft.                                                                                                                                                                                                                                                                                                                                         |
| 2026-10-14 | Names its draft pull request and issue.                                                                                                                                                                                                                                                                                                              |
| 2026-10-15 | Fixes from review round 1. The path of `origin_url` and the closed path check as breaking changes with the upgrade and rollback order, `origin_query`, the vendor claim, the adapters and the `Host` that is signed, what a signed read sends and what a reader sees on a refusal, the rule for `directory_index`, the missing tests and the header. |
| 2026-10-15 | Corrections from review round 2. The changelog entry, the `x-amz-` rule this series adds, the rebuilt address, the methods the `400` applies to, and the statements about S3 attributed to AWS's documentation.                                                                                                                                      |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/36-object-storage` at `f5f8ac2d0`. Issues found, four of them blocking. Section 2 counted the path of `origin_url` as an optional setting that is off unless set, when it changes the requests of every deployment whose document has one, left out that the closed path check now answers `404` to a path still changing after four rounds of percent-decoding on every deployment, said the query is always kept when `origin_query = "strip"` leaves it off, and said no setting is a vendor's when AWS signing is. This revision states both changes as breaking with the upgrade and rollback order, puts `origin_query` in the design, says that this series adds the AWS signing setting where core already kept AWS signing and that a later series moves it, and adds the adapters, the `Host` that is signed, what a signed read sends, what a reader sees when a read is refused, the exact rule for `directory_index`, the tests that are missing and the pull request that holds the stack's design documents.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `1465a29d1`, of the round 1 fixes and the new text. One blocking issue, in the changelog, and three smaller ones. This revision moves the path in `origin_url` and the closed path change into the Changed section of the changelog as breaking entries and corrects the changelog entry's count of settings, says that this series adds the `x-amz-` header rule to core, says that signing rebuilds the address through the URL parser and that a signed origin answers a method other than `GET` or `HEAD` with `405` before the dot segment check, and attributes what the page says about S3 to AWS's documentation.
