# GDPR Compliance

Consent signal handling in Trusted Server.

## Overview

Trusted Server reads consent signals from each request, decodes them,
and applies built-in enforcement rules to consent-gated activities
such as EC creation and EID forwarding. The publisher configures how
signals are interpreted, meaning how Global Privacy Control is read,
how conflicting signals are resolved, and when stored signals expire.
Which countries and US states fall under which jurisdiction's rules is
not set here, because the `permissions.yaml` rules tree states it. See
the [Permission Model](/guide/permission-model).
The per-activity gates and their fail-closed defaults are built in.

## Policy Posture

Trusted Server is technology. It is neutral on policy. Deployers
operate under different laws and policies, and each decides how to
configure the consent surface for their deployment. Trusted Server's
role is to provide the controls and respect them at request time.

## Consent Management

### Consent Validation

Signals are read from the request and evaluated by built-in
per-activity gates.

```rust
// Placeholder example
if !validate_consent(&request, &policy) {
    return reject_activity();
}
```

### Consent Sources

Trusted Server can interoperate with multiple consent signal formats:

- TCF v2 format (the IAB Transparency and Consent Framework encoded string)
- Global Privacy Protocol (GPP)
- Global Privacy Control (GPC) request header
- Publisher-defined custom signals
- First-party consent cookies

References to _TCF v2 format_ on this page refer to the encoded string
schema. The Transparency and Consent Framework as a policy framework
is one option a deployer can elect. It is not the assumed default.

## Implementation

### Checking Consent

```javascript
// Placeholder example
const hasConsent = await trustedServer.checkConsent({
  purposes: ['storage', 'personalization'],
  vendors: [vendor_id],
})
```

### Consent Storage

- Signals are read from the request on every transaction.
- A minimal consent snapshot is stored as EC entry metadata in the KV
  identity graph. Request-time interpretation always uses the live
  request signals.
- Signals are passed through to integrations the publisher has
  configured to receive them.

## Privacy Controls

### User Rights

Where the publisher's regime grants user rights (for example GDPR's
access, erasure, portability, objection), Trusted Server provides the
hooks the publisher uses to honor them at the edge. The shape depends
on the regime and the publisher's implementation.

### Data Minimization

Trusted Server collects only what the publisher has configured:

- EC IDs (subject to the publisher's policy)
- Request metadata used by configured integrations
- No name, email, or account identifier fields supplied by the user

## Configuration

Configure consent handling in the `[consent]` section of
`trusted-server.toml`. The block below is illustrative. See the
[Configuration Reference](/guide/configuration) for the full surface.

```toml
[consent]
mode = "interpreter"           # or "proxy" (forward raw strings without decoding)
max_consent_age_days = 365     # expiration check for dated signals

[consent.us_privacy_defaults]
gpc_implies_optout = true      # how the Sec-GPC header is interpreted

[consent.conflict_resolution]
mode = "restrictive"           # or "newest" / "permissive"
```

Each field tunes how signals are interpreted. The per-jurisdiction
gates and their fail-closed defaults are built in.

Which jurisdiction applies to a visitor is not configured here. The
`[consent.gdpr] applies_in` and `[consent.us_states] privacy_states`
lists are retired, and a `jurisdiction` attribute on the
`permissions.yaml` rules tree does their job. Every node of that tree
may name the jurisdiction for the places it covers, a node that names
none inherits the nearest one above it, and the top of the tree answers
a visitor whose country cannot be resolved. One file therefore carries
the permission baselines and the jurisdiction assignment together. The
[Permission Model](/guide/permission-model) documents the tree, so this
page does not repeat it.

## Operational Behavior

- Consent checks run before consent-gated activities (EC creation,
  EID forwarding).
- Missing signals fail closed in regulated and unknown jurisdictions.
  Resolution of conflicting signals is configurable (restrictive,
  newest, or permissive).
- Audit logging records the consent decision per gated activity.
- Regional rules are applied per detected jurisdiction, which the rules
  tree assigns from the visitor's country and region.

## Best Practices

1. Configure the consent surface to match the deployer's policy and
   jurisdictional scope.
2. Document the mechanisms used so users, partners, and regulators can
   see what is in effect.
3. Honor withdrawal of consent in the same configuration that
   captured it.
4. Review the configured policy periodically.
5. Retain consent records to the extent required by applicable law and
   the deployer's own policy.

## Next Steps

- [Permission Model](/guide/permission-model)
- [Configuration Reference](/guide/configuration)
- [Edge Cookies](/guide/edge-cookies)
- [Architecture](/guide/architecture)
