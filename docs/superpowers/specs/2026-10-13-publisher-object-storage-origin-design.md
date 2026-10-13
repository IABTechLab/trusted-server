# Design Spec: A publisher's site served from a folder of object storage

**Status:** Proposed, 2026-10-13. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-page-middleware-design.md`, `2026-10-12-middleware-creates-a-missing-file-design.md`, `2026-04-28-multi-backend-asset-proxy-design.md`
**Related PRs:** none yet
**Last updated:** 2026-10-13

> **Why this spec exists.** A publisher whose site is static files in object storage can only be served by an asset route, and an asset route skips the page pipeline, so none of the publisher's `[[fetch]]` or `[[serve]]` middleware runs on its pages.

## 1. Problem

A publisher whose site is static files in object storage can only be served by an asset route, and an asset route skips the page pipeline, so none of the publisher's `[[fetch]]` or `[[serve]]` middleware runs on its pages. The publisher proxy does run the pipeline, and three things keep it from reaching such a site. It sends the request path to the origin's host and discards any path in `origin_url`, so a site kept in a folder of a bucket has no address. It sends its requests unsigned, and only an asset route can sign. And object storage answers `folder/` with nothing, where a web server answers with `folder/index.html`.

## 2. Design

Three optional settings in `[publisher]`. Each is off unless set, and a document without them sends the origin request it sent before and serializes to the same bytes, so a build that predates them still reads it. A document that sets one is refused by such a build, because `[publisher]` refuses a field it does not know, so the build is deployed before the document is pushed.

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

**The origin path.** A path in `origin_url` is kept, and the request path is added after it with exactly one `/` between them, whichever side carries one. The request's query is kept. An `origin_url` with no path, or the path `/`, sends the request path as it arrived, which is what every existing document does. A query in `origin_url` is ignored, as it was.

The publisher proxy builds the origin address in one place, `handle_publisher_request` in `publisher.rs`, and that address now comes from `publisher_origin_path_and_query`. The streamed page, the buffered page, the template cache key, a `HEAD` and every other method take the address built there. The publisher path follows no redirect. A redirect the origin sends to itself has the origin path taken off its `Location`, as `2026-10-13-origin-redirect-design.md` sets out.

**Signing.** `origin_auth` is the table an asset route's `auth` takes, read as the same type, with the same fields, defaults and secret store key references, and it signs through the same function, `apply_asset_origin_auth`. There is one signing path. The request is signed last, once its address and `Host` are final, and it is sent to the address that was signed, so the two cannot differ.

What keeps a signed read safe:

| The risk                                                          | What prevents it                                                                                                                                                         |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| The reader's credentials reach the storage service                | `Cookie`, `Authorization` and `Proxy-Authorization` are removed before signing                                                                                           |
| The reader's browsing history reaches the storage service         | `Referer` is removed                                                                                                                                                     |
| A reader's cache decides what the publisher's credentials ask for | `If-None-Match`, `If-Modified-Since`, `If-Match`, `If-Unmodified-Since`, `If-Range` and `Range` are removed, so every signed read asks for the whole object              |
| The reader sets a header the signing scheme reads                 | Every header in the scheme's own namespace is removed, and the signer writes its own                                                                                     |
| The publisher's credentials are used to write or delete           | Only `GET` and `HEAD` are signed. Any other method is answered `405` and nothing is sent                                                                                 |
| A reader climbs out of the folder `origin_url` names              | With a path in `origin_url`, a request path with a `.` or `..` segment, as written or after up to four rounds of percent-decoding, is answered `400` and nothing is sent |
| The signed request, with any session token, travels in the clear  | `origin_url` must use `https` when `origin_auth` is set                                                                                                                  |

An origin with no `origin_auth` is sent the reader's headers and methods as before.

**The index file.** `directory_index` names one file. A request path ending in `/`, the root among them, has the name added for the origin request only. A path without the trailing `/` is sent as asked, so `/news` is not guessed to be a folder. The reader's address does not change, and neither does the path the middleware and the creative opportunity rules see.

**What is refused when the settings load.**

- An `origin_auth` with an empty or malformed `region`, with credentials that are empty once resolved, with a `type` or a field it does not know, or beside an `origin_url` that is not `https`. The message names `publisher.origin_auth`.
- An `origin_auth` with an empty secret key reference, by `ts config validate` and `ts config push`, naming the reference, such as `publisher.origin_auth.access_key_id`.
- A `directory_index` that is not one file name, being empty, `.` or `..`, or holding a `/`, a `%`, a space, a query or a fragment. The message names `publisher.directory_index`.

An asset route's `auth` is refused in the words it was, naming the asset route.

**Why the publisher proxy and not a route of its own.** The page pipeline, the template cache, the making of a file the origin does not have and the cache policy for a path all hang off the publisher path. A second path for a site kept in storage would carry each of them again, and a page would behave differently according to where its bytes are kept.

**Why the settings and not a module.** Each of the three changes the request core already makes to the one origin `[publisher]` names. None is a vendor's, and the signer is the one core already has for asset routes.

## 3. What is not in this change

- **Validators and ranges on a signed origin.** A browser revalidating a file through a signed publisher origin is sent the file again, and a range request is answered with the whole file. An asset route forwards both, and stays the place for large media.
- **A missing object read as a missing file.** A file the origin does not have is made when the origin answers `404`. Some object storage answers a missing object with `403`, and with `404` only where the credentials may also list the bucket. No storage service was called in testing this change, so a deployment that wants a file made checks what its storage answers for an object it does not have.
- **Addresses in a page that name the storage host.** A site that links to itself by relative or root-relative addresses is unaffected. An absolute address naming the storage host is rewritten to the reader's host as before. In a style sheet or a script the folder is removed with it, because the whole of `origin_url` is matched there. In an HTML document only the host is matched, so the folder stays in the path, and the address then names nothing beneath the folder.
- **Media types.** The page pipeline runs on what the origin says a file is, so each object is stored with its media type.
- **A trailing slash on `origin_url`** is refused, as it was. The join gives one `/` whether or not the path ends in one.
- **A second signing scheme.** `origin_auth` takes the one type an asset route takes.

## 4. Acceptance

Tests in `trusted-server-core`, where the code is:

- A publisher with none of the three settings sends each origin address it sent before, a path with a dot segment and a doubled slash among them, with the reader's cookies, `Authorization` and referrer, and a `POST` as a `POST`.
- A path in `origin_url` is kept and joined with one `/`, with and without a trailing `/` on either side, and the query is kept.
- `directory_index` is added to `/` and to a path ending in `/`, and to no other path, and the page pipeline is given the reader's own path.
- A signed request carries the signature the signer gives for the address, `Host` and method that were sent, and none of the reader's headers the table above lists, with one assertion for each. A `HEAD` is signed as a `HEAD`, and `origin_query = "strip"` leaves the query off before signing.
- A signed origin answers any method but `GET` and `HEAD` with `405`, and a request path with a dot segment under a path in `origin_url` is answered `400`, and neither is sent.
- Each malformed `origin_auth` and `directory_index` is refused when the settings load, with the setting named, and the documented example in `trusted-server.example.toml` loads when uncommented.
- The credentials of `origin_auth` are resolved from the secret store as an asset route's are, masked in the configuration view, never served by it, and kept out of debug output.

Seven deliberate faults were made, in four runs, and each was caught by a test: the reader's headers left on, the signing namespace's headers left on, a `HEAD` signed as a `GET`, the origin path dropped, the index file dropped, the `405` removed and the dot segment check removed.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-13 | First draft. |
