# Public Suffix List snapshot reference (normative)

The vendored Mozilla PSL revision used for registrable-domain
computation (hook spec §4a): the implementation PR vendors the list file
and records its upstream commit hash here. Rules: ICANN **and** private
sections apply; hostnames are IDNA-mapped before matching; IP literals
and single-label hosts have no registrable domain (cookie falls back to
host-only). Updating the snapshot is a reviewed spec change.

| Field                  | Value                                                                |
| ---------------------- | -------------------------------------------------------------------- |
| Upstream repository    | `publicsuffix/list`                                                  |
| Upstream commit        | `e1b8015c3b2f0f4f8c18659c2480fc1a22c07b20`                           |
| Upstream source path   | `public_suffix_list.dat`                                             |
| Required vendored path | `crates/trusted-server-core/data/public_suffix_list.dat`             |
| Required hash path     | `crates/trusted-server-core/data/public_suffix_list.sha256`          |
| Required provenance    | `crates/trusted-server-core/data/public_suffix_list.provenance.json` |

The implementation copies the source bytes at that commit without editing
and writes the lowercase 64-hex SHA-256 plus one trailing LF (no filename or
other fields) to the required hash path. CI verifies the bytes, hash, and
commit reference together; updating any one without the others fails. The
provenance file is canonical JSON with exactly
`{upstream_repository, upstream_commit_oid, upstream_commit_tree_oid,
source_path, source_blob_oid, source_sha256_hex}`. The vendoring PR description
quotes the same commit/tree/blob values and the independent command output
that verified the raw upstream SHA-256 and byte-for-byte vendored copy. A
commit OID without its tree and source-blob witness does not satisfy the
release gate.
