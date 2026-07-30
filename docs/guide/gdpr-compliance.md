# GDPR Compliance

Consent signal handling in Trusted Server.

## Overview

Trusted Server reads consent signals from each request, decodes them,
and applies built-in enforcement rules to consent-gated activities
such as EC creation and EID forwarding. The publisher configures how
signals are interpreted: which countries and US states map to each
jurisdiction's rules, how Global Privacy Control is read, how
conflicting signals are resolved, and when stored signals expire.
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
- Global Privacy Platform (GPP)
- US Privacy String
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

[consent.gdpr]
applies_in = ["DE", "FR"]      # countries mapped to the GDPR rules

[consent.us_states]
privacy_states = ["CA", "CO"]  # US states mapped to the US state rules

[consent.us_privacy_defaults]
gpc_implies_optout = true      # how the Sec-GPC header is interpreted

[consent.conflict_resolution]
mode = "restrictive"           # or "newest" / "permissive"
```

Each field tunes how signals are interpreted. The per-jurisdiction
gates and their fail-closed defaults are built in.

## Operational Behavior

- Consent checks run before consent-gated activities (EC creation,
  EID forwarding).
- Missing signals fail closed in regulated and unknown jurisdictions.
  Resolution of conflicting signals is configurable (restrictive,
  newest, or permissive).
- Audit logging records the consent decision per gated activity.
- Regional rules are applied per detected jurisdiction.

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

- [Configuration Reference](/guide/configuration)
- [Edge Cookies](/guide/edge-cookies)
- [Architecture](/guide/architecture)
