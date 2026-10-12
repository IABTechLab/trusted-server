# Design Spec: Attestation

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#7, ready for review, answering IABTechLab/trusted-server#161. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`, `2026-01-15-attestation-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#7
**Last updated:** 2026-10-12

> **Why this spec exists.** The Tech Lab's attestation issue (#161) asks for a reviewed design before any attestation lands, and its January proposal (`2026-01-15-attestation-design.md`) names two required core decisions, configuration kept apart from the build and modular integration crates.

## 1. Problem

The Tech Lab's attestation issue (#161) asks for a reviewed design before any attestation lands, and its January proposal (`2026-01-15-attestation-design.md`) names two required core decisions, configuration kept apart from the build and modular integration crates. Nothing in core serves evidence of who operates a deployment and which build it runs, so a reader, a publisher or a regulator has only the operator's word.

## 2. Design

**What the evidence proves, and what it does not.** A signature that verifies proves that whoever made it held the operator's signing key. Under that signature the evidence states the host the request arrived for, the publisher whose settings answered, the platform service and the build that answered, the time, and the nonce when one was sent.

It does not prove four things, and the guide says so first.

1. **What code is running.** A module on a CDN cannot read its own code, so the `build` fields are the operator's own report of what it deployed.
2. **A hardware measurement.** Nothing here takes one. The trust rests on the operator's key alone.
3. **What the site serves.** The evidence says who operates the deployment serving a host, not what pages the origin returns through it.
4. **That production answered.** A staging environment signs with the same key, and the evidence says so in `platform.staging`.

```toml
[attestation]
operator = "Example Operator"
context = "example-attestation:v1"
verify_url = "https://verifier.example/verify?host=publisher.example"
```

| Address                                                        | Answer                                                                                                                     |
| -------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `[attestation] endpoint`, which defaults to `/_ts/attestation` | A page stating the claim, linking to the verifier and showing the envelope. It says "Do not take this page's word for it." |
| The same address with `.json`                                  | The envelope, as `application/json`                                                                                        |
| The default address, where the endpoint is not the default     | `301` to the endpoint, with the query kept                                                                                 |
| An endpoint's own prefix, such as `/_ex`                       | A list of the two pages. Any other address or method beneath that prefix is `404` from the deployment                      |

The envelope carries the evidence's exact bytes as signed, in base64url, the signature with its key id and algorithm, a readable copy of the evidence that no check reads, and the address of the verifier.

| Evidence field | Source                                                                                         |
| -------------- | ---------------------------------------------------------------------------------------------- |
| `v`            | The format version, `1`                                                                        |
| `operator`     | `[attestation] operator`                                                                       |
| `publisher`    | `publisher.domain` in the settings                                                             |
| `host`         | The request's `Host` header, lower case, without a port. A forwarded host header is never read |
| `platform`     | The adapter's name. On Fastly also the service id, service version, staging flag and location  |
| `build`        | The crate version, and the build time, commit and build run the build was given                |
| `nonce`        | The caller's `nonce` query parameter, only when one was sent                                   |
| `issuedAt`     | The platform's clock                                                                           |

The signed bytes are the configured context, one line feed, then the payload. The signature is ECDSA on P-256 with SHA-256, made as RFC 6979 describes, so the same evidence always carries the same signature. The context stops a signature the same key made for any other purpose from passing as evidence.

A build is given its facts and its keys as build inputs.

| Build input                                            | What it gives                                                                                                                                      | Without it                                                     |
| ------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| `TRUSTED_SERVER_ATTESTATION_KEYS`                      | A file, the key schedule, with one key on each line as the key id, the Unix second the key comes into force and the private key, separated by tabs | No key, so the endpoint answers `503` and nothing else changes |
| `TRUSTED_SERVER_COMMIT` and `TRUSTED_SERVER_BUILD_RUN` | `build.commit` and `build.run`                                                                                                                     | `unknown`                                                      |
| `SOURCE_DATE_EPOCH`                                    | `build.builtAt`                                                                                                                                    | The time of the build                                          |

| **How it meets the scope of #161.**                                                      | #161 asks for                                                                                                                                                                                                          | Where it is |
| ---------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- |
| The verifier                                                                             | Outside this repository. The guide's "Verifying evidence" lists the seven checks a verifier makes and which of them need the relying party's input                                                                     |
| The threat model                                                                         | "What the evidence proves", "Replay and relay" and "Limits" in the guide. Replay onto another domain, relay through another site, a copied page and a stored answer are each met by a field inside the signed evidence |
| The trust root                                                                           | The operator's key schedule. A verifier holds the matching public keys and the period each is in force                                                                                                                 |
| The claim set                                                                            | The table above, version `1`                                                                                                                                                                                           |
| Freshness and replay                                                                     | `issuedAt` and the caller's nonce are signed, so a verifier refuses an old time or a nonce that is not its own. Every answer is sent `Cache-Control: no-store`                                                         |
| Which configuration changes affect the claims                                            | `publisher.domain` and `[attestation] operator`. Nothing else of the settings is claimed, and `/_ts/config` is where a deployment shows the settings it runs                                                           |
| Claim creation and signing in the deployment lifecycle                                   | Keys are a build input. The key in force is the latest to have started, so a build carries the next key before it is needed, and it logs a warning while it signs with the last key it holds                           |
| Safe failure and rotation, exposing no secret                                            | No key in force or an unreadable key answers `503` at the endpoint alone. A settings document cannot name a key, and the section refuses any field it does not read                                                    |
| Tests that an authorized deployment verifies and that tampered or mismatched claims fail | The 37 tests of the module, and the harness below on the running adapter                                                                                                                                               |
| The transport exercised end to end, and documented                                       | `scripts/attestation-local-test.sh` and `docs/guide/attestation.md`                                                                                                                                                    |

**Where this meets #1235.** IABTechLab/trusted-server#1235 says a release binary must carry nothing of an operator's, so that everyone runs the same attested bytes. This pull request compiles the operator's signing keys into the build. The two meet here and the choice is one for the task force.

- **What compiling the keys buys.** No key sits in a store the settings or the running service can reach. A missing or broken store entry cannot take a site down, where the settings loader refuses the whole configuration when a secret it names is absent. A build with no schedule answers `503` at one address.
- **What it costs.** An operator who attests builds its own binary, so that binary is not the Tech Lab's released bytes.
- **The other custody.** Reading the schedule from the secret store keeps one binary for everyone. The signing step is two functions over a schedule, the key in force and the signature, so that change is contained.

The same issue asks which build facts the runtime can report. Here they are the version, the build time, the commit and the build run, each the builder's own report.

**Where this differs from the January proposal.** `docs/superpowers/specs/2026-01-15-attestation-design.md` proposed a signed in-toto statement with a config hash and an integration list at `/.well-known/trusted-server-attestation.json`, and claims carried in each bid request. This is narrower. It claims who operates the deployment and which build it says it runs, in an envelope a verifier checks with one public key and no JSON canonicalization, because the bytes are checked exactly as they arrived. Nothing here prevents a config hash or request claims being added to the evidence under a later version.

## 3. What is not in this change

- **A verifier.** None is in this repository. The guide says what one checks, and the harness is a small one.
- **The key custody**, as above.
- **The platform fields off Fastly.** The Axum, Cloudflare and Spin adapters report their own name and `unknown` for the rest.
- **The compiled schedule** is checked by the harness alone, because test builds carry no key. The reading of a schedule file is unit tested.

## 4. Acceptance

The pull request's description records how it was checked, with CI green on all 19 checks.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
