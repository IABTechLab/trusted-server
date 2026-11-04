# Publisher attestation

**Date:** 2026-11-04. **Status:** Proposed contract for maintainer and user review; no implementation or provider qualification is claimed.

**Issues:** [#161](https://github.com/IABTechLab/trusted-server/issues/161), [#1427](https://github.com/IABTechLab/trusted-server/issues/1427). **Predecessor:** binary attestation [#1235](https://github.com/IABTechLab/trusted-server/issues/1235), [#1435](https://github.com/IABTechLab/trusted-server/pull/1435), [#1449](https://github.com/IABTechLab/trusted-server/pull/1449), and subsequent release-evidence work.

**Repository baseline:** `origin/main` at `51730bdaf841e4d7bb96c2258e5e98169bdff9a7`. Release-branch observations are dependencies, not functionality assumed to exist on this baseline. This specification introduces no code, credentials, enrollment, remote writes, or release authorization.

## 1. Decision and assurance boundary

Publisher attestation connects a publisher host, its operator, a specific deployment, and an authorized public key to signed evidence and independent, time-bounded verifier assessments. It answers who made the declaration, what an independent party checked, how, and when. It does not establish continuous measured execution.

The selected architecture is publisher-produced evidence plus portable assessments from zero, one, or multiple independent verifiers. Publishers retain their keys. Consumers independently configure trusted verifier keys and acceptance policies. A registry is an optional directory and presentation service; it is not a mandatory administrator or key custodian.

The complete work includes independent deployment appraisal. An evidence-only milestone is useful, but MUST be labelled self-declared and MUST NOT be presented as an independently verified official deployment.

The intended strong result is:

> An identified verifier checked authorization for a publisher host and key, observed a specified production service/version, authenticated its selected executable against an eligible official release, and issued an assessment with an explicit observation time and expiry.

That result is available only after the selected provider method passes qualification. A provider package fingerprint match alone does not currently establish that result. The companion [Fastly binding qualification specification](2026-11-04-publisher-attestation-fastly-qualification-design.md) owns that gate.

### 1.1 Alternatives considered

| Approach                                                                 | Benefit                                                                                                | Cost or limitation                                                                     | Decision                                                       |
| ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| Publisher evidence plus independent, portable assessments                | Allows custom builds, multiple verifiers, and consumer choice; separates declarations from observation | Requires enrollment, provider qualification, and explicit freshness policy             | Selected                                                       |
| Operator signatures alone                                                | Small endpoint; useful identity and challenge evidence                                                 | Cannot independently establish installed official bytes or honest configuration        | Permitted delivery milestone, not the complete assurance claim |
| One administrator generates publisher keys and approves every deployment | One operational authority and policy                                                                   | Concentrates impersonation capability and governance; unnecessary for interoperability | Rejected                                                       |
| Hardware/provider-enforced measured execution and signing                | Could strengthen code-to-key binding                                                                   | No qualified capability established here                                               | Future separately specified assurance method                   |

### 1.2 Scope

In scope: host and operator authorization; deployment identity; publisher evidence; runtime key custody and rotation; independently verified release inputs; Fastly deployment observations; custom-build treatment; configuration claim boundaries; verifier assessment and withdrawal contracts; reference verification; safe networking; privacy; compatibility; acceptance tests; and operational responsibilities.

Out of scope: auction/bid signing, message-body integrity, auction execution proofs, bid replay caches, auction key delegation, transaction schemas, inventory truth, consent-input truth, legal-entity certification, broad configuration compliance certification, a hosted commercial registry, a registry dashboard, or hardware attestation. This work reserves stable deployment/key/assessment identifiers for future consumers; it does not design the next roadmap layer.

The first production provider is Fastly. Core formats and pure verification are portable. Axum supplies local test transport. Cloudflare, Spin, and reusable Fastly identity claims require their own runtime qualification before production assessment support. No new official adapter distributions are implied.

### 1.3 Normative language and ownership

MUST, MUST NOT, SHOULD, and MAY express requirements in the sense of BCP 14. Stable PA identifiers identify requirement groups. The wire version is `1`; schema names and message types below are proposed private protocol identifiers, not claims of IANA registration.

| Owner                                 | Responsibility                                                                                                    |
| ------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| Release producer and Layer 1 verifier | Authenticate executable bytes, source/workflow identity, official publication, and applicable release eligibility |
| Publisher/operator                    | Authorize enrollment, control DNS and deployment, provision runtime keys, maintain configuration and permissions  |
| Attester in Trusted Server            | Produce typed, fresh evidence from one actual request-used snapshot                                               |
| Independent verifier                  | Authenticate enrollment, collect provider observations, apply a named policy, sign assessments and status         |
| Consumer/relying party                | Supply independent trust roots, check bindings/freshness/status, and decide acceptance                            |
| Registry/directory                    | Distribute signed objects and clearly label their assurance; optional                                             |

## 2. Terminology and threat model

### PA-IDENTITY: keep the identities distinct

**Publisher host** is the public DNS name being checked, such as `news.example.com`. **Backend origin** is the upstream content source and is not publisher identity. **Publisher ID** is an opaque UUID assigned by the publisher; it is not evidence of legal identity. **Operator ID** is a stable HTTPS identifier chosen by the operator; ownership/association is an enrollment fact, not trusted because it is a URL. Verifiers MUST NOT fetch arbitrary operator-ID URLs.

**Deployment ID** is a publisher-assigned UUID stable for one intended logical deployment. Separate production/staging deployments MUST use different deployment IDs and keys. Cloning a service does not authorize reuse of an enrollment. **Binding ID** is the digest of the exact version/configuration/key descriptor defined below. A new service version, attestation key, or covered configuration value changes the binding ID. Assessment renewal does not change it.

Consumers MUST compare the full binding, not assume UUID uniqueness makes assertions true. Enrollment authorizes the tuple `(host, publisher_id, operator_id, deployment_id, provider, service_id, environment, attestation_key_thumbprint)`; versions and binding IDs are separately checked at assessment time.

### 2.1 Attacker capabilities and residual trust

Assume an attacker can copy endpoints and assessments, replay or relay challenges, manipulate unsigned discovery, control a custom binary and its reported metadata, change publisher configuration or service versions, upload unusual packages, obtain a publisher runtime key through deployment control, submit malicious enrollment/package URLs, and operate an untrusted verifier. Assume network outages and partial provider observations.

| Threat                                                    | Required control                                                                       | Remaining limitation                                                      |
| --------------------------------------------------------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Forged publisher or verifier signature                    | Pinned keys, strict typed signatures, key-purpose separation                           | A stolen trusted key can sign                                             |
| Evidence copied to another host                           | Expected host, independent authorization, fresh challenge, authenticated HTTPS         | A live same-host relay is not measured execution proof                    |
| A clean attestation path hides different traffic handling | Explicit endpoint-only claim; provider service/version observations                    | No proof that all site traffic follows this path                          |
| Custom code reports an official commit                    | Independently authenticated executable binding                                         | Self-reported headers/build facts are not measurements                    |
| Alternate code uses an endorsed key                       | Monitoring, short-lived assessments, withdrawal, isolated key scope                    | Operator-controlled software keys are not exclusive to approved code      |
| Ambiguous package produces matching fingerprint           | Provider binding qualification, structural evidence, stronger selected-module identity | Local package inspection alone cannot constrain a malicious remote upload |
| Configuration changes without activation                  | Actual snapshot binding and coverage declaration                                       | Uncovered/dynamic settings and propagation remain outside the claim       |
| SSRF or credential leakage                                | Fixed provider authority, constrained enrollment targets, bounded fetches, redaction   | Trust in configured resolver/provider remains                             |
| Compromised verifier or misleading directory              | Consumer-pinned trust roots/policies; signed provenance of assessments                 | An accepted verifier can lie                                              |

Consumers still rely on Fastly reporting its state truthfully, on verifier collection and signing being honest, on the release authority, and on the declared clock/status assumptions. No result promises absence of vulnerabilities, continuous execution, honest publisher inputs, or secret non-extractability.

Monitoring reduces exposure; it does not close the code-to-key gap. Binding version 42 does not invalidate its signed assessment when version 43 activates. A consumer rejects the mismatch only after trustworthy newer evidence/status reaches it, or the old assessment expires. No finite probe set proves universal routing or parser behavior.

## 3. Protocol artifacts and common encoding

### PA-WIRE: one strict signed envelope

Evidence, assessments, and status use compact JWS. The response body is the compact ASCII serialization, not a JSON object with an unsigned readable evidence copy. The protected header contains exactly `alg`, `kid`, and `typ`:

| Field | Version 1 rule                                                                                                   |
| ----- | ---------------------------------------------------------------------------------------------------------------- |
| `alg` | Exactly `Ed25519` as registered by RFC 9864; no polymorphic algorithm identifier                                 |
| `kid` | Base64url, unpadded SHA-256 JWK thumbprint of the signing public key                                             |
| `typ` | Exactly `ts-publisher-evidence+jws`, `ts-publisher-assessment+jws`, or `ts-publisher-status+jws`, as appropriate |

All header fields are protected. Reject unprotected headers, detached payloads, `none`, HMAC, legacy `EdDSA`, alternate curves, embedded trust keys, key URLs, certificate URLs, `crit`, compression, and unencoded payload extensions. Signature length is exactly 64 bytes. Public keys are JWK objects containing exactly `kty: "OKP"`, `crv: "Ed25519"`, and a canonical unpadded base64url `x` encoding 32 bytes. Private JWK material MUST NOT appear on a public surface. The transport uses JWS rules; implementations MUST NOT introduce the fork's separate context-prefix format under the same message type.

Sign and verify the original JWS segments; never reserialize a decoded payload to check its signature. Parse UTF-8 JSON with duplicate-member rejection, including nested objects, bounded depth 16, and no trailing data. Unknown members anywhere in a version 1 payload are rejected. The closed schemas below use an explicit `extensions` object, initially required empty; a nonempty extension requires a later supported profile/version. Arrays are limited to 16 entries unless a smaller limit is stated. Claim strings have a 256-byte UTF-8 limit unless specified otherwise. This bound is not applied to compact JWS serialization or its encoded segments. An assessment-list item is an explicitly exempt compact-JWS string of at most 32,768 ASCII bytes; the list remains bounded to 16 items and 524,288 total response bytes. These transport bounds also apply to unsigned discovery/list responses. Their decoded claim strings retain the ordinary bound. Structural JSON Schema `maxLength` counts Unicode code points, so receivers must independently enforce the UTF-8 byte and raw-response bounds before accepting data. Integers are nonnegative JSON integers no greater than `9007199254740991`. Time fields are UTC Unix seconds, not milliseconds.

Use SHA-256, lowercase 64-character hex, for document/content identifiers, except `kid` as above and Fastly's explicitly labelled SHA-512 fingerprint. `digest` means `{ "algorithm": "sha256", "value": <hex> }`. Canonical descriptor/projection hashes use JCS over the defined public JSON value. JWS signatures themselves do not require JCS. Object digests of signed objects hash the exact compact JWS bytes; do not hash a readable rendering.

The JOSE and canonicalization rules are based on RFC 7515, RFC 8037 as updated by RFC 9864, RFC 7638, RFC 8725, and RFC 8785. RFC 9864 replaces the deprecated polymorphic `EdDSA` identifier with `Ed25519` for this new protocol; a library that cannot enforce that protected identifier is not conforming and must not silently substitute it. These standards define encoding and validation primitives; the publisher claim semantics here are project requirements.

The accompanying [wire schema](schemas/publisher-attestation-v1.schema.json) uses JSON Schema draft-07 with local references and no remote schema fetch. Its definitions cover protected headers, signed payloads, discovery/list envelopes, enrollment challenges, and stored public metadata. The [public verification vectors](fixtures/publisher-attestation-v1-vectors.json) retain exact signed bytes, public keys, expected context, and positive/negative outcomes; no signing private key is retained. They are protocol fixtures, not provider observations or official-deployment qualification.

Schema validation is necessary but insufficient. Duplicate JSON members, canonical base64url, actual UTF-8 byte lengths, canonical host handling, JWK thumbprints, JCS/digest equality, signature/type/key agreement, time intervals, operator authorization, policy eligibility, status, and cross-object equality require the separate semantic checks specified here. Consumers reject a schema-valid object that fails any applicable semantic check. A valid signature on an unknown/legacy algorithm identifier or wrong message type is still rejected. Published vectors must be checked through two independent cryptographic implementations and include changed-signature, changed-context, stale, and mismatched-binding cases before wire freeze.

### PA-BINDING: exact deployment descriptor

The `binding` object is closed and has these required fields:

| Field             | Type and meaning                                                                                                                                  |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `schema`          | Literal `ts-publisher-binding-v1`                                                                                                                 |
| `host`            | Canonical authorized publisher host                                                                                                               |
| `publisher_id`    | Canonical lowercase UUID                                                                                                                          |
| `operator_id`     | Stable HTTPS identifier, no userinfo/query/fragment                                                                                               |
| `deployment_id`   | Canonical lowercase UUID                                                                                                                          |
| `environment`     | `production`, `staging`, or `development`                                                                                                         |
| `platform`        | Object: `provider` (`fastly` or `axum`), `service_id` (string or null), `service_version` (positive integer or null), `staging` (boolean or null) |
| `attestation_key` | Object: `kid`, public `jwk`, `not_before`, `not_after`                                                                                            |
| `configuration`   | Object: `schema` literal `ts-publisher-identity-v1`, `digest`, and `projection` defined in section 7                                              |
| `extensions`      | Empty object                                                                                                                                      |

`binding_id` is the lowercase 64-character hexadecimal string SHA-256 of JCS(`binding`), not a nested digest object. `attestation_key.kid` MUST equal its JWK thumbprint and the evidence signing header `kid`. Its validity interval MUST be nonempty and contain the entire evidence lifetime. Fastly service identity is adapter-observed runtime identity, still a declaration to an external verifier until corroborated. Null service fields are allowed only for development Axum evidence, never for a Fastly production profile. Unknown staging MUST be null, never silently false. Fastly staging must agree with the declared environment; `development` cannot pass production policy.

The descriptor MUST NOT include verifier assessment IDs, verifier list order, latest store state fetched after the request, private configuration hashes, or a claimed self-measurement of the binary. This avoids circular identity and changing the binding whenever an assessment renews.

### PA-EVIDENCE: publisher payload

| Required field            | Type and rule                                                                                                                                                                   |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `schema`                  | `ts-publisher-evidence-v1`                                                                                                                                                      |
| `binding`                 | Exact descriptor above                                                                                                                                                          |
| `binding_id`              | Matching descriptor digest                                                                                                                                                      |
| `nonce`                   | Canonical base64url encoding of exactly 32 random bytes supplied by the caller                                                                                                  |
| `issued_at`, `expires_at` | `issued_at < expires_at <= issued_at + 60`, within signing-key validity                                                                                                         |
| `build_report`            | Object with `declared_class` (`custom` or `unspecified`) and nullable `source_commit` (40 lowercase hex), `package_version` (string), `target` (string), and `variant` (string) |
| `extensions`              | Empty object                                                                                                                                                                    |

All five `build_report` members are required; only the four build-fact members may be null when unavailable. `declared_class` is an operator declaration; `unspecified` does not mean official. These are declarations, not an installed-artifact digest. Use existing immutable owning-product build facts when present; do not add operator/tag/time inputs to Layer 1 builds. An external tag belongs in verified release evidence, not a new compiled publisher input.

The caller supplies expected host, nonce, environment, and trust policy out of band. No payload-selected verifier URL or public key can establish trust. A verifier MUST correlate the nonce to one outstanding collection operation, expire it after 60 seconds, and consume it once. A valid signature does not establish authorization before enrollment checks.

### PA-ASSESSMENT: verifier payload

| Required field                    | Type and rule                                                                                        |
| --------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `schema`                          | `ts-publisher-assessment-v1`                                                                         |
| `issuer`                          | Consumer-configured verifier HTTPS identifier                                                        |
| `assessment_id`, `enrollment_id`  | Verifier-assigned canonical UUIDs                                                                    |
| `binding`, `binding_id`           | Full endorsed descriptor and its digest                                                              |
| `evidence_digest`                 | SHA-256 of collected evidence compact JWS                                                            |
| `observed_from`, `observed_until` | Ordered observation interval covering all successful checks                                          |
| `issued_at`, `expires_at`         | Ordered times subject to section 8                                                                   |
| `policy`                          | Object: `id` string, `version` positive integer, `digest` of the verifier's retained policy document |
| `result`                          | `satisfied`, `not_satisfied`, or `indeterminate` under that policy                                   |
| `checks`                          | Object described immediately below                                                                   |
| `build`                           | Build appraisal object described immediately below                                                   |
| `reason_codes`                    | Unique sorted array of codes from section 10, empty allowed                                          |
| `extensions`                      | Empty object                                                                                         |

`checks` contains exactly `host_authorization`, `key_possession`, `host_service_binding`, `active_version`, and `configuration`. Each is an object with `outcome` (`pass`, `fail`, `unavailable`, `not_assessed`), `method` (nonempty profile identifier string or null), `observed_at` (integer or null), and `evidence_digest` (digest or null). A pass requires a non-null method, non-null observation time, and retained evidence digest. `not_assessed` requires all three other fields null; a fail/unavailable outcome names the attempted method and may have null observation/evidence when collection did not complete. An unavailable/not-assessed outcome MUST NOT satisfy a required check.

`build` contains exactly `provenance` (`official`, `custom`, `unknown`), `assurance` (`artifact_bound`, `fingerprint_only`, `declared`), `method` (string), `artifact_sha256` (digest or null), `provider_files_hash` (128 lowercase hex or null), `release` (object or null), and `coverage` (literal `selected_executable_only` or `declaration_only`). A non-null `release` contains `repository`, `tag`, `source_commit`, `asset_name`, `artifact_sha256`, and `verification_evidence_digest`; it names authenticated Layer 1 evidence, not runtime strings. `repository` is a nonempty `owner/name` string matched to the configured Layer 1 source; `tag` is a nonempty tag string validated under that release policy; `source_commit` is 40 lowercase hex characters; `asset_name` is one basename without slash, backslash, or dot-segment components; both digest members are SHA-256 digest objects. None of these members may be null inside a non-null release.

`provenance: official` requires `assurance: artifact_bound`, a non-null executable digest, and a verified eligible release with the same digest. `custom` may be independently observed or declared; the assurance field is mandatory and MUST be displayed alongside it. A publisher saying it customized code does not prove those bytes are installed. A custom classification requires the signed custom declaration or a retained custom-build appraisal; absence from the verifier's known release list alone means unknown, not custom. `fingerprint_only` MUST have null executable digest/release and MUST NOT be rendered as official. It records a provider fingerprint comparison, not a stronger claim. An artifact-bound custom build has a non-null independently bound digest and null official release. All other declared builds have null artifact digest/release and `coverage: declaration_only`.

Two built-in appraisal contracts are defined:

| Policy ID                       | Version 1 acceptance condition                                                                                                                                                                                                                         |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `publisher-declaration-v1`      | Authorized enrollment; fresh evidence; exact authorized key/binding; valid configuration identity projection; explicit build declaration. Host-service and installed-artifact checks may be not assessed; the result must remain visibly self-declared |
| `fastly-official-deployment-v1` | All five checks pass; qualified Fastly active production service/version and host binding; artifact-bound official eligible executable; identity projection independently corroborated as scoped in section 7                                          |

A verifier may add its own named policy, with retained policy digest, explicit check requirements, and consumer opt-in. A custom policy is not interchangeable with either built-in policy. No policy here certifies consent behavior, module safety, or all configuration. Additional profiles require their own reviewed specification, qualification, and negative tests before consumers enable them.

### PA-STATUS: signed withdrawal and key status

A verifier publishes a status object for a requested assessment. Required fields are `schema: ts-publisher-status-v1`, `issuer`, `enrollment_id`, `assessment_id`, `binding_id`, `publisher_key_kid`, `sequence`, `assessment_status`, `enrollment_status`, `publisher_key_status`, `reason_codes`, `issued_at`, `expires_at`, and empty `extensions`.

Statuses are `current`/`withdrawn` for the assessment, `active`/`withdrawn` for enrollment, and `active`/`compromised`/`retired` for the publisher key. `sequence` is an increasing positive integer per enrollment, durably persisted by the verifier. Withdrawal and compromise are terminal for their identifier. Re-enrollment uses new identifiers; an expired assessment is not extended by a status response. A current status means no withdrawal is recorded, not that a fresh provider observation occurred. Before each current-status issuance or reuse, recheck the operator authorization record, enrollment, publisher key, assessment, and verifier signing-key validity at the current time. Its expiry is no later than the earliest of those authorization/assessment expiries and issued_at+60; a still-valid signature cannot extend an expired operator delegation. On operator-record expiry/withdrawal, stop current-status issuance and positive assessment publication, mark affected enrollment authorization withdrawn, and advance durable generation/sequence. Time passing across an expiry is a denial condition even if no event has yet updated the generation.

Status MUST satisfy `issued_at < expires_at <= issued_at + 60`. Consumers persist the highest seen sequence per verifier/enrollment and reject lower sequences. A fresh consumer may still accept an older signed status within its bounded lifetime; no instantaneous revocation is promised. Compromised-key status affects all assessments for that key in that enrollment; known cross-enrollment reuse triggers investigation and withdrawal of each affected enrollment. Status history and assessment expiry remain independent.

## 4. Enrollment and independent trust bootstrap

### PA-ENROLLMENT: authorize before endorsing

The reference verifier accepts enrollment only through an authenticated operator workflow, not an anonymous public URL submission that automatically grants trust. Required private enrollment input is the full authorized tuple from PA-IDENTITY, publisher contact/recovery authorization, public key and validity, and selected policy. Provider observations additionally require an existing Fastly service ID, intended host mapping, and narrowly scoped provider credentials. The declaration profile requires no provider credentials or release acquisition and records provider checks as not assessed. A Fastly service ID remains part of the operator-declared enrolled tuple; only explicit Axum development fixtures may use null service identity as permitted by PA-BINDING. No signing private key is submitted to a verifier.

The authenticated enrollment account MUST have a verifier-maintained authorization record permitting the exact `operator_id` it submits. That record binds account identity, the exact canonical operator identifier, permitted publisher/deployment scope, authorization method, approver or delegating authority, evidence digest, validity interval, and withdrawal state. Version 1 uses a preapproved account association or documented delegation checked through an independently trusted onboarding channel; a self-submitted URL, publisher DNS proof, key possession, or an authenticated login alone does not establish that association. Do not fetch an arbitrary operator URL to manufacture proof. If the record is absent, expired, outside scope, or withdrawn, fail enrollment with `operator_unauthorized` before any positive assessment. Retain and recheck the authorization record on renewal, recovery, and assessment publication. A legitimate operator-identifier change requires new authorization, not URL normalization or an old publisher-key signature.

The verifier MUST also establish DNS control for the exact host and proof of possession of the exact key, rather than treating either as sufficient alone. A parent publisher domain is descriptive; it does not authorize arbitrary subdomains. Every host is enrolled explicitly. Neither a wildcard nor a string suffix check grants enrollment.

The verifier creates a single-use enrollment challenge: a closed JSON object with `schema: ts-publisher-enrollment-challenge-v1`, `issuer`, `host`, `publisher_id`, `operator_id`, `deployment_id`, `provider`, `service_id`, `environment`, `publisher_key_kid`, random 32-byte base64url `nonce`, and `expires_at` at most ten minutes after creation. The operator publishes `ts-publisher-v1=<base64url(SHA-256(JCS(challenge)))>` as a TXT value at `_ts-attestation.<host>`. The verifier retains the exact challenge and hash, checks that value before its deadline using a documented trusted resolver, and does not accept unrelated existing tokens. A TXT record split into DNS character strings is joined within that one record; separate records are never concatenated.

The verifier then fetches fresh HTTPS publisher evidence and verifies it with the proposed key, matches the enrolled tuple, and corroborates provider host/service ownership and production version where the chosen policy requires them. Only after these steps may it issue an enrollment ID and assessment. DNS proves technical control under the resolver trust model, not legal ownership or that all requests use Trusted Server. DNSSEC validation status is retained; absence does not become a false DNSSEC claim. The operator may remove the enrollment TXT value after success. An explicit loopback development fixture substitutes a manually pinned host/key authorization for DNS and uses a separate test trust configuration; its environment is development, it cannot produce the production profile, and its objects MUST NOT be accepted by production trust configuration.

Enrollment expires after at most 30 days and requires new DNS and key-possession challenges to renew. A new key requires new proof and enrollment authorization; possession of an old key alone cannot authorize recovery. Production/staging, host, publisher/operator, deployment ID, or service changes require explicit enrollment review. A new service version within the enrolled service requires a new assessment/binding, not automatically new DNS proof. Suspected DNS/host ownership changes suspend renewal until reauthorization.

### PA-TRUST: discovery is not authority

Consumer trust configuration supplies verifier ID, pinned public keys and validity, permitted policies/methods, acceptable releases, expected host/environment, observation age, and status requirements. An operator's advertised verifier list is only a hint. Unknown issuers, self-provided trust roots, unapproved policies, and additional untrusted endorsements cannot turn rejection into acceptance.

The first consumer policy accepts one independently configured verifier; multiple-verifier support means evaluating each signed assessment independently. A consumer may require a threshold of distinct configured issuers whose accepted assessments match the same binding ID. Repeated keys/objects from one issuer count once. This is not evidence that organizations are operationally independent. There is no protocol-wide majority vote or mandatory registry.

Trust-anchor rotation occurs through an authenticated consumer configuration update with an overlap period. A new root published solely in an untrusted assessment is not accepted. Verifiers retain historical public keys for audit verification. Compromise of a verifier key requires consumer trust withdrawal and reissuance through trusted recovery, not a status response signed only by that compromised key.

## 5. Publisher HTTP surface and runtime behavior

### PA-HTTP: fixed, small endpoint

| Request                                                                  | Response                                               |
| ------------------------------------------------------------------------ | ------------------------------------------------------ |
| `GET /_ts/attestation.json?nonce=<challenge>`                            | `200 application/jose` with evidence compact JWS       |
| `GET /_ts/attestation/keys.json`                                         | `200 application/json` with versioned public discovery |
| Missing/invalid nonce, duplicate nonce, unknown query member             | `400` with bounded generic error code                  |
| Disabled attestation or unsupported method on either exact reserved path | `404`; no origin fallback and no signing               |
| Enabled but unavailable key/identity/clock or snapshot mismatch          | `503`; no unsigned success payload                     |
| Local configured rate limit exceeded                                     | `429`; no signed payload                               |

Only GET is supported. HEAD and OPTIONS return 404; browser clients need no custom request headers. Only the two exact paths in the table are reserved; neighboring paths retain existing routing and may reach the publisher fallback. No whole-prefix takeover or configurable endpoint is introduced in version 1. The evidence endpoint requires exactly one `nonce` parameter with the canonical encoding above. The keys endpoint accepts no query parameters. Request bodies are rejected with 400. Nonce-free evidence is not a conforming freshness check.

All responses on these paths, including errors and public discovery, MUST have `Cache-Control: no-store, private`, `Surrogate-Control: no-store`, and `X-Content-Type-Options: nosniff`. No ETag, Last-Modified, Set-Cookie, authentication redirect, or shared-cache lifetime is emitted. Conditional requests do not produce 304. Responses are independent of reader cookies and contain no user IDs or client IP. Wildcard CORS is permitted for these public GET resources with no credentials; credentialed CORS is prohibited. A human-readable status page is not required. Any later UI must explain declarations versus verifier results and cannot present its own rendering as verification.

Discovery is `{ "schema": "ts-publisher-key-discovery-v1", "host": <host>, "keys": [...] }`. Each of at most four entries contains only `kid`, public `jwk`, `not_before`, and `not_after`. Include active and overlap keys; no private key, secret reference, provider token, or mutable URL-as-key-authority. Discovery is untrusted until matched to enrollment/assessment, and cannot replace retained historical key records. It contains no verifier trust roots.

### PA-ROUTING: trusted host selection before evidence

Use one host-normalization profile at enrollment, operator input, trusted ingress parsing, DNS lookup, TLS/SNI, and host-index lookup: IDNA2008 with Unicode 18.0 UTS #46 revision 36 ToASCII, nontransitional processing, `UseSTD3ASCIIRules=true`, `CheckHyphens=true`, `CheckBidi=true`, `CheckJoiners=true`, and DNS-length verification. Remove one final ASCII dot before conversion, lowercase the ASCII result, and require valid labels of 1–63 octets and a host of at most 253 ASCII octets. A second terminal dot or empty label fails. A-label input must pass the same processing and round-trip validation; no IDNA2003 or transitional fallback is allowed. Every host in a signed payload, challenge, stored enrollment, or lookup path MUST already be that canonical ASCII form; reject noncanonical wire spelling rather than rewriting signed claims. For example, Unicode operator input `faß.example` becomes `xn--fa-hia.example`, never `fass.example`. The TLS connection and DNS enrollment token must refer to the resulting same name. Updating Unicode/profile rules requires reviewed compatibility vectors and a protocol/profile version decision, not an ambient library upgrade. Reject userinfo, percent escapes in authority, wildcard names, empty labels, IP literals, invalid ports, and ambiguous/conflicting authority sources. Production uses HTTPS port 443; another port is not silently treated as the same origin. A plain Host value and URI authority, if both present, must agree after normalization. Forwarded headers supplied by clients never select signed host identity. The original trusted ingress host is retained before any module can rewrite request headers.

Resolve the intended host's settings through the existing trusted configuration selector, then require configured attestation host and publisher authorization to match that ingress host. Unknown/mismatched hosts MUST NOT borrow another host's settings or sign a statement; return 421 when host selection is invalid. Multi-host deployments require explicit per-host bindings. This spec does not depend on an unmerged configuration-by-host implementation; the first release may require one enrolled host per configured deployment.

Dispatch evidence after ingress sanitization and host/snapshot resolution, but before Edge Cookie creation, consent processing, device classification, bot challenges, request filters, publisher backend calls, and auctions. An explicit attestation exception to generic authentication is needed so a verifier can reach the public endpoint; it applies only to these exact read-only paths and is covered by route tests. Normal routes retain existing authentication and filtering.

This needs an explicit startup seam as well as a route exception. The baseline constructs auction/integration state and resolves ordinary application secrets before normal dispatch. Implementation must acquire the verified nonsecret configuration snapshot and attestation identity independently of those unrelated initialization steps, then dispatch the evidence request without creating the auction application. When ordinary application construction succeeds, both paths must use the same loader normalization and snapshot provenance; retained sandboxes must not mix snapshots. An unrelated bidder/secret initialization failure is not an attestation-key failure and must not force the proof path through that failing component. Evidence describes the attestation request's actual snapshot, not a guarantee that ordinary site traffic is healthy. Invalid own host/identity configuration still fails safely.

Response finalization MUST enforce attestation headers last, including when generic `response_headers` configuration or middleware tries to loosen privacy. A diagnostic endpoint is not a path to execute arbitrary modules or sign arbitrary bytes. Provider identity and trusted host come from adapter-owned data, not custom integration headers.

### PA-CONFIG: proposed operator configuration

```toml
[attestation]
enabled = true
host = "news.example.com"
publisher_id = "11111111-1111-4111-8111-111111111111"
operator_id = "https://operator.example.com/publisher-attestation"
deployment_id = "22222222-2222-4222-8222-222222222222"
environment = "production"
active_key = "publisher-evidence-2026-11"
discovery_keys = ["publisher-evidence-2026-11"]
build_class = "unspecified"
```

This is a proposed configuration contract, not a claim that these fields already load. Default is absent/disabled; `build_class` defaults to `unspecified` and accepts only `unspecified` or `custom`. Unknown keys are rejected. Host, IDs, environment, and the selected key metadata must validate when enabled. The selected name is an opaque local key reference, never a private key. `discovery_keys` defaults to a list containing `active_key`; an explicit list must contain 1–4 unique valid local key names and include `active_key`. The discovery endpoint performs bounded metadata reads of those exact names, ordered by thumbprint in its public response; it never enumerates a provider store. Missing, malformed, duplicate-thumbprint, or inconsistent metadata in that list returns discovery 503 rather than publishing a partial set. Evidence signing reads only the active key metadata/secret and may still succeed when an overlap entry is unavailable. Disabled mode ignores the list without store reads. Key metadata and private key bytes live in dedicated runtime stores described next. It MUST NOT permit overriding observed provider/service/version, choosing a signature algorithm, choosing arbitrary signed claims, or supplying a purported official checksum.

Disabled attestation reads no attestation stores. Invalid structural configuration fails ordinary configuration validation with redacted errors; a temporarily missing/malformed key at runtime fails only the attestation endpoint. The config loader MUST NOT eagerly resolve the endpoint's optional private key as a required site-wide secret. Endpoint admission validates the effective metadata and secret together, so a partially propagated rotation returns 503 rather than mislabelling the signing key.

## 6. Key custody, provisioning, and recovery

### PA-KEYS: dedicated publisher keys

Publisher evidence keys are distinct from Layer 1 release signatures, existing request-signing keys, verifier assessment keys, TLS keys, and future auction keys. No modification to today's request-signing protocol or rotation endpoints is authorized by this spec.

Provision a publisher-generated Ed25519 key through an authenticated operator tool or existing provider secret mechanism. The private seed is exactly 32 bytes encoded as canonical standard base64 in a dedicated secret-store entry. Metadata contains the public JWK, thumbprint, validity interval, and secret entry reference. The runtime derives the public key from the loaded seed and compares it with metadata before signing. A metadata/key mismatch is 503. Secrets never enter source, build inputs, artifact metadata, shared CI caches, app-config JSON, generated documentation, or verifier enrollment. A provisioning tool MUST NOT print or return private key bytes in ordinary output.

Version 1 preserves the existing portable store inventories and defaults unchanged. Store nonsecret metadata in the existing logical config store `trusted_server_config` at `ts-publisher-key-<local-name>`, and private seeds in the existing logical secret store `trusted_server_secrets` at `ts-publisher-<local-name>`. A local name is 1–48 lowercase ASCII letters, digits, or hyphens, starts with a letter, and is unique within that deployment. The prefixed secret name therefore remains at most 61 characters. Metadata has the closed schema `schema: ts-publisher-key-metadata-v1`, `kid`, `jwk`, `not_before`, `not_after`, and `secret_name` (exactly the prefixed secret name). Verify that metadata and the derived public key agree. The ordinary app-config secret resolver MUST NOT eagerly resolve these endpoint-only entries.

These prefixes are reserved for attestation provisioning. Before writing, the operator tool checks existing entries and ordinary app-config secret references, including each adapter's normalized key/environment mapping, and refuses collisions; it must not overwrite an unrelated value. Rotation replaces only entries explicitly owned by that deployment/key. Physical IDs and resource-link mappings remain operator deployment context, not public claims or new build inputs. Fastly reads by the existing logical links; Axum uses its existing config file/flat environment secret mapping. Cloudflare/Spin flat secret namespaces remain unchanged and production attestation there remains unsupported until separately qualified.

Do not add a second logical secret store, a second config store, or unconditional attestation resource links to `edgezero.toml`/adapter store metadata. Pinned EdgeZero v0.0.11 treats Axum, Cloudflare, and Spin as single-secret-store adapters; shared inventory expansion would reject strict validation, config push, and provision before feature enablement is considered. Existing-store namespaces avoid that regression and impose no additional store quota or empty-store linkage requirement on disabled deployments. Fastly requires an initial config entry before association under its documented provisioning path; existing application-config provisioning already supplies it. Enabling attestation adds validated entries only after existing link/access/quota checks succeed. No empty new store, placeholder private key, or sentinel is needed. Disabled mode performs zero attestation-entry reads/writes and requires no new resources.

A future operator may isolate keys in additional physical stores only through an adapter-scoped, separately reviewed capability/provisioning change; this spec does not authorize changing the shared inventory to do so. Store sharing does not mean key reuse: cryptographic purposes, metadata, prefixes, and enrollment are separate, but application code with access to the shared secret store can read those keys. Verifier credentials remain read-only; absence of physical isolation is explicit in the residual trust model.

Sharing the public release executable does not imply sharing keys between publishers. Existing store linkage is observed provider context, not proof of non-extractability or per-key access isolation. Verifiers receive no write permission to these stores and do not generate publisher keys. Axum fixtures use the existing environment-backed read adapter and a test-only mapping; adding portable logical names does not imply provider namespace isolation or write support on Cloudflare/Spin, whose production paths remain unsupported.

Key validity is at most 90 days. Metadata may publish a successor before activation. Operator rotation stages new metadata/secret, includes the successor in `discovery_keys`, and stages DNS authorization, changes `active_key`, then completes fresh endpoint proof and obtains an assessment for the new key/binding. Until that assessment is available, the new evidence is unendorsed and a consumer requiring an assessment rejects it; normal site traffic continues. Version 1 does not promise an uninterrupted attestation transition. Keep the old public key verifiable for at least the maximum assessment lifetime plus status/cache allowance. Never sign after key expiry. No "last key signs forever" fallback. There is no promise of an atomic distributed store/config transaction: readers observing inconsistent metadata fail the endpoint, and rotation tests must cover ordering and propagation.

The endpoint checks key validity and current time on every response even if parsed key bytes are sandbox-cached. A reusable sandbox cannot extend an expired key or assessment. Cached metadata has the same documented snapshot lifetime as its bound settings; changing a store does not retroactively update a running snapshot. Operators must plan instance recycling or bounded refresh where required by a qualified adapter.

On compromise, stop positive renewal, mark affected enrollments/keys compromised, provision a fresh key through trusted operator recovery, re-prove host control, and issue new assessments. Old-key signatures cannot authorize the replacement alone. Routine retirement does not imply historical signatures were forged; compromise and observation unavailability are separate reasons. Historical evidence remains verifiable as historical data, not current authorization.

## 7. Configuration claims and actual snapshot identity

### PA-SNAPSHOT: commit only to a defined public projection

Version 1 claims a deliberately narrow identity projection:

```json
{
  "schema": "ts-publisher-identity-v1",
  "host": "news.example.com",
  "publisher_id": "11111111-1111-4111-8111-111111111111",
  "operator_id": "https://operator.example.com/publisher-attestation",
  "deployment_id": "22222222-2222-4222-8222-222222222222",
  "environment": "production"
}
```

The object is closed. It is built from effective normalized settings after defaults and permitted overrides, alongside that request's settings snapshot. `configuration.digest` is SHA-256 of its JCS representation; descriptor identity fields MUST equal their projection counterparts. The key and provider version are already separately bound in the descriptor.

This digest says nothing about auction rules, consent policies, bidder lists, integration safety, source contents, mutable dictionaries, runtime data, resolved secret values, or the full settings document. Neither built-in policy may claim those properties. A broader policy projection is a follow-up requiring named fields, normalization, privacy review, and independent check methods; merely hashing more JSON is not approval of behavior.

Capture verified envelope identity and configuration selector privately before `BlobEnvelope::into_data()` and secret resolution discard them, and carry that provenance beside the same immutable settings snapshot. Do not publicly expose an arbitrary full-config hash: low-entropy sensitive settings can be guessed. An opaque internal snapshot ID and a public projection digest serve different purposes. A timestamp or envelope checksum is not proof of publisher authorization.

For the official-deployment policy, the verifier independently reads the enrolled service's linked configuration selector/envelope, validates the supported loader's normalization rules, reconstructs only the public projection without resolving secrets, and compares it with fresh evidence. The provider's actual selector/linkage and relevant runtime overrides must be observable or constrained by the qualified method. If hidden overrides or inaccessible selector state can change the projection, the configuration check is unavailable; do not infer it from a matching self-report alone.

Read configuration identity and active version before and after the evidence challenge. If either covered identity changes, retry the whole bounded collection once; otherwise report indeterminate. Matching before/after reads cannot exclude a change-and-restore attack or prove global edge convergence. The assessment records the collection interval and coverage. It cannot turn an observation of one POP into a claim about every POP.

Default Fastly, reusable Fastly, Axum, Cloudflare, and Spin can have different settings lifetimes. Evidence MUST describe the actual retained snapshot, not the latest config store fetched later. When fresh evidence and a retained assessment disagree on covered identity/key/version, a consumer rejects the combination and requests reassessment. It never relabels a stale binding as current.

## 8. Collection, freshness, and monitoring

### PA-COLLECTION: deterministic independent assessment

For each assessment, the verifier performs these steps under an operation ID and a total 60-second collection deadline. Provider/configuration/release steps apply only when required by the selected policy or explicitly requested as additional observations. The declaration profile skips those steps and records `not_assessed`; it still validates enrollment, evidence, actual descriptor/projection consistency, clock, and key possession. Axum loopback tests use only that explicit development path:

1. Load an active enrolled tuple, trusted publisher key, and appraisal policy; load provider credentials and authenticated release eligibility policy only when the selected collection requires them. Checking current enrollment state is a new observation; the retained original DNS proof has its own historical timestamp and 30-day authorization deadline and is not misrepresented as a new DNS lookup.
2. Read provider host/service mapping, production active version, relevant resource links/selector, and public configuration identity. Record retrieval timestamps and retained response digests privately.
3. Fetch fresh evidence with a new unpredictable nonce over authenticated HTTPS to the expected host. Verify signature, clock, enrolled key, host, environment, descriptor/projection hashes, and all tuple values.
4. Compare reported service/version with independently observed state. Never use a report's service ID as authorization to query another service.
5. Apply the qualified selected-executable binding method, if available. Verify retained official release bytes and eligibility; classify the outcome without upgrading a fingerprint match into artifact binding.
6. Repeat the version/covered-configuration observation. On a detected race, discard the collection and make at most one fresh attempt within a new bounded operation. Never combine old/new observations into one successful interval.
7. Calculate results under the named policy; sign an assessment referencing exact evidence and policy digests. Persist the object and evidence before advertising success.

No write or deployment is necessary for collection. Missing credentials, 403/429, outage, incomplete audit pages, malformed provider responses, or unavailable release evidence produce unavailable/indeterminate results, not a successful downgraded official assessment. A verifier may separately issue a clearly named declaration assessment after its own required checks; that is a different result, not continuation of the failed policy.

### PA-TIME: bounded acceptance

Evidence lifetime is at most 60 seconds. Verifier assessment issuance MUST occur within 60 seconds after `observed_until`. An assessment MUST satisfy `observed_from <= observed_until <= issued_at < expires_at <= observed_from + 900`. Its expiry is also no later than operator-authorization-record, publisher-key, enrollment, or verifier-signing-key authorization expiry. The verifier computes that minimum at commit/publication, not just when collection begins. An operator record with no independently established finite validity interval cannot authorize a positive assessment. Old observations cannot receive a new 15-minute lease by re-signing them.

Consumers require `issued_at <= now + 30`, unexpired evidence/assessment/status, and `observed_until <= issued_at`. Clock skew allowance applies to future issuance and not-before checks; it never extends an expiry. A consumer may require a shorter observation age or lifetime. All interval and cross-object relationships are validated even when signatures are correct. Missing/unreliable runtime time is endpoint 503; verifier clock failure stops issuance; consumer clock failure stops current-assurance acceptance.

The default verifier renews positive assessments every five minutes, subject to provider rate limits and a configured queue. It polls version/host/covered-configuration state at least every 60 seconds while offering the monitored official-deployment policy; audit events may trigger earlier rechecks but are not assumed complete. If that monitoring interval cannot be maintained, stop representing the service as currently monitored and stop new positive official assessments until a complete collection succeeds. Previously issued assessments retain their explicitly bounded validity unless withdrawn.

Changes to active version, authorized host mapping, covered projection, linked key context, or enrolled authorization trigger reassessment and withdrawal of known stale assessments. A confirmed key compromise also changes key status. API unavailability alone is not a declaration that the publisher key was compromised. Monitoring credentials being revoked prevents further positive collection.

The default official consumer policy requires a signed status no older than 60 seconds, in addition to an unexpired assessment. Status retrieval failure denies current official assurance; an offline consumer may verify historical signatures but MUST label freshness/withdrawal unknown. Worst-case detection and propagation include polling/event delay and the remaining accepted status lifetime; no immediate withdrawal guarantee is claimed. If status is not required by a separately configured policy, exposure may last until assessment expiry.

## 9. Consumer verification and networking

### PA-CONSUMER: acceptance algorithm

A current publisher check MUST:

1. Take expected host/environment, trusted issuers/keys, permitted policies/methods, and freshness/status settings from independent configuration.
2. Generate a fresh nonce and fetch publisher evidence. Strictly validate JWS and schema with an enrolled/endorsed publisher key; unsigned discovery may supply candidate bytes only.
3. Verify each candidate assessment with an allowed verifier key and exact message type, then validate policy, result, intervals, enrollment status, build assurance, release eligibility, and required check outcomes.
4. Require full descriptor and binding-ID equality between fresh evidence and assessment, including host, deployment ID, service/version/environment, projection, key material, and validity.
5. Fetch signed status from the independently configured verifier base; match issuer, enrollment, assessment, binding, and publisher key; enforce sequence, timing, and non-withdrawn/non-compromised state.
6. Apply the consumer's distinct-issuer threshold to matching accepted assessments. Return structured reasons and the accepted scope. No acceptable assessment means unendorsed, not a registry-verified success.

The assessment-only operation is explicitly historical/bounded-observation verification. Without fresh publisher evidence it cannot claim that the host responded now. Conversely, a fresh publisher response cannot refresh an old assessment. Consumers MUST NOT accept `result: satisfied` without evaluating the policy and concrete required fields.

### PA-NETWORK: constrain all external fetches

Publisher checks require TLS certificate and hostname validation with SNI and authority set to the expected enrolled host. No redirects, downgrade to HTTP, userinfo, URL fragments, caller-selected port, or alternate host supplied by a response. Fixed endpoint paths prevent following a page's attacker-selected verify link. Development HTTP is allowed only for explicitly configured loopback fixtures under the declaration profile, never production.

Reference-verifier enrollment and publisher fetching MUST defend against SSRF: reject loopback, private, link-local, multicast, unspecified, reserved/nonpublic, and provider metadata destinations; resolve and validate all connection candidates, pin a validated address for the connection, and revalidate on every new connection. Do not merely validate a hostname before a client re-resolves it. Resolver trust and allowed enterprise exceptions require explicit operator configuration and cannot produce the public production profile. Proxy environment variables are disabled by default in the reference collector; explicitly configured egress proxies are trusted components recorded in its policy.

Provider API traffic uses an independently configured fixed Fastly authority, not report-selected URLs. Provider credentials are never forwarded to publisher, release, discovery, redirect, or status endpoints. Release evidence acquisition follows Layer 1's trusted repository/workflow/publication policy. Verifier status URLs are constructed from configured verifier base and validated UUID, not fetched from an arbitrary assessment member.

Each network call has a five-second deadline; transient idempotent reads may retry once within the collection deadline, respecting bounded Retry-After. HTTP error bodies are capped at 4 KiB and redacted. Evidence is capped at 16 KiB, discovery at 16 KiB, assessment at 32 KiB, and status at 8 KiB, measured after any HTTP decoding. Do not accept unlimited decompression or chunked growth. Provider/package limits belong to the qualification spec. Endpoint rate limiting is an operator/platform deployment responsibility; tests cover bounded work per request without assuming a global counter across WASM instances.

### PA-REFERENCE: tools and registry boundary

The delivery includes a host-target reference verifier/collector and a pure verification library, with machine-readable results and offline historical verification. Planning must select its concrete crate/CLI placement without making runtime core depend on host HTTP, GitHub CLI subprocesses, or credentials. Prefer a small host-only component exposed through the existing operator CLI when dependency/target separation permits. Names of future commands are not frozen by this spec.

The reference collector consumes operator-supplied enrollment and credentials, retains evidence, and produces signed assessments/status. Its local state writes are atomic and restrictive. A production operator owns scheduling, authenticated administration, durable sequence allocation, backup, serving, abuse controls, and signer isolation. Allocate status sequences and persist withdrawal state atomically before signing or publishing status, following PA-RECOVERY below. A failed publication must not undo recorded withdrawal. After restart, never reset a sequence; if durable monotonic state cannot be recovered, stop current issuance and use authenticated recovery with a new enrollment identifier rather than risk rollback. A deployment of this reference component is not automatically an endorsed public registry. A full multi-tenant hosted registry and UI require a separate operational design.

### PA-RECOVERY: fence stale collectors and restored state

Each enrollment has an authoritative authorization generation in addition to its public status sequence. Changing operator authorization, enrollment/key status, accepted binding, or withdrawal increments that generation in a serialized durable transaction. A collector captures the generation when it starts and, immediately before committing a positive assessment, rechecks the generation, current authorization/key/enrollment validity, expiry deadlines, and policy. Commit uses compare-and-swap against that generation; an intervening withdrawal/change discards the candidate. Signing/outbox publication is likewise gated on the current generation, so a pre-withdrawal job cannot publish a new positive assessment or `current` status afterward. Already delivered objects retain only the bounded exposure explicitly described in PA-TIME.

Local persistence alone does not prove a restored backup is current. The production reference deployment MUST have a trusted recovery checkpoint outside the rollback domain of its ordinary backups. That checkpoint binds enrollment ID, highest allocated sequence, authorization generation, a digest of the authoritative authorization/withdrawal state, and a current signer fencing epoch. It is updated monotonically and acknowledged before any affected object is signed/released. A checkpoint digest covers authorization state rather than secrets or raw credential responses. A database transaction/outbox and checkpoint protocol must treat a partially acknowledged transition as pending and fail closed; it cannot sign using a guessed earlier state. A deployment unable to supply and qualify this checkpoint mechanism cannot offer the monitored production profile.

Every restart or restore verifies local state against the trusted checkpoint and obtains the current exclusive signer epoch. A lower sequence/generation, inconsistent state digest, unknown checkpoint, stale epoch, or checkpoint outage fences signing, positive collection commits, and current-status publication with `state_recovery_required`. A restored process does not automatically overwrite the checkpoint or reset sequence state. Authorized recovery restores the latest corroborated state or withdraws the affected enrollment through trusted administration and re-enrolls with a new identifier; an old backup/key cannot authorize its own clean replacement. The checkpoint/signing owner must ensure a fenced old worker cannot continue publishing through another outbox or signer instance. Retained historical verification remains available without a current-assurance claim.

Qualification must exercise restore of an internally valid backup that predates a withdrawal, crashes on both sides of checkpoint acknowledgment, duplicate/fenced signer instances, and a delayed collector completing after compromise. This contract specifies the required invariant; the concrete transaction/checkpoint backend and its failure proof belong to P3's bounded operational plan. No distributed-store atomicity or safe backup restoration is assumed merely because local files use atomic rename.

### PA-PUBLICATION: verifier read surfaces

A minimal verifier publication service exposes these read-only paths under an independently configured HTTPS base:

| GET path                                                           | Response                                                                                                                               |
| ------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| `/v1/hosts/{canonical-host}/assessments?environment={environment}` | JSON with exactly `schema: ts-publisher-assessment-list-v1`, `host`, `environment`, and `assessments` (at most 16 compact JWS strings) |
| `/v1/assessments/{assessment_id}`                                  | Exact assessment compact JWS, `application/jose`                                                                                       |
| `/v1/assessments/{assessment_id}/status`                           | Signed status compact JWS, `application/jose`                                                                                          |

The host lookup makes assessment discovery possible without putting changing assessment IDs in publisher evidence. Its list is only a transport hint; each signed object is independently checked against the fresh binding and configured issuer. The list includes the newest unexpired assessment for each current binding/policy, suppresses withdrawn entries, and returns 409 if more than 16 matching entries require an explicit policy/assessment selection. Each compact JWS list item is explicitly permitted up to 32,768 ASCII bytes, overriding the ordinary claim-string limit; the entire decoded response including JSON framing is capped at 524,288 bytes. Return 409 for a list that exceeds either the entry-count or total-byte bound rather than silently omitting applicable assessments. Unknown IDs/hosts return 404, unavailable status returns 503, and no public route exposes admin operations. Public responses use `no-store`; no redirect or unsigned success status is accepted. Consumers may instead receive explicit assessment files/IDs from independent input. The reference consumer's provider abstraction supports lookup, assessment, and status retrieval; it cannot assume a publisher-advertised endpoint is trusted. Enrollment, withdrawal, trust updates, and signer operations require authenticated administration and auditable authorization.

## 10. Failure model, diagnostics, and privacy

### PA-FAILURE: distinguish rejection from uncertainty

| Reason code                  | Meaning                                                                            | Default effect                              |
| ---------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------- |
| `malformed`                  | Invalid syntax, schema, limits, or inconsistent hashes                             | Reject                                      |
| `signature_invalid`          | Wrong key, message type, or signature                                              | Reject                                      |
| `operator_unauthorized`      | Claimed operator identifier lacks valid independent account association/delegation | Reject enrollment/issuance                  |
| `state_recovery_required`    | Durable authorization history or fencing cannot be established                     | Stop signing/current status                 |
| `host_mismatch`              | Expected host or enrolled authorization differs                                    | Reject                                      |
| `environment_mismatch`       | Wrong staging/production or unavailable identity                                   | Reject production                           |
| `key_unauthorized`           | Unknown/not-yet-valid/expired/retired key                                          | Reject current acceptance                   |
| `key_compromised`            | Confirmed compromise recorded by trusted authority                                 | Reject; rotate/re-enroll                    |
| `nonce_mismatch`             | Missing, stale, repeated, or wrong challenge                                       | Reject freshness                            |
| `stale`                      | Evidence, observations, assessment, enrollment, or status too old                  | Reject current acceptance                   |
| `binding_mismatch`           | Version/key/projection/deployment tuple differs                                    | Reject combination; reassess                |
| `release_ineligible`         | Authentic release not accepted under configured policy                             | Reject official profile                     |
| `artifact_mismatch`          | Qualified observation binds a different executable                                 | Fail official check                         |
| `artifact_binding_unproven`  | Only fingerprint/declaration or unqualified method available                       | Indeterminate official check                |
| `configuration_unobservable` | Effective covered projection cannot be corroborated                                | Indeterminate official check                |
| `provider_unavailable`       | Outage, timeout, permission, incomplete state                                      | Indeterminate; stop positive renewal        |
| `observation_race`           | Covered state changed during collection                                            | Retry bounded operation; then indeterminate |
| `withdrawn`                  | Assessment or enrollment withdrawn                                                 | Reject current acceptance                   |
| `status_unavailable`         | Required status cannot be validated                                                | Reject current official assurance           |
| `policy_untrusted`           | Unknown issuer/policy/method or threshold not met                                  | Reject endorsement                          |
| `clock_unavailable`          | Reliable current time unavailable                                                  | Stop current assurance                      |

No API response should reduce these distinctions to a universal `trusted: true`. Public verdicts include expected host, issuer/policy, observation interval, expiry, build class/assurance, coverage, and reason codes. A declaration can satisfy the declaration profile without satisfying official-deployment policy. A custom build is not intrinsically a failure; it fails a policy that requires official bytes.

Attestation failure does not shut down normal publisher traffic by default. It removes the attestation claim or returns an endpoint error. Any site-wide blocking policy belongs to explicit operator configuration outside this feature. Ordinary request-signing behavior remains unchanged.

### PA-PRIVACY: minimize disclosures and retained evidence

Public objects disclose host, IDs, key, provider service/version, public identity projection, build declarations, appraisal scope/times, and potentially official release identity. Operators explicitly opt in to this disclosure. No browser identities, IPs, cookies, consent strings, bidder credentials, backend URLs, full configuration, private keys, or provider tokens appear in public evidence.

Log operation IDs, outcome codes, durations, and shortened safe object IDs through `log`; do not log nonce query strings, bodies, raw config, secret references, provider authorization headers, or sensitive provider error payloads. Public errors use generic codes; privileged diagnostics are separately access controlled. Do not publish arbitrary low-entropy private digests.

The reference verifier retains exact public signed objects, sanitized observation records, policy/release evidence identities, and enrollment/recovery audit events for 90 days by default in restricted storage. Public historical objects may be archived longer under a stated operator policy. Raw credential-bearing responses and signing secrets are excluded from evidence archives; retention and credential backup have separate access controls. Restrict state directories and files to the owning account. Disable unrelated third-party analytics on a reference verification page, if one is added later.

Fastly read-only credentials may still expose account-wide information: service restrictions on global read scopes do not imply isolation of non-service endpoints. Before enrollment, document actual endpoint permissions, resource exposure, rotation/expiry, and separation from unrelated account assets. Prefer a dedicated deployment account where required by the observed permission model. No verifier requires provider write/deploy rights.

## 11. Repository integration and compatibility

### PA-INTEGRATION: fit existing seams

| Existing surface                                             | Proposed work and constraint                                                                                                       |
| ------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/config.rs` and `settings.rs` | Typed opt-in attestation settings, strict validation, narrow projection                                                            |
| `config_payload.rs` and `settings_data.rs`                   | Retain provenance beside the actual settings snapshot before secret resolution; do not change existing secret semantics            |
| `platform/` runtime services                                 | Adapter-owned identity and dedicated store access; no publisher-controlled identity override                                       |
| `trusted-server-adapter-fastly/src/app.rs` and `main.rs`     | Exact early route, request-local identity/snapshot, default and reusable admission semantics                                       |
| `trusted-server-adapter-axum/`                               | Local declaration fixtures; no fake production service identity                                                                    |
| Cloudflare and Spin adapters                                 | Preserve existing routes/builds; unsupported production attestation remains explicit; qualify separately before adding claims      |
| `request_signing/`                                           | Reuse reviewed crypto primitives only where useful; retain separate keys, entry namespaces, types, and endpoints                   |
| Host CLI/reference verifier                                  | Enrollment input, collection, offline verification, public assessment/status generation; target-isolated dependencies              |
| Layer 1 release evidence                                     | Authenticate retained raw executable bytes and eligibility; no runtime GitHub verification or new publisher-specific release build |
| JS bundles and publisher HTML                                | No new JS injection, user flow, or auction modification                                                                            |

Core protocol types and validation should have narrow modules and strong domain types for host, deployment ID, binding ID, key thumbprint, and times. Use `error-stack` and existing runtime store/HTTP abstractions. Runtime clock access must use the repository's WASM-safe clock conventions. No dependency is selected solely by this design; verify support across targeted builds before implementation.

Do not depend on the fork's full module/source/filter stack to ship this feature. An existing or merged module endpoint seam may host it if it can enforce the routing/key/snapshot contract; otherwise add the smallest explicit adapter route. The architecture is independent of code placement. Do not add a broad generic attestation plugin framework.

Existing deployments remain disabled and preserve normal behavior. Existing `/.well-known/trusted-server.json` key discovery and `/verify-signature` retain their current purposes; they do not become publisher-attestation authorities. No automatic migration of their keys is allowed. The old fork's bespoke ES256 envelope and compiled-key schedule are not wire-compatible with this version. Operators must enroll new runtime keys; do not silently accept legacy objects under new assurance labels.

The historical January proposal is background only. Its full-config commitments and embedded final-binary checksum must not become requirements by implication. The release specification/guide on #1435 distinguishes raw executable identity, source facts, and deployment observations; consume verified inputs without assuming a later release index is shipped. A candidate/unsigned index cannot authorize an official assessment.

## 12. Delivery boundaries and qualification gates

This is the cross-component architecture contract. It is not an implementation plan or permission to activate live credentials/services. Each delivery milestone gets its own bounded implementation plan and any necessary phase-specific spec amendment; provider experiments require explicit authorization of named disposable resources.

| Milestone                            | Deliverable                                                                                           | Exit gate                                                                           |
| ------------------------------------ | ----------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| P0: provider feasibility             | Retained Fastly source/provider evidence and supported method decision                                | Companion qualification review; unresolved official binding remains closed          |
| P1: publisher evidence               | Runtime keys, strict schemas, host/snapshot binding, endpoint, pure/reference verifier, fixtures/docs | Declaration-profile end-to-end proof; visibly no independent installed-code claim   |
| P2: independent deployment appraisal | Enrollment, qualified provider/release/config collection, signed assessments/status                   | Strong profile enabled only for methods passing P0 and runtime tests                |
| P3: operational adoption             | Operator-run renewal/status service and recovery runbooks                                             | Monitoring/status SLOs, credential exposure review, rollback and incident rehearsal |

P1 does not close the full independent-verification scope of #161. If P0 cannot justify a strong binding, maintainers may ship the declaration profile and fingerprint observations, but must record the unsupported official-deployment claim and revise acceptance rather than quietly mark it achieved. A production registry dashboard is not an acceptance dependency. Future auction/bid work is not an acceptance dependency.

No calendar commitment is made here. Endpoint work, provider feasibility, and a continuously operated verifier are different effort categories. Estimates must follow the qualified method, phase scope, dependencies, and reviewer capacity; signature code size is not a measure of complete assurance-system effort.

## 13. Verification and acceptance matrix

### PA-TESTS: evidence before assurance

Tests use fictional hosts/data and generated test-only keys, never operator credentials or production signing keys. A fixture's declared expected result cannot stand in for provider qualification. CI tests must include independent verification of produced JWS bytes and malicious negative fixtures, not only round-trip tests through the same serializer.

| Test family            | Required cases                                                                                                                                                                                                                                                | Requirements                           |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------- |
| T1 encoding/crypto     | Cross-implementation vector; altered segments; wrong key/type/curve/algorithm; unsigned-header/key-URL rejection; duplicate nested members; limits; canonical thumbprints/hashes                                                                              | PA-WIRE, PA-BINDING                    |
| T2 freshness           | Exact nonce match; duplicates/invalid encoding; consumed challenge; future/expired/key-boundary times; unavailable clock; old observation re-signing                                                                                                          | PA-EVIDENCE, PA-TIME                   |
| T3 host/enrollment     | Exact host; parent/suffix/lookalike rejection; IDNA/trailing dot/port/authority conflicts; forwarded-host spoofing; DNS-token reuse/expiry; recovery without old-key authority                                                                                | PA-IDENTITY, PA-ENROLLMENT, PA-ROUTING |
| T4 routes/privacy      | No bot/auth/EC/consent/auction dependency; disabled reserved paths, unsupported methods, unchanged neighboring paths; no backend calls; conditional requests; finalizer cannot overwrite no-store; CORS; no user identifiers                                  | PA-HTTP, PA-ROUTING, PA-PRIVACY        |
| T5 key lifecycle       | No key; corrupt seed; public-key mismatch; rotation propagation orders; bounded explicit discovery list, missing overlap metadata, and no enumeration; expiry in cached sandbox; staging isolation; old-key compromise; no secrets in logs/build/config/cache | PA-CONFIG, PA-KEYS                     |
| T6 snapshots           | Actual retained versus latest config; normalization; projected versus uncovered changes; hidden overrides unavailable; multi-host mismatch; before/after races; cached sandbox                                                                                | PA-SNAPSHOT, PA-COLLECTION             |
| T7 provider/release    | Strong method positive/negative proof; fingerprint-only never official; mutated wasm/manifest; raw digest versus package digest; malformed archive; wrong service/version/staging; release signer/source/membership/ineligibility                             | PA-ASSESSMENT, companion specification |
| T8 assessments/status  | Exact full binding; unknown policy; satisfied with missing required check; expiry anchored to observations; withdrawn enrollment; compromised key; sequence rollback; status outage; multiple issuers and duplicates                                          | PA-STATUS, PA-TRUST, PA-CONSUMER       |
| T9 network abuse       | Redirect/SNI mismatch; DNS rebinding; nonpublic addresses; proxy leakage; provider-token forwarding; oversize/compression; deadlines/retries/429; no report-selected service lookup                                                                           | PA-NETWORK                             |
| T10 runtime/operations | Running Fastly evidence to independent host verifier; Axum fixture; version/config/key changes; monitoring outage; rotation/recovery; status publication failure; retained state restart                                                                      | PA-REFERENCE, PA-COLLECTION, PA-TIME   |

Additional mandatory cases for the correction contract are: valid publisher DNS/key plus an unauthorized operator identifier; namespaced-entry collision and disabled strict validation/provision/config push across all adapters without inventory expansion; absent optional attestation entries; same canonical host across enrollment/ingress/TLS/index lookup; schema-valid but semantically invalid objects; a complete assessment-list entry exceeding 256 bytes and accepted within its explicit bound; stale backup/checkpoint mismatch; and a collector racing withdrawal; an operator delegation expiring between collection and commit; assessment/status expiry capped at that earlier deadline; and a current-status request after delegation expiry before any timer/event updates state. These extend T1/T3/T5/T8/T10 and do not require auction/bid implementation.

Before publisher-evidence handoff, run changed core/adapter tests with target-matched aliases, Fastly reusable tests if that path is touched, host verifier/CLI tests, strict applicable clippy, formatting, domain lint, documentation checks, and an actual running-adapter fixture. Use native tests for protocol logic and Viceroy for Fastly endpoint behavior. Cloudflare/Spin compilation/parity checks demonstrate compatibility only, not production provider qualification. No HTML EOF harness is required unless implementation changes publisher/HTML processing. Before an implementation PR handoff, run all repository-mandated CI gates, documenting any unavailable external runtime checks without calling them passed.

Acceptance of this design requires review of the contracts and explicit unresolved-provider gates, not a claim that implementation tests have run. Acceptance of the full feature requires:

- **AC1:** Authorized publisher evidence verifies independently; malformed, forged, stale, cross-host, cross-environment, and unauthorized-key objects fail.
- **AC2:** Keys are runtime-only and separate from release/request signing; rotation/expiry/compromise behavior is demonstrated.
- **AC3:** The exact request-used identity projection and service/version/key binding survive the real runtime transport.
- **AC4:** A qualified independent method authenticates the installed selected executable before official provenance is asserted. If that gate remains unresolved, only the narrower milestone is accepted; the full independent-verification feature remains incomplete and the strong profile stays unsupported. Any reduced-scope closure requires an explicit maintainer disposition.
- **AC5:** Declaration, custom, fingerprint-only, official, unavailable, failed, and withdrawn outcomes cannot be confused by the reference consumer or documentation.
- **AC6:** Signed assessments/status, monitoring outages, sequence recovery, and multiple-verifier trust policy are exercised end to end.
- **AC7:** No auction/bid behavior changes, new user-data collection, publisher-specific official build inputs, or mandatory central administrator are introduced.

## 14. Prior work and references

The role separation follows RFC 9334: attester, verifier, relying party, reference-value provider, and endorser are roles, not a requirement for one administrator. This design borrows the operator endpoint's narrow declarations and exact-byte verification lessons while requiring independent evidence for stronger appraisal.

| Work                                                                                                                                                         | Learning and treatment                                                                                                                                                                                                                         |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [jwrosewell/trusted-server#7](https://github.com/jwrosewell/trusted-server/pull/7), inspected at `59e09b6cd9632b8198deaa92b38ed7d5b841e06e`                  | Historical closed/unmerged attestation prototype: bounded nonce, explicit limits, endpoint-local errors, independent OpenSSL harness. Reject compiled private keys and endless last-key signing; it supplies no installed-artifact measurement |
| [IABTechLab/trusted-server#1297](https://github.com/IABTechLab/trusted-server/pull/1297) and [#1353](https://github.com/IABTechLab/trusted-server/pull/1353) | Related upstream stack history; attestation was removed from #1353, which is endpoint-seam work, not an implemented assurance dependency                                                                                                       |
| [jwrosewell/trusted-server#34](https://github.com/jwrosewell/trusted-server/pull/34), inspected at `12b1f467aeab9cfb5650e7fbc2b32f654d323ebd`                | Distinguish publisher identity from backend source and outbound Host; source refactor is not required here                                                                                                                                     |
| [jwrosewell/trusted-server#47](https://github.com/jwrosewell/trusted-server/pull/47), inspected at `86f77a124d49bcfe27d00be2b397b481786340fb`                | Request-cycle parity and terminal privacy protections matter; no independent executable verification supplied                                                                                                                                  |
| [#846](https://github.com/IABTechLab/trusted-server/issues/846)                                                                                              | Secret migration background. Current baseline already resolves typed secret references; open issue status is not proof of current plaintext blob behavior                                                                                      |

Primary provider sources: [Fastly package API](https://www.fastly.com/documentation/reference/api/services/package/), [token scopes](https://www.fastly.com/documentation/reference/api/auth-tokens/user/), [edge data stores](https://www.fastly.com/documentation/guides/compute/edge-data-storage/about-edge-data-stores/), [secret-store API](https://www.fastly.com/documentation/reference/api/services/resources/secret-store/), and [audit events](https://www.fastly.com/documentation/reference/api/account/events/). These support provider semantics, not an inference of exclusive runtime key access or globally consistent observation.

Normative encoding references: RFC 7515 (JWS), RFC 8037 as updated by RFC 9864 (fully specified Ed25519 JOSE), RFC 7638 (JWK thumbprints), RFC 8725 (strict validation and message separation), and RFC 8785 (JCS). Verify interoperability and target support during implementation qualification; no external standard certifies this project's claim accuracy.

Host normalization additionally fixes Unicode 18.0, IDNA2008 (RFCs 5890–5894), and UTS #46 revision 36 with the explicit nontransitional options in PA-ROUTING. Using JWS does not claim EAT conformance; RFC 9711 EAT alignment or registered public media types may be considered later without weakening this private profile's validation.

## Review log

No provider experiment, runtime test, or implementation approval is implied by document approval.

- **Round 1 (Codex, independent security and runtime reviewers):** Security review approved with an assessment-discovery advisory. Runtime review found unconditional provider/release inputs on the declaration path, incomplete dedicated-store deployment wiring, a contradiction between exact-path reservation and unknown-path handling, and an acceptance clause that could obscure incomplete strong assurance. Added policy-conditional collection and isolated development authorization, manifest/adapter/managed store wiring and disabled compatibility (superseded by the existing-store layout in the correction pass below), exact-path routing with unchanged neighboring paths, explicit narrower-milestone disposition, and independently configured assessment lookup. Also specified metadata shape, rotation ordering, and durable status recovery.
- **Round 2 (Codex, both independent reviewers):** Security review approved. Runtime review confirmed the earlier fixes and found that overlap-key discovery lacked an inventory supported by the store interface. Added the bounded `discovery_keys` configuration, exact metadata reads without enumeration, malformed/missing overlap failure behavior, and corresponding rotation/test requirements.
- **Round 3 (Codex, both independent reviewers):** Both approved with no remaining blocking findings. Approval is for the contracts and planning readiness only; Fastly executable binding, runtime behavior, and production operation remain unqualified until their explicit gates pass.
- **Follow-up detailed review (Codex, three independent security, provider/runtime, and standards reviewers):** Found five medium contract issues: shared second-secret-store registration conflicts with pinned adapter capabilities; empty Fastly metadata-store linkage is not supported by the documented provisioning path; operator-identifier authorization lacked an explicit proof; the new JOSE profile selected deprecated `EdDSA`; and assessment-list strings conflicted with the common limit. Also recommended precise IDNA processing, machine-readable schemas/public vectors, and recovery/collector fencing. Corrected all eight areas: compatible existing-store namespaces, independent operator account/delegation authorization, RFC 9864 `Ed25519`, explicit transport bounds, pinned normalization, structural schemas and public negative fixtures, and PA-RECOVERY. Strict validation of the unchanged inventory passed with fictional configuration; no provision/config-push write was attempted.
- **Second detailed pass after corrections (Codex, fresh independent security and feasibility reviewers):** Found one additional authorization-continuity gap: a delegation could expire before an otherwise valid assessment/status. Capped assessment and current-status validity at the finite operator authorization deadline, rechecked it at publication/reuse, and added expired-authority and assessment-outlives-authority vectors. Both reviewers then approved without remaining blocking findings. Independent fixture checks used Node 24.20.0/OpenSSL and Rust 1.98.1/ed25519-dalek; these establish fixture interoperability, not provider or production qualification.
