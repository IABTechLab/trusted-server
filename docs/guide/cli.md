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

| Situation                                                                                     | Result                                                                                                                                  |
| --------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Only one page was crawled                                                                     | Literal path. One observation cannot distinguish a literal from a template.                                                             |
| The ad unit never varied by section                                                           | Literal path.                                                                                                                           |
| A section's slug is not derivable from its URL (`/car-research` requesting `.../carresearch`) | Literal path; the round-trip check catches it.                                                                                          |
| No root page was seen, so `section_root` is unknown                                           | Literal path rather than a guessed fallback.                                                                                            |
| Two path segments could both be the section                                                   | No template; the ambiguity is reported.                                                                                                 |
| The ad unit varies by device, geo, or anything the URL cannot supply                          | **No `gam_unit_path` at all** for that slot.                                                                                            |
| Crawled pages report different GAM network ids                                                | The run fails; the pages are not one property.                                                                                          |
| More than a quarter of crawled pages return no slots                                          | The run fails. That is the signature of bot protection serving challenge pages, and writing from it would silently narrow the slot set. |

Every run checks that the config it produced still loads before replacing the
file, and `--dry-run` runs the same check — a clean preview is evidence the
config loads, not just that it parses.

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
hand-tuned fields and gains this run's patterns, and a hand-written
`gam_unit_path` template is preserved. `--replace` discards existing slots
instead, which also discards any template you wrote by hand.

Behind bot protection, pass a valid clearance cookie. The crawl reuses one
browser session, so clearance earned on the first page carries to the rest:

```bash
ts audit ad-templates generate https://publisher.example/ --cookie 'datadome=<value>'
```

### Checking for a device split

Publishers often serve a different ad unit per device
(`/network/desktop/news` against `/network/mobile/news`). A desktop-only crawl
cannot see that — it infers a template correct for desktop and silently wrong
for every mobile impression.

```bash
ts audit ad-templates generate https://publisher.example/ --profiles desktop,mobile
```

Each page is loaded once per profile. Where the profiles disagree, the slot is
written with its div and formats but **no** `gam_unit_path`, so the runtime
falls back to the default unit rather than bidding on one that does not exist.

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
