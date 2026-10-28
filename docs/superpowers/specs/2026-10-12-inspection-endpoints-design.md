# Design Spec: Inspection endpoints

**Status:** Proposed, 2026-10-12. Raised as pull request IABTechLab/trusted-server#1350, row 9 of the stack in IABTechLab/trusted-server#777, for Tech Lab review. The two earlier pull requests for the same commit, jwrosewell/trusted-server#4 and IABTechLab/trusted-server#1294, are closed.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, in IABTechLab/trusted-server#1342, which holds every design document of the stack
**Related PRs:** IABTechLab/trusted-server#1350, IABTechLab/trusted-server#1348, jwrosewell/trusted-server#4 (closed), IABTechLab/trusted-server#1294 (closed)
**Issues:** IABTechLab/trusted-server#1268
**Last updated:** 2026-10-26

> **Why this spec exists.** A reader, a regulator or an auditor has only the operator's word for what a deployment runs, decides and keeps. Three endpoints in this change show those three things, and this spec says where each endpoint answers, what each shows and to whom.

## 1. Problem

A deployment decides what a request is permitted, holds the settings that decide how pages are changed, and may keep a record against the identifier it writes to a browser. None of that can be read by the person it concerns without a credential or the operator's word. The administration endpoints that exist answer an operator alone, and they leave the publisher's domain in a later change.

## 2. Design

**Five addresses.** Three endpoints answer at five addresses, all under `/_ts` and all to `GET`, and none asks for a credential. One handler in core answers each address, and all four adapters (Fastly, Axum, Cloudflare and Spin) route to it, so the addresses are the same on every deployment.

| Address                 | Form                         | Answers                                 | Status                                                                               |
| ----------------------- | ---------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------ |
| `/_ts/permissions`      | A page                       | Anyone                                  | `200`                                                                                |
| `/_ts/permissions.json` | The same information as JSON | Anyone                                  | `200`                                                                                |
| `/_ts/config`           | A page                       | Anyone                                  | `200`, `404` when `[inspect] config = false`, or `500` when the view cannot be built |
| `/_ts/config.json`      | The same information as JSON | Anyone                                  | The same as `/_ts/config`                                                            |
| `/_ts/data`             | A page, with no `.json` form | A browser opening the address as a page | `200`, or `403` for any other request                                                |

**Headers.** Every answer the permissions and configuration handlers build, the configuration's `404` and `500` among them, sends `Cache-Control: no-store` and `Access-Control-Allow-Origin: *`, with `Content-Type: text/html; charset=utf-8` for a page or `application/json` for data. An error from reading the request's state is plain text and carries neither header. The data address sends the headers listed under the data endpoint and no `Access-Control-Allow-Origin`. The statuses in the table are the handlers' own, and the failures under "When something goes wrong" are the others.

**Pages.** A page is the JSON answer, indented and escaped, inside a short HTML document marked `noindex`, and the page says that `.json` added to its address gives the same information as data. The data page says nothing of a data form because it has none.

**Methods.** `GET` is the only method any adapter answers at these addresses. Fastly, Axum and Spin send `POST`, `HEAD`, `OPTIONS`, `PUT`, `PATCH` and `DELETE` on these addresses to the publisher fallback, the catch-all route that forwards a request to the publisher's origin, as they do for their other named addresses. Cloudflare registers no other method for them, so those methods reach its shared catch-all route, described next.

**Other methods on Cloudflare.** Cloudflare registers its shared catch-all route at `/` and `/{*rest}` for every method (lines 984 to 987 of `crates/trusted-server-adapter-cloudflare/src/app.rs` at `497d553c6`), so a `POST`, `HEAD`, `OPTIONS`, `PUT`, `PATCH` or `DELETE` to one of these addresses matches that route. The route's function, `dispatch` (lines 688 to 780), refuses a request only for the administration paths, being those that start with `/_ts/admin` and the retired `/admin/keys` aliases (`deny_admin_diagnostic_fallback`), and none of these five addresses is one of them. The function then runs the modules' request preparers (explained under "What runs before the handler" below), and then serves a tsjs asset for a `GET`, hands the request to a module route that matches its method and path, or otherwise treats the request as one for the publisher. A `POST` to `/_ts/config`, `/_ts/permissions` or `/_ts/data` therefore ends in the publisher branch (lines 736 to 777), after the preparers have run, unless a module has declared a route for that method and address.

