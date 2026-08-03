# Provider-code registry (normative, append-only, never reused)

Four-character codes (`[a-z0-9]`, zero-padded) used in physical graph
keys (providers spec §6.3). Allocation is a reviewed commit to this
file; codes are immutable and never recycled, including for retired
providers.

| Code   | Provider                                                                                                                                                                                    | Allocated  | Status |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- | ------ |
| `hmac` | Built-in HMAC EC provider (hmac identity rows use the reserved verbatim key grammar; this code does appear in `w` rowless-withdrawal keys and in non-key contexts — provenance, registries) | 2026-08-02 | active |
