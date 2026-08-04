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

## The single pointer matrix (normative — decision × session mode × pointer)

This is the one authoritative contract; the hook spec §4a references it
and carries no duplicate lists. Session mode is **cookie** in v1
(sessionByHeader is startup-rejected; a header-mode column is added by
the sign-off-23 opt-in, never implicitly). No wildcard rows exist —
every accepted name is enumerated.

| Pointer             | Respond (cookie mode)                                                        | Continue (cookie mode)                |
| ------------------- | ---------------------------------------------------------------------------- | ------------------------------------- |
| `Set-Cookie`        | typed `datadome` cookie operation                                            | typed `datadome` cookie operation     |
| `X-Set-Cookie`      | unclassified → batch handling (v1: sessionByHeader rejected)                 | unclassified → batch handling         |
| `Location`          | forward (3xx only), replace                                                  | rejected                              |
| `Content-Type`      | forward (owns its body), replace                                             | rejected                              |
| `Cache-Control`     | restricted merge; invariant pass last                                        | rejected                              |
| `Pragma`            | drop-individually, logged                                                    | drop-individually, logged             |
| `X-DataDome`        | forward, owner-scoped typed telemetry                                        | forward, owner-scoped typed telemetry |
| `X-DD-B`            | drop-individually, logged (header-session artifact; harmless in cookie mode) | drop-individually, logged             |
| anything not listed | invalidate the batch → Continue                                              | invalidate → effects dropped          |

**Fixtures**: DataDome's documented challenge response (`Set-Cookie`,
`Pragma`, `X-DataDome`, `Cache-Control`) asserts the decision stays
**Respond** with exactly the mapped fields; the documented allow example
(`Set-Cookie X-DD-B`) asserts Continue proceeds with the cookie applied
and `X-DD-B` dropped-and-logged — neither fixture may fail open.

Note: the vendor's `X-Set-Cookie` response field is never forwarded as a
header; in v1 it is unclassified because sessionByHeader is rejected at
startup (hook spec §4a).
