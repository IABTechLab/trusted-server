# Provider-specific HTTP auction opt-in

## Intent

Permit an explicitly configured auction provider to use unencrypted HTTP for an approved demo or test endpoint. Keep HTTPS as the default. Do not turn one provider's exception into permission for another provider.

This documents the existing reconstructed feature at `2c79ea59`. Its formal design reviews occur before publication readiness, not before the earlier implementation. It does not authorize deployment or production use of unencrypted auction traffic.

## Configuration contract

`ProviderConfig.allow_insecure_http` is a boolean, defaults to `false` when absent and is omitted during serialization while false. An explicit true value survives serialization. Existing provider documents therefore keep their default wire shape.

For one provider:

| Parsed endpoint                             | Flag false or absent                  | Flag true                             |
| ------------------------------------------- | ------------------------------------- | ------------------------------------- |
| Absolute HTTPS with host                    | Accept subject to existing validation | Accept subject to existing validation |
| Absolute HTTP with host                     | Reject                                | Accept subject to existing validation |
| Username/password, fragment or missing host | Reject                                | Reject                                |
| Relative URL or another scheme              | Reject                                | Reject                                |

The flag is an operator-selected permission, not proof that an endpoint is approved, isolated or safe. HTTP exposes request and response data on the network. It adds no DNS/private-network allowlist and cannot make an insecure connection confidential.

Example, with fictional inputs:

```toml
[auction.providers.demo]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "http://bids.example.com:8000"
allow_insecure_http = true
```

## Boundaries

Auction-plan compilation owns this validation. Read the flag from the same provider whose endpoint is being compiled. Do not use a global toggle, profile-wide mutable state, another provider's flag or an environment fallback.

Keep existing profile validation and endpoint normalization. In particular, Prebid Server origin/canonical-path normalization still chooses `/openrtb2/auction`; other existing profile restrictions remain authoritative. HTTPS is not downgraded when the flag is true.

Transport selection, redirect policy, timeout/cutoff handling, consent, bidder eligibility, request signing, caching and response limits remain unchanged. Accepting a configured URL does not promise that every runtime can reach it or override a runtime's network restrictions.

Existing runtime/test provider literals explicitly select false unless that test is about the opt-in. No credentials or real deployment endpoints belong in examples or fixtures.

## Required proof

- HTTP fails without the flag and succeeds with that provider's explicit true value.
- A second HTTP provider without the flag still fails, even beside an opted-in provider.
- True does not allow credentials, fragments, relative URLs or another scheme.
- Default serialization omits the flag, absent deserialization returns false and true round-trips.
- Existing HTTPS/profile normalization cases continue passing.
- All affected adapter consumers compile and pass their target-matched checks in the final stack. A native-only plan test is not hosted/network qualification.

The focused existing command is `cargo test --locked -p trusted-server-core auction::plan::tests`. Application and adapter checks follow the repository CI gate list. No cloud request or deployment is needed to establish the configuration contract.

## Non-goals

No global HTTP policy, automatic TLS fallback, alternate transport, credentials-in-URL support, new provider profile, network allowlist, production security approval or deployment workflow.

## Review log

The implementation existed before these independent read-only reviews. They are not retroactive preimplementation approval or deployment authorization.

| Date       | Round          | Frozen target                                                                                                           | Result                                                                                                                        |
| ---------- | -------------- | ----------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| 2026-11-10 | 1              | Contract SHA256 `e4ad3f9a8743fd332bd12b239cab94a6e6aa538428b6404fc0d14be5635c186e`; implementation `dd2ca144..2c79ea59` | No blocking or nonblocking findings. Provider-local permission, URL validation, serialization and consumer changes inspected. |
| 2026-11-10 | 2, independent | Same frozen contract and complete 17-file implementation delta                                                          | No findings or requested changes. No tests or builds executed by the reviewer.                                                |

The formatted contract retained the same meaning. This extraction replays the HTTP unit without relay ancestry onto current main; the review log records its original reviewed inputs, not a whole-tree review of later changes. Current target-matched checks remain required before readiness.

## Related

- [Auction orchestration guide](../../guide/auction-orchestration.md)
