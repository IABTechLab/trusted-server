# Design Spec: Integration Response-Header Hook

**Status:** Draft
**Author:** Engineering
**Issue references:** #782
**Related specs:** `2026-07-30-pluggable-providers-design.md`
**Last updated:** 2026-07-31

> **Context.** Issue #782 already specifies this feature well; its done-when
> is the contract. PR #838 shipped the trait and registry wiring with **no
> adapter call site** — `apply_response_headers` had zero production
> callers, so the feature existed only in its own unit test. This short spec
> restates the contract plus the two details the issue left open (ordering
> and collision policy), and adds the rule that prevents a repeat: the hook
> lands with a consumer or not at all.

---

## 1. Overview

Integrations can today rewrite request-path behavior (proxies, attribute
rewriters, head injectors) but cannot mutate **response** headers. The hook
adds that: an integration registers a response-header mutator via its
`IntegrationRegistration` builder, and every adapter applies all registered
mutators to the outbound response for HTML document responses it processed.

## 2. Contract

- `IntegrationRegistration::builder(ID).with_response_mutator(...)` registers
  a mutator; `IntegrationRegistry::apply_response_headers(...)` applies all
  registered mutators in registration order.
- **Every adapter calls the apply point** on its outbound-response path for
  processed documents. The call site lives in shared response-finalization
  code where one exists; where adapters finalize independently, each adapter
  gains the call and a test proving it.
- Mutators run **after** Trusted Server's own response-header handling
  (EC Set-Cookie emission, EC header clearing, privacy headers) so a
  mutation cannot be silently stripped by a later core pass. The ordering is
  a fresh decision this spec makes — PR #838 never wired the hook, so there
  is no existing insertion point to inherit; the implementer places the call
  at the end of each adapter's response finalization, and the §4.3 tests pin
  it there.

## 3. Collision policy

- Mutators may not touch **reserved surface**, which is defined at two
  granularities because `Set-Cookie` is multi-valued: (a) reserved header
  _names_ — the `x-ts-*` namespace and the consent/privacy headers core
  emits; (b) reserved cookie _names_ within `Set-Cookie` — `ts-ec`,
  `ts-eids`, and the other `ts-*` cookies core owns. An integration may
  append its own `Set-Cookie` values; it may not set or expire a reserved
  cookie name. Violations are dropped and logged at `warn` with the
  integration id. The reserved-cookie list is a single constant next to the
  cookie definitions, not duplicated in the hook.
- For non-reserved headers, the mutator API distinguishes **append** from
  **replace** explicitly; the default is append (for `Set-Cookie`, append is
  the only non-reserved operation — replace is not offered). Replacing a
  header the origin set is a deliberate act, visible in the mutator's code.
- Later registrations see earlier mutations (order = registration order,
  which is deterministic).

## 4. Done-when (from #782, sharpened)

1. Trait + builder + registry application, each public item documented.
2. **At least one real consumer ships in the same PR** — an existing
   integration registering a mutator for a real need (or, failing a real
   need, the feature waits; scaffolding with only self-referential tests is
   dead code and will be removed).
3. Every adapter applies mutations on its outbound path, with a per-adapter
   route test asserting an integration-set header appears in the response.
4. A parity-suite case asserts identical mutation behavior across adapters.
5. Reserved-header and append/replace semantics covered by unit tests.

## 5. Size and sequencing

This is a ~150-line feature plus tests, with zero coupling to the provider
architecture or the permission model. It lands as its own small PR **when
its first real consumer is identified** (§4.2) — at any point in the epic's
sequence, blocking nothing and blocked by nothing. If no consumer
materializes, it does not land; being unblocked is not a reason to ship
scaffolding.
