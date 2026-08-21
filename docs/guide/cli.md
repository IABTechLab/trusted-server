# Trusted Server CLI

The Trusted Server CLI binary is `ts`. It is a host-target operator tool for
configuration, page audits, and EdgeZero-backed lifecycle commands.

## Install from source

From the repository root, install the `ts` binary with the workspace Cargo alias:

```bash
cargo install-cli
```

The alias runs `cargo install --path crates/trusted-server-cli --bin ts --locked --force`.
Because it does not pass an explicit `--target`, Cargo builds the CLI for your
current host platform. The binary is installed into Cargo's bin directory,
usually `~/.cargo/bin`; make sure that directory is on your `PATH`.

For example, add Cargo's bin directory to your current shell session:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Verify the install:

```bash
ts --help
```

## Common workflow

```bash
ts config init
# Edit trusted-server.toml
ts config validate
ts auth login --adapter fastly
ts provision --adapter fastly
ts config push --adapter fastly
ts serve --adapter fastly
```

## Configuration commands

Create a starter Trusted Server config:

```bash
ts config init
```

`config init` accepts `--app-config <path>` and the compatibility alias
`--config <path>`.

Validate a local config before pushing it to platform storage:

```bash
ts config validate
```

Push Trusted Server config through EdgeZero:

```bash
ts config push --adapter fastly
```

