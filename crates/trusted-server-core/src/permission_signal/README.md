# Permission signals

A permission is the primitive. This is the seam that decides whether one is
set, and it is open so that a new way of learning a permission can be added
without changing core.

"Signal" rather than "consent" because consent is only one of the things
arriving here. Global Privacy Control is a setting a browser sends, not an
answer anyone gave to a question, and a jurisdiction rule is neither. Consent
is one kind of signal, so the seam takes the wider name and the consent
subsystem keeps the narrower one.

## Why the providers are not in core

Privacy is a non-price factor of competition. Publishers, browsers and
standards bodies compete on it, and schemes come and go. Compiling a closed
list of schemes into core would settle that competition in code, because
the schemes built in would be the only ones a deployment could act on, and
the core maintainers would be deciding which privacy schemes exist.

So core holds the trait, the ordering, the country baseline, and the policy
vocabulary for what a deployment decides about the shipped schemes, being
whether a TCF record answers, which signals count as a US-style opt-out and
what an opt-out takes away. It holds no scheme's wire format and no scheme's
meaning. Every scheme lives in its own crate outside core, including the four
that ship by default, so none of them is privileged by being the one that
happens to be built in. Core does not know what a TCF purpose is, because the
mapping from purpose to Data Use lives in the TCF crate, and a deployment
that runs no TCF carries no table of another scheme's numbers. The purposes
are the IAB TCF Europe purposes and what they grant are IAB Tech Lab Privacy
Taxonomy Data Uses, so a reader checks the mapping against the industry's own
documents rather than against us, and docs/guide/permission-model.md lists it
in full. Two purposes have no Data Use yet, so the crate carries a proposed
key for one and the TCF identifier for the other until the taxonomy adds
them.

A scheme that is not one of the four is added the same way, as a crate that
reads its own signal from the request, without a change to core. The next one
is Model Terms for Marketing (MTM), where a publisher and the parties it passes
data to agree to be bound by a published set of terms, and what a provider
reads is whether that agreement covers this request. MTM arrives in a following
pull request, and it is one of many terms schemes rather than the only one,
because a publisher, a trade body or a regulator can each publish terms and
each set becomes a provider. The four here are a starting set and not the list.

## The hierarchy

Permissions are resolved in layers, each amending the one before.

```text
  country / region rules       the baseline: granted, requires-signal, denied
        |
        v
  provider 1  (configured order)    may amend
        |
        v
  provider 2                         may amend
        |
        v
  ...                                may amend
        |
        v
  the permission state for this request
```

The baseline comes from `permissions.yaml`, keyed by country and region, with
the top node of the rules tree standing in for a request whose place is
unknown. A geo provider supplies the place. No geo provider means no country,
so every request resolves at that top node, and only a lookup that failed
resolves at the requires-signal floor, because a place that could not be
determined must not be treated as the declared default.

Providers are then asked in order. Each sees what the providers before it
settled on and may amend it. A provider with no opinion returns `Neutral` and
leaves the prior value standing, which is different from refusing.

## The order is the policy

The last provider with an opinion decides, so the order is the policy. It is
a deployment's to set, not this code's to assume.

```toml
[permission_signal]
sources = ["gpc", "gpp-sale-opt-out", "us-privacy", "tcf"]
```

A provider not on the list does not run, and there is no separate switch. A
publisher who does not want to act on Global Privacy Control removes `"gpc"`
from the list, and the provider that reads the header then does not run. One
caveat: core's consent pipeline can also synthesize a US Privacy opt-out from
that header for a visitor in a US state, when the consent settings say to,
which they do by default, and the `us-privacy` provider then acts on the
record it produced. A publisher who wants the header to have no effect at all
turns that setting off as well. Leaving the section out entirely runs every
provider the adapter offers, in the order it offers them, so a signal is never
quietly ignored because someone forgot to list it. An unknown or repeated name
is refused at startup, so a typo cannot silently stop a scheme being honored.

The default order asks the signal with no interface of its own first and the
ones carrying a choice made through an interface after. Global Privacy
Control is a browser setting, so it revokes personalization on arrival,
whereas a GPP sale opt-out, a US Privacy string and a TCF record each carry
an answer a person gave, so they are asked later and amend it. A deployment
wanting the browser setting to stand over a later answer puts `gpc` last.

Trusted Server takes no view on which scheme should win. That is a question
about a jurisdiction and a publisher.

## A provider can see the others

