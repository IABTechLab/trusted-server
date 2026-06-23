# Dev Proxy

Test a production publisher hostname against a dev or staging upstream — with a
real browser, real TLS, and no DNS change — using `ts dev proxy`.

## What it does

`ts dev proxy` is a TLS-terminating forward (MITM) proxy that runs on your
machine. When you open `https://www.example-publisher.com` in a browser pointed
at it, the address bar shows the production hostname but the request is served
by the upstream you specify — a Trusted Server Compute service, a staging
instance, or `localhost`. No production DNS, Fastly service, or certificate is
touched, and no other users are affected.

**Why a local proxy is necessary.** The browser binds TLS SNI to the URL
hostname. Fastly routes by SNI to the service that owns that domain. So even
if you rewrite `/etc/hosts` or use `--host-resolver-rules`, the SNI still
delivers the request to the production service. The only way to reach a
different upstream while keeping the production hostname in the address bar is
to rewrite the SNI in flight — which requires terminating the browser's TLS
locally.

To terminate TLS, the proxy presents a certificate for the production hostname.
For this to produce a green padlock in Chrome, Firefox, and Safari — and to
satisfy HSTS — the certificate must be signed by a CA the browser trusts. `ts
dev proxy` generates a per-machine Certificate Authority on first run; you trust
it once.

**Primary use case.** Validate routing and behavior of a new or changed Trusted
Server deployment at the publisher's real domain — cookies, `Host`-sensitive
logic, CMP/consent flows, first-party context — before any DNS cutover.

**Non-goals.** Not a production proxy or load-testing tool. Does not modify any
Fastly service. Local only, developer-facing.

## Build and run

`ts dev proxy` is part of `crates/trusted-server-cli`, a native binary excluded
from the workspace (the workspace default target is `wasm32-wasip1`). Build and
run it with an explicit native target:

```bash
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy --help
```

### Passing the rewrite rule

The upstream is always passed explicitly — there is no inference from
`trusted-server.toml` or any config file. Give a single rule with the `-f`/`-t`
shorthand, or one or more `--map FROM=TO` rules:

```bash
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy \
  -f www.example-publisher.com -t trusted-server-example.edgecompute.app
```

With no `--map`/`-f`/`-t`, the proxy exits with
`no rewrite rule: pass --map FROM=TO (or -f/--from with -t/--to)`.

### Explicit rule and browser launch

```bash
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy \
  --map www.example-publisher.com=trusted-server-example.edgecompute.app \
  --launch chrome,firefox,safari
```

`--launch` takes a comma list (`chrome`, `firefox`, `safari`) or `all`. When
omitted the proxy runs without opening any browser.

### Other examples

```bash
# Gated staging upstream, Firefox only:
ts dev proxy \
  -f www.example-publisher.com \
  -t staging.example.net \
  --basic-auth dev:secret \
  --launch firefox

# Local instance over plain HTTP, no browser:
ts dev proxy \
  -f www.example-publisher.com \
  -t localhost:3000 \
  --upstream-plaintext
```

## First run: CA setup

On first run the proxy generates a per-machine Certificate Authority and prints:

```
generated dev CA at ~/Library/Application Support/trusted-server/dev-proxy/ca-cert.pem
— run `ts dev proxy ca install` to trust it
```

The CA key is stored with mode `0600` outside the repository and is never
committed.

### Trust the CA on macOS (Chrome and Safari)

```bash
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy ca install
```

This adds the CA to the macOS login keychain (no `sudo` required; prompts for
your login password). Chrome and Safari both consult the macOS keychain and
will trust the proxy's certificates immediately.

### Trust the CA in Firefox

Firefox does not reliably consult the macOS login keychain. When you use
`--launch firefox`, the proxy automatically imports the CA into the temporary
Firefox profile's NSS database using `certutil`. If you are pointing an existing
Firefox profile at the proxy manually, run:

```bash
certutil -A -n "Trusted Server DEV-ONLY Proxy CA" \
  -t "CT,," \
  -i "$(cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
    --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy ca path)" \
  -d "$HOME/Library/Application Support/Firefox/Profiles/<profile>"
```

### Revoking trust when done

```bash
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy ca uninstall
```

This removes the CA from the macOS keychain. Run it when you are finished —
the CA is trusted for ~10 years and its key sits on disk.

### Security note

The dev CA is a standing MITM capability on your machine. Its key (`ca-key.pem`)
must be treated like a credential:

- It is generated per-machine and never committed to the repository.
- The CA directory has mode `0700` and the key file `0600`.
- The CA CN is `Trusted Server DEV-ONLY Proxy CA — DO NOT TRUST IN PRODUCTION`.
- Trust it only on a development machine you control.
- Run `ca uninstall` when done; run `ca regenerate` to rotate.

## CA companion commands

```bash
ts dev proxy ca path        # print the CA certificate path
ts dev proxy ca install     # trust the CA (macOS login keychain)
ts dev proxy ca uninstall   # remove the CA from the trust store
ts dev proxy ca regenerate  # generate a new CA (invalidates prior trust)
```

`ca path` and `ca install` generate the CA if it does not exist yet, so they
work on a freshly cloned machine before the proxy has been run.

## Host header behavior

