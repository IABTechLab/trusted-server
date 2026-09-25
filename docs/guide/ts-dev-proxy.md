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

## Install and run

`ts dev proxy` supports macOS and Linux. Linux automation supports native
Chrome/Chromium and Firefox. Safari is macOS-only; Windows, Snap and Flatpak
automation are unsupported. Linux commands never use root, change system trust,
or change desktop-wide proxy settings.

Install or update the `ts` binary from the repository root. See
[the CLI guide](./cli.md#install-from-source) for details.

```bash
cargo install-cli
```

Rerun `cargo install-cli` after pulling CLI changes to refresh the installed
binary. Then trust the dev CA once (see
[the development CA](#the-development-ca)), map the production hostname to your
upstream, and launch a browser:

```bash
ts dev proxy ca install
ts dev proxy \
  --map www.example-publisher.com=trusted-server-example.edgecompute.app \
  --rewrite-host \
  --launch chrome
```

`--rewrite-host` sends the upstream `Host: <TO>` so upstreams that reject the
default `Host: <FROM>` — most dev and staging services, which aren't configured
for the production hostname — still serve a page. Trade-off: against a real
Trusted Server adapter, first-party URLs then render on the upstream host, not the
production domain. To keep them on the production domain, point at an upstream that
accepts `Host: <FROM>` and omit `--rewrite-host` — see
[Host header behavior](#host-header-behavior).

Run `ts dev proxy --help` to list every flag.

### Passing the rewrite rule

The upstream is always passed explicitly — there is no inference from
`trusted-server.toml` or any config file. Give a single rule with the `-f`/`-t`
shorthand, or one or more `--map FROM=TO` rules:

```bash
ts dev proxy -f www.example-publisher.com -t trusted-server-example.edgecompute.app
```

With no `--map`/`-f`/`-t`, the proxy exits with
`no rewrite rule: pass --map FROM=TO (or -f/--from with -t/--to)`.

Connection options — `--rewrite-host`, `--basic-auth`/`--basic-auth-file`,
`--insecure`, and `--upstream-plaintext` — apply to every mapping, not per-rule.

### Explicit rule and browser launch

```bash
ts dev proxy \
  --map www.example-publisher.com=trusted-server-example.edgecompute.app \
  --launch chrome,firefox
```

`--launch` takes a comma list (`chrome`, `firefox`, or `safari` on macOS) or `all`.
On Linux, `all` selects Chrome and Firefox; explicit `safari` is rejected. When
omitted the proxy runs without opening any browser. With multiple `--map` rules,
every mapping is proxied, but `--launch` opens only the first rule's `FROM`
hostname — navigate to the others manually.

### Other examples

```bash
# Gated staging upstream, Firefox only:
ts dev proxy \
  -f www.example-publisher.com \
  -t staging.example.net \
  --rewrite-host \
  --basic-auth-file ./staging-creds.txt \
  --launch firefox

# Local instance over plain HTTP, no browser:
ts dev proxy \
  -f www.example-publisher.com \
  -t localhost:3000 \
  --upstream-plaintext
```

## The development CA

The proxy relies on a per-machine Certificate Authority that your browser must
trust. `ts dev proxy ca install` (in the quick start above) generates it if
needed and trusts it in one step, so you don't have to run the proxy first.
Running the proxy also generates the CA on first use, printing:

```
generated dev CA at ~/Library/Application Support/trusted-server/dev-proxy/ca-cert.pem
— run `ts dev proxy ca install` to trust it
```

The CA key is written with mode `0600` and, by default, is stored outside the
repository; it is never committed.

### Trust the CA on macOS (Chrome and Safari)

```bash
ts dev proxy ca install
```

This adds the CA to the macOS login keychain (no `sudo` required; prompts for
your login password). Chrome and Safari both consult the macOS keychain and
will trust the proxy's certificates immediately.

### Trust the CA on Linux (Chrome and Chromium)

Install NSS tools yourself if `certutil` is missing. Package names are
`libnss3-tools` on Debian/Ubuntu, `nss-tools` on Fedora, and `nss` on Arch.
The CLI never installs packages.

```bash
ts dev proxy ca install
ts dev proxy --map www.example.com=staging.example.com --launch chrome
```

`ca install` changes only your shared NSS database, not the system CA bundle.
Native Chrome/Chromium M146 and newer use
`$XDG_DATA_HOME/pki/nssdb`, or `~/.local/share/pki/nssdb` when XDG is unset or
empty. Without a legacy directory, `ca install` rejects a relative
`XDG_DATA_HOME`; unset it or use an absolute path. An existing `~/.pki/nssdb` takes precedence. If both directories exist,
only the legacy directory is selected. The command prints the exact destination.
Restart browsers after trust changes.

A temporary `--user-data-dir` does **not** isolate Chrome's CA trust. NSS trust
also affects other native Chrome/Chromium profiles for your user. The CLI records
every managed destination and certificate identity in `managed-nss-trust.json`
beside the CA, so uninstall still visits previous destinations after XDG or legacy
path selection changes. Keep this record and use the same `--ca-dir` when removing
trust. Do not move or edit the NSS stores or journal while trust is installed.

On Linux the default CA directory is
`$XDG_DATA_HOME/trusted-server/dev-proxy`, or
`~/.local/share/trusted-server/dev-proxy`. An absolute `XDG_DATA_HOME` also overrides
the CA location on macOS. Print the PEM path with `ts dev proxy ca path`.
Manual clients need no persistent browser trust:

```bash
curl --cacert "$(ts dev proxy ca path)" --proxy http://127.0.0.1:18080 https://www.example.com
```

Chrome discovery tries `google-chrome`, `google-chrome-stable`, `chromium`, then
`chromium-browser` on PATH. Firefox requires native `firefox` on PATH. Discovery
skips known Snap/Flatpak executable paths but does not inspect shell wrappers.
On Ubuntu, the transitional Firefox and Chromium packages can install launchers
such as `/usr/bin/firefox` or `/usr/bin/chromium-browser` that start Snap. Resolving
symlinks does not detect that redirection. Confirm that the selected launcher uses
a native installation; Snap/Flatpak wrappers are unsupported and may not honor the
supplied profile or trust configuration.

### Trust the CA in Firefox

Firefox does not reliably consult the macOS login keychain. When you use
`--launch firefox`, the proxy imports the CA into the temporary Firefox profile's
NSS database using `certutil`. `certutil` is not built into macOS — install it
with `brew install nss`, or the Linux package listed above. If import fails,
the proxy prints an error and skips Firefox launch. If you are pointing an existing Firefox profile at the
proxy manually, find its root directory in Firefox's `about:profiles` and replace
`<profile-directory>` below with that full path:

```bash
certutil -A -n "Trusted Server DEV-ONLY Proxy CA — DO NOT TRUST IN PRODUCTION" \
  -t "C,," \
  -i "$(ts dev proxy ca path)" \
  -d "sql:<profile-directory>"
```

`C,,` grants server-certificate CA trust, matching the temporary-profile import.

### Revoking trust when done

```bash
ts dev proxy ca uninstall
```

This removes managed trust from the macOS login keychain or recorded Linux NSS
databases. Run it when you are finished, because
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
ts dev proxy ca install     # trust the CA (login keychain or user NSS)
ts dev proxy ca uninstall   # remove the CA from the trust store
ts dev proxy ca regenerate  # generate a new CA (invalidates prior trust)
```

`ca path` and `ca install` generate the CA if it does not exist yet, so they
work on a freshly cloned machine before the proxy has been run.

`ca regenerate` first removes managed persistent trust, then generates fresh
key material. Missing tools, failed queries or deletions, malformed journals,
and certificate identity conflicts abort rotation without changing the old CA
files. Fix the reported failure and retry; do not delete the journal to bypass it.
Run `ca install` afterward to trust the new CA.

Close launched browsers and stop running proxies **before rotating**. Removal does
not revoke manually imported copies, already-running Firefox temporary profiles,
or CA material already held by a running proxy. Remove external imports yourself.
A CA-directory lock serializes CA commands and initial loading. A second lock in
each shared NSS directory serializes `ts` trust changes across different CA
directories. Neither lock controls external browser or `certutil` writers.

All generated dev CAs share a subject name. Linux `ca install` queries NSS using
the CA file to export the matching-subject certificates, rather than reconstructing
other users' nicknames from NSS's display table. It rejects a different same-subject
certificate before import. Remove the first CA using its original `--ca-dir`, or
resolve an existing manual import, before installing a CA from another directory.
Reinstalling the identical certificate is supported.

A manual NSS nickname equal to the CA certificate's absolute path takes precedence
over the filename query. The CLI rejects that unrelated certificate without changing
trust. Remove the manual entry or reimport it under a different nickname before
retrying.

A manual nickname that visually imitates a managed `ts-dev-proxy-<hash>` entry,
for example by adding a trailing space, can still make NSS's padded listing
ambiguous. Such a collision stops the operation even if the exact managed nickname
is absent. Resolve the manual nickname collision before retrying; the CLI does not
interpret a failed named lookup as proof of absence.

Failed queries, failed exports, and invalid certificate output still stop the
operation. They are not treated as proof of absence or skipped with a warning.
Keep the CA files and journal, and use the command, database path, and NSS error in
the diagnostic to investigate. Check that `certutil` is installed and the reported
store is accessible. Retry a busy operation after the other process exits; an I/O
or unsupported-filesystem-lock error needs its underlying cause fixed instead.
For a damaged NSS store or a manual certificate conflict, coordinate recovery with
whoever manages that store before retrying. Do not delete unrelated certificates,
reset the database, or discard the journal to force rotation. Initializing a new
store may leave an empty database behind if installation later fails, but no
certificate is imported until the subject check succeeds.

## Host header behavior

The proxy always sends `X-Forwarded-Host: <FROM>` (the production hostname) — the
standard "original host" header for a forward proxy. Trusted Server core anchors
all HTML/URL rewriting to it (it prefers `X-Forwarded-Host`, then `Host`), so
**first-party URLs stay on the production domain regardless of the `Host` header —
as long as the upstream preserves `X-Forwarded-Host`**. That decouples routing
(`Host`) from the first-party host. (Real Trusted Server adapters strip inbound
`X-Forwarded-Host`; the caveat below covers what that means with `--rewrite-host`.)

By default `Host: <FROM>` too. Fastly routes by SNI (`= TO`) and passes `Host`
through unchanged, so `Host: <FROM>` reaches the upstream — but the upstream still
has to be configured to accept it, which a dev or staging service usually isn't.

**Targeting a specific server by IP.** To point at a particular server or load
balancer — for example when the `TO` hostname isn't in DNS yet — keep `--to` a
hostname (so the TLS SNI and certificate stay valid) and pin its connection
address with `--resolve HOST:IP` (like curl's `--resolve`, repeatable):

```bash
ts dev proxy \
  --from www.example-publisher.com \
  --to ts.example-publisher.com \
  --resolve ts.example-publisher.com:192.0.2.10 \
  --launch chrome
```

The proxy dials `192.0.2.10` while the SNI stays `ts.example-publisher.com` and
`X-Forwarded-Host` stays `www.example-publisher.com` — so TS rewrites first-party
URLs onto the production domain. This keeps the tool self-contained — no
`/etc/hosts` edit. (Pointing `--to` at a bare IP instead would make the SNI an IP,
which sends no SNI extension at all, so a host-routed endpoint serves its default
vhost.) Add `--insecure` if the endpoint serves a certificate that doesn't match
the hostname.

**Sending `Host: TO`.** If your upstream routes or validates on its _own_
hostname (e.g. a Fastly Deliver service that rejects an unconfigured `Host`), pass
`--rewrite-host` to send `Host: <TO>`. The proxy still stamps
`X-Forwarded-Host: <FROM>`, so first-party URL rewriting stays anchored to `FROM`
**as long as the upstream preserves that header**.

> **Caveat with real Trusted Server adapters.** The Fastly and Spin adapter
> request paths strip inbound `X-Forwarded-Host` before routing, so with
> `--rewrite-host` a real Trusted Server upstream falls back to `Host` (`TO`) and
> emits first-party URLs on `TO`, not `FROM`. Even so, `--rewrite-host` is the
> right choice for most upstreams — dev and staging services rarely have the
> production hostname configured and would reject the plain `Host: <FROM>`. Drop
> it only when the upstream is configured to accept `Host: <FROM>` and you
> specifically need first-party URLs anchored to `FROM` — the `--resolve` example
> above is exactly that case.

The TLS SNI is always the `TO` host either way:

| Form             | `Host` header | `X-Forwarded-Host` | TLS SNI   |
| ---------------- | ------------- | ------------------ | --------- |
| _(omitted)_      | `FROM`        | `FROM`             | `TO` host |
| `--rewrite-host` | `TO` host     | `FROM`             | `TO` host |

**Port handling:** with `--rewrite-host` and a non-default `TO` port (e.g.
`localhost:3000`), the port is included in the `Host` header but never in the SNI
(a bare hostname; a port in SNI is invalid).

## Non-loopback listen

The proxy binds `127.0.0.1:18080` by default. A non-loopback `--listen` is
rejected unless you also pass `--allow-non-loopback`:

```bash
ts dev proxy \
  --map www.example-publisher.com=trusted-server-example.edgecompute.app \
  --listen 0.0.0.0:18080 \
  --allow-non-loopback
```

Even with `--allow-non-loopback`, unmatched `CONNECT` authorities are refused
(`403`) rather than blind-tunneled, so the proxy cannot act as an open CONNECT
proxy on the LAN.

## All options

Read the complete option list from the installed version:

```bash
ts dev proxy --help
ts dev proxy ca --help
```

Proxy routing is flags-only. CA and Linux NSS paths honor HOME and absolute
XDG_DATA_HOME as described above. For certificate management, see
[CA companion commands](#ca-companion-commands).

## Browser details

| Browser | How the proxy is configured                                                                                                                                                                                                 | CA trust                                                |
| ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| Chrome  | Temp `--user-data-dir`; `--proxy-server="https=127.0.0.1:<port>"` (HTTPS only — plain HTTP goes direct)                                                                                                                     | macOS login keychain or Linux user NSS via `ca install` |
| Firefox | Temp profile with `user.js` setting `network.proxy.ssl` (HTTPS only — `network.proxy.http` is unset so plain HTTP goes direct)                                                                                              | CA imported into the profile's NSS DB at launch         |
| Safari  | System PAC at `http://127.0.0.1:<port>/proxy.pac` via `networksetup` on the active network service, scoped to the configured `FROM` hosts; then opens Safari at the first rule's `FROM` URL; prior setting restored on exit | macOS login keychain via `ca install`                   |

Unlike Chrome/Firefox (which run in a throwaway profile), Safari uses your
**system** proxy settings, so `--launch safari` sets the macOS auto-proxy on the
active network service (e.g. Wi-Fi) and then opens Safari at the `FROM` URL.

Changing the system network proxy requires admin, so `--launch safari` runs the
`networksetup` command under `sudo` — it prompts **once** for your password in
the terminal (only that command is elevated; the proxy keeps running as you). If
`sudo` is declined or there is no terminal (e.g. the proxy is backgrounded), it
prints the exact `networksetup` command and the System Settings path so you can
set the PAC manually. The change is system-wide (all apps) but PAC-scoped to the
`FROM` hosts, and — on a clean exit — only while the proxy runs. The prior setting — including
whether auto-proxy was enabled or disabled — is saved and restored. On a clean
exit (Ctrl-C) the restore uses `sudo` and may prompt once more if a long run
outlived the cached credential. After a hard kill (`SIGKILL`) the next
`ts dev proxy` run restores the leftover state non-interactively (`sudo -n`), or,
if it can't, keeps the saved state and prints the exact manual `networksetup`
command.

## Troubleshooting

Mapped HTTP/1.1 upstream connections are reused automatically. With debug
logging enabled, a clean shutdown prints an aggregate, redacted proxy metrics
summary that can help distinguish DNS, connection, TLS, and pool wait latency;
it never includes request URLs, headers, credentials, or certificate contents.

| Symptom                                        | Cause                                                                                                                                                                                       | Fix                                                                                                                                                                                                                                                  |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "unknown domain" or `404` from upstream        | The upstream service does not accept `Host: <FROM>` (the default). A domain can be active on only one Fastly service at a time, so you cannot add the production hostname to a dev service. | Use a Trusted Server Compute upstream (routes by SNI, not `Host`), or pass `--rewrite-host` to send `Host: <TO>`.                                                                                                                                    |
| Upstream returns `401`                         | Upstream is behind Basic auth.                                                                                                                                                              | Pass `--basic-auth user:pass` or `--basic-auth-file ./creds.txt`.                                                                                                                                                                                    |
| Upstream unreachable (`502` / `503`)           | Upstream service is down or the domain is not provisioned.                                                                                                                                  | Verify the upstream URL and its Fastly service health.                                                                                                                                                                                               |
| Browser shows an untrusted-certificate warning | The dev CA is not trusted in the browser.                                                                                                                                                   | Run `ts dev proxy ca install` for Chrome and Safari. For Firefox, use `--launch firefox` (auto-imports when `certutil` is installed — `brew install nss`) or run `certutil` manually (see above). After `ca regenerate`, re-trust with `ca install`. |
| Listen address already in use                  | Another process holds port 18080.                                                                                                                                                           | Pass `--listen 127.0.0.1:18081` (or another free port).                                                                                                                                                                                              |
| `--listen` rejected as non-loopback            | A non-loopback address was given without the required flag.                                                                                                                                 | Add `--allow-non-loopback`.                                                                                                                                                                                                                          |

## Isolated Linux browser proof

After `cargo build_cli_linux`, run the disposable browser check from the repository
root. It requires an installed native Chrome/Chromium and `certutil`, but never
changes your real HOME, profiles, CA, or NSS databases.

```bash
python3 scripts/test-linux-dev-proxy-browser.py \
  --browser chromium --layout xdg --logs /tmp/ts-browser-proof
```

Repeat with `--layout default` and `--layout legacy` to check path selection.
The check uses browser HTTPS through the proxy to a local HTTP fixture. It verifies
missing trust fails, installed trust loads, revoked trust fails after restart,
and plain HTTP bypasses a stopped proxy. It never disables the browser sandbox or
bypasses certificate errors. This check does not prove Firefox or packaged-browser
support.

### Linux NSS trust regression tests

The real-NSS tests require `certutil` and OpenSSL. They use disposable HOME, CA,
and NSS paths and do not read or change the user's NSS database.

```bash
cargo test_cli_linux --test proxy_trust_linux -- --include-ignored
```
