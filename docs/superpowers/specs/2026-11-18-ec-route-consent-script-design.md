# EC route consent script

Issue #1599 implements the browser half of ID-9, plus WB-6's merge of route
results and SID-2's localStorage opt-out copy, from the server-side identity
design. Stack on #1646 (`feat/1574-axum-shared-identity`), which supplies the
shared EC route, and merge #1620 (identity-only tsjs mode, #1594) as the other
browser dependency.

The route and server contract are implemented by #1646. This change also
supplies the part of #1598 (T19) needed for activation: server-side Prebid
head injection tied to the mounted identity app's record-store capability.
Safari lifetime and deployed qualification remain separate acceptance work.

## Activation

The server injects `window.__tsjs_prebid.ecRoute` as `/_ts/api/v1/ec` only
when Prebid uses identity mode and the mounted shared identity app has a bound
record store and Sourcepoint is not enabled. Auction mode, absent stores and
adapters returning 501 omit it. Enabled Sourcepoint owns consent synchronization
through `/_ts/consent`, regardless of SDK rewriting or current CMP availability,
so Prebid's EC reporter stays off on those pages. This prevents independent
queues from racing; identity capture and navigation-mirror merging remain active.
Those Sourcepoint pages do not gain EC-route minting on the current page.
The capability is set after store binding, rather than inferred from settings.
The assignment follows config creation and precedes the external Prebid bundle
and tsjs execution, including the prepared head-injection path.

The browser accepts only a path beginning with exactly one `/` that resolves
to `location.origin`. It reports consent and merges the answer in identity mode.
Its existing transport can accept a manually supplied route in auction mode,
but normal server injection does not activate that path.

## Consent capture

Observe the publisher's existing CMP APIs; never install a CMP, stub or
synthesize consent. With no CMP API there is no reading and no call.

- **TCF:** subscribe with `__tcfapi('addEventListener', 2, cb)`. A callback with
  `success === true` that has `gdprApplies === false`, or `eventStatus`
  `tcloaded` or `useractioncomplete`, is a reading of `gdprApplies`,
  `tcString` and the true-valued keys of `purpose.consents`,
  `purpose.legitimateInterests`, `vendor.consents` and
  `vendor.legitimateInterests`. Other events, and refusals, are not readings;
  the previous reading stands. Remove a listener from the API that accepted it.
  A replaced TCF API is resubscribed. Listener IDs belong to their accepting
  API, so stale callbacks never remove IDs through an unrelated replacement.
  A transferred listener held by an unreachable stub stays inert under the
  generation guard instead of risking the active CMP subscription.
- **GPP:** reuse #1535's reviewed `GppConsentCapture` from
  `src/shared/gpp_capture.ts`, shared by Prebid and Sourcepoint with common
  API tests in `test/shared/gpp_capture.test.ts`. It installs an accessor on `__gpp` only when the
  global is a plain writable property, as #1535 does. A ready snapshot with
  `applicableSections` all in 1..65535 is a reading of `gppString` and
  `applicableSections`. Its unavailable callback (pending registration, a
  not-ready signal, `[-1]` "not applicable", a CMP error) is not a reading:
  the previous reading stands.
- **Discovery:** read both globals at install, on `DOMContentLoaded`, on
  resource `load` events (capture phase), on `visibilitychange` and on
  persisted `pageshow`, and in identity mode on the existing capture tick.
  Re-subscribe or refresh only when a global's function differs from the one
  in use, so the tick never pings an unchanged CMP. No accessor is installed
  over `__tcfapi`.

Each new reading produces a snapshot combining the latest TCF and GPP readings.

## Request contract (for T19)

`POST <ecRoute>` with `credentials: 'same-origin'`, `cache: 'no-store'`,
`redirect: 'error'`, `keepalive: true` and `Content-Type: application/json`.
The browser adds `Origin`. The body is

```json
{ "tcString"?: string, "gppString"?: string, "applicableSections"?: number[] }
```

A present field is the CMP's current value and replaces that consent cookie for
the request (`euconsent-v2` for `tcString`, `__gpp` and `__gpp_sid` for the GPP
fields). An absent field means this page has no reading for it, and the server
uses the request's cookie as a navigation does. A TCF reading with
`gdprApplies === true` sends `tcString`, which must match `[A-Za-z0-9._-]+`. A
reading with `gdprApplies === false` sends nothing for TCF: the server's EID
gate keys GDPR off the TC string's presence, so telling it to drop the cookie
would release IDs a navigation withholds. `applicableSections` appears only
with `gppString`, contains at most 32 IDs, and GPP values use #1535's
cookie-safe validation. Readings with more than 32 IDs are omitted rather than
truncated. Exact request bodies are checked against the server's shared
`crates/trusted-server-js/lib/test/fixtures/ec-route-contract.json` fixture
both at queue serialization and at the fetch boundary.

