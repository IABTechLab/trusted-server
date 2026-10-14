# Design Spec: A source module says where the publisher path and the asset routes fetch from

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-13-publisher-object-storage-origin-design.md`, `2026-10-13-origin-redirect-design.md`, `2026-10-13-request-addressing-design.md`, `2026-04-28-multi-backend-asset-proxy-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1332
**Last updated:** 2026-10-14

> **Why this spec exists.** Core's publisher path fetches from the origin `[publisher]` names and each asset route from the origin it names, so no module can say where a request's content comes from, and where it comes from is written in two places in two shapes.

## 1. Problem

Where a deployment's content comes from is written twice. `[publisher]` carries `origin_url`, `origin_host_header_override`, `origin_auth` and `directory_index` beside who the publisher is, and every `[[proxy.asset_routes]]` entry carries its own `origin_url` and `auth`. Both are fetched by code in core that knows one kind of source, an HTTP origin.

A deployment whose origin depends on the request has no place for the code that chooses it. An origin chosen from the request's own address, as on a host that serves many publishers by path, or by a rule of the deployer's own, would otherwise be one more branch in core's fetch, running for every deployment.

The fetch and serve phases change a response. They do not choose where it comes from, so a middleware is the wrong place too.

## 2. Design

**A source module says where content comes from.** Core declares the interface. The HTTP origin fetch is the one source core supplies, as the module `origin`. A module a build links offers another under its own name, as every other module type is offered.

**One shape for where content comes from.** A source is selected in a module section, `module` naming it and a table beneath it holding its settings, the shape `[geo]` and `[prefix]` already use. The publisher's source is `[source]`. An asset route's is the route's own `source` table.

```toml
[publisher]
domain = "www.example.com"
cookie_domain = ".example.com"
proxy_secret = "proxy_secret_name"

[source]
module = "origin"

[source.origin]
url = "https://origin.example.com"
host_header = "www.example.com"

[[proxy.asset_routes]]
prefix = "/static/"

[proxy.asset_routes.source]
module = "origin"

[proxy.asset_routes.source.origin]
url = "https://bucket.object-store.example/site"
auth = { type = "s3_sigv4", region = "eu-west-1", access_key_id = "id_name", secret_access_key = "key_name" }
```

`[publisher]` keeps who the publisher is and how its pages are processed. Every field that said where the pages come from moves to the `origin` source's table, under the names the table above shows:

| Was                                       | Is                                        |
| ----------------------------------------- | ----------------------------------------- |
| `[publisher] origin_url`                  | `[source.origin] url`                     |
| `[publisher] origin_host_header_override` | `[source.origin] host_header`             |
| `[publisher] origin_auth`                 | `[source.origin] auth`                    |
| `[publisher] directory_index`             | `[source.origin] directory_index`         |
| `[[proxy.asset_routes]] origin_url`       | `[proxy.asset_routes.source.origin] url`  |
| `[[proxy.asset_routes]] auth`             | `[proxy.asset_routes.source.origin] auth` |

An asset route keeps `prefix`, `path_pattern`, `target_path` and `image_optimizer`, which say which requests it answers and what path it asks for. A path in an asset route's `url` names a folder the target path is added under, as a path in the publisher's `url` does. A document that writes a field where it used to be is refused at startup, with the new place named in the message, and a document with no `[source]` is refused. A source table naming a module the build does not link is refused with the names on offer, as every other module selection is.

The configuration page masks `url` and `host_header` by default, as it masked the fields they replace, and the signing secrets are declared at their new paths.

**What a source answers.** For each request a source first answers where the content comes from: the origin's scheme, host, port and folder, the `Host` header to send, how the request is signed and whether its query is kept, and the folder's index document. Core's caches key on that answer, and core rewrites the origin's host in the page, follows the redirect rule and tells middleware the origin host from it, so a source that answers differently for each request is kept apart in every one of those places.

Core fetches the location a source answers through the same send, the same early send, streaming and the same cache paths as today. The `origin` source answers the address, `Host` and signing the old fields named, and the cache keys are made from them in the same form, so an existing deployment's stored pages and assets keep their keys.

**One call for every path.** The publisher's page, the publisher's assets held in the asset cache, and each asset route ask their source the same way, so content a source answers for is fetched and cached on the same terms whichever path asked.

**The request a source is given.** The method, the path and query the request continues with, its headers, and the prefix request addressing removed, if any. That lets a source take where to fetch from out of the address.

## 3. What is not in this change

- Any source other than `origin`. A test supplies one that answers a different origin for each request, which is what a source reading the address does.
- A source supplying the response itself, for content that has no HTTP origin. That is a second method on the interface, added with the first source that needs it.
- Selecting a source per path for the publisher's pages. A deployment wanting that writes an asset route.

## 4. Tests

- Each old field is refused naming its new place, in a document and in a configuration blob, and a document with no `[source]` is refused.
- `[source.origin]` reads, trims and checks its address, `Host`, index file and signing, and refuses an unknown key, naming the setting. Environment variables set the same fields at their new paths.
- A location gives the host with its port, the `Host` it sends, a folder from the address's path, and an identity in the form the caches used before, so the stored keys do not change.
- A source answering a different origin for each request sends each request to its own origin and tells the page pipeline that request's origin host.
- An unknown source name is refused with the names on offer.
- An asset route reads its source, fetches its target path under the source's folder, and signs with the source's signing.
- The configuration page masks the origin's address and `Host` by default and declares the signing secrets at their new paths.