**What runs around the handler.** Every adapter wraps these routes in its outer layers. The response finalizer adds its geo and version headers, then applies the operator's `response_headers`. An operator's header replaces a header of the same name on the permissions and configuration answers, except `Cache-Control` and the headers that control a shared cache, which an operator cannot set on a response marked `no-store`. A `[[handlers]]` rule (HTTP Basic authentication for the paths its pattern matches) whose pattern matches an address answers `401` to a request without the rule's credentials, before the handler runs. The settings require a rule only for the `/_ts/admin` addresses, so none is required for these five.

**What runs before the handler.** On Fastly the modules' request preparers (each can clean up a query parameter or cookie its module reserves) and the request filters (bot protection modules use them to challenge a request) never run for these addresses, because Fastly returns the answer from the route's handler and does not go on to the step that runs them. Fastly builds no services for `/_ts/config`, and builds the request's services for `/_ts/permissions` and `/_ts/data` before it answers. On Spin the preparers never run for these addresses either, because the route's handler answers, and Spin has no filters. Axum and Cloudflare run the preparers, then answer, and run no filters. On Axum and Cloudflare a preparer that refuses a request ends it with an error response before the handler runs. A preparer's change to the request therefore reaches the permissions answer on Axum and Cloudflare and not on Fastly or Spin. No adapter runs an Edge Cookie finalizer (the step that sets the cookie and writes the identity row) for these addresses, because Fastly answers before its finalizer runs and the other three adapters have none, so nothing is written for the reader.

**The permissions endpoint.** The answer is the evaluation a page receives as `window.tsjs.permissions`, which holds `set` (the permissions that are set), `awaiting` (the ones still waiting for a signal), `signals` (each signal a module read and found valid, as received) and `tdls` (the terms documents the data is available under), with three things added that a person asking why a permission is not set needs and a page does not.

