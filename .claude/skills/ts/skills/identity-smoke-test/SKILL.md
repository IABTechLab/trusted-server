---
name: identity-smoke-test
description: Use when smoke-testing Trusted Server identity (Edge Cookie `ts-ec`, the `__Host-ts-eids-mirror` EID mirror, CMP consent sync to `/_ts/consent`) on a deployed service through `ts dev proxy` and Chrome DevTools, or when asked whether a deployment's identity behavior matches its branch.
---

# Identity smoke test through `ts dev proxy`

## Overview

Drive a throwaway Chrome through `ts dev proxy` against a deployed Trusted Server, record its document and `/_ts/` traffic over the Chrome DevTools Protocol, and compare each page view with the identity lifecycle the deployed branch documents. Read cookies from raw protocol events, never from `document.cookie` or the DevTools request list, and never print identity values.

## 1. Ask for the inputs

Ask only for what the user has not already given. Ask for file paths, never for secret values.

| Input                    | Proxy flag                                                                        | Notes                                                                                                                         |
| ------------------------ | --------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Publisher hostname       | `--map <FROM>=<TO>` (FROM)                                                        | The hostname the browser shows, for example `www.publisher.example`                                                           |
| Deployment host          | `--map <FROM>=<TO>` (TO, `HOST[:PORT]`)                                           | Keep it a hostname; pin an address with `--resolve HOST:IP`                                                                   |
| Send `Host: <TO>`?       | `--rewrite-host`                                                                  | Usually yes: most deployments reject `Host: <FROM>`                                                                           |
| Forwarding token file    | `--forwarder-secret-file <path>`                                                  | When the service has `[trusted_forwarder]` and its front door does not supply the public host                                 |
| Upstream basic auth file | `--basic-auth-file <path>`                                                        | `USER:PASS` in a file                                                                                                         |
| Anything unusual         | `--listen`, `--ca-dir`, `--insecure`, `--upstream-plaintext`, `--connect-timeout` | Defaults: listen `127.0.0.1:18080`, per-machine CA                                                                            |
| Expected build           | none                                                                              | Commit or branch; compared with `x-ts-version`                                                                                |
| Operator config          | none                                                                              | Path to the deployment's `trusted-server*.toml`; read it locally for CMP, Prebid mode, `ec.partners`, `ec.identity_providers` |
| A test article URL       | none                                                                              | Optional; otherwise pick a same-site article link from the homepage                                                           |

Read the config with secret-looking lines filtered out (`grep -viE 'secret|passphrase|token|password|auth|bearer|key\s*='`). Note:

- the consent integration and whether it supplies GPP, TCF or both;
- the `[[ec.partners]]` with `bidstream_enabled` and `mirror_enabled`, and the `[[ec.identity_providers]]` that own each `source_domain`. Only provider-owned UIDs are mirrored;
- `ec.max_age_days`, `ec.sliding_refresh` and `ec.browser_gate`;
- partner cookie names, which you pass to `capture.mjs --cookie`.

## 2. Set up

1. **Free ports.** `lsof -nP -iTCP:18080 -iTCP:9222 -sTCP:LISTEN` must print nothing. Otherwise the scripts can attach to another Chrome, one that bypasses the proxy.
2. **CA trust.** `cargo run -p trusted-server-cli -- dev proxy ca path` prints the CA. If Chrome does not trust it yet, ask the user to run `cargo run -p trusted-server-cli -- dev proxy ca install`, which prompts for the keychain or NSS password. Later, a page URL of `chrome-error://…` in `probe.mjs state` means Chrome still does not trust it.
3. **Proxy, without `--launch`.** `--launch chrome` starts a Chrome with no debugging port, which automation cannot attach to. Run this in the background from the repository root, then wait for `listening on 127.0.0.1:18080`:

   ```bash
   cargo run -p trusted-server-cli -- dev proxy --map <FROM>=<TO> --rewrite-host [other flags]
   ```

   `cargo run_cli_macos` and `cargo run_cli_linux` are equivalent aliases.

4. **Baseline through the proxy.**

   ```bash
   curl -s --proxy http://127.0.0.1:18080 --cacert "<ca path>" -o /dev/null -D - \
     -H 'Sec-Fetch-Dest: document' -H 'Accept: text/html' https://<FROM>/
   ```

   Check `x-ts-version` against the expected build. It can be a tag, a branch name or a short SHA, not always the full commit. Read `x-geo-info-available`, `x-geo-country` and `x-geo-region`: they decide the jurisdiction for the whole run. Stop and report in either of these cases:

- `/health` does not answer `200`, or `/` or `/ready` answers `503`. That is the startup-failure router.
- `/` answers `401`. The site needs `--basic-auth-file`.

If the branch has `deploy/examples/native-aws-workload/DEPLOYMENT.md`, read its smoke-test section for deployment-specific steps such as site-wide basic auth.

5. **Chrome with the proxy and a debugging port**, in the background, with a profile in the session scratch directory:

   ```bash
   "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
     --user-data-dir="<scratch>/identity-smoke-chrome" --no-first-run --no-default-browser-check \
     --proxy-server="https=127.0.0.1:18080" --remote-debugging-port=9222 about:blank
   ```

   On Linux use `google-chrome` with the same flags. `curl -s http://127.0.0.1:9222/json/version` confirms it is up.

6. **Scripts.** `scripts/` in this skill needs Node 22 or newer. Run them as `node <skill dir>/scripts/<name>.mjs`.
   - **`reset.mjs --origin https://<FROM> [--origin https://<CMP host>]`** turns the browser into a fresh visitor. It clears all cookies, plus the storage of the given origins, so stored consent goes too.
   - **`capture.mjs --host <FROM> [--cookie <name> ...] <url|reload|watch> [settleMs]`** records one page view: the document and the `/_ts/`, `/first-party/`, `/integrations/` and `/auction` traffic. It prints:
     - timing;
     - the identity cookies each request sent (`?` when Chrome reported no cookie header);
     - the `Set-Cookie` names, marked `EXPIRED` when `Max-Age=0`;
     - the final cookie jar.

     `watch` records without navigating. Run the next command only after a `watch` exits.

   - **`probe.mjs --host <FROM> <state|eids|gpp|pbjs|links>`** reads one of: the injected tsjs config and public origin; the EID sources in the mirror and `ts-eids`; the CMP's own GPP state; the EID sources each Prebid auction carried; or same-site article links for phase 3.

## 3. Run the phases

Before you start, read the expected behavior from the deployed branch: `docs/guide/edge-cookies.md` (lifecycle, finalization, consent model, mirror cookie), `docs/guide/integrations/prebid.md` (identity mode) and the guide for the enabled CMP. Where they disagree with code, the code wins; check `crates/trusted-server-core/src/ec/` and the CMP's tsjs module under `crates/trusted-server-js/lib/src/integrations/`.

| Phase                        | Do                                                                                                                 | Expect                                                                                                                                                                                                                                                                                                                               |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1. Fresh visitor, first page | `reset.mjs`, then `capture.mjs https://<FROM>/`, `probe.mjs state`                                                 | No `ts-ec` and no mirror (no consent signal yet). `publicOrigin` is `https://<FROM>` (the deployment host means host forwarding failed); `window.__tsjs_prebid` matches the config's mode; tsjs and bundle scripts load from `/static/tsjs=` and `/integrations/`                                                                    |
| 2. Consent signal            | GDPR: accept in the CMP banner (section 4). US state: the CMP's default GPP. Non-regulated: nothing to do          | GPP: `__gpp`, `__gpp_sid` and `_ts_gpp_src` appear, and `POST /_ts/consent` answers `204` with no `ts-ec` (`403`: origin or host mismatch; `503`: no record store; `200`: the publisher answered, not TS). TCF: `euconsent-v2` or `__Host-ts-tcf` appears and nothing is posted; the server reads it on the next request             |
| 3. Second page view          | `capture.mjs <article URL>` (from `probe.mjs links`), `probe.mjs eids`                                             | `ts-ec` set with `Domain=.<publisher.domain>; HttpOnly; Secure; SameSite=Lax` and `ec.max_age_days`; `__Host-ts-eids-mirror` set with provider-owned sources in `[[ec.partners]]` order, each `exp` within the cap. Sources from after-response providers may only land on the next page                                             |
| 4. Return visit              | `capture.mjs reload`, `probe.mjs eids`, `probe.mjs pbjs`                                                           | The same `ts-ec` prefix is sent and not re-minted (with `ec.sliding_refresh = true`, a re-set with the same prefix is expected); the mirror is still in the final jar and no response expired it; auctions carry the mirrored sources, including partners only the server resolves. No `auctionInit` means "not covered", not a fail |
| 5. Withdrawal                | Start `capture.mjs watch 60000`, opt out in the CMP (section 4), then, after the watch exits, `capture.mjs reload` | GPP: the opt-out `POST /_ts/consent` answers `204` with `ts-ec … EXPIRED` (`204` means the tombstone was stored; `503` means it was not). TCF: nothing is posted, and the reload's document expires `ts-ec`. Either way the next page sets no `ts-ec` and no mirror                                                                  |

