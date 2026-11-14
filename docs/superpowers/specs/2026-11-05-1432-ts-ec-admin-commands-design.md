# `ts ec show` and `ts ec eids` — design

Issue: #1432. Related: #1430 (`ts keys` commands, same admin-client concern),
#1271 (proposal to remove the admin routes; this work does not wait on it).

## Problem

Operators call two admin diagnostic routes with `curl`:

- `GET /_ts/admin/ec/{id}` returns the stored identity-graph record for an EC ID
  and a preview of the partner EIDs that record can contribute to an auction.
- `GET /_ts/admin/eids` decodes the request's `ts-eids` and `sharedId` cookies
  and previews which configured partners cookie ingestion would match or drop.

`curl -u admin:<password>` puts the shared admin password in shell history and
in `ps` output. The EIDs route also makes the operator assemble a `Cookie`
header by hand from values copied out of browser developer tools.

## Goals

- `ts ec show --service <url> <ec-id>` and
  `ts ec eids --service <url> [--ts-eids <value>] [--shared-id <value>]`.
- The admin password comes only from `TRUSTED_SERVER_ADMIN_PASSWORD`.
- The same service URL checks, username default and `401` hint as
  `ts cache purge`, through one shared client rather than a copy.
- Response data reaches the operator's terminal and nowhere else.

## Non-goals

- No command for the bare `GET /_ts/admin/ec` route, which reads the EC ID from
  the request's `ts-ec` cookie. `ts ec show <ec-id>` covers it: the operator
  passes the cookie's value as the argument.
- No server-side change. The routes, their auth and their response shapes stay
  as they are.
- No `ts keys` commands; #1430 owns them.

## Design

### Shared admin client

`ts cache purge` was the only admin caller. Its transport code moves out of
`commands/cache/purge.rs` into a crate-level `admin_client` module, and purge
now calls it. #1430 asked for one client shared with purge; it has no PR yet,
so this change creates the shared client and #1430 reuses it.

`AdminClient::new(service, username)`:

1. Parses `--service` and applies the existing
   `url_guard::require_credential_safe_transport`: HTTPS, or HTTP only for a
   loopback host, and no userinfo.
2. Rejects a path, query or fragment, so the admin path cannot be redirected.
3. Reads `TRUSTED_SERVER_ADMIN_PASSWORD`. The URL is checked first, so an unsafe
   `--service` is reported even when the password is also missing.

`AdminClient::send` builds a current-thread Tokio runtime, installs the rustls
crypto provider, and sends one request with basic auth, a 30-second timeout and
redirects disabled. Without the last setting, a redirect to a login page would
arrive as a `200`, and a cross-origin redirect would resend the credentials. It
returns the status and the body as text. `AdminResponse::error_text` returns the
`error` field of a JSON body, or the trimmed body otherwise.

`ADMIN_PASSWORD_ENVIRONMENT_VARIABLE` and the `401` hint move into this module.
The constant's old public path, `commands::cache::ADMIN_PASSWORD_ENVIRONMENT_VARIABLE`,
was public library API but has no users in this repository; it is removed rather
than re-exported.

Purge keeps its request bodies, its acknowledgment checks and its status hints.
One error message changes: a failure to read the response body now reads "could
not read the response from {endpoint}" instead of naming the purge
acknowledgment.
The endpoint-joining and URL-rejection unit tests move with the code they test.

### `ts ec show`

1. Validates the EC ID with `trusted_server_core::ec::generation::is_valid_ec_id`,
   the function the server itself uses, so the two cannot disagree. An invalid
   ID fails before the password is read or any request is sent. The check also
   rules out `/` and `.` segments, so the argument cannot escape the route.
2. Sends `GET /_ts/admin/ec/{id}` with no cookies.
3. On a non-success status, exits non-zero with the status, a hint and the
   server's `error` text:
   - `401`: the shared hint naming the username and the environment variable.
   - `404`: "no record for this EC ID". The server's own text adds that KV reads
     are eventually consistent.
   - `501`: the deployment has no EC identity graph; only the Fastly adapter
     holds one, and only when an EC store is configured. Fastly answers `501`
     too when `ec.ec_store` is unset.