By default the proxy sends `Host: <FROM>` (the production hostname) to the
upstream. This is required for Trusted Server core to rewrite first-party URLs
correctly: it anchors all HTML/URL rewriting to the inbound `Host`, so keeping
`Host = FROM` ensures rewritten links stay on the production domain.

This works well against a Trusted Server Compute upstream because Fastly routes
by SNI (`= TO`) and passes `Host` through to the application unchanged.

If your upstream validates or routes on its own hostname, pass `--rewrite-host`:

```bash
ts dev proxy \
  --map www.example-publisher.com=staging.example.net \
  --rewrite-host \
  --launch chrome
```

With `--rewrite-host`, the proxy sends `Host: staging.example.net`. An
`X-Orig-Host: www.example-publisher.com` header is always sent informally.

**Port handling.** When `--rewrite-host` is active and `TO` carries a
non-default port (e.g. `localhost:3000`), the port is included in the `Host`
header but never in the SNI (a bare hostname; a port in SNI is invalid).

## Non-loopback listen

The proxy binds `127.0.0.1:8080` by default. A non-loopback `--listen` is
rejected unless you also pass `--allow-non-loopback`:

```bash
ts dev proxy \
  --map www.example-publisher.com=trusted-server-example.edgecompute.app \
  --listen 0.0.0.0:8080 \
  --allow-non-loopback
```

Even with `--allow-non-loopback`, unmatched `CONNECT` authorities are refused
(`403`) rather than blind-tunneled, so the proxy cannot act as an open CONNECT
proxy on the LAN.

## All options

```
ts dev proxy [OPTIONS] [COMMAND]

Options:
      --map <FROM=TO>           Rewrite rule (repeatable)
  -f, --from <HOST>             Single-rule FROM (pairs with --to)
  -t, --to <HOST[:PORT]>        Single-rule TO (pairs with --from)
      --listen <ADDR>           Listen address [default: 127.0.0.1:8080]
      --allow-non-loopback      Permit non-loopback --listen (disables blind tunnel)
      --launch <LIST>           Browsers to launch (chrome,firefox,safari or all)
      --rewrite-host            Send Host: <TO> instead of the default <FROM>
      --basic-auth <USER:PASS>  Inject Basic auth (visible in ps — prefer --basic-auth-file)
      --basic-auth-file <PATH>  Read USER:PASS from a file
      --insecure                Skip upstream TLS certificate verification
      --upstream-plaintext      Connect to upstream over plain HTTP
      --ca-dir <PATH>           CA cert/key directory [default: ~/Library/Application Support/
                                trusted-server/dev-proxy on macOS]
```

The tool is flags-only; there are no environment variable overrides.

## Browser details

| Browser | How the proxy is configured                                                                                                                                               | CA trust                                        |
| ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| Chrome  | Temp `--user-data-dir`; `--proxy-server="https=127.0.0.1:<port>"` (HTTPS only — plain HTTP goes direct)                                                                   | macOS login keychain via `ca install`           |
| Firefox | Temp profile with `user.js` setting `network.proxy.ssl` (HTTPS only — `network.proxy.http` is unset so plain HTTP goes direct)                                            | CA imported into the profile's NSS DB at launch |
| Safari  | System PAC at `http://127.0.0.1:<port>/proxy.pac` via `networksetup` on the active network service, scoped to the configured `FROM` hosts; prior setting restored on exit | macOS login keychain via `ca install`           |

Safari's system proxy change is system-wide (all apps) while the proxy is
running. On a clean exit the prior setting is restored. After a hard kill (`SIGKILL`)
the next `ts dev proxy` run detects and restores the leftover state, or prints
the manual `networksetup` command.

## Troubleshooting

| Symptom                                        | Cause                                                                                                                                                                                       | Fix                                                                                                                                                                                                |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "unknown domain" or `404` from upstream        | The upstream service does not accept `Host: <FROM>` (the default). A domain can be active on only one Fastly service at a time, so you cannot add the production hostname to a dev service. | Use a Trusted Server Compute upstream (routes by SNI, not `Host`), or pass `--rewrite-host` to send `Host: <TO>`.                                                                                  |
| Upstream returns `401`                         | Upstream is behind Basic auth.                                                                                                                                                              | Pass `--basic-auth user:pass` or `--basic-auth-file ./creds.txt`.                                                                                                                                  |
| Upstream unreachable (`502` / `503`)           | Upstream service is down or the domain is not provisioned.                                                                                                                                  | Verify the upstream URL and its Fastly service health.                                                                                                                                             |
| Browser shows an untrusted-certificate warning | The dev CA is not trusted in the browser.                                                                                                                                                   | Run `ts dev proxy ca install` for Chrome and Safari. For Firefox, use `--launch firefox` (auto-imports) or run `certutil` manually (see above). After `ca regenerate`, re-trust with `ca install`. |
| Listen address already in use                  | Another process holds port 8080.                                                                                                                                                            | Pass `--listen 127.0.0.1:8081` (or another free port).                                                                                                                                             |
| `--listen` rejected as non-loopback            | A non-loopback address was given without the required flag.                                                                                                                                 | Add `--allow-non-loopback`.                                                                                                                                                                        |