Jurisdiction: `x-geo-info-available: false` is Unknown and fails closed; GB or EU is GDPR (TCF Purpose 1, plus Purpose 4 for the mirror); a US privacy state follows GPP, US Privacy and `Sec-GPC`; anything else is non-regulated, where `ts-ec` can arrive on the first page view. A US vantage point cannot test the GDPR path: say so rather than guessing. With `ec.browser_gate = "strict"`, EC creation needs a trusted JA4 fingerprint, which a front door that does not forward one never supplies, so no phase mints an EC.

To tell "the mirror was never set" from "it was set, then removed", use the `Set-Cookie` timeline and the final jars:

- **Set, then removed by the server:** an `EXPIRED` mirror on any response proves the cookie existed. Name that response and the request body just before it.
- **Set, then removed by script:** a positive set that later vanishes from the jar with no expiry.
- **Never set:** no positive set, no `BLOCKED` entry and no expiry in any capture. Look for the cause in the config: a partner that is not mirrored, a source with no owning provider, or a missing TCF Purpose 4.

Known regression on `rc/identity` `c3e336666` (#1678): every page after the first posts `{"gppString":null}` between two real snapshots, and that response expires the mirror, so phase 4 fails. Report it as #1678 if it still reproduces.

## 4. Drive the CMP

Either drive it with the Chrome DevTools CLI attached to the same browser, or ask the user to click while `capture.mjs watch 180000` records. Use one driver at a time: two people toggling the same switch cancel out.

```bash
npx -y -p chrome-devtools-mcp@1.14.0 chrome-devtools start --browserUrl http://127.0.0.1:9222
npx -y -p chrome-devtools-mcp@1.14.0 chrome-devtools take_snapshot <pageId>
npx -y -p chrome-devtools-mcp@1.14.0 chrome-devtools click <pageId> <uid>
```

- Open the privacy manager from the page's own link or button (for example "Do not sell or share", "Privacy settings"); find it with `probe.mjs state` or a snapshot.
- A new snapshot renumbers every `uid`: take one snapshot, then click.
- A click on a switch may only focus it. Snapshot again and check whether `checked` changed; if not, `press_key <pageId> Space`.
- Confirm the saved choice with `probe.mjs gpp` (GPP US sections: `SaleOptOut` `1` means opted out, `2` means not) or the TCF string, not with the dialog.

## 5. Report

One table: phase, expected, observed (with timestamps and cookie names and attributes from `capture.mjs`), pass or fail. Then list what the run could not cover: jurisdictions the vantage point cannot reach, record-store contents, and the JA4 browser check (the store sees the proxy's TLS stack, not Chrome's).

Keep publisher names, hostnames, URLs and config values out of anything shared: issues, PRs, commits and docs. The scripts already reduce cookie values to lengths.

## 6. Clean up

- Run `chrome-devtools stop` if you used the CLI.
- Stop Chrome.
- Stop the proxy with an interrupt (Ctrl-C or `SIGINT`), not `SIGKILL`.
- Remove the profile directory and any capture files.
- Run `ts dev proxy ca uninstall` only if this run installed the CA.
- Run the `lsof` check again to confirm both ports are free.

## Common mistakes

| Mistake                                                                  | Fix                                                                                                                                    |
| ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| `ts dev proxy --launch chrome`                                           | Launch Chrome yourself with `--remote-debugging-port`                                                                                  |
| Using the session's DevTools MCP browser                                 | It has no proxy; attach to the proxied Chrome instead                                                                                  |
| Reading `Set-Cookie` from the DevTools request list                      | Repeated headers are folded into one value; use `capture.mjs`                                                                          |
| Listing requests on a heavy page                                         | The list keeps the last 1000, so the document drops out; use `capture.mjs`                                                             |
| Looking for `ts-ec` in `document.cookie`                                 | It is `HttpOnly`; read the jar from `capture.mjs`                                                                                      |
| Opening `http://` URLs                                                   | The proxy only carries HTTPS (`--proxy-server=https=…`); plain HTTP goes straight to production                                        |
| Calling "no mirror" a missing provider UID without checking the timeline | Run `capture.mjs reload`: an `EXPIRED` mirror on a `/_ts/consent` response after the document set it is a regression, not "no IDs yet" |
| Calling "no EC" after the first page view                                | The EC arrives on the next navigation once a consent signal exists                                                                     |
| Running DevTools performance tools                                       | They can send page URLs to the CrUX API; do not use them on publisher pages                                                            |
