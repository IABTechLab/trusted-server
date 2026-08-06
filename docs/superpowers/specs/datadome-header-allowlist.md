# DataDome field allowlists (normative, checked-in — vendor request and response directions)

Adding or changing any name here is a reviewed commit to this file and
a spec change. This file holds the **only** normative Protection API
request-field and response-pointer lists; the hook spec §4a references it
and carries no duplicate or per-decision lists of its own.

## Protection API request fields (browser request → DataDome only)

`SecurityUse` admits only the fields below to the configured DataDome
Protection API endpoint. They are request-scoped and are never persisted in
the identity graph, copied to publisher upstream or another integration, or
logged as raw values.

The endpoint is the fixed core-owned
`https://api-fastly.datadome.co/validate-request`; “configured endpoint” in
this file means that DataDome protection is enabled, not that an operator may
supply an authority. Redirect following is disabled. No TS-controlled
advertising identifier, consent-store key, graph value, request query, or full
referrer is admitted. The normalized publisher URL path remains disclosed and
may itself contain publisher-chosen data; sign-offs 23/28 must classify that
surface, its retention, and DSR handling rather than calling the entire URL
identity-free.

Core-derived fields:

- `Key`, `IP`, `Method`, `Protocol`, `Host`, `ServerHostname`, `Request`
- `RequestModuleName`, `ModuleVersion`, `TimeRequest`, `Port`
- `ServerName`, `ServerRegion`
- `ClientID` from the single unambiguous `datadome` cookie only. The form key
  is always present because the Protection API declares it mandatory; its
  value is the empty string when no unambiguous cookie exists
- `CookiesLen`, `AuthorizationLen`, and `PostParamLen` as lengths only
- `HeadersList`, containing only the source header names admitted by the
  next list plus `authorization`, `content-length`, and `cookie` (whose values
  remain length-only/ClientID-only above). Names are lowercased, comma-separated,
  and retain received field-line order, including repeated admitted names;
  arbitrary/custom header names are excluded. An adapter that cannot preserve
  received header order does not qualify this integration until DataDome
  approves a canonical replacement order in sign-off 28

Core derives those fields identically on every qualified adapter:

- `Key` is the resolved DataDome server secret and is never obtained from
  request/config text; `IP` and `Port` are the remote address and TCP source
  port from trusted connection metadata. Missing `Key`, `IP`, or `Port` skips
  the call through the metered fail-open path; no sentinel is synthesized
- `Method` is the validated HTTP method token; `Protocol` is exactly `http` or
  `https` from the adapter request URI; `Host` is the normalized ASCII request
  authority with a non-default port retained; `ServerHostname` is trusted TLS
  SNI/local-host metadata, omitted when unavailable
- `Request` is only the URL path. Empty path becomes `/`; dot segments are
  removed, percent escapes are preserved without percent-decoding and
  normalized to uppercase hex, and the complete query and fragment are
  discarded before the security view exists
- `RequestModuleName` is the literal `trusted-server`; `ModuleVersion` is the
  build's checked-in Trusted Server version; `TimeRequest` is the request-ingress
  Unix timestamp in decimal microseconds, captured once before integration
  processing and constrained to `0..=2^53-1`
- `ServerName` is the adapter-qualified deployment/service name and
  `ServerRegion` is its adapter-qualified region code; either is omitted when
  the platform cannot supply it without request input
- `CookiesLen`, `AuthorizationLen`, and `PostParamLen` are decimal byte counts
  of the received field/body surfaces before redaction. Overflow beyond an
  unsigned 64-bit count skips the call; it never wraps or truncates

Exact request-header value mappings:

- `Accept` ← `accept`; `AcceptCharset` ← `accept-charset`;
  `AcceptEncoding` ← `accept-encoding`; `AcceptLanguage` ← `accept-language`
