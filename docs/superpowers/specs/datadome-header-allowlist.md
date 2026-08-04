# DataDome header allowlist (normative, checked-in — request and response directions)

The complete set of response-named header pointers the security channel
(hook spec §4a) may copy into the owner-scoped publisher-upstream
overlay. Every `X-DataDome-*` name not listed here is rejected. Adding a
name is a reviewed commit to this file and a spec change.

| Header                | Direction                   | Scope                                                                                                |
| --------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------- |
| `X-DataDome-ClientID` | response → upstream overlay | Owner-scoped overlay only; never the shared request view; vendor egress governed by sign-off item 23 |

## Response-direction allowlist (browser-response pointers)

Headers a DataDome decision may set on the outgoing response, beyond
the typed security cookie (hook spec §4a). Empty rows below the base
set mean: nothing else is accepted until a reviewed commit adds it.

| Header                    | Decision                     | Semantics                                   |
| ------------------------- | ---------------------------- | ------------------------------------------- |
| `Location`                | Respond (3xx) only           | replace                                     |
| `Content-Type`            | Respond only (owns its body) | replace                                     |
| `Cache-Control`, `Pragma` | Respond only                 | restricted merge; invariant pass still last |

Note: the vendor's `X-Set-Cookie` response field is **not** a
forwardable header — it lowers into the typed `datadome` cookie
operation (hook spec §4a) and never reaches the browser as a header.