`config validate`, `config diff`, and `config push` use EdgeZero's typed
app-config loader. By default that loader applies `TRUSTED_SERVER__...`
environment overlays before validation, comparison, and blob creation. EdgeZero
v0.0.4 only overrides leaves already present in the TOML; add newly introduced
fields to existing configs before relying on their overrides. Pass `--no-env`
for file-only operation. See [Configuration](/guide/configuration#environment-variable-overrides-typed-cli)
for migration and rollback guidance.

`config push` publishes a single EdgeZero `BlobEnvelope` containing the validated
Trusted Server settings JSON. This blob model is intentional because full
Trusted Server configs can exceed Fastly limits when split into one config-store
entry per setting.

### Diagnose ad-template configuration

The static `ts config ad-templates` commands evaluate local configuration
without launching a browser:

| Command                                  | Purpose                                                                   |
| ---------------------------------------- | ------------------------------------------------------------------------- |
| `lint`                                   | Summarize configuration and report invalid slot page patterns.            |
| `match <path-or-url> [--details]`        | List matching slots; `--details` includes divs, paths, formats/providers. |
| `check <path-or-url> --expected-slot ID` | Assert the exact matching slot set; repeat `--expected-slot`.             |
| `check <path-or-url> --expect-no-slots`  | Assert that no slots match.                                               |
| `explain <path-or-url>`                  | Print every runtime ad-stack gate and its final yes/no verdict.           |

`check --allow-extra-slots` permits matches beyond the repeated
`--expected-slot` values. It conflicts with `--expect-no-slots`.

`explain` models a GET navigation with consent allowed by default. Use
`--method <METHOD>`, `--non-navigation`, `--prefetch`, `--bot`, or
`--consent-denied` to model another request. Provider configuration is printed
as a separate advisory; it does not change the runtime gate verdict.

Every `ts config ad-templates ...` and `ts audit ad-templates ...` command
accepts the same config-location flags:

| Flag                  | Behavior                                                                      |
| --------------------- | ----------------------------------------------------------------------------- |
| `--app-config <PATH>` | Read this app config instead of deriving `<app.name>.toml` from the manifest. |
| `--manifest <PATH>`   | Read this manifest; defaults to `edgezero.toml`.                              |
| `--no-env`            | Disable `TRUSTED_SERVER__...` overlays for read-only commands.                |

The mutating audit generator always edits file-backed values and never writes
environment-only overlays into TOML, including during `--dry-run`.

For CI-oriented assertions, exit code 0 means the assertion passed, 1 means the
command ran and found drift (`config ad-templates check` or audit verification
with `--strict`), and 2 means argument parsing, configuration, browser launch,
or another tool operation failed.

## Lifecycle commands

Lifecycle commands delegate to the selected EdgeZero adapter:

```bash
ts auth login --adapter fastly
ts build --adapter fastly
ts provision --adapter fastly
ts deploy --adapter fastly
ts serve --adapter fastly
```

## Audit a public page

`ts audit` loads a public page in a fresh headless Chrome/Chromium session,
collects rendered JavaScript asset evidence, detects known Trusted Server
integrations, and writes local draft artifacts.

Chrome or Chromium must be installed locally. The command checks common PATH
names and standard macOS/Linux install locations.

```bash
ts audit generate https://publisher.example
```

By default, the command writes:

| File                  | Purpose                                                                  |
| --------------------- | ------------------------------------------------------------------------ |
| `js-assets.toml`      | JavaScript asset inventory, detected integrations, counts, and warnings. |
| `trusted-server.toml` | Draft Trusted Server config based on the starter template and final URL. |

The generated config is a draft. Review it, replace placeholders/secrets, adjust
publisher-specific settings, then run:

```bash
ts config validate
```

If a config already exists, avoid overwriting it:

```bash
ts audit generate https://publisher.example --no-config
```

Use custom output paths when reviewing artifacts first:

```bash
ts audit generate https://publisher.example \
  --js-assets audit/js-assets.toml \
  --config audit/trusted-server.toml
```

Use `--force` only when replacing existing output files is intentional:

```bash
ts audit generate https://publisher.example --force
```

The legacy `ts audit <url>` form remains a compatibility alias for artifact
generation. New automation should use `ts audit generate <url>`.

## Generate ad-template slots from a live site

`ts audit ad-templates generate <url>` discovers the publisher's ad slots and
rewrites the `[creative_opportunities]` slot array in `trusted-server.toml` in
place, preserving every other section and comment.

```bash
ts audit ad-templates generate https://publisher.example/
```

It samples the site rather than a single page. Ad slots repeat per site
section, so the crawl is sized by the publisher's taxonomy — a dozen sections —
not its catalogue:

1. Load the requested page and read its links and, from `robots.txt`, its
   sitemap.
2. Group both into candidate sections, keeping one landing page and one article
   per section.
3. Load those pages, recording each slot's div, sizes, and GAM ad-unit path.
4. Reconcile every slot across the pages it appeared on.
5. Infer a `{section}` ad-unit template if the evidence proves one.
6. Verify the result loads, then write it.

### What it writes

Given a site whose ad units track the section, the run produces:

```toml
[creative_opportunities]
enabled = true
gam_network_id = "99999"
section_root = "homepage"
section_segment = 0

[[creative_opportunities.slot]]
id = "ad-header-0"
div_id = "ad-header-0"
gam_unit_path = "/{network_id}/example/{section}"
page_patterns = ["/", "/deals", "/deals/*", "/news", "/news/*"]
formats = [{ width = 728, height = 90 }]
```

Each section contributes **two** patterns. `*` crosses `/` in this glob
dialect, so `/news/*` matches `/news/a/b` but not the bare `/news` landing
page; emitting only the star form would drop the landing page from the slot.

Sizes are unioned across pages, so a format that renders only on articles
survives alongside the homepage's.

### When it keeps literal paths, and when it refuses

A wrong ad-unit template makes the publisher bid against inventory that does not
exist, so the command prefers a narrow literal path over a plausible guess.

| Situation                                                                               | Result                                                                                                                                                                                                                        |
| --------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Only one page was crawled                                                               | Literal path. One observation cannot distinguish a literal from a template.                                                                                                                                                   |
| The ad unit never varied by section                                                     | Literal path.                                                                                                                                                                                                                 |
| A section's slug is not derivable from its URL (`/site-news` requesting `.../sitenews`) | The slot is omitted; the note lists the ad-unit paths it used and says none generalized.                                                                                                                                      |
| No crawled page lacked a section segment, so `section_root` is unwitnessed              | No template is written and the reason names the crawl gap. A slot that merely never appears on the root (a sidebar, an in-article unit) still templates, borrowing the `section_root` another slot witnessed; a note says so. |
| Two path segments could both be the section                                             | No template; the ambiguity is reported.                                                                                                                                                                                       |
| The ad unit varies by device, geo, or anything the URL cannot supply                    | The refused slot is omitted and the reason is written as a note.                                                                                                                                                              |
| Crawled pages report different GAM network ids                                          | The run fails; the pages are not one property.                                                                                                                                                                                |
| More than a quarter of crawled pages return no slots                                    | The run fails. That is the signature of bot protection serving challenge pages, and writing from it would silently narrow the slot set.                                                                                       |
| Several live elements normalize onto one div-id prefix                                  | The whole group is omitted, on every page of the crawl. A prefix resolves to at most one element and the exact ids change per render; the prefix is named in a note.                                                          |
| A per-render token sits before the placement part of a div id                           | The slot is omitted from a single observation and the family prefix is named in a note; no stable prefix identifies one element.                                                                                              |

Every run checks that the config it produced still loads before replacing the
file, and `--dry-run` runs the same check — a clean preview is evidence the
config loads, not just that it parses. Dry-run stdout is a zero-context unified
diff containing only the managed creative-opportunity fields; notes and refusal
reasons go to stderr, so unrelated config and secrets are not printed. Crawl
progress also goes to stderr, one line per phase and page — for example
`Auditing desktop [2/17]: /news`. Progress renders the path only, never the
origin, userinfo, query, or fragment, and there is no flag to suppress it. A
`--dry-run` that changes nothing says so on stderr too, leaving stdout an empty
diff.

### Bounding and steering the crawl

```bash
# Cover more of a large site.
ts audit ad-templates generate https://publisher.example/ --max-sections 20 --max-pages 41

# Audit exactly one page, as earlier releases did.
ts audit ad-templates generate https://publisher.example/ --max-pages 1

# Set the patterns yourself; this disables pattern inference entirely.
ts audit ad-templates generate https://publisher.example/ \
  --page-pattern '/' --page-pattern '/news' --page-pattern '/news/*'

# Preview without writing.
ts audit ad-templates generate https://publisher.example/ --dry-run
```

Re-running merges into the existing slots: a slot seen again keeps its
hand-tuned fields and gains this run's patterns and newly observed formats, and a hand-written
`gam_unit_path` template is preserved. `--replace` discards existing slots
instead, which also discards any template you wrote by hand.

A merge refuses to change the section policy that preserved `{section}` slots
were written against: if the config already sets `section_root` (or
`section_segment`) and this run infers different values, the run fails and asks
for `--replace` as an explicit migration. A config whose `{section}` slots have
no `section_root` at all is a different case — the runtime rejects such a file
outright — so the first merge adopts the inferred policy and makes it loadable
instead of demanding `--replace`.

Locale-prefixed sites are inferred at their observed section depth. Only real
ISO 639-1 language codes are read as a locale prefix, so a two-letter _section_
root such as `/tv` or `/us` keeps sections at the first segment. For
example, `/en/news/story` can produce `section_segment = 1`; generated patterns
retain the locale prefix (`/en/news` and `/en/news/*`). The crawler never
invents an unwitnessed locale or section.

Behind bot protection, pass a valid clearance cookie. The crawl reuses one
browser session, so clearance earned on the first page carries to the rest, and
`--page-delay-ms` spaces the requests — an unpaced crawl is both discourteous to
the origin and likelier to be challenged partway through:

```bash
ts audit ad-templates generate https://publisher.example/ \
  --cookie '<NAME>=<VALUE>' --page-delay-ms 1500
```

Some origins refuse a headless browser outright regardless of the cookie.
`--headful` runs a visible one, which is also the quickest way to _see_ whether
a challenge is being shown:

```bash
ts audit ad-templates generate https://publisher.example/ --headful
```

### Sites behind a consent platform

Publishers gate slot definition behind their consent platform, and the audit
runs in a throwaway browser profile with no consent cookie. Left alone, such a
site defines no slots at all and looks identical to a site with no ad stack.

The crawl therefore answers the two IAB interfaces every compliant platform
exposes — TCF v2 and US Privacy — as a consenting, out-of-scope reader, before
any page script runs. This changes only what the audit browser sees; it does not
affect the publisher's own readers. Pass `--no-assume-consent` to observe the
un-consented page instead.

When a page still yields no slots, the run reports GPT's observable state —
whether the library reached `apiReady`, how many queued commands never drained,
how many scripts ran. An empty slot registry has several very different causes,
and that line distinguishes them.

### Auditing a production hostname served locally

`ts dev proxy` serves a production hostname from a local Trusted Server.
Auditing through it keeps the page's origin, cookie scope, and any origin checks
in the ad stack matching production rather than `localhost`:

```bash
ts dev proxy --map www.publisher.example=127.0.0.1:7676 --upstream-plaintext --rewrite-host

ts audit ad-templates generate https://www.publisher.example/ \
  --browser-proxy 127.0.0.1:18080 --danger-accept-invalid-certs
```

`--danger-accept-invalid-certs` covers the proxy's MITM certificate when the
throwaway browser profile does not trust its CA; installing that CA
(`ts dev proxy ca`) is preferable. Against a real origin the flag is dangerous —
the audit sends any `--cookie` session upstream and treats the response as
evidence, so an invalid certificate could mean an impersonator is both
harvesting the session and fabricating the result.

Note that a local Trusted Server injects its own configured slots into the page,
so a run through the proxy can rediscover config it already has. Slot ids that
are absent from the current config are the publisher's own.

### Slots that change div id on every render

Some ad stacks build div ids from a per-render token, so one placement arrives
under a new id on every page. Those ids match nothing at runtime, so the run
declines to write them and reports the group instead:

```text
note: skipped 3 slot(s) that look like one placement under a per-render div id
      on `/123456789/publisher/overlay` (ex_slot_a1_overlay_1, …);
      they share the prefix `ex_slot`. Add it once by hand with a div_id prefix
      that is stable across renders
```

The detection is by evidence, not by recognising token shapes: candidates share
an ad-unit path and formats, and what separates a fragmented placement from two
legitimate siblings on one unit is co-occurrence — real siblings appear together
on a page, fragments never do. The suggested prefix is a starting point only, not
written as a `div_id`, because it reaches only as far as the observed tokens
happen to agree.

### Checking for a device split

Publishers often serve a different ad unit per device
(`/network/desktop/news` against `/network/mobile/news`). A desktop-only crawl
cannot see that — it infers a template correct for desktop and silently wrong
for every mobile impression.

```bash
ts audit ad-templates generate https://publisher.example/ --profiles desktop,mobile
```

Each page is loaded once per profile. Where the profiles disagree, the slot is
omitted and the diagnostic explains the conflicting paths. The generator does
not fall back to a fabricated default ad unit.

### Deploy ordering for templated config

> **A config containing `section_root` or `section_segment` is not
> rollback-safe.** These keys are rejected outright by a Trusted Server binary
> that predates ad-unit templating, and the rejection fails the _entire_
> configuration load — not just the ad-template section — so every route serves
> an error. This is a full-site outage, not a degraded ad stack.

When a run reports that it wrote a `{section}` template:

1. Deploy the template-aware binary **first**.
2. Then `ts config push`.
3. Do **not** roll that binary back while the config is live.

A run that did not template writes neither key, and leaves the config exactly as
rollback-safe as it was.

### Audit safety defaults

Every `ts audit` browser session validates TLS certificates. This matters
because `--cookie` sends a real session to the origin and the page's own
response becomes the audit's evidence, so a certificate-invalid host could both
harvest the session and fabricate what the audit reports. Override only for a
host you control with a known self-signed certificate:

```bash
ts audit page https://staging.publisher.example --danger-accept-invalid-certs
```

`ts audit ad-templates verify` matches configured slots against the
**post-redirect** path, so it refuses a redirect that leaves the requested
origin rather than accepting another site's evidence as verification. Allow it
for a known redirect between your own properties (for example apex to `www`):

```bash
ts audit ad-templates verify https://publisher.example/ --allow-cross-origin-redirect
```

Verification accepts multiple URLs and reuses one browser/profile. Add
`--strict` to return exit 1 when a confirmable slot is missing or partially
confirmed, and `--json` for the stable machine-readable report. Video- and
native-only slots are reported as `unconfirmable`; that records a checker
limitation and does not fail strict mode. A live out-of-page slot with no sizes
against banner-configured formats is reported `partial` and does fail strict
mode. `--scroll` enables the optional second evidence phase and labels evidence
first seen after the deterministic scroll.

Browser-backed ad-template generation and verification share `--chrome`,
`--headful`, `--browser-proxy`, `--no-assume-consent`,
`--settle-quiet-ms`, `--settle-max-ms`, and
`--danger-accept-invalid-certs`. Verification also accepts
`--browser-profile desktop|mobile`; generation uses
`--profiles desktop,mobile` to compare both profiles. `--cookie NAME=VALUE` is
repeatable and creates host-only, root-path cookies; HTTPS targets also mark
them Secure. Verification refuses cookies when URLs span multiple origins. The quiet settle window
must not exceed the maximum.

`ts audit` is not an EdgeZero adapter command. It has no `--adapter` option and
it does not provision resources, push config, build, deploy, or contact platform
APIs.

## Generate an external Prebid bundle

`ts prebid bundle` builds the local external Prebid browser bundle configured in
`trusted-server.toml`.

```toml
[integrations.prebid.bundle]
adapters = ["rubicon", "kargo"]
user_id_modules = ["sharedIdSystem"]
```

Run the command after installing JS dependencies:

```bash
cd crates/trusted-server-js/lib && npm ci
cd ../../..
ts prebid bundle
```

By default, generated artifacts are written to `dist/prebid/`, and the command
updates `integrations.prebid.external_bundle_sha256` and
`integrations.prebid.external_bundle_sri` in `trusted-server.toml`. Upload the
generated JavaScript file yourself, set `external_bundle_url` to its HTTPS
asset URL, and include that host (plus any redirect targets) in
`proxy.allowed_domains` before running `ts config validate` or `ts config push`.

Use custom paths when needed:

```bash
ts prebid bundle --config publisher-a.toml --out build/prebid
```

`ts prebid bundle` is local-only. It has no `--adapter` option and does not
upload, provision, deploy, or push config.
