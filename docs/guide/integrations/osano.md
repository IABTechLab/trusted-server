# Osano Integration

**Category**: CMP (Consent Management Platform)
**Status**: Development
**Type**: Browser Consent Mirror

## Overview

The Osano integration mirrors consent signals exposed by Osano's browser IAB APIs into standard first-party consent cookies that Trusted Server can read on later requests.

The browser module reads:

- `__uspapi` for US Privacy strings
- `__gpp` for GPP strings and applicable section IDs
- `__tcfapi` for TCF v2 consent strings

It writes the corresponding first-party cookies when Osano reports ready consent data:

| Cookie            | Source signal                   |
| ----------------- | ------------------------------- |
| `us_privacy`      | USP string from `__uspapi`      |
| `__gpp`           | GPP string from `__gpp`         |
| `__gpp_sid`       | GPP applicable section IDs      |
| `euconsent-v2`    | TCF string from `__tcfapi`      |
| `_ts_consent_src` | Ownership marker set to `osano` |

## Configuration

Add the following to `trusted-server.toml`:

```toml
[integrations.osano]
enabled = true
```

No additional server-side settings are required for the initial Osano integration.

## Request Timing Limitation

Osano consent mirroring runs in the browser after Trusted Server has already served the current page response. That means the first page request cannot include cookies written by this browser-side mirror.

Server-side behavior that depends on `us_privacy`, `__gpp`, `__gpp_sid`, or `euconsent-v2` should expect those mirrored cookies on subsequent requests after Osano's IAB APIs have become available and the browser module has written the cookies.

## Cookie Ownership and Clearing

The Osano mirror uses `_ts_consent_src=osano` to mark cookies it owns. It preserves existing unmarked consent cookies and cookies marked as owned by another mirror.

When Osano later reports a ready-but-empty consent value, the mirror clears stale Osano-owned cookies only after receiving an Osano readiness event. This avoids deleting previous consent cookies during CMP startup before Osano has finished loading.

## Endpoints

The Osano integration does not register Rust proxy endpoints in this version. It only enables the `tsjs-osano` browser module.

## See Also

- [Integrations Overview](/guide/integrations-overview)
- [Configuration](/guide/configuration)