- `CacheControl` ← `cache-control`; `Connection` ← `connection`;
  `ContentType` ← `content-type`; `From` ← `from`; `Origin` ← a successfully
  parsed `origin` serialized as scheme + ASCII host + non-default port only;
  `Pragma` ← `pragma`; `Referer` ← a successfully parsed `referer` reduced to
  scheme + ASCII host + non-default port only; `UserAgent` ← `user-agent`;
  `Via` ← `via`
- `SecCHDeviceMemory` ← `sec-ch-device-memory`; `SecCHUA` ← `sec-ch-ua`;
  `SecCHUAArch` ← `sec-ch-ua-arch`; `SecCHUAFullVersionList` ←
  `sec-ch-ua-full-version-list`; `SecCHUAMobile` ← `sec-ch-ua-mobile`;
  `SecCHUAModel` ← `sec-ch-ua-model`; `SecCHUAPlatform` ←
  `sec-ch-ua-platform`
- `SecFetchDest` ← `sec-fetch-dest`; `SecFetchMode` ← `sec-fetch-mode`;
  `SecFetchSite` ← `sec-fetch-site`; `SecFetchStorageAccess` ←
  `sec-fetch-storage-access`; `SecFetchUser` ← `sec-fetch-user`
- `X-Requested-With` ← `x-requested-with`

Request-field multiplicity is normalized **before** parsing, truncation, and
form encoding, and adapters expose every received field line rather than a
preselected first/last value. Every admitted value line must contain valid HTTP
field-value octets **and** valid UTF-8 after OWS removal; otherwise the vendor
call is skipped through the metered fail-open path, because adapter-specific
byte-to-string replacement is forbidden:

- The list-valued source fields are exactly `accept`, `accept-charset`,
  `accept-encoding`, `accept-language`, `cache-control`, `connection`,
  `pragma`, `via`, `sec-ch-ua`, and `sec-ch-ua-full-version-list`. Core removes
  leading and trailing optional whitespace from each field value, rejects a
  value containing invalid field-value octets, and combines all field lines
  (including empty values) in received order with the two literal bytes `, `.
  This one normalized value is then parsed where the mapping above requires
  parsing and is then bounded. Commas inside an individual value are not split
  and reserialized.
- Every other admitted value-bearing source header in the exact mapping above
  is singleton. Zero lines means omit the DataDome field. Exactly one valid
  line is OWS-normalized and processed. Two or more lines — even identical —
  are ambiguous and skip the vendor call through the metered fail-open path;
  core never chooses first, last, or comma-joined. In particular this applies
  to `origin`, `referer`, `user-agent`, `content-type`, `from`, every remaining
  `sec-ch-*`/`sec-fetch-*` field, and `x-requested-with`.
- `authorization` and `content-length` are security singletons for this view.
  Repetition skips the vendor call before either length or `HeadersList` is
  constructed. `AuthorizationLen` is the byte length of the one
  OWS-normalized value. `PostParamLen` is always the byte length of the body
  actually presented to core, not the numeric `content-length` value; a
  malformed or body-inconsistent `content-length` is rejected by the shared
  HTTP request boundary before integrations run.
- Multiple `cookie` field lines are permitted. Core OWS-normalizes them and
  joins them in received order with the literal bytes `; ` for the shared RFC
  cookie parser. `CookiesLen` is the byte length of that canonical joined
  value. `ClientID` is populated only when the parsed result contains exactly
  one syntactically valid `datadome` pair; malformed cookie syntax or duplicate
  `datadome` pairs produces the required empty `ClientID` value without
  exposing another cookie. The original cookie values never enter the vendor
  payload.
- After successful normalization, `HeadersList` records the lowercased name of
  every admitted received field line in original line order, so repeated list
  fields and cookie lines remain repeated. A rejected request produces no
  `HeadersList` and no vendor call. Per-field caps apply to the single
  normalized value; the 24,576-byte cap applies after complete form encoding.