A snapshot with nothing to send makes no call. A TCF reading with
`gdprApplies === true` and an invalid `tcString` is not sent; a GPP reading in
the same snapshot still is, unless that TCF reading is a known withdrawal
(below). Such a snapshot makes no call at all, because without its TC string
the server would answer from the stale `euconsent-v2` cookie. The page fences
suppression instead (see Merging) and keeps its merged IDs cleared until a later
valid reading is answered.

Responses: `200` with a JSON body `{"v":1,"eids":[...]}` in the mirror
envelope's decoded shape, including any `exp` (WB-4's view for that request,
SharedID only under SID-2 and SID-6); `204` means no IDs. Any other status, a
redirect, a network error, a body over 16 KiB or an invalid envelope is a
failure.

Every page view whose snapshot has a field to send makes one call, because the
page cannot see the HttpOnly `ts-ec` or know what the navigation decided. An
unchanged returning reader gets an answer with no `Set-Cookie` (server side,
T19). A TCF-only page outside GDPR scope has nothing to send and relies on the
navigation, which already applied that signal from its cookies.

## Ordering

At most one call is in flight. A snapshot whose request body equals the
newest waiting snapshot's, or with none waiting the one in flight, or with none
in flight the last one successfully sent, is ignored. A fence (see Merging)
forgets the last successfully sent snapshot, so after it an unchanged reading
is sent again.

Each snapshot is classified against the previous snapshot. A reading
_changes_ when the previous snapshot had a reading from the same API with a
different value; an API's first reading is _new_.

- **Known withdrawal:** TCF reads `gdprApplies === true` with Purpose 1 or
  Purpose 4 not consented. On the server only a Purpose 1 denial in the GDPR
  jurisdiction removes the EC; a Purpose 4 denial strips EIDs and keeps it, and
  the page clears its merged IDs in both cases.
- **Barrier:** a known withdrawal; a changed TCF reading that turns a
  previously true purpose or vendor consent or legitimate interest false, or
  turns `gdprApplies` from false to true; a changed GPP reading, which the
  browser cannot classify without decoding GPP sections; and a new reading
  that the navigation could not have applied: a TCF reading with
  `gdprApplies === true` whose `tcString` differs from the `euconsent-v2`
  cookie, or a GPP reading whose `gppString` differs from the `__gpp` cookie. A
  cookie name that appears more than once in `document.cookie` counts as a
  mismatch, because the browser and the server may pick different copies.
- **Grant:** any other snapshot.

A returning reader whose CMP keeps its signal in those cookies therefore sends
a grant on load and keeps the mirror the navigation issued. Any CMP that
answers an API without writing that API's cookie (Sourcepoint without its
cookie integration, or a TCF CMP that also exposes `__gpp` but writes no
`__gpp` cookie) makes the first reading a barrier, so merging waits for the
route's answer on each page view. This is an accepted cost: the navigation
could not have applied that signal, so its mirror may predate an opt-out.

A new snapshot replaces the last waiting one only when that waiting snapshot is
a grant. Waiting barriers are never replaced; later snapshots queue behind
them. Every withdrawal therefore reaches the server after any grant in flight
and before any later grant: with grant A in flight, withdrawal B and grant C
are sent as A, B, C.

A barrier that fails stays at the head of the queue and is resent on the next
trigger (a new reading, `visibilitychange`, `focus` or persisted `pageshow`),
with no background retry timer, as #1535's consent sync does. A grant that
fails is dropped: resending an older grant on focus could override a
withdrawal made in another tab. Only a new reading produces another grant. A
reading equal to the dropped grant is ignored only when it also equals the
last snapshot successfully sent, which the server already holds.

On `pagehide` a waiting snapshot is not sent ahead of the call in flight, so a
withdrawal made while a call is in flight can be lost when the tab closes. This
deviates from "every withdrawal reaches the server" for that page: the server
keeps the earlier grant's record and cookies until the reader's next page,
whose navigation carries the CMP's consent cookies where the CMP writes them
and whose first reading is sent as a barrier where it does not. Sending it
concurrently would break the in-order guarantee instead.

## Merging into Prebid (identity mode)