Amending well sometimes needs to know who set the prior value. A provider is
given the whole ordered list and its own position in it, so it can look up a
peer by name, see whether a peer it cares about is configured at all, and ask
a peer directly what that peer makes of a permission.

That is what makes a rule like "personalization is off, but only because
Global Privacy Control set it, so my answer supersedes it" expressible. The
rule itself belongs to whichever provider wants it. This seam only makes the
information available.

Consulting a peer goes one level deep. A provider answering a consultation
cannot consult in turn, so two providers asking each other cannot loop.

## The terms the data is available under

A provider may also declare the terms documents the request's data is available
under, through `tdls` on the trait, and core carries what every configured
provider declared on the permission state. Whoever receives the data reads them
to decide whether those are terms they accept, and whether they may pass the
data on. No declaration means no terms were declared, which is not the same as
terms permitting anything, so a recipient needing a basis and finding none has
none.

Each entry is the address of a published document a person can read, and the
document must never be edited once published, which is why a version belongs in
its address. A document that can be rewritten tomorrow means a recipient can
never prove what it agreed to, and one edit silently rewrites the basis of every
transaction already sent under it. That is a property of how the document is
published, so the [`Tdl`](crate::tdl::Tdl) type refuses only an address nothing
could fetch.

None of the four schemes here carries terms, so the list is empty until a terms
scheme runs, Model Terms for Marketing (MTM) being the first of many rather than
the only one. The name matches the `tdl` member the Data Labels work puts on a
node of an `OpenRTB` request, which is where these travel once a bid request
carries them.

## Withdrawal is a separate question

A provider may also say that the request explicitly *withdraws* a permission,
which is different from not granting it. A withdrawal of storage expires the
browser cookie and writes the authoritative tombstone against the identifier.
A permission that is merely not set strips the response headers and leaves an
already-issued identifier alone, so a returning visitor is not permanently
withdrawn before they get to answer. Most schemes have no such notion, a
browser setting and a sale opt-out included, and only TCF answers it. Core
then scopes the answer to the jurisdiction, so a refusal only withdraws where
the storage baseline did not grant storage outright, because where it did
the identifier never depended on the record.

## Writing a provider

Implement `PermissionSignalProvider` in a crate that depends on core. Answer
`Neutral` for a permission the provider has no opinion on, including when the
signal it reads is absent from the request. Returning `Revoke` for an absent
signal turns silence into refusal and would revoke the permission on every
request not carrying that scheme, which is most of them.

Read the request through `SignalInput::evidence`, which offers headers,
cookies, the path and the query, so a scheme core has never heard of can read
its own signal. The decoded consent record is offered too, for the schemes
core's consent pipeline already decodes, caches against the identifier and
expires. Prefer the record where it exists, because a provider that
re-decodes the wire would skip the cached record on a returning visitor and
answer differently from every other reader of the same request.

Read the policy for what is a deployment's decision rather than the scheme's,
such as which signals count as an opt-out and what an opt-out takes away, so
that a deployment can change those without changing a provider.

Register the crate at the adapter's composition root, where every adapter
builds its `RuntimeServices`. The adapter lists the providers it links, in the
default order, and `build_permission_signal_providers` selects and orders them
from configuration.

## The providers supplied

Four crates ship, under `crates/permission-signal/`, and a deployment
configuring nothing gets all four in this order:

| Identifier         | Crate        | Reads                                             |
| ------------------ | ------------ | ------------------------------------------------- |
| `gpc`              | `gpc`        | The `Sec-GPC` header, Global Privacy Control      |
| `gpp-sale-opt-out` | `gpp`        | The US sale opt-out in a GPP string               |
| `us-privacy`       | `us-privacy` | The sale opt-out in a US Privacy string           |
| `tcf`              | `tcf`        | A TCF v2 record, with its purpose mapping in code |

The three opt-outs are separate rather than one so that a publisher who does
not act on Global Privacy Control can remove it and keep the other two.

## What is not a provider

A consent record that arrives and cannot be read revokes, ahead of the
providers and whichever of them are configured. That is error handling, not a
signaling scheme, so it is not in the list and cannot be removed. A
publisher chooses which signals to act on; they do not choose what happens
when one of those signals arrives unreadable. An unreadable record is a
preference someone expressed that could not be read, which is not the same as
no record at all, so it must not degrade to the no-signal baseline.

It overrides rather than taking a place in the order, because the ordered rule
would otherwise let a readable record from one scheme overwrite the refusal
caused by an unreadable one from another.
