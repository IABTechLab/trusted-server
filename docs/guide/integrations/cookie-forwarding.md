# Integration Cookie Forwarding

Browser requests to a first-party integration route can carry the publisher's session, authentication, identity, and consent cookies. Trusted Server limits which cookies reach third-party upstreams. The publisher-origin request has a separate purpose: it preserves the browser cookies needed by the publisher application.

## Prebid Server

Prebid Server (PBS) requests allow only these exact, case-sensitive consent cookie names:

- `euconsent-v2`
- `__gpp`
- `__gpp_sid`
- `us_privacy`

The `consent_forwarding` setting selects how consent travels:

| Mode             | Outbound `Cookie` header            | OpenRTB body consent                                                            |
| ---------------- | ----------------------------------- | ------------------------------------------------------------------------------- |
| `both` (default) | Only the four consent cookies above | Included                                                                        |
| `cookies_only`   | Only the four consent cookies above | Cookie-sourced consent is omitted; policy-sourced consent can still be included |
| `openrtb_only`   | Omitted                             | Included                                                                        |

Unrelated cookies are excluded in every mode. This includes publisher session and authentication cookies, `ts-ec`, `ts-eids`, and `sharedId`. An unreadable, non-ASCII Cookie header is omitted rather than forwarded unchanged. Malformed cookie pairs are omitted; accepted values retain their original framing, including quoted values and comma-separated decimal GPP section IDs. When filtering leaves no allowed cookies, the outbound Cookie header is omitted.

For example, in `both` mode this inbound header:

```http
Cookie: session_id=example-session; euconsent-v2=example-consent; theme=dark
```

produces this PBS header:

```http
Cookie: euconsent-v2=example-consent
```

Cookie forwarding and identity forwarding are separate decisions. Excluding identity cookies from the raw header does not remove the server's identity-resolution path. Trusted Server can read the relevant first-party cookies, resolve identity, and populate `user.id` and `user.ext.eids` under the applicable consent rules. Forwarding a consent cookie also does not authorize identity forwarding by itself.

See [Prebid integration](./prebid) and [GDPR compliance](/guide/gdpr-compliance) for configuration and consent behavior.

## Reviewed Upstream Surfaces

| Surface                                                         | Cookie behavior                                                                                                                |
| --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| PBS auction provider                                            | Uses the mode-dependent consent cookie allowlist above                                                                         |
| Standard OpenRTB and APS auction providers                      | Construct fresh requests without copying the browser Cookie header                                                             |
| Lockr                                                           | Omits the browser Cookie header; the SDK supplies selected identity data through its `firstPartyCookies` request-body contract |
| Permutive and Didomi                                            | Copy curated request headers that exclude Cookie                                                                               |
| DataDome SDK and browser API proxy                              | Copy curated request headers that exclude Cookie                                                                               |
| GTM, GPT, Testlight, generic third-party proxy, and asset proxy | Default forwarded-header sets exclude Cookie                                                                                   |
| Sourcepoint                                                     | Uses its own partner-specific allowlist, described below                                                                       |
| Ad server mock mediator and EC partner pull sync                | Construct fresh requests without copying the browser Cookie header                                                             |
| Publisher origin                                                | Preserves browser cookies for the publisher application                                                                        |

### Sourcepoint

Sourcepoint's proxy allows its established cookie names:

- `consentUUID`
- `euconsent-v2`
- `dnsDisplayed`
- `ccpaApplies`
- `signedLspa`
- `_sp_su`
- `consentDate`
- `usnatUUID`
- `consentDateUsnat`
- `globalcmpUUID`
- `consentDateGlobalcmp`

Operators can additionally configure one custom Sourcepoint authentication cookie through `auth_cookie_name`. Unrelated publisher cookies are excluded, and an unreadable Cookie header is omitted. This partner contract is independent of the PBS consent cookie allowlist. See [Sourcepoint integration](./sourcepoint#cookie-forwarding-and-caching).

### DataDome Protection

The server-side DataDome protection request extracts the `datadome` cookie as a client identifier when an explicit client-ID header is absent. It also sends the total inbound Cookie header length as `CookiesLen`. It does not send the full browser Cookie header or unrelated cookie values through those fields.

### Publisher Origin and Browser Requests

Requests to the configured publisher origin intentionally retain browser cookies so sessions and authenticated application behavior continue to work. Third-party integration filtering must not remove those cookies from the publisher-origin request.

Browser calls to Trusted Server's own page-bids, click, and signing routes can also include same-origin credentials. The upstream integration policy applies when Trusted Server constructs a subsequent third-party request.