A result is applied only when nothing is waiting as it arrives; otherwise it is
discarded. Applying stores the result's EIDs together with the mirror cookie's
raw value at that moment.

Before each publisher auction the wrapper chooses one source:

- While merging is suppressed, none.
- Otherwise, the applied route result while the mirror cookie's raw value is
  unchanged since it was applied (this covers a browser that blocks or drops
  the cookie).
- Otherwise, the mirror cookie as in #1620. A changed cookie (another tab's
  withdrawal, a later response, expiry) supersedes the in-memory result.

Every UID's `exp` is checked at each merge, and deduplication and
request-local merging are unchanged from #1620. Route results live only in
memory; they never reach Prebid's configuration or the native `ts-eids`
capture.

Queuing a barrier, or discarding a result, clears the applied route result.
Queuing a barrier also suppresses merging until a result is applied, which
happens only when nothing is waiting, so suppression lasts through every call
after the barrier.

A known withdrawal whose TCF reading cannot be sent queues nothing and sets a
fence: every call is stamped with the fence count when queued, the fence count
rises with each such withdrawal, and an answer lifts suppression only when its
call carries the current count. The answer to any call queued before the
withdrawal therefore cannot lift it, and only a later valid reading's answer
can. A later reading equal to a call still waiting or in flight is not queued
again; that call is re-stamped instead, since its answer reflects the current
reading.

A known withdrawal also expires `__Host-ts-eids-mirror` from script at once
(`Path=/; Secure; Max-Age=0`), because a grant in flight may still deliver that
cookie before the withdrawal is answered. Native Prebid IDs
remain under Prebid's own consent handling and are not touched.

Suppression and clearing apply to the tab that observed the barrier. Another
tab of the same site sees the change only through the mirror cookie: when the
withdrawal's answer expires it, that tab's applied result is superseded. A
grant answer that lands in the meantime can briefly re-set the mirror for
other tabs.

The merge is applied when the publisher calls `requestBids`. Prebid may start
the auction later, after its consent and User ID hooks and `auctionDelay`; a
withdrawal in that window does not remove IDs already merged into that
auction's request-local `ortb2`. Prebid's own activity controls still apply to
it on the normal request path.

## SharedID opt-out (SID-2)

Use Prebid 10.26's own predicate: localStorage `_pubcid_optout` is active when
its value is truthy and `_pubcid_optout_exp` is falsy or
`new Date(exp).getTime() - Date.now() > 0`. In both modes, when it is active
and no truthy `_pubcid_optout` cookie exists, write
`_pubcid_optout=1; Path=/; Secure; SameSite=Lax`, host-only, with `Max-Age`
of one year or, with a valid `_exp`, the whole seconds left until it,
whichever is shorter. Also write the marker
`_ts_pubcid_optout_src=ls` with the same attributes, following the
`_ts_gpp_src` pattern of the Sourcepoint integration. When the localStorage
opt-out is not active and the marker is present, expire both. A cookie the
publisher wrote without the marker is left alone; a publisher write over a
marked copy cannot be told apart and is expired with it. Any storage or cookie
error leaves both untouched.

Check at install, on `visibilitychange`, on each identity-mode capture tick,
before each route call and, in auction mode, before each `requestBids`.

While a `_pubcid_optout` cookie has a truthy value or the localStorage opt-out
is active,
merging drops `pubcid.org` IDs as a defense in depth; the server already omits
them.

## Alternatives

- Merging route results in auction mode: not needed; its server auction reads
  identity from the record.
- A route-local accessor on `__tcfapi`: rejected; it would contend with the
  managed User ID watch when that is armed, and discovery works without it.
- Decoding GPP to classify US withdrawals: rejected; treating every GPP change
  as a barrier keeps withdrawals ordered at the cost of extra calls and brief
  merge suppression.
- Waiting for every present CMP to report before the first call: rejected; a
  stub that never readies, or a refused subscription, would block calls
  forever. Absent fields fall back to cookies instead.
- Implementing T19 here: out of scope; it depends on #1591, #1592, #1593 and
  #1600, none of which has landed.

## Verification

- Unit tests: activation and path validation; TCF readings, refusal, late and
  replaced CMP; GPP capture tests carried from #1535 plus not-applicable and
  not-ready handling; request body and options; deduplication against the
  queue tail including A, B, A; an out-of-scope TCF reading sending no TCF
  field and, alone, no call; a duplicated consent cookie making a new reading a
  barrier; grant coalescing; barrier retention and A, B, C
  ordering; partial TCF revocation as a barrier; failure retained at the head
  and resent on triggers; discard of overtaken results; clearing and
  suppression; route result superseded by a changed cookie; expiry at merge;
  response validation; opt-out predicate, copy, `_exp`, marker removal and
  storage errors; auction mode calling the route without merging.
