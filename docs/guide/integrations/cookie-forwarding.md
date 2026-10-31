# Integration Cookie Forwarding

Browser requests to a first-party integration route can carry the publisher's session, authentication, identity, and consent cookies. Trusted Server limits which cookies reach third-party upstreams. The publisher-origin request has a separate purpose: it preserves the browser cookies needed by the publisher application.

## Prebid Server

Prebid Server (PBS) requests allow only these exact, case-sensitive consent cookie names:

- `euconsent-v2`
- `__gpp`
- `__gpp_sid`
- `us_privacy`

The `consent_forwarding` setting selects how consent travels:

| Mode             | Outbound `Cookie` header            | OpenRTB body consent                                                                         |
| ---------------- | ----------------------------------- | -------------------------------------------------------------------------------------------- |
| `both` (default) | Only the four consent cookies above | Included                                                                                     |
| `cookies_only`   | Only the four consent cookies above | Only affected unsafe signals fall back to the body; other cookie-sourced signals are omitted |
| `openrtb_only`   | Omitted                             | Included                                                                                     |

Unrelated cookies are excluded in every mode. This includes publisher session and authentication cookies, `ts-ec`, `ts-eids`, and `sharedId`. An unreadable, non-ASCII Cookie header is omitted rather than forwarded unchanged. Cookie parsing uses the same parser as consent extraction: names and values are trimmed, and the last occurrence of each consent cookie wins. Values with unsafe outbound framing are omitted. In `cookies_only`, only the signal whose cookie cannot be forwarded safely is included in the OpenRTB body, so it cannot disappear from both channels. The GPP string and its section IDs fall back together if either GPP cookie is unsafe; safely forwarded TC and US Privacy signals remain omitted. Already valid values retain their framing, including quoted values. Decimal GPP section IDs with whitespace, such as `2, 6`, are normalized to `2,6`. When filtering leaves no allowed cookies, the outbound Cookie header is omitted.

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

| Surface                                                         | Cookie behavior                                                                                                                                                             |
| --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| PBS auction provider                                            | Uses the mode-dependent consent cookie allowlist above                                                                                                                      |
| Standard OpenRTB and APS auction providers                      | Construct fresh requests without copying the browser Cookie header                                                                                                          |
| Lockr                                                           | Omits Cookie; forwards selected standard headers and client `x-*` headers except TS-internal names; the SDK supplies selected identity data in its `firstPartyCookies` body |
| Permutive                                                       | Copies selected standard headers and client `x-*` headers except TS-internal names; excludes Cookie                                                                         |
| Didomi, DataDome SDK and browser API proxy                      | Copy curated request headers that exclude Cookie                                                                                                                            |
| GTM, GPT, Testlight, `/first-party/proxy`, JS and asset proxies | Forward fixed header allowlists that exclude Cookie                                                                                                                         |
| Sourcepoint                                                     | Uses its own partner-specific allowlist, described below                                                                                                                    |
| Ad server mock mediator and EC partner pull sync                | Construct fresh requests without copying the browser Cookie header                                                                                                          |
| Publisher origin                                                | Preserves browser cookies for the publisher application                                                                                                                     |

### Identity Values Outside Cookie Headers

The table describes raw `Cookie` headers. Some upstream contracts also carry selected identity values separately:

- The `/first-party/proxy` creative proxy adds the request's EC ID, read from the `x-ts-ec` header or else the `ts-ec` cookie, as a `ts-ec` query parameter. GTM, GPT, Testlight, the JS asset proxy, and the Prebid bundle fetch turn this off in code; it is not an operator setting.
- Click redirects add the request's EC ID as a `ts-ec` query parameter on the target URL.
- Testlight puts the `ts-ec` cookie value into its rewritten request body as `user.id`.
- EC partner pull sync sends the EC ID to the partner as an `ec_id` query parameter.

These selected-value contracts are independent of the PBS cookie allowlist; excluding an identity cookie from a raw header does not remove its other uses.

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

The server-side DataDome protection request extracts the `datadome` cookie as a client identifier when an explicit client-ID header is absent. It also sends the length of the selected inbound Cookie header value as `CookiesLen`. It does not send the full browser Cookie header or unrelated cookie values through those fields.

### Publisher Origin and Browser Requests

Requests to the configured publisher origin intentionally retain browser cookies so sessions and authenticated application behavior continue to work. Third-party integration filtering must not remove those cookies from the publisher-origin request.

Browser calls to Trusted Server's own page-bids, click, and signing routes can also include same-origin credentials. The upstream integration policy applies when Trusted Server constructs a subsequent third-party request.