4. On success, deserializes the body into CLI-local view types that ignore
   unknown fields. This happens in both modes, so a `200` that is not a lookup
   response (a proxy's page, for example) is refused rather than printed as
   the record. The parse error reports only the line and column, because
   `serde_json`'s message can quote the offending value and CLI errors are
   logged.
5. Default output: EC ID, store and generation, tombstone (`yes`, `no`, or
   `unknown` when the entry did not parse), any `entry_error` or
   `metadata_error`, the auction EIDs one UID per line, and each skipped
   partner ID with its server reason code plus a plain explanation. Reason codes
   the CLI does not know are preserved, with control characters escaped. When `auction` is absent, the
   output says the entry did not validate, so the auction attaches no EIDs from
   it. A closing line repeats the server's caveat that live auctions also apply
   per-request consent checks.
6. `--json` prints the response body unchanged, adding a final newline only when
   the body lacks one.

The CLI-local view types are deliberate: the server's `Eid`/`Uid` types use
`deny_unknown_fields`, so reusing them would break an older CLI whenever the
service adds a field.

### `ts ec eids`

1. Requires at least one of `--ts-eids` and `--shared-id`, enforced by a clap
   argument group and again in `run_eids` for library callers.
2. Builds `Cookie: ts-eids=<value>; sharedId=<value>`, with the cookie names
   taken from `trusted_server_core::constants`. The server splits the header on
   `;` (`cookies::extract_cookie_value`), so a value containing `;` would arrive
   as an extra cookie. Values containing `;`, whitespace, control or non-ASCII
   characters are refused, and the error does not echo the value. Empty values
   are refused. A browser never sends any of these inside a cookie value.
   Non-ASCII is refused because the server reads the `Cookie` header with
   `to_str()`, which fails for the whole header; both cookies would then be
   reported as not received, with no error.
3. Sends `GET /_ts/admin/eids`. A `401` gets the shared hint. A `404` means the
   service predates the route.
4. Default output: whether each cookie was received and parsed (the server's
   `cookie_present`, `parse_error`, `sharedid_present`), the configured partner
   count, the parsed EIDs, matched partner IDs with their UIDs, and unmatched
   sources with the reason code (`no_partner`, `no_valid_uid`) plus an
   explanation. A closing line repeats the server's caveat that the preview
   reads the cookies alone.
5. `--json` prints the response body unchanged, as for `show`.

`--json` on `eids` goes beyond the issue, which asks for it only on `show`. It
costs a few lines and gives both commands the same interface.

## Adapters

The CLI is a host-target binary. It changes no adapter code.

| Adapter    | `ts ec show`                                   | `ts ec eids` |
| ---------- | ---------------------------------------------- | ------------ |
| Fastly     | Real lookup; `501` when `ec.ec_store` is unset | Real preview |
| Axum       | `501`; the command explains Fastly-only        | Real preview |
| Cloudflare | `501` (same handler as Axum; not run here)     | Real preview |
| Spin       | `501` (same handler as Axum; not run here)     | Real preview |

## Configuration

No new configuration. `--username` defaults to `admin`, matching the example
config's `^/_ts/admin` handler and `ts cache purge`.

## Consent and privacy

The responses carry raw consent strings and partner UIDs. A successful response
is written only to the command's standard output: never to disk, and never
through the `log` crate. A refusal is returned as a CLI error, which the binary
logs to the terminal. Its text is the server's `error` field, which for these
routes holds fixed messages, not record data, or an adapter's plaintext `5xx`
message. A success body that does not match the expected shape is reported by
position only, never by quoting its content. The CLI logger runs at `info`, so
`reqwest`'s debug and trace logging is not emitted.

The default `show` summary prints `entry_error` and `metadata_error`. The server
builds these from parse failures, so they can quote stored values, and
`metadata_error` can embed the raw metadata bytes. Like the rest of the record,
they go to standard output only. `raw_body` is printed only by `--json`.

Human-readable summaries escape control characters in all response-derived text,
including sources, UIDs and diagnostic errors. Refusal messages apply the same
escaping before reaching the CLI logger. This prevents decoded cookie values or
stored records from injecting terminal controls or additional report lines.
Printable text stays unchanged, and `--json` retains the original response bytes
and formatting, apart from the documented missing final newline.

`--ts-eids` and `--shared-id` are command-line arguments, as the issue
specifies, so their values appear in shell history and `ps`. They are cookie
values from the operator's own browser, not the shared admin password. A later
change could read them from standard input if that turns out to matter.

## Failure handling

| Condition                                    | Result                                          |
| -------------------------------------------- | ----------------------------------------------- |
| Malformed EC ID                              | Refused locally; nothing sent                   |
| No cookie value, or an unsafe one            | Refused locally; nothing sent                   |
| Plaintext non-loopback or credentialed URL   | Refused locally; nothing sent                   |
| Password unset                               | Refused locally; nothing sent                   |
| Redirect                                     | Not followed; non-success exit                  |
| `401` / `404` / `501`                        | Non-success exit with a specific hint           |
| Other non-success, including plaintext `5xx` | Non-success exit with the status and body text  |
| `400` from `show`                            | Unreachable: the ID is validated locally first  |
| Network failure or 30-second timeout         | Non-success exit naming the endpoint            |
| `200` that is not the expected JSON          | Non-success exit; nothing printed as success    |
| Corrupt stored entry                         | Success; `entry_error` shown, tombstone unknown |

## Compatibility and rollout

The commands are new. They work against any deployed service that has the two
routes, which predate this change. `ts cache purge` keeps its flags, output and
exit codes; only the body-read error message changes (see above).

Documentation: the "Admin Diagnostic Endpoints" section of
`docs/guide/api-reference.md` shows both commands ahead of the `curl` fallback,
with the password read by `read -rs` so it stays out of shell history.
`docs/guide/cli.md` lists the commands in its command index.

## Testing

- Unit tests: endpoint joining and URL rejection (moved from purge), JSON
  `error` extraction, cookie-header construction and rejection, and rendering of
  both summaries, including unknown reason codes, tombstones and corrupt entries.
- `tests/ec_admin.rs` drives both commands against the local
  `support_origin::FixtureServer`: the request method, path, basic auth and
  absence of cookies on `show`; byte-for-byte `--json` (body plus the added final newline); no request for a
  malformed ID; `404`, `501` and `401` messages; a non-lookup `200`; no redirect
  following; the exact `Cookie` header on `eids`; the missing-cookie and
  missing-password refusals with no request sent; and plaintext-URL refusal.
- The integration tests decode the basic-auth header and check the exact
  `username:password`, including a non-default `--username`, and cover `eids`
  refusals (`401`, `404`, plaintext `500`) and `eids --json`.
- `tests/cache_purge.rs` is unchanged and still passes against the shared client.
- A runtime check against `fastly compute serve` with seeded EC records and
  against the Axum binary exercises both commands and purge end to end.

## Review log

- Round 1 (agent-assisted review, fresh session): checked the spec's code claims
  against the source and found nothing blocking. Advisories, all addressed: Fastly
  also returns `501` without an EC store (hint extended, adapter table
  corrected); non-ASCII cookie values were sent and made the server drop the
  whole header silently (now refused); a schema-mismatch error could quote a
  response value into the log (now reports position only); the purge body-read
  message change, the constant's public-API status, missing failure cases and
  the docs work were unstated (all now stated).
- Round 2 (agent-assisted review, fresh session, different model): re-verified
  the code claims and the issue's acceptance list; nothing blocking. Advisories:
  `--json` trimmed trailing whitespace, so it was not literally unchanged (now
  prints the body as received, adding only a missing final newline); the privacy
  section did not mention that `entry_error` and `metadata_error` can quote
  stored values (now stated). Not changed: a `sharedId` value starting with `-`
  must be passed as `--shared-id=<value>`, which is standard argument parsing.
