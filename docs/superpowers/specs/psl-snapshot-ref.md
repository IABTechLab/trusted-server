# Public Suffix List snapshot reference (normative)

The vendored Mozilla PSL revision used for registrable-domain
computation (hook spec §4a): the implementation PR vendors the list file
and records its upstream commit hash here. Rules: ICANN **and** private
sections apply; hostnames are IDNA-mapped before matching; IP literals
and single-label hosts have no registrable domain (cookie falls back to
host-only). Updating the snapshot is a reviewed spec change.

| Field           | Value                                                     |
| --------------- | --------------------------------------------------------- |
| Upstream commit | _recorded by the implementation PR that vendors the list_ |
| Vendored path   | _recorded alongside_                                      |
