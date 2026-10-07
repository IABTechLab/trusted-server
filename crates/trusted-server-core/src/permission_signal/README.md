# Permission signals

A permission is the primitive. This is the seam that decides whether one is
set, and it is open so that a new way of learning a permission can be added
without changing core.

"Signal" rather than "consent" because consent is only one of the things
arriving here. Global Privacy Control is a setting a browser sends, not an
answer anyone gave to a question, and a jurisdiction rule is neither. Consent
is one kind of signal, so the seam takes the wider name and the consent
subsystem keeps the narrower one.

## What core holds

The reasons the schemes live outside core are on the docs page,
docs/guide/permission-signals.md.

Core holds the trait, the ordering, the country baseline, and the policy
vocabulary for what a deployment decides about the shipped schemes, being
whether a TCF record answers, which signals count as a US-style opt-out and
what an opt-out takes away. It holds no scheme's wire format and no scheme's
meaning. Every scheme lives in its own crate outside core, including the five
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

A scheme that is not one of the five is added the same way, as a crate that
reads its own signal from the request, without a change to core. The fifth,
Model Terms for Marketing (MTM), is the first terms scheme, where a publisher
and the parties it passes data to agree to be bound by a published set of
terms, and what the module reads is the visitor's answer to a preference
platform, one of three words in the first party cookie
`__mtm_pref`, a name any platform may set. It is one of many terms schemes
rather than the only one, because a publisher, a trade body or a regulator can
each publish terms and each set becomes a module. The five here are a
starting set and not the list.

## The hierarchy

Permissions are resolved in layers, each amending the one before.

```text
  country / region rules       the baseline: granted, requires-signal, denied
        |
        v
  module 1  (configured order)    may amend
        |
        v
  module 2                         may amend
        |
        v
  ...                                may amend
        |
        v
  the permission state for this request
```

The baseline comes from `permissions.yaml`, keyed by country and region, with
the top node of the rules tree standing in for a request whose place is
unknown. A geo module supplies the place. No geo module means no country,
so every request resolves at that top node, and only a lookup that failed
resolves at the requires-signal floor, because a place that could not be
determined must not be treated as the declared default.

Modules are then asked in order. Each sees what the modules before it
settled on and may amend it. A module with no opinion returns `Neutral` and
leaves the prior value standing, which is different from refusing.

## The order is the policy

The last module with an opinion decides, so the order is the policy. It is
a deployment's to set, not this code's to assume.

```toml
[permission-signal]
modules = ["gpc", "gpp", "us-privacy", "tcf", "mtm"]
```

A module not on the list does not run, and there is no separate switch. A
publisher who does not want to act on Global Privacy Control removes `"gpc"`
from the list, and the module that reads the header then does not run. One
caveat: core's consent pipeline can also synthesize a US Privacy opt-out from
that header for a visitor in a US state, when the consent settings say to,
which they do by default, and the `us-privacy` module then acts on the
record it produced. A publisher who wants the header to have no effect at all
turns that setting off as well. Leaving `modules` out, or the section
entirely, runs every module the adapter offers, in the order it offers them,
so a signal is never quietly ignored because someone forgot to list it. An
unknown or repeated name is refused at startup, so a typo cannot silently stop
a scheme being honored.

A module is named by its crate folder below `crates/permission-signal`, so
`gpp` and `us-privacy` are the folders those crates live in, and a name may
also be written in full, as `permission-signal.gpp`. A module that gains
settings will take them in a `[permission-signal.<name>]`
block named for it. None of the five here has settings, so `modules` is the
only key the section accepts, and a block or any other key is refused as an
unknown field rather than ignored.

The default order asks the signal with no interface of its own first and the
ones carrying a choice made through an interface after. Global Privacy
Control is a browser setting, so it revokes personalization on arrival,
whereas a GPP sale opt-out, a US Privacy string and a TCF record each carry
an answer a person gave, so they are asked later and amend it. A deployment
wanting the browser setting to stand over a later answer puts `gpc` last.

Trusted Server takes no view on which scheme should win. That is a question
about a jurisdiction and a publisher.

## A module can see the others

Amending well sometimes needs to know who set the prior value. A module is
given the whole ordered list and its own position in it, so it can look up a
peer by name, see whether a peer it cares about is configured at all, and ask
a peer directly what that peer makes of a permission.

That is what makes a rule like "personalization is off, but only because
Global Privacy Control set it, so my answer supersedes it" expressible. The
rule itself belongs to whichever module wants it. This seam only makes the
information available.

Consulting a peer goes one level deep. A module answering a consultation
cannot consult in turn, so two modules asking each other cannot loop.

## The terms the data is available under

A module may also declare the terms documents the request's data is available
under, through `tdls` on the trait, and core carries what every configured
module declared on the permission state. Whoever receives the data reads them
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