1. `storageWithdrawn` says whether the request explicitly withdrew storage.
2. `modules` says which signal modules the publisher named (`configured`, which is `null` when the publisher named none and the adapter's own list runs) and which of them produced a signal used in the answer (`contributed`).
3. `version` is the version of the core crate that answered. That is the workspace package version, `0.1.0` at `497d553c6`, and not the build identifier the `X-TS-Version` header carries.

Row 4 of the stack (IABTechLab/trusted-server#1345) selects the permission policy when a service is deployed (`2026-07-30-permission-model-design.md` §3.1). This row builds on row 4 and adds a fourth, `policy`, to `permissions_payload` (`inspect/permissions.rs`). The field names the policy file whose rules the answer resolved, as the settings give it in `[permissions] policy`. The field is `null` only for settings that name no policy, which no loaded domain has, because the load refuses them. A name is a file's stem, which two versions of a file share, so the SHA-256 the configuration endpoint shows is what matches the answer to one exact copy of the file.

One function spells the page's shape, and a test checks that the answer holds every key the page is given.

The handler resolves the request's state with the call a page request uses, from the request's signals (its cookies and headers) and the location of its client address, and writes nothing for the reader. The answer shows the asker the signals the asking request carried and nothing about any other request. The answer is never stored, and any origin may read it, so a page on another site can render it. A request from another origin carries no cookies, so no stored signal reaches the resolution. The `Sec-GPC` header the browser sends and the location of the client address still do.

**The configuration endpoint.** Anyone can read the scripts a page runs, and the configuration endpoint lets anyone read the configuration that runs at the edge too. The answer holds `version` (the core crate's version, as above), `settings` and `masked`. `settings` is the settings the deployment holds after defaults are applied, with each masked value replaced by `XXXX` and every object's keys sorted, so the same settings always give the same bytes. `masked` lists every masked path. The view is built from the settings alone, so every request gets the same one, and the view is built again on every request, with no rate limit.

`settings.permissions` holds `policy`, the policy file's name as the settings give it, and `sha256`, the SHA-256 of that file. The push also writes `text`, the file itself, into the blob (the settings as `ts config push` stores them) where the policy travels inside it, and row 4's runtime loader takes that text out of the settings when it loads the policy (`load_domain_policy` in `config_payload.rs`). So the view, which is the masked settings, shows `permissions` as `policy` and `sha256` only and never `text`, with no code of its own for that (`build_view` in `inspect/config.rs`). Neither `policy` nor `sha256` is masked by default, so a publisher can check which exact file a domain runs. `[inspect] config = false`, or a `hide` that covers `permissions.sha256`, withholds the SHA-256 from every address, and `/_ts/permissions` then shows only the name.

Three kinds of value are masked.

1. Every secret, whatever the publisher's document says. The settings loader records each value it fills from the secret store, and those paths are masked, along with every field declared a secret. Then any string that still contains one of those values is masked wherever it sits, and a key that contains one is renamed `XXXX` (or `XXXX-2` and on where that name is taken).
2. Every value sensitive by default, unless the publisher shows it, being the origin URLs (`publisher.origin_url` and `proxy.asset_routes[].origin_url`), the origin host override (`publisher.origin_host_header_override`), the Basic-auth path and username (`handlers[].path` and `handlers[].username`), and the store names and ids (`ec.ec_store`, `request_signing.config_store_id`, `request_signing.secret_store_id` and `auction.creative_store`). Any other value the settings mark as redacted is sensitive by default too.
3. Everything the publisher hides.

Every other value shows in clear, which includes `trusted_client_ip.ip_header` and `trusted_client_ip.auth_header`, the names of the headers that carry a trusted client address and its shared secret. A value that is empty (null, an empty string, an empty list or an empty table) shows as it is and is not listed as masked.

The publisher decides with a new `[inspect]` section, which holds `config`, `show` and `hide`. `config` defaults to `true`, and `config = false` answers `404` at both configuration addresses, with the message `This publisher does not publish its configuration.` in the body. `show` and `hide` default to empty. Each entry is a path pattern, being keys joined by `.`, with `[]` for every element of a list and `[N]` for one, such as `proxy.asset_routes[].origin_url` or `handlers[0].path`. A pattern names the values at exactly its own depth, and a key that holds `.`, `[` or `]` cannot be named. `show` reveals only a value that is masked by default and is not a secret. `hide` masks a value whatever `show` says, and a secret is masked whatever either says.

A pattern that would not be honored as written refuses the configuration, both when a deployment is validated and when the settings load, so a publisher cannot believe a value is hidden when it is shown. Six cases are refused.

1. A pattern that matches no value in the configuration.
2. A `show` that names a secret.
3. A pattern that another pattern of the same list already covers.
4. A `show` that a `hide` covers.
5. A pattern that changes nothing the view shows, being a `show` of values that are not masked by default or a `hide` of values that are masked or empty already.
6. Any `show` or `hide` beside `config = false`.

Every place that checks patterns goes through `validate_patterns` (`inspect/config.rs`), being `ts config validate`, `ts config push`, `ts config diff`, `ts audit generate` and the load, and from row 51 `ts config migrate`. `permissions.text` is never shown, so `validate_patterns` refuses a pattern under `permissions.text`, and judges every other pattern against the settings as the view shows a domain the push wrote and the loader loaded, with `permissions.text` gone and `permissions.sha256` present, because the push always writes it (`as_the_view_sees_a_pushed_domain`). So a pattern that names `permissions.sha256` is judged alike before the push and at the load. `ts config validate` takes no `--adapter`, so it cannot know whether the push adds the text (`2026-07-30-permission-model-design.md` §3.1, items 2 and 10).

At deploy time the secret fields still hold the names of keys in the secret store. The deploy check therefore knows which leaves are secrets, including the passphrase in an Edge Cookie module table the operator gave a label of their own and the secrets a module declares, and cannot scan for their values.

A stored document is unchanged. The marking that lets the view find sensitive values is a mode of one serialization call, every other serialization writes what it wrote before, and the `[inspect]` section is left out of a document unless the publisher wrote one. Tests check both.

**The data endpoint.** A deployment that writes an identifier to a browser can show the person using that browser what the deployment keeps against that identifier. The page shows the identity graph record stored against the identifier in the request's own `ts-ec` cookie. The address takes no identifier of its own, so no other record can be asked for. The page shows the whole record as it is stored, with the Edge Cookie's value and each partner's identifier replaced by `XXXX`. These are the fields the record can hold, and a field with no value is left out.

- `v`, the schema version, and `created`, the time the record was made.
- `consent`, which holds the raw TCF and GPP strings as received, `ok` (false once consent is withdrawn) and `updated`.
- `geo`, which holds `country`, `region`, `asn` and `dma`.
- `pub_properties`, which holds `origin_domain` and `seen_domains`.
- `device`, which holds `is_mobile`, `ja4_class`, `platform_class`, `h2_fp_hash` (the first 12 hexadecimal characters of a hash of the HTTP/2 settings the browser sent) and `known_browser`.
- `network`, which holds `cluster_size`.
- `ids`, which names each partner that holds an identifier of its own for the same browser, with the identifier masked.

The answer is `held`, `identifier` (always `XXXX`), `created` and `consent_updated` written as dates, `withdrawn`, the `record` above, and `version`.

The Edge Cookie is `HttpOnly`, so a script on the page cannot read the identifier, and the page does not undo that.

1. Only a browser opening the address as a page is answered, which the browser marks with `Sec-Fetch-Mode: navigate` and `Sec-Fetch-Dest: document`. Anything else is answered `403` as plain text. A browser sends these headers only to a trustworthy origin, so every browser on a plain `http://` host other than localhost is answered `403` too.
2. The page is sent with `Content-Security-Policy: sandbox; default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`. The `sandbox` directive gives the document an origin of its own, so a page that opened it cannot read it, and the rest lets nothing in the page run or load except its own style. Five more headers go with the policy, being `Cache-Control: no-store, private`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and `Cross-Origin-Resource-Policy: same-origin`. The `403` carries the same headers.
3. Core marks these headers as required. The response finalizer applies an operator's `response_headers` first and sets the required headers again after them, so a site-wide policy an operator writes cannot replace them.
4. The Edge Cookie's value and each partner's identifier show as `XXXX`. Everything else in the record shows, so whoever holds the Edge Cookie's value can read the partner names, the consent strings, the location and the device values, though not the identifiers.
5. The answer is never stored, and no other origin is told it may read the answer, because the page sends no `Access-Control-Allow-Origin`.

The refusal, the sandbox and the readers of the page were checked by hand in Chrome, against a local stand-in server that answered by the same rule with the same headers. The series holds no script or test for that check, and the check did not include an operator's own `response_headers`.

| Attempt                                                                      | Result                                                          |
| ---------------------------------------------------------------------------- | --------------------------------------------------------------- |
| A page's `fetch`                                                             | `403`                                                           |
| A page's `fetch` and `XMLHttpRequest` with the two headers set by the script | `403`, because the browser drops them                           |
| A frame, and an embedded object                                              | `403`                                                           |
| A page that opened the address with `window.open`                            | The page loaded, and the page that opened it could not read it  |
| The same with the sandbox policy left out                                    | The page that opened it read it, so the policy is what stops it |
| A service worker registered for the whole site                               | It read the page, as it reads every navigation on the site      |

So two parties other than the reader can read the page. The first is a service worker the publisher's own site registers for the whole site, and the second is whoever holds an identifier and sends it as the cookie with both headers. Neither finds an identifier in the page, which is why none is shown, though both read the rest of the record.

The record is shown as it is stored, and a test lists every field the record has, so a field added to the record fails that test until it is decided whether the field is an identifier.

Only the Fastly adapter keeps an identity graph, and it keeps one only where `ec.ec_store` names a store. Axum, Cloudflare and Spin always answer that nothing is held, and so does Fastly without `ec.ec_store`.

When nothing is held, the page answers `200` with `held` false and a `why` that gives one of four reasons. They are that the deployment keeps no record, that the request carried no Edge Cookie, that the cookie is not one the deployment issued, and that no record is stored against the cookie (a record made in the last few seconds may not be readable yet).

**When something goes wrong.**

1. A refusal when the settings load, an `[inspect]` pattern among the causes, stops the deployment from building its state. Every address on the deployment then answers as plain text, these five included. Fastly, Axum and Cloudflare answer with the status of the error's kind (`status_code` in `error.rs`), which is `500` for a configuration error such as a refused pattern, with the generic message "An internal error occurred" (`user_message` in the same file), and log the reason, which Cloudflare's own logger does only from row 54. Spin answers with the status of the error's kind too, with that status's reason phrase, such as "Internal Server Error", as the body (`startup_error_router` in the Spin adapter's `app.rs`). It answered a fixed `503` with "Service Unavailable" at `497d553c6`, and row 4 of the stack (IABTechLab/trusted-server#1345) moved it to the rule the other adapters follow. Spin writes the reason to its log as well, which records nothing until row 54 gives Spin a logger. `GET /health` still answers `200` on Fastly and Spin.
2. The configuration view fails only if the settings cannot be written as JSON. The address then answers `500` with the message `The configuration could not be shown.`, as a page or as JSON by its address, and logs the failure.
3. The permissions and data handlers answer an error from reading the request's state as any route answers an error, in plain text. A `Cookie` header that is not valid UTF-8 is `400`, and a selected Edge Cookie module that cannot be built is `500`. A failed location lookup is not an error. The request's permissions then resolve at the strictest level, where a permission is set only by a signal, and the failure is logged.
4. The data handler never answers that nothing is held when it could not read the store. A store that cannot be opened or read, or a stored record that does not parse or validate, is `503` as plain text.

## 3. What is on by default, and what is not in this change

Nothing selects this change. All five addresses are registered on all four adapters with no setting written, and the route table in `docs/guide/api-reference.md` lists each as `always`. Upgrading therefore starts all five answering at once.

| Address                                        | Fastly                                                     | Axum                         | Cloudflare                   | Spin                         | Switch                                          |
| ---------------------------------------------- | ---------------------------------------------------------- | ---------------------------- | ---------------------------- | ---------------------------- | ----------------------------------------------- |
| `/_ts/permissions` and `/_ts/permissions.json` | Answers                                                    | Answers                      | Answers                      | Answers                      | None                                            |
| `/_ts/config` and `/_ts/config.json`           | Answers                                                    | Answers                      | Answers                      | Answers                      | `[inspect] config = false`, which answers `404` |
| `/_ts/data`                                    | Answers, from the identity graph when `ec.ec_store` is set | Answers that nothing is held | Answers that nothing is held | Answers that nothing is held | None                                            |

**Rollout.** For a deployment that is already running:

1. The configuration is published by default, because `config` is `true`. An upgraded deployment shows every setting that is not a secret, not sensitive by default and not named in `hide` to anyone, including a page on any other site. `[inspect] config = false` withholds the configuration. Nothing withholds the permissions or data addresses, although a `[[handlers]]` rule can put either behind a password.
2. A document without `[inspect]` is stored as it was before. An instance built from `split/8-module-context`, the previous row of the stack, refuses a document that carries the section, because it reads the table as the section of a module type, which accepts only `module`, `modules` and tables. A document with `[inspect]` therefore has to wait until every instance has this change.
3. A pattern that changes nothing is refused, and what changes nothing depends on the defaults of the build that loads the document. A release that changes what is masked by default can make an `[inspect]` section that loaded before refuse to load, and a refusal at load stops the deployment.
4. `resolve_secret_references_with`, a public function of core, now returns the `ResolvedSecrets` it recorded in its `Ok` value, where it returned `()`, so a caller outside core has to change.

**Not in this change.**

- A way to change anything. All five addresses are read only.
- Operator data. It stays in the platform's own stores behind the platform's own authentication, and never comes through the publisher's domain.
- An identity graph on Axum, Cloudflare or Spin, which answer that nothing is held.
- A `.json` form of the data page, a rate limit on any of the five addresses, or a cached copy of the configuration view.
- A change to what a configuration push writes. A stored document is the same as before. Row 4 adds `sha256`, and `text` where the policy travels inside the blob, to `[permissions]`, and this row shows `policy` and `sha256`, which are all the settings keep once row 4's loader has read the policy.
- Removing the administration endpoints from the publisher's domain, which a later change does.

**Open questions.** Each of these is a question about the code. This spec records what the code does and leaves the questions open.

1. Should the permissions and data addresses have a switch like `[inspect] config = false`? The change gives them none and records no reason for the difference.
2. Should the permissions answer be resolved after the modules' request preparers have run, as on Axum and Cloudflare, or without them, as on Fastly and Spin?
3. Is every setting that shows by default meant to be public? `trusted_client_ip.auth_header`, the name of the header that carries the shared secret, shows today.
4. Should the data page also send `Cross-Origin-Opener-Policy: same-origin`? It sends no such header today.
5. Should the configuration view be built once, where it is built again on every request today?

## 4. Tests

- The permissions tests check that the answer holds every key a page is given, names each signal and the module that read it, reports the configured and contributing modules apart (with `null` when none is named), shows a withdrawal of storage, still answers with no signals, escapes a hostile signal value, and resolves the permissions of the request that asks. They also check that the answer names the policy (`the_payload_names_the_policy_whose_rules_it_resolved`).
- The configuration tests check the pattern grammar and each refusal with its reason, the deploy check, that every secret core declares is masked and never served (and that the test exercises each declared secret), that every value sensitive by default is masked, that a secret a module declares is masked, that a secret copied into an undeclared field is caught by the value scan, `show` and `hide`, `404` when the configuration is not published, sorted keys, and that an ordinary serialization and a stored document are unchanged. They also check that `permissions.policy` and `permissions.sha256` show unmasked, that `permissions.text` never shows and neither address serves the file (`the_view_shows_the_policy_by_name_and_digest_and_never_its_file`), that the deploy check and the load both refuse a pattern under `permissions.text` (`a_pattern_under_the_policy_file_is_refused`), and that a pattern naming `permissions.sha256` is judged alike before the push and at the load (`a_pattern_naming_the_policy_digest_is_judged_alike_before_and_after_the_push`).
- The data tests check that each of seven kinds of request that is not a browser opening the page is refused, that the record is shown with every identifier masked, that every field of the record is listed, that a known record gives the documented answer, each reason for nothing held, that a store that cannot be read is an error, and the headers on both the page and the refusal.
- Core checks that a required header outlasts an operator's header of the same name, and the Fastly finalizer test runs the data page through the finalizer under an operator's `Content-Security-Policy` and `X-Frame-Options`.
- Each adapter's route tests request the addresses and check the status and the content type. They check that the configuration masks a secret and that the data page answers `403` and `200` as it should, and Fastly's also check that no Edge Cookie finalize state is attached.
- The parity tests send the same requests to Axum, Cloudflare and Spin and compare the configuration, the permissions and the data page.

**Not tested.** Six things have no test.

1. A held record through the Fastly adapter. The Fastly route tests configure no `ec.ec_store`, so they reach only the answer that nothing is held, and core's tests read an in-memory graph.
2. The data page's headers under an operator's `response_headers` on Axum, Cloudflare and Spin. Core tests the function the four finalizers share, and the Fastly finalizer test runs the data page through the Fastly finalizer, but no test runs the page through the Axum, Cloudflare or Spin finalizer.
3. A signal through an adapter. The route tests and the parity test request the permissions addresses with no cookie or header, so an answer that carries a signal is tested only in core, with a state built by hand.
4. A browser. The series holds no script or test for the browser check in Section 2.
5. Fastly in the parity tests, which compare Axum, Cloudflare and Spin and leave Fastly to its own route tests.
6. The list of values sensitive by default. The test that each is masked uses a hand-written list of nine paths, so a field newly marked sensitive, or a sensitive value left unmarked, fails no test. The data page's record has a guard of this kind and the configuration view does not.

## 5. Acceptance

The fork's CI passed on the series head, `497d553c6`, all nineteen checks. That commit was the head of IABTechLab/trusted-server#1350 when it was reviewed on 2026-10-15, and of the two closed pull requests.

On `33282f0c9`, the last of the five original commits, the whole check also passed locally on Windows: `cargo fmt --all -- --check`, six Clippy runs with warnings denied, `cargo test -p trusted-server-core` (2,615 passed), the Fastly tests under Viceroy (3,626), the Axum alias (900), Cloudflare (59), Spin (93), the seam probe (11), the parity tests (20, three of them new here), the configuration generator's tests (8), the integration tests that need no container (29), the permissions inspector's wasm build, and the docs format, lint and build. The two core-only commits, `5fb5f0793` and `16771b18f`, were each linted with the adapter changes set aside, so each commit builds on its own. Two commits came after `33282f0c9` (`1c02104bd` and `a085a3ce7`), and these counts are not re-measured here.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand.                                                                                                                                                                                                                                                                                                                                                                         |
| 2026-10-15 | Fixes from review round 1. The addresses with their methods, forms, statuses and headers, the defaults and the one switch, what runs around each handler, the data page's full header set and its protection from an operator's headers, the failure paths, the rollout, the tests and the untested paths, the open questions, the header and the acceptance lines brought to the series head, and the review log.                      |
| 2026-10-16 | Fixes from review round 2. The statement that the preparers and filters never run for these addresses on Fastly and Spin, where a request with a method other than `GET` goes on Cloudflare, the matching wording of the second open question, and the review log.                                                                                                                                                                      |
| 2026-10-26 | The permissions answer names the policy file row 4 selects, the configuration view shows its name and SHA-256 but never its text, a pattern under `permissions.text` is refused, and checks count `permissions.sha256` as present, as this row's code does. A failed load answers its error's status on every adapter, Spin included, because row 4 moved Spin off a fixed `503`. The status names the pull request but not as a draft. |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/9-visibility-endpoints` at `497d553c6`. Issues found, three of them blocking. The design said a deployment whose settings do not select the change behaves as before when all five addresses answer on all four adapters with no setting, named no address, method, `.json` form, status or header for the permissions and configuration endpoints, and described the data page's protection as `Content-Security-Policy: sandbox` when the code sends a longer policy and five more headers and keeps them under an operator's headers. This revision states what the code does at that head for the addresses, the defaults and the one switch, the failure paths, the rollout, the tests and their gaps, and lists five questions that wait for code changes.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/9-visibility-endpoints` at `497d553c6`. Approved, none blocking. This revision says that the modules' request preparers and the request filters never run for these addresses on Fastly and Spin, in place of saying that the answer comes before them, and says where a request with a method other than `GET` goes on Cloudflare, which is the shared catch-all route and then the publisher.
