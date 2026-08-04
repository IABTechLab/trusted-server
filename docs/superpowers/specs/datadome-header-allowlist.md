# DataDome header allowlist (normative, checked-in — request and response directions)

Adding or changing any name here is a reviewed commit to this file and
a spec change. This file holds the **only** normative pointer lists;
the hook spec §4a references it and carries no duplicate or
per-decision lists of its own.

## Request-direction pointer (vendor response → publisher-upstream overlay)

The complete set of vendor-response header pointers the security
channel (hook spec §4a) may copy into the owner-scoped
publisher-upstream overlay. Every `X-DataDome-*` name not listed here
is rejected.

| Header                | Direction                   | Scope                                                                                                |
| --------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------- |
| `X-DataDome-ClientID` | response → upstream overlay | Owner-scoped overlay only; never the shared request view; vendor egress governed by sign-off item 23 |

## The single pointer matrix (normative — decision × session mode × pointer)

This is the one authoritative browser-response contract. Session mode
is **cookie** in v1 (sessionByHeader is startup-rejected; a header-mode
column is added by the sign-off-23 opt-in, never implicitly). No
wildcard rows exist — every accepted name is enumerated, and **every
cell terminates in exactly one outcome**.

| Pointer             | Respond (cookie mode)                                                                                                                                  | Continue (cookie mode)                     |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------ |
| `Set-Cookie`        | typed `datadome` cookie operation (hook spec §4a parser; exactly one `datadome` field)                                                                 | typed `datadome` cookie operation          |
| `X-Set-Cookie`      | **invalidate the batch → Continue** (mode mismatch: TS never requests header sessions in v1; a vendor response asserting one must not half-apply)      | **invalidate the batch** → effects dropped |
| `Location`          | forward (3xx only), replace                                                                                                                            | rejected                                   |
| `Content-Type`      | forward (owns its body), replace                                                                                                                       | rejected                                   |
| `Cache-Control`     | restricted merge; invariant pass last                                                                                                                  | rejected                                   |
| `Pragma`            | drop-individually, logged (response `Pragma` has no standardized meaning, RFC 9111 §5.4)                                                               | drop-individually, logged                  |
| `X-DataDome`        | forward, owner-scoped typed telemetry                                                                                                                  | forward, owner-scoped typed telemetry      |
| `X-DD-B`            | drop-individually, logged (header-session artifact; the vendor's cookie-mode allow example emits it — the drop is the named divergence in sign-off 28) | drop-individually, logged                  |
| anything not listed | invalidate the batch → Continue                                                                                                                        | invalidate → effects dropped               |

Singleton-field multiplicity (`Location`, `Content-Type`, `X-DataDome`,
`X-DD-B`, `X-Set-Cookie` appearing more than once) invalidates the
batch atomically; list-valued fields (`Cache-Control`, `Pragma`) join
per RFC 9110 §5.3 before their cell applies (hook spec §4a).

**Fixtures**: DataDome's documented challenge response (`Set-Cookie`,
`Pragma`, `X-DataDome`, `Cache-Control`) asserts the decision stays
**Respond** with exactly the mapped fields; the documented allow example
(`Set-Cookie X-DD-B`) asserts Continue proceeds with the cookie applied
and `X-DD-B` dropped-and-logged — neither fixture may fail open.