Of the five schemes here only MTM carries terms, declaring the versioned Model
Terms document whenever a PMP answer is present, and it is the first of many
rather than the only one. The name matches the `tdl` member the Data Labels work puts on a
node of an `OpenRTB` request, which is where these travel once a bid request
carries them.

## Who could still answer

A permission whose baseline requires a signal, and for which every module
answered `Neutral`, is not set, and it is also not refused. Resolution records
it as awaited, and the page reads the list as `awaiting` beside `set`, so a
prompt that has not run yet can be told from a visitor who said no.

Waiting is only right for a permission some configured module could grant.
So each module declares, through `grants` on the trait, which permissions it
can ever answer `Grant` for under the policy it is given. An opt-out revokes and
declares nothing. TCF declares every Data Use a purpose maps to, and nothing
when the policy silences the record. The assembly keeps as awaited only what
some module declared, and everything else that requires a signal and got none
is simply unset.

## The signals that were valid

Each module also says, through `valid_signal` on the trait, which signal it
read from the request and used, as it was received. Core carries what every
configured module vouched for on the permission state, and the page reads
the list as `signals` beside `set`, `awaiting` and `tdls`. A page, a bid
request or a person reading the state relies on exactly those signals and no
other.

A signal that was absent, could not be read, has expired, or that no
configured module acts on is not in the list, and nothing says which. What
each of those means for the permissions is the decision of the module for
that scheme, taken in `signal` and taken silently. Nothing is logged, because
an unreadable record is a visitor's preference and not an operational fault.
The shipped modules read their own unreadable record as the refusal it
may have carried, on the permissions their scheme covers, and say nothing
about any other scheme. Core answers nothing ahead of the modules, so the
order decides what a readable record from one scheme means beside an
unreadable one from another, as it decides everything else.

A signal nobody vouched for goes no further. After assembly the consent
context keeps only the raw strings the valid signals name, so a corrupt or
expired record never reaches a bid request or anything else Trusted Server
sends on.

## Withdrawal is a separate question

A module may also say that the request explicitly _withdraws_ a permission,
which is different from not granting it. A withdrawal of storage expires the
browser cookie and writes the authoritative tombstone against the identifier.
A permission that is merely not set strips the response headers and leaves an
already-issued identifier alone, so a returning visitor is not permanently
withdrawn before they get to answer. Most schemes have no such notion, a
browser setting and a sale opt-out included, and only TCF answers it. Core
then scopes the answer to the jurisdiction, so a refusal only withdraws where
the storage baseline did not grant storage outright, because where it did
the identifier never depended on the record.

## Writing a module

Implement `PermissionSignalModule` in a crate that depends on core, and give
it its name through `module_name!()`, which is the name configuration uses. Answer
`Neutral` for a permission the module has no opinion on, including when the
signal it reads is absent from the request. Returning `Revoke` for an absent
signal turns silence into refusal and would revoke the permission on every
request not carrying that scheme, which is most of them.

Read the request through `SignalInput::evidence`, which offers headers,
cookies, the path and the query, so a scheme core has never heard of can read
its own signal. The decoded consent record is offered too, for the schemes
core's consent pipeline already decodes, caches against the identifier and
expires. Prefer the record where it exists, because a module that
re-decodes the wire would skip the cached record on a returning visitor and
answer differently from every other reader of the same request.

Read the policy for what is a deployment's decision rather than the scheme's,
such as which signals count as an opt-out and what an opt-out takes away, so
that a deployment can change those without changing a module.

Register the crate at the adapter's composition root, where every adapter
builds its `RuntimeServices`. The adapter lists the modules it links, in the
default order, and `build_permission_signal_modules` selects and orders them
from configuration.

## The modules supplied

Five crates ship, under `crates/permission-signal/`, and a deployment
configuring nothing gets all five in this order:

| Name         | Crate        | Reads                                               |
| ------------ | ------------ | --------------------------------------------------- |
| `gpc`        | `gpc`        | The `Sec-GPC` header, Global Privacy Control        |
| `gpp`        | `gpp`        | The US sale opt-out in a GPP string                 |
| `us-privacy` | `us-privacy` | The sale opt-out in a US Privacy string             |
| `tcf`        | `tcf`        | A TCF v2 record, with its purpose mapping in code   |
| `mtm`        | `mtm`        | The PMP answer, under the Model Terms for Marketing |

The three opt-outs are separate rather than one so that a publisher who does
not act on Global Privacy Control can remove it and keep the other two.

## What is not a module

Nothing. Core reads no scheme and answers for none, an unreadable record
included. A publisher chooses which signals to act on by listing modules,
and each module decides for its own scheme what an absent, unreadable or
expired signal means. A scheme that is not listed does not run, whatever the
request carries for it.
