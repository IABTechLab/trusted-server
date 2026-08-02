# DataDome request-header allowlist (normative, checked-in)

The complete set of response-named header pointers the security channel
(hook spec §4a) may copy into the owner-scoped publisher-upstream
overlay. Every `X-DataDome-*` name not listed here is rejected. Adding a
name is a reviewed commit to this file and a spec change.

| Header                | Direction                   | Scope                                                                                                |
| --------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------- |
| `X-DataDome-ClientID` | response → upstream overlay | Owner-scoped overlay only; never the shared request view; vendor egress governed by sign-off item 23 |