For an optional mapped value, zero received lines omits both source and mapped
field; one or more lines whose OWS-normalized values are all empty omits the
mapped form field but retains each received source name in `HeadersList`. If at
least one list-valued line is nonempty, empty siblings remain represented in
the exact received-order `, ` join. Mandatory `ClientID` and the three length
fields follow their explicit rules instead of this optional-field omission.

Adapter qualification fixtures feed the same ordered repeated-field corpus to
every host and assert byte-identical form fields, lengths, `HeadersList`, and
reject/omit outcomes. The corpus includes repeated list fields, identical and
different singleton duplicates, multiple cookies, duplicate `datadome`
cookies, empty values, invalid octets, and headers whose individual values
contain commas; invalid UTF-8 is a skip, never replacement decoding.

`true-client-ip`, `x-forwarded-for`, and `x-real-ip` are not admitted in v1.
The trusted `IP` field already supplies connection provenance; copying raw
forwarding headers would let a client or unqualified proxy manufacture vendor
evidence. A future adapter-normalized forwarding chain requires a separately
named typed field and vendor sign-off, never reuse of the raw header mapping.

Platform host evidence:

- `TlsProtocol`, capped by TS at 32 bytes
- `JA4`, capped by TS at 128 bytes, only when the operator explicitly sets
  `[integrations.datadome] expose_host_fingerprints_to_vendor = true`;
  the default is `false`, omission is represented by absence rather than an
  empty field, and startup logs the additional vendor disclosure
- `TlsCipher` is omitted in v1: DataDome defines it as the ordered list of
  cipher suites offered by the client, while `RuntimeServices::client_info()`
  exposes only the negotiated cipher. Substituting that value would silently
  change the field's meaning
- `H2Fingerprint` is omitted in v1 because the current Protection API contract
  does not define such a request field

`X-DataDome-ClientID` is never a Protection API source in cookie-mode v1.
No wildcard (`Sec-CH-*`, `Sec-Fetch-*`, `X-*`, or otherwise) expands this
list.

The following limits are bytes of the decoded field value before form
encoding. Truncation is UTF-8-boundary-safe. `XForwardedForIP` alone truncates
from the end; every other bounded field retains its prefix:

- 8 bytes: `SecCHDeviceMemory`, `SecCHUAMobile`,
  `SecFetchStorageAccess`, `SecFetchUser`
- 16 bytes: `SecCHUAArch`
- 32 bytes: `SecCHUAPlatform`, `SecFetchDest`, `SecFetchMode`, and the TS cap
  on `TlsProtocol`
- 64 bytes: `ContentType`, `SecFetchSite`, and the TS cap on `ServerRegion`
- 128 bytes: `AcceptCharset`, `AcceptEncoding`, `CacheControl`, `Connection`,
  `From`, `Pragma`, `SecCHUA`, `SecCHUAModel`, `X-Requested-With`, and the TS
  cap on opt-in `JA4`
- 256 bytes: `AcceptLanguage`, `SecCHUAFullVersionList`, `Via`
- 512 bytes: `Accept`, `ClientID`, `HeadersList`, `Host`, `Origin`,
  origin-only `Referer`, `ServerHostname`, and `ServerName`
- 768 bytes: `UserAgent`
- 2,048 bytes: path-only `Request`

`Key`, `AuthorizationLen`, `CookiesLen`, `IP`, `Method`, `ModuleVersion`,
`Port`, `PostParamLen`, `Protocol`, `RequestModuleName`, and `TimeRequest` are
unbounded per-field by the vendor table but remain subject to the total bound.
The complete `application/x-www-form-urlencoded` body, including field names,
`=`/`&` separators, and percent-encoding expansion, must be at most **24,576
bytes**. Core constructs and measures the whole payload before issuing the
request. It does not silently drop optional fields to fit: overflow skips the
vendor call and takes the same metered fail-open `Continue` path as a transport
failure.