- Real browser through Fastly and Axum serving the built tsjs: with no
  `ecRoute` injected, no route call is made (preservation). With `ecRoute`
  supplied by a fixture and the route answered by a controlled responder, a
  controlled bidder receives the route's ID, and grant/withdrawal/regrant
  ordering, suppression and the opt-out copy hold. The responder stands in for
  T19; this does not qualify T19's server behavior or Safari's cookie lifetime.
- Repository CI gates.

## Review log

- Round 1 (two independent reviewers, requirements and adversarial): removed a
  first-call gate that a never-ready GPP stub or refused TCF subscription could
  block forever; added `gdprApplies`; ignored transient GPP states;
  deduplicated against the queue tail so A, B, A regrants are sent; kept failed
  calls at the head instead of dropping withdrawals; treated any lost TCF
  purpose, vendor or legitimate interest as a barrier; cleared route results on
  barriers and discards and let a changed mirror cookie supersede them;
  checked `exp` at merge; adopted Prebid's opt-out predicate verbatim and made
  storage errors no-ops; extended route calls to auction mode; corrected the
  `__tcfapi` accessor claim and acknowledged the `__gpp` accessor.
- Round 2 (independent reviewer): dropped failed grants instead of resending
  them on focus, so a stale grant cannot override another tab's withdrawal;
  defined change versus new readings and made a new reading a barrier unless
  the navigation's consent cookie already carried it, so CMPs that keep GPP
  outside cookies are not merged before the answer; gave `gdprApplies: false`
  a server meaning matching how the consent module derives GDPR from the TC
  string; recorded the `pagehide`, cross-tab and late-auction limits; aligned
  the opt-out cookie test with Prebid's truthy check. Superseded TCF
  subscriptions are already ignored by generation.
- Round 3 (independent reviewer): removed `gdprApplies` from the contract,
  since the server's EID gate keys GDPR off the TC string and a "no TC string"
  instruction would release IDs a navigation withholds; out-of-scope TCF
  readings now send nothing for TCF. Stated deduplication as the queue tail
  only, so a repeated withdrawal after an intervening grant is sent. Treated
  duplicate consent cookie names as mismatches. Recorded the cost for CMPs
  that answer `__gpp` without writing its cookie.
- Round 4 (verification of round 3): corrected the per-page-view call
  sentence for out-of-scope TCF pages, noted the server's Purpose 1 versus
  Purpose 4 effect, added verification for the round-3 rules, and placed
  cookie parsing in the route client rather than the pure queue.
- Implementation review (independent reviewer, then re-review of fixes): a
  failed grant no longer holds back what queued behind it, while a failed
  barrier still waits for a trigger; an overtaken grant answer is proven to
  clear an older result; the 16 KiB response cap is enforced in bytes and
  against a declared length; a superseded TCF stub subscription is removed
  through the CMP that now holds it. Each fix has a regression that fails when
  the fix is reverted.
- PR self-review (independent reviewer): a known withdrawal whose TC string
  cannot be sent queues nothing, so the grant already in flight lifted its
  suppression when answered. Such a withdrawal now fences suppression: only an
  answer to a call queued after it lifts it. Removed two test-only members and
  made the no-route preservation test cover both modes.
- Fix re-review: a withdrawal with an unsendable TC string now fences even when
  its GPP part is still sent, because the server answers that call from the
  stale `euconsent-v2` cookie; and after a fence, a reading equal to the last
  call is sent again so its answer can lift suppression.
- Final review: a post-fence grant that failed left the older answered state
  deduplicating the next identical reading, so suppression stayed on. Replaced
  the repeat flag with forgetting the last answered state at a fence, and
  stopped an identical fenced withdrawal from re-fencing and resending. Moved
  the fence rules into the Request contract, Ordering and Merging sections.
- Fence re-review: skipping the fence for a withdrawal whose GPP-only body
  matched an earlier call let that earlier call's answer lift suppression.
  Such a withdrawal is now not sent at all, so it has no cookie-based answer
  and needs no repeat rule; each report re-fences without a call.
- Last-delta review: a later reading equal to a call still pending was
  deduplicated without a new stamp, so that call's answer could not lift the
  fence. Such a call is now re-stamped.
