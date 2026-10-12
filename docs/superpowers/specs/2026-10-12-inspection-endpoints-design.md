# Design Spec: Inspection endpoints

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#4, ready for review, answering IABTechLab/trusted-server#1268. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#4
**Last updated:** 2026-10-12

> **Why this spec exists.** A deployment decides what a request is permitted, holds the settings that decide how pages are changed, and may keep a record against the identifier it writes to a browser.

## 1. Problem

A deployment decides what a request is permitted, holds the settings that decide how pages are changed, and may keep a record against the identifier it writes to a browser. None of that can be read by the person it concerns without a credential or the operator's word. The administration endpoints that exist answer an operator alone, and they leave the publisher's domain in a later change.

## 2. Design

**The permissions endpoint.** The answer is the evaluation a page receives as `window.tsjs.permissions`, with three things added that a person asking why a permission is not set needs and a page does not, being whether storage was explicitly withdrawn, which signal modules the publisher configured and which of them contributed a signal, and the version that answered.

It resolves the request's state as a page's is resolved and writes nothing for the reader. The answer is never stored. It is readable from any origin so another site can render it, and a request from another origin carries no cookies, so that site reads the resolution of a request with no stored signal.

**The configuration endpoint.** Anyone can read the scripts a page runs, and this lets anyone read the configuration that runs at the edge too. It shows the settings the deployment holds after defaults are applied, with values masked as `XXXX` and every masked path listed.

Three kinds of value are masked.

1. Every secret, whatever the publisher's document says. The settings loader records each value it fills from the secret store, and those paths are masked, along with every field declared a secret. Then any string that still contains one of those values is masked wherever it sits, a key included.
2. Every value sensitive by default, unless the publisher shows it, being the origin URLs, the origin host override, the Basic-auth path and username, and the store names and ids.
3. Everything the publisher hides.

The publisher decides with a new `[inspect]` section, which holds `show`, `hide` and `config = false`, the last of which answers `404`. A pattern that would not be honored as written refuses the configuration, both when a deployment is validated and when the settings load, so a publisher cannot believe a value is hidden when it is shown.

A stored document is unchanged. The marking that lets the view find sensitive values is a mode of one serialization call, every other serialization writes what it wrote before, and the `[inspect]` section is left out of a document unless the publisher wrote one. Tests check both.

**The data endpoint.** A deployment that writes an identifier to a browser can show the person using that browser what it keeps against it. The page shows the identity graph record stored against the identifier in the request's own `ts-ec` cookie, as it is stored, being when it was made, the consent recorded, the location and device class kept, and which partners hold an identifier of their own for the same browser.

The Edge Cookie is `HttpOnly`, so a script on the page cannot read the identifier, and the page does not undo that.

1. Only a browser opening the address as a page is answered, which the browser marks with `Sec-Fetch-Mode: navigate` and `Sec-Fetch-Dest: document`. Anything else is answered `403`.
2. The page is sent with `Content-Security-Policy: sandbox`, so the document has an origin of its own and a page that opened it cannot read it.
3. No identifier is shown. The Edge Cookie's value and each partner's identifier show as `XXXX`.
4. It is never stored, and no other origin is told it may read it.

These were checked in Chrome against a local server answering by the same rule with the same headers.

| Attempt                                                                      | Result                                                          |
| ---------------------------------------------------------------------------- | --------------------------------------------------------------- |
| A page's `fetch`                                                             | `403`                                                           |
| A page's `fetch` and `XMLHttpRequest` with the two headers set by the script | `403`, because the browser drops them                           |
| A frame, and an embedded object                                              | `403`                                                           |
| A page that opened the address with `window.open`                            | The page loaded, and the page that opened it could not read it  |
| The same with the sandbox policy left out                                    | The page that opened it read it, so the policy is what stops it |
| A service worker registered for the whole site                               | It read the page, as it reads every navigation on the site      |

So two parties other than the reader can read the page. The first is a service worker the publisher's own site registers for the whole site, and the second is whoever holds an identifier and sends it as the cookie with both headers. Neither finds an identifier in the page, which is why none is shown.

The record is shown as it is stored, and a test lists every field the record has, so a field added to the record fails that test until it is decided whether the field is an identifier.

Only the Fastly adapter keeps an identity graph, so Axum, Cloudflare and Spin answer that nothing is held.

## 3. What is not in this change

Nothing beyond the design above. A deployment whose settings do not select the change behaves as before.

## 4. Acceptance

On 33282f0c9, locally on Windows, the whole check passed: `cargo fmt --all -- --check`, six Clippy runs with warnings denied, `cargo test -p trusted-server-core` (2,615 passed), the Fastly tests under Viceroy (3,626), the Axum alias (900), Cloudflare (59), Spin (93), the seam probe (11), the parity tests (20, three of them new here), the configuration generator's tests (8), the integration tests that need no container (29), the permissions inspector's wasm build, and the docs format, lint and build. Each of the two core commits was also linted with the adapter changes set aside, so each commit builds on its own.

CI passed on 33282f0c9, all nineteen checks.

The pull request's description records how it was checked, with CI green on all 19 checks.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
