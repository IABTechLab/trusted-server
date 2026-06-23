# Technical Specification: `ts dev proxy` — local production-hostname dev proxy

**Status:** Draft
**Author:** Engineering
**Crate:** `crates/trusted-server-cli` (binary `ts`)
**Command:** `ts dev proxy`
**Last updated:** 2026-06-22

> A standalone prototype of the core proxy (tokio + hyper + rustls + rcgen) has
> been validated end-to-end against a live Fastly service: it rewrites a
> production hostname → an alternate upstream with TLS SNI swap, reaches the
> correct Fastly POP, and injects Basic auth. This spec generalizes that
> prototype into a `ts dev proxy` subcommand and adds a per-machine dev
> Certificate Authority so Chrome, Firefox, and Safari all work.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Background and Constraints](#2-background-and-constraints)
3. [Design Decisions](#3-design-decisions)
4. [CLI Surface](#4-cli-surface)
5. [Architecture](#5-architecture)
6. [Module Structure](#6-module-structure)
7. [Local Certificate Authority](#7-local-certificate-authority)
8. [Request Rewriting](#8-request-rewriting)
9. [Browser Orchestration](#9-browser-orchestration)
10. [Configuration](#10-configuration)
11. [Security Considerations](#11-security-considerations)
12. [Constants and Defaults](#12-constants-and-defaults)
13. [Error Handling](#13-error-handling)
14. [Testing Strategy](#14-testing-strategy)
15. [Implementation Order](#15-implementation-order)
16. [Out of Scope / Future Work](#16-out-of-scope--future-work)

---

## 1. Overview

`ts dev proxy` is a local developer tool that lets you open a **production
publisher hostname** (e.g. `https://www.example-publisher.com`) in a real
browser and have it served by a **dev or staging upstream** — a Trusted Server
Compute service (`*.edgecompute.app`), a staging Fastly service, or
`localhost` — **without changing any production DNS, VCL, certificates, or
affecting any other user.**

It is a TLS-terminating forward (MITM) proxy that runs entirely on the
developer's machine. The browser is pointed at it; for the configured
hostname(s) it rewrites the request to the chosen upstream (including the TLS
SNI) while the address bar continues to show the production hostname.

**Primary use case:** validate the routing and behavior of a new or changed
Trusted Server deployment at the publisher's real domain — cookies,
`Host`-sensitive logic, CMP/consent flows, first-party context — before any DNS
cutover.

**Non-goals:** not a production proxy, not a load-test tool; it does not modify
the upstream Fastly service. Local-only, developer-facing.

---

## 2. Background and Constraints

The naive approaches do not work, and the reasons drive the design:

1. **The browser binds TLS SNI to the URL host.** A request to
   `https://www.example-publisher.com` always sends SNI
   `www.example-publisher.com`.
2. **Fastly routes by SNI to the service that owns the domain.** That domain is
   active on the production service, so the SNI is delivered to **production** —
   regardless of any `/etc/hosts` or `--host-resolver-rules` IP override (all
   Fastly anycast IPs route by SNI).
3. **A Fastly domain can be active on only one service at a time.** The new
   service cannot claim the production hostname while prod still serves it.

Therefore the only way to reach an alternate upstream while the browser shows
the production hostname is to **rewrite the SNI between the browser and the
upstream**, which requires terminating the browser's TLS locally — a MITM proxy.
No browser flag, extension, or hosts entry can do it, because none of them
decouple SNI from the URL.

**TLS trust.** Terminating the browser's TLS means presenting a certificate for
the production hostname. Chrome can ignore cert errors
(`--ignore-certificate-errors`), but **Safari and Firefox have no such flag**.
To support all three uniformly, the proxy presents certificates from a local
**Certificate Authority** the developer trusts once. A trusted chain also
satisfies **HSTS**, which an "ignored" cert does not.

---

## 3. Design Decisions

Resolved during brainstorming and design review (2026-06-22):

| Decision        | Choice                                                                                                                                                                                                                                              |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Browser scope   | **All three** (Chrome, Firefox, Safari) in v1 via a CA.                                                                                                                                                                                             |
| CA provenance   | **Generated per-machine on first run**, stored in the user data dir (`--ca-dir`), key `0600`; **never committed**. Trust once per machine.                                                                                                          |
| Browser launch  | `--launch` takes a **list** and has **no default** — if unset, just run the proxy (no browser). Each listed browser is launched and configured against the proxy.                                                                                   |
| Safari proxy    | Best-effort system PAC via `networksetup`, **restored on exit**; falls back to printed instructions.                                                                                                                                                |
| Transport       | HTTP/1.1 both legs in v1 (h2 deferred).                                                                                                                                                                                                             |
| Bind            | Loopback only by default; non-loopback requires `--allow-non-loopback` and disables blind tunnel/forward, so it can't become an open proxy (§11).                                                                                                   |
| Crate wiring    | **Excluded** from the workspace (like `integration-tests`), _not_ a non-default member — the repo pins the build target to `wasm32-wasip1` and this binary is native (§6).                                                                          |
| Default `Host`  | `Host = FROM` (preserve the production host) — required because TS core anchors URL rewriting to the inbound `Host`. `--rewrite-host` sends `Host = TO` for upstreams that route/validate on their own host. `X-Orig-Host` is informational (§8.3). |
| Unmatched hosts | **Blind-tunnel**, decided from the CONNECT authority before terminating TLS; only matched hosts are MITM'd (§5, §11).                                                                                                                               |

---

## 4. CLI Surface

```
ts dev proxy [OPTIONS]
```

### 4.1 Options

| Flag                   | Value                            | Default                                                                                                     | Description                                                                                                                                                                                                |
| ---------------------- | -------------------------------- | ----------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--map`                | `FROM=TO` (repeatable)           | —                                                                                                           | Rewrite rule: requests to `FROM` are served from `TO`.                                                                                                                                                     |
| `-f, --from`           | `HOST`                           | —                                                                                                           | Shorthand for a single rule's `FROM`. Optional when `FROM` is inferable from config (§10.2).                                                                                                               |
| `-t, --to`             | `HOST[:PORT]`                    | —                                                                                                           | Shorthand for a single rule's `TO`. Combines with `--from`, or with the inferred publisher domain when `--from` is omitted. A non-default port is kept in the upstream `Host` but never in the SNI (§8.3). |
| `--listen`             | `ADDR`                           | `127.0.0.1:8080`                                                                                            | Proxy listen address. A non-loopback address is **rejected** unless `--allow-non-loopback` is also set.                                                                                                    |
| `--allow-non-loopback` | flag                             | false                                                                                                       | Permit binding a non-loopback `--listen`. Even then, blind tunnel/forward of **unmatched** hosts is disabled (only configured rules are served), so the proxy can't act as a generic open proxy (§11).     |
| `--launch`             | `chrome,firefox,safari` \| `all` | _unset_                                                                                                     | Comma list of browsers to launch + configure (`all` = `chrome,firefox,safari`); **if omitted, just run the proxy** (no browser).                                                                           |
| `--rewrite-host`       | flag                             | false                                                                                                       | Send `Host: <TO>` upstream instead of the default `<FROM>` (see §8.3).                                                                                                                                     |
| `--basic-auth`         | `USER:PASS`                      | —                                                                                                           | Inject `Authorization: Basic …` toward gated upstreams. **Convenience only** — visible via `ps`/shell history; prefer `--basic-auth-file`.                                                                 |
| `--basic-auth-file`    | `PATH`                           | —                                                                                                           | Read `USER:PASS` from a file (preferred over `--basic-auth`).                                                                                                                                              |
| `--insecure`           | flag                             | false                                                                                                       | Skip **upstream** certificate verification.                                                                                                                                                                |
| `--upstream-plaintext` | flag                             | false                                                                                                       | Connect to upstream over HTTP (e.g. `localhost:3000`).                                                                                                                                                     |
| `--ca-dir`             | `PATH`                           | `$XDG_DATA_HOME/trusted-server/dev-proxy` (macOS: `~/Library/Application Support/trusted-server/dev-proxy`) | Where the per-machine CA cert/key are stored (generated on first run).                                                                                                                                     |

### 4.2 Companion subcommands

```
ts dev proxy ca path        # print the per-machine CA certificate path
ts dev proxy ca install     # add the CA to the OS trust store (macOS login keychain; prompts)
ts dev proxy ca uninstall   # remove the CA from the OS trust store (revoke trust when done)
ts dev proxy ca regenerate  # regenerate the per-machine CA (invalidates prior trust)
```

### 4.3 Examples

```bash
# Default: infer rule from project config, run proxy only (no browser):
ts dev proxy

# Explicit map to a Compute service, launch+configure all three browsers:
ts dev proxy --map www.example-publisher.com=trusted-server-example.edgecompute.app \
  --launch chrome,firefox,safari

# Gated staging upstream, Firefox only:
ts dev proxy -f www.example-publisher.com -t staging.example.net \
  --basic-auth dev:secret --launch firefox

# Local instance, just run the proxy (no browser):
ts dev proxy -f www.example-publisher.com -t localhost:3000 \
  --upstream-plaintext
```

---

## 5. Architecture

```mermaid
sequenceDiagram
    participant B as Browser<br/>(proxy = 127.0.0.1:8080)
    participant P as ts dev proxy
    participant U as Upstream<br/>(Compute / staging)

    Note over B,P: address bar stays https://www.pub.com
    B->>P: CONNECT www.pub.com:443
    P-->>B: 200 (tunnel established)
    P->>P: TLS-accept with leaf cert for www.pub.com<br/>(signed by local CA)
    B->>P: GET / — Host: www.pub.com (over MITM TLS)
    P->>P: match rule www.pub.com → TO<br/>SNI→TO; keep Host: FROM (default); add X-Orig-Host; inject auth
    P->>U: GET / — Host: www.pub.com (FROM), SNI=TO (over TLS)
    Note over P,U: SNI=TO → valid cert + Fastly routing; Host=FROM by default (--rewrite-host sends Host=TO)
    U-->>P: response
    P-->>B: response (over MITM TLS)
```

**Per-connection flow:**

1. Browser issues `CONNECT host:443`. The proxy parses the authority and matches
   it against the rule table **before replying** — the `200` is deferred until it
   knows it can serve the tunnel:
   - **No match → blind tunnel.** Connect to `host:port` first; reply `200` only
     after the upstream TCP connect succeeds (a connect failure returns a proper
     `502` to the browser), then pipe bytes verbatim — no leaf minted, nothing
     decrypted, so unrelated browsing is never MITM'd. (Refused with `403` on a
     non-loopback bind — §11.)
   - **Match → MITM.** Mint/select the leaf for `host`, reply `200`, then
     TLS-accept the tunnel with that leaf (from the local CA, cached per host).
2. On the MITM path, read decrypted HTTP/1.1 requests **in a loop** — one
   keep-alive tunnel carries many sequential requests.
3. For each request: rewrite upstream target + SNI to `TO`, set `Host` (§8.3),
   add `X-Orig-Host: <FROM>`, inject auth if configured. An `Upgrade:`
   (WebSocket) request is out of scope in v1 (§16): log a clear note and close
   rather than corrupting the stream.
4. Proxy opens a TLS (or plaintext) connection to `TO`, forwards the request,
   streams the response back through the MITM TLS, and keeps the tunnel open for
   the next request.
5. The pass-through case is handled entirely at step 1 (blind tunnel); a matched
   tunnel never falls through to an unrewritten upstream.

---

## 6. Module Structure

The CLI is a **native host binary**, distinct from the wasm32 workspace default.

```
crates/trusted-server-cli/
  Cargo.toml                 # [[bin]] name = "ts"; native deps (tokio net, hyper, rustls, rcgen)
  src/
    main.rs                  # clap root; dispatches `dev`
    commands/
      dev/
        mod.rs               # `Dev` subcommand group
        proxy/
          mod.rs             # `ProxyArgs`; orchestration (run / ca / launch)
          server.rs          # accept loop, CONNECT upgrade, request handler
          ca.rs              # CertAuthority: load-or-generate per-machine CA, mint+cache per-host leaves
          rewrite.rs         # RuleTable, Rule, RewriteOutcome
          browser.rs         # launch+configure Chrome/Firefox/Safari; PAC generation
          config.rs          # arg + project-config resolution into RuleTable
```

**Workspace integration.** Add `crates/trusted-server-cli` to the `[workspace]
exclude` list (alongside `crates/integration-tests`) — **not** `members`. The
repo pins `build.target = "wasm32-wasip1"` in `.cargo/config.toml`, so every
workspace member is built for wasm by the CI gates (`cargo test --workspace`,
`cargo clippy --workspace`); a native binary (tokio/hyper/rustls/rcgen) cannot
compile for wasm and would break those gates for everyone. It must therefore be
excluded, exactly like `integration-tests`. An excluded crate is **not**
addressable by `-p` from the workspace root, and the root config still forces
wasm, so build/run it with an explicit native target:

```bash
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy …
```

(mirrors how `scripts/integration-tests.sh` runs the excluded `integration-tests`
crate via `--manifest-path` + a detected host `--target`.) Note the binary name
`ts` collides with the common `ts` timestamp tool (moreutils); consider also
installing a less ambiguous alias (e.g. `tsrv`).

**Dependencies & lints.** Excluded crates inherit neither
`[workspace.dependencies]` nor `[workspace.lints]`, so this crate pins its own
deps — including its **own** `tokio` features (`net`, `rt-multi-thread`,
`macros`, `io-util`), since the workspace's wasm-oriented set lacks `net` — and
declares its **own** `[lints.clippy]`. Mirror the workspace posture (deny
`unwrap_used`/`panic`), use `error-stack` for fallible paths, and route
user-facing output through a thin helper (with a local
`#![allow(clippy::print_stdout)]` if that restriction lint is enabled).

---

## 7. Local Certificate Authority

### 7.1 Provenance

The CA cert and key are **generated on the developer's machine on first run** and
stored under `--ca-dir` (default `$XDG_DATA_HOME/trusted-server/dev-proxy`, or
`~/Library/Application Support/trusted-server/dev-proxy` on macOS, where
`$XDG_DATA_HOME` is normally unset — resolve via a platform data-dir helper). The
directory is created `0700` and the key file written mode `0600`. The CA is
**never committed** and never leaves
the machine; each developer trusts their own CA once.

- CN: `Trusted Server DEV-ONLY Proxy CA — DO NOT TRUST IN PRODUCTION`.
- Validity: ~10 years (rotation = re-run `ca regenerate` + re-trust).
- The default `--ca-dir` lives outside the repo; the key must never be committed.

### 7.2 `CertAuthority`

```rust
struct CertAuthority {
    issuer: rcgen::Issuer<'static>,                       // loaded-or-generated CA cert + key
    leaves: Mutex<HashMap<String, Arc<ServerConfig>>>,    // per-host leaf cache
}
```

- **Load-or-generate** at startup: read `ca-cert.pem`/`ca-key.pem` from
  `--ca-dir`; if absent, generate the CA, persist it (key `0600`), and print a
  one-time "trust this CA" hint.
- **Mint** a leaf per **matched** CONNECT host (unmatched hosts are blind-tunneled
  and never get a leaf — §5): `subject_alt_name = [host]`, short validity
  (≤ 90 days), signed by `issuer`; wrap in a `rustls::ServerConfig` (ALPN
  `http/1.1`); cache keyed by host. Sign _outside_ the cache lock and
  double-check before insert so concurrent first-time hosts don't serialize on
  the signing work. (An IP-literal host needs an IP-type SAN, not DNS.)
- **Acceptor selection:** the CONNECT handler knows the host and selects the
  cached `ServerConfig` directly — no SNI `ResolvesServerCert` in v1.

### 7.3 Trust installation

- `ts dev proxy ca path` prints the CA cert path under `--ca-dir`.
- `ts dev proxy ca install` (macOS) adds the CA to the **login** keychain (no
  sudo):
  `security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db ca-cert.pem`
  (prompts). Do **not** pass `-d` — that targets the admin/System keychain and
  requires root. Prints instructions on failure / other OSes.
- `ts dev proxy ca uninstall` removes the CA again
  (`security delete-certificate -c "<CA CN>"`), so trust can be fully revoked
  when you're done (§11).
- Firefox does not consult the macOS login keychain reliably; it is trusted
  per-profile at launch by importing the CA into the profile's NSS DB (§9).

---

## 8. Request Rewriting

### 8.1 Rule table

```rust
struct Rule {
    from: String,      // matched case-insensitively, port-stripped
    to: Authority,     // host + optional port (default 443; 80 with --upstream-plaintext)
    preserve_host: bool,
    plaintext: bool,
}
struct RuleTable(Vec<Rule>);   // first match wins; unmatched => pass-through
```

In v1, `preserve_host` (default **true**) and `plaintext` are set on every rule
from the global flags — `--rewrite-host` clears `preserve_host`, and
`--upstream-plaintext` sets `plaintext`; the per-rule fields exist so a future
per-`--map` override can be added without a struct change. `-f/--from` +
`-t/--to` is sugar for a single `--map FROM=TO`.

### 8.2 Matching

The MITM-vs-tunnel decision is made first, from the **CONNECT authority** (§5
step 2). On the MITM path each request is then matched by host (from `Host`,
else the CONNECT authority) to select the rule — case-insensitive, ignoring
`:port`, first match wins. On a loopback bind, a CONNECT authority with no
matching rule is blind-tunneled unchanged, so the proxy stays usable for normal
browsing; on a non-loopback bind (`--allow-non-loopback`) unmatched authorities
are instead refused with `403` (§11), never blind-tunneled.

### 8.3 Header rewriting on match

| Header                        | Action                                                 | Rationale                                                                                                                          |
| ----------------------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------- |
| upstream connection + **SNI** | `rule.to` **host only** (port stripped)                | SNI is a bare hostname; a `:port` in SNI is invalid and breaks the handshake                                                       |
| `Host`                        | `rule.from` (default) or `rule.to` if `--rewrite-host` | TS core anchors URL rewriting to the inbound `Host`; preserving `FROM` keeps rewritten URLs on the production domain (see caveats) |
| `X-Orig-Host`                 | `rule.from`                                            | informational record of the real first-party host (see caveat)                                                                     |
| `Authorization`               | set if `--basic-auth` and not already present          | clear `401` gates on staging upstreams                                                                                             |
| `Proxy-Connection`            | removed                                                | hop-by-hop hygiene                                                                                                                 |

**Why `Host = FROM` is the default (resolved).** The §1 goal — validate cookies,
`Host`-sensitive logic, CMP/consent, and first-party context at the _real_
domain — requires the upstream to see `Host = FROM`. Trusted Server core derives
`request_host` from the inbound `Host` (`RequestInfo::from_request` in
`http_util.rs`) and anchors all HTML/RSC URL rewriting to it
(`request_url = "{scheme}://{request_host}"` and `rewrite_bare_host_at_boundaries`
in `publisher.rs` / `rsc_flight.rs`). With `Host = TO` the app would rewrite every
first-party URL onto the Compute/staging host — wrong for the primary use case —
so the default preserves `FROM`.

This works against a TS **Compute** upstream because Fastly routes by SNI
(`= TO`, a domain provisioned on that service) and passes `Host` through to the
program unchecked. A Fastly **Deliver** / host-validating upstream may reject an
unconfigured `Host` ("unknown domain"); and because a domain can be active on
only one service (§2 ¶3), you cannot add the live production domain to a separate
dev service. For those upstreams, pass `--rewrite-host` (sends `Host = TO`) or add
the domain to the service.

`X-Orig-Host: FROM` is still sent for upstreams that opt to honor it, but it is
**informational only**: TS core does not read it today and in fact _strips_
spoofable forwarded host headers (`X-Forwarded-Host`, etc.) as an anti-spoofing
measure. Reconcile any future trusted-`X-Orig-Host` contract with the existing
`publisher.origin_host_header_override` knob. **Validation:** an integration test
must assert that, by default, rewritten HTML/RSC output stays on `FROM` (not `TO`).

**Port handling.** When the `Host` value is the upstream (`--rewrite-host`) and
`TO` carries a non-default port (e.g. `localhost:3000`,
`staging.example.com:8443`), the port **is** included in the `Host` header but
**never** in the SNI (a bare hostname). This mirrors the existing split in
`publisher.rs` (`origin_host_without_port` vs `origin_host_header`).

### 8.4 URI normalization

Ensure the upstream request URI is origin-form (`path?query`); routing is driven
by the `Host` header. Requests read off a CONNECT tunnel are already origin-form,
so this is a no-op for the MITM-HTTPS path.

**Plain HTTP.** v1 proxies **HTTPS only** — launched browsers are configured to
send only `https://` URLs to the proxy (§9), so plain `http://` goes `DIRECT`.
The proxy still handles a stray absolute-form HTTP request defensively: it
blind-forwards it to the URL host unchanged (never undefined behavior) and
applies no rewrite rules. Full absolute-form plain-HTTP rewriting is future work
(§16).

**Local routes.** Origin-form requests addressed to the proxy's **own** listen
address — notably `GET /proxy.pac` for Safari (§9) — are served locally and never
forwarded. The listener dispatches these _before_ proxy handling: a request is
proxy traffic only if it is `CONNECT` or absolute-form; an origin-form request to
a local route (`/proxy.pac`, a health check) is answered directly. On a
non-loopback bind (§11), blind-forwarding is disabled, so only `CONNECT` to
matched rules and these local routes are answered.

---

## 9. Browser Orchestration

`--launch` is a comma list with **no default** — when omitted, the proxy runs
without launching any browser. When set, each listed browser is launched in a
throwaway/temporary profile configured against the proxy, opening the first
rule's `FROM` URL.

| Browser     | Launch + configure                                                                                                                                                                                                                 | Trust                                                                                                                                                                                                                                               |
| ----------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **chrome**  | temp `--user-data-dir`, `--proxy-server="https=127.0.0.1:<port>"` (per-scheme — **HTTPS only**, plain HTTP stays direct), `--no-first-run`, open URL                                                                               | local CA via OS keychain (`ca install`)                                                                                                                                                                                                             |
| **firefox** | temp profile `user.js` (not `prefs.js`, which Firefox owns and rewrites): `network.proxy.type=1` + **`network.proxy.ssl` host+port only** (leave `network.proxy.http` unset, so plain HTTP stays direct); `firefox -profile <tmp>` | Import the CA into the profile's NSS DB with `certutil -A` (robust, no sudo). `security.enterprise_roots.enabled=true` is unreliable on macOS — it reads the **admin/System** keychain, not the login keychain — so NSS import is the primary path. |
| **safari**  | no per-app proxy: best-effort, **system-wide** `networksetup -setautoproxyurl <service>` on the active service, scoped to `FROM` via the PAC; open URL; **restore prior setting on exit**                                          | local CA via macOS keychain (`ca install`)                                                                                                                                                                                                          |

PAC (Safari/system scoping) sends only `FROM` hosts to the proxy, everything
else `DIRECT`:

```javascript
function FindProxyForURL(url, host) {
  if (url.substring(0, 6) == 'https:' && host == 'www.example-publisher.com')
    return 'PROXY 127.0.0.1:8080'
  return 'DIRECT'
}
```

**Safari/macOS implementation notes:** macOS frequently ignores `file://` PAC
URLs, so the proxy **serves the PAC as a first-class local route** — an
origin-form `GET /proxy.pac` on the proxy's own listen address, dispatched before
proxy forwarding (§8.4) — and points `networksetup -setautoproxyurl` at
`http://127.0.0.1:<port>/proxy.pac`. (It may instead bind a separate loopback
port for the PAC; either way the PAC is served locally, never proxied.) The
target must be the **active** network service, which `-listallnetworkservices`
does **not** identify (it only lists every service). Detect the default-route
interface with `route -n get default` (`interface: enX`), then map that device to
its service name via `networksetup -listnetworkserviceorder` (entries carry
`(Hardware Port: …, Device: enX)`); set, and later restore, the PAC on **that**
service. Chrome/`--ignore-certificate-errors` is not used (we rely on the trusted
CA); a developer who prefers not to trust the CA can launch Chrome manually with
that flag.

Because `networksetup` changes are **system-wide** (every app, not just Safari),
the proxy persists the prior auto-proxy state to a file and restores it via an
exit hook plus signal handlers. A hard kill (`SIGKILL`) skips cleanup, so on the
next run `ts dev proxy` re-reads that file and restores it (or prints the manual
`networksetup` command). On multi-service machines it must target the correct
service, and managed networks may require admin rights.

If any browser can't be auto-configured, print its manual steps and continue
with the others.

---

## 10. Configuration

### 10.1 Precedence

CLI flags > project-config inference (§10.2) > built-in
defaults. `--map`/`-f`/`-t` rules are unioned (first-match-wins by declared
order). `--from` and `--to` may be supplied independently: a lone `--to` pairs
with the inferred `FROM`, and a lone `--from` pairs with the inferred `TO`
(§10.2). A rule is complete only when both sides resolve; otherwise the tool
errors with what it could and couldn't infer.

### 10.2 Project-config inference (zero-arg ergonomics)

With no `--map`/`-f`/`-t`, infer a single rule from the Trusted Server project
config so the common case is argument-free:

- `FROM` ← the publisher first-party domain (`publisher.domain` in
  `trusted-server.toml` — the public hostname, **not** `publisher.origin_url`'s
  host, which is the upstream origin).
- `TO` ← a dev-proxy upstream that **must be added to config**: no existing field
  carries the Compute/staging hostname (`fastly.toml` has only `service_id`, and
  `edgecompute.app` appears only in comments). Add an explicit field, honored
  only when no `--map`/`-f`/`-t`/`--to` is given:

  ```toml
  [dev_proxy]
  upstream = "trusted-server-example.edgecompute.app"
  ```

Until `[dev_proxy].upstream` exists, zero-arg `ts dev proxy` cannot infer `TO`:
exit with a clear error showing the inferred `FROM` and asking for `--to`/`--map`.
If `FROM` is ambiguous (multiple publishers), list candidates.

The tool is **flags-only** — there are no `TS_DEV_PROXY_*` environment-variable
overrides. Every setting is a CLI flag (§4); the only file inputs are
`trusted-server.toml` (inference, §10.2) and `--basic-auth-file`.

---

## 11. Security Considerations

- **Per-machine CA key (never committed).** `ca-key.pem` is generated on the
  developer's machine and stored under `--ca-dir` with mode `0600`. It is never
  committed and never leaves the machine, so a repo leak cannot MITM anyone.
  - CN explicitly marks it **DEV-ONLY / DO NOT TRUST IN PRODUCTION**.
  - Proxy binds **loopback only** by default; a non-loopback `--listen` is
    **rejected** unless `--allow-non-loopback` is passed. Even then, blind
    tunnel/forward of unmatched hosts is **disabled** off loopback (unmatched
    `CONNECT` gets `403`, only configured rules are served), so the tool can
    never become a generic open CONNECT/HTTP proxy on the LAN.
  - Leaves are short-lived; the CA is never used by any deployed artifact.
  - `ca regenerate` rotates the CA (forces re-trust).
  - `ca uninstall` removes it from the trust store. Trust is **not** auto-revoked
    on exit, so an OS-trusted 10-year dev CA whose key sits on disk is a standing
    MITM risk if that key is ever exfiltrated by user-level malware: run
    `ca uninstall` when finished and treat `ca-key.pem` like a credential.
  - Docs state plainly: trust this CA only on a dev machine you control.
- **`--insecure` is loud.** Disables upstream verification; print a banner while
  active. Independent of the browser-side MITM trust.
- **No secret logging.** Redact `Authorization` and `Cookie`; log method, host,
  path, and chosen upstream only.
- **Credential input.** `--basic-auth USER:PASS` is **convenience only** — argv
  is visible via `ps` and shell history. Prefer `--basic-auth-file`; the file is
  read once at startup and never logged.
- **Only matched hosts are decrypted.** Launched browsers proxy **HTTPS only**
  (§9) and unmatched CONNECT authorities are blind-tunneled (§5), so unrelated
  browsing is never MITM'd.
- **Production credentials reach `TO`.** With the default `Host = FROM`, the
  browser attaches the production hostname's cookies and any existing
  `Authorization` for `FROM`, and the proxy forwards them to `TO` — and injected
  `--basic-auth` is _skipped_ when an `Authorization` is already present (§8.3).
  Point `TO` only at a dev/staging upstream you control. Launched **temp
  profiles** start with no real cookies, so prefer `--launch` over running the
  proxy against your everyday browser profile; for manual use, treat `TO` as
  receiving real first-party session data. (A future `--scrub-request-headers`
  could drop `Cookie`/`Authorization` toward `TO`, at the cost of session
  fidelity.)
- **Scope reminder.** Mutates traffic only on the developer's machine; performs
  no changes to Fastly services, DNS, or certificates. The Safari/system
  auto-proxy change is system-wide while active and is reverted on exit (and
  recovered on the next run after a hard kill — §9).

---

## 12. Constants and Defaults

| Name                      | Value                                                                                                                                 |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| Default listen            | `127.0.0.1:8080`                                                                                                                      |
| Default `--launch`        | _unset_ (proxy only)                                                                                                                  |
| CA storage dir            | `$XDG_DATA_HOME/trusted-server/dev-proxy/{ca-cert.pem,ca-key.pem}` (macOS: `~/Library/Application Support/…`; dir `0700`, key `0600`) |
| CA CN                     | `Trusted Server DEV-ONLY Proxy CA — DO NOT TRUST IN PRODUCTION`                                                                       |
| Leaf validity             | ≤ 90 days                                                                                                                             |
| ALPN (both legs)          | `http/1.1`                                                                                                                            |
| Injected real-host header | `X-Orig-Host`                                                                                                                         |
| Upstream port (default)   | `443` (`80` with `--upstream-plaintext`)                                                                                              |

---

## 13. Error Handling

`error-stack` with actionable messages mapped to the failures we actually hit:

| Condition                                | Detection                 | Message guidance                                                                                                                                                                                      |
| ---------------------------------------- | ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Upstream TLS `unrecognized_name`         | rustls alert on connect   | "`TO` has no TLS cert for its SNI — verify the domain is provisioned on the upstream Fastly service."                                                                                                 |
| Upstream `401`                           | response status           | "Upstream is gated; pass `--basic-auth user:pass`."                                                                                                                                                   |
| Upstream `503` / connect refused         | response/IO               | "Upstream unreachable or backend unhealthy; check the service and its origin healthcheck."                                                                                                            |
| CA not trusted (browser warning)         | n/a (browser-side)        | Surface `ca install` / Firefox-profile note in the run banner.                                                                                                                                        |
| Listen addr in use                       | bind error                | Suggest `--listen` with another port.                                                                                                                                                                 |
| Upstream "unknown domain"                | `404` / Fastly error body | "The default `Host = FROM` isn't a domain the `TO` service accepts. Use a TS Compute upstream (routes by SNI), pass `--rewrite-host` to send `Host = TO`, or add the domain to the upstream service." |
| `Upgrade:` / WebSocket on a matched host | request `Upgrade` header  | "Upgrades aren't proxied in v1 (§16); the connection is closed with a logged note."                                                                                                                   |

Per-request errors become a `502` with a short diagnostic body plus a logged
line; the accept loop continues. The process never panics on a single bad
request.

---

## 14. Testing Strategy

**Unit (`rewrite.rs`):** host matching (case-insensitivity, port stripping,
first-match-wins, no-match pass-through); header outcomes (default `Host=FROM` +
`X-Orig-Host`; `--rewrite-host` sends `Host=TO`; non-default `TO` port in `Host`
but not SNI; auth injected only when absent); URI normalization.

**Unit (`ca.rs`):** CA is generated on first run and reloaded from `--ca-dir` on
the next run (key file mode `0600`); minted leaf carries the requested SAN,
chains to the CA, and is cached (second call returns the same `Arc`).

**Integration (`crates/integration-tests`, native):** local HTTPS upstream with
a known self-signed cert; run the proxy with `--insecure`; client configured to
use the proxy and trust the dev CA; assert address-host preserved, request
reaches upstream with rewritten `Host`/SNI + `X-Orig-Host`, response streamed
back. Cover `--basic-auth` clearing a `401`; unmatched-host **blind tunnel**
(bytes piped, no leaf minted, dev CA never presented); and **multiple sequential
requests over one keep-alive tunnel**.

**Manual matrix (documented):** Chrome / Firefox / Safari each load the `FROM`
URL through the proxy with the CA trusted and reach the upstream with a valid
padlock and the production hostname in the address bar.

---

## 15. Implementation Order

1. **Crate skeleton.** `trusted-server-cli` with `ts` binary, clap root, `dev`
   group, `proxy` stub. Workspace wiring (**excluded** crate, native target).
2. **Rewrite core.** `RuleTable`/`Rule`/matching/outcome, fully unit-tested.
   Pure logic, no I/O.
3. **Local CA + minting.** Generate the CA on first run into `--ca-dir` (key
   `0600`, outside the repo); `CertAuthority` loads-or-generates and mints+caches
   per-host leaves. Unit-tested.
4. **Proxy server.** CONNECT upgrade, MITM TLS via minted leaf, upstream forward
   (TLS + `--insecure` + `--upstream-plaintext`). End-to-end against a real
   upstream.
5. **Header/auth polish.** `--basic-auth`/`--basic-auth-file`, `X-Orig-Host`,
   `--rewrite-host` (default preserves `Host = FROM`), secret redaction in logs.
6. **Browser orchestration.** `--launch` list (no default — unset runs proxy
   only): Chrome + Firefox profiles, Safari PAC via `networksetup` with
   restore-on-exit; PAC generation; `ts dev proxy ca {path,install,uninstall,regenerate}`.
7. **Project-config inference.** Zero-arg resolution from `trusted-server.toml`
   / `.env.ts.*`.
8. **Docs.** A `docs/guide/` page: setup, per-browser trust, the §13
   troubleshooting table, and the per-machine CA security note.

Steps 1–4 already deliver a usable tool; each step is independently shippable.

---

## 16. Out of Scope / Future Work

- **HTTP/2 upstream** (v1 forces HTTP/1.1 on both legs).
- **Absolute-form plain-HTTP proxying / rewriting** (v1 proxies HTTPS only; a
  stray `http://` request is blind-forwarded, not rewritten — §8.4).
- **WebSocket / non-HTTP upgrades** through the MITM tunnel.
- **Response rewriting / fixture injection** (mock upstreams, latency).
- **Multiple simultaneous upstreams per host** (A/B / weighted).
- **Windows/Linux trust + Safari automation** beyond printing instructions.
- **Recording/replay** of proxied traffic.