This is deliberately narrower than DataDome's currently documented required
surface: notably, it withholds `CookiesList` and omits empty source-header
fields. Product/vendor sign-off 28 therefore requires written confirmation
that this exact reduced profile is supported. Until that confirmation and
adapter conformance fixtures exist, the DataDome integration is not
release-qualified.

## Request-direction pointer (vendor response → publisher-upstream overlay)

The complete set of vendor-response header pointers the security
channel (hook spec §4a) may copy into the owner-scoped
publisher-upstream overlay. Every `X-DataDome-*` name not listed here
is rejected.

| Header                | Direction                   | Scope                                                                                                                                                                  |
| --------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `X-DataDome-ClientID` | response → upstream overlay | Disabled by default; admitted only with `expose_client_id_to_origin = true`. Owner-scoped publisher overlay only, never the shared request view or another integration |

## The single pointer matrix (normative — decision × session mode × pointer)

This is the one authoritative browser-response contract. Session mode
is **cookie** in v1 (sessionByHeader is startup-rejected; a header-mode
column is added by the sign-off-23 opt-in, never implicitly). No
wildcard rows exist — every accepted name is enumerated, and **every
cell terminates in exactly one outcome**.

| Pointer             | Respond (cookie mode)                                                                                                                                              | Continue (cookie mode)                        |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------- |
| `Set-Cookie`        | typed `datadome` cookie operation (hook spec §4a parser; exactly one `datadome` field)                                                                             | typed `datadome` cookie operation             |
| `X-Set-Cookie`      | **invalidate the batch → Continue** (mode mismatch: TS never requests header sessions in v1; a vendor response asserting one must not half-apply)                  | **invalidate the batch** → effects dropped    |
| `Location`          | forward on a 3xx Respond, replace; **on a non-3xx Respond → invalidate the batch → Continue** (a redirect field without a redirect status is a malformed decision) | **invalidate the batch** (effects dropped)    |
| `Content-Type`      | forward (owns its body), replace                                                                                                                                   | **invalidate the batch** (effects dropped)    |
| `Cache-Control`     | restricted merge; invariant pass last                                                                                                                              | **invalidate the batch** (effects dropped)    |
| `Pragma`            | drop-individually, logged (response `Pragma` has no standardized meaning, RFC 9111 §5.4)                                                                           | drop-individually, logged                     |
| `X-DataDome`        | forward, owner-scoped typed telemetry                                                                                                                              | forward, owner-scoped typed telemetry         |
| `X-DD-B`            | forward as a browser-response security signal; never copy to publisher-upstream or another integration                                                             | forward as a browser-response security signal |
| anything not listed | invalidate the batch → Continue                                                                                                                                    | invalidate → effects dropped                  |

Singleton-field multiplicity (`Location`, `Content-Type`, `X-DataDome`,
`X-DD-B`, `X-Set-Cookie` appearing more than once) invalidates the
batch atomically; list-valued fields (`Cache-Control`, `Pragma`) join
per RFC 9110 §5.3 before their cell applies (hook spec §4a).

`X-DD-B` is security-owned when DataDome is enabled. Before applying the fresh
security batch, core removes every pre-existing instance from the origin,
cached ordinary artifact, 304 metadata update, core response, or ordinary
mutator. A valid pointed vendor value then uses **replace-all** and the final
response cardinality must be exactly one; if the fresh vendor batch does not
point to it, final cardinality is zero. Append is never allowed. Fixtures cover
origin collision, cache-hit collision, 304 collision, repeated vendor fields,
and one valid fresh value, proving “exactly once” at final emission rather than
merely inside the vendor batch.

**Fixtures**: DataDome's documented challenge response (`Set-Cookie`,
`Pragma`, `X-DataDome`, `Cache-Control`) asserts the decision stays
**Respond** with exactly the mapped fields; the documented allow example
(`Set-Cookie X-DD-B`) asserts Continue proceeds with the cookie applied
and `X-DD-B` forwarded exactly once — neither fixture may fail open.
