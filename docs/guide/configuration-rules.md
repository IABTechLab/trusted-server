# Configuration Rules

This page is for the people who write a Trusted Server configuration file and
for the people who write the code that reads it. It sets out the few rules
every part of the file follows, what Trusted Server checks before it serves a
request, and why the file is built this way.

## One rule for every provider

Almost everything Trusted Server can switch on is a provider: the Edge Cookie
identity provider, the location and device providers, the permission signals,
the demand sources in an auction, the ad server that picks the winner, and the
page integrations. Every one of them is configured the same way.

```toml
[<type>]
provider = "<name>"            # or a list, where several run

[<type>.<name>]                # only when the provider has settings
setting = "value"
```

1. **The type is the job.** Each type is one top-level table, named for what
   its providers do: `ec`, `geo`, `device`, `permission_signal`, `demand`,
   `adserver` and `integration`.
2. **`provider` chooses what runs.** A type that runs one provider takes a
   string. A type that runs several takes a list.
3. **`[<type>.<name>]` holds the settings.** A provider with nothing to set
   needs no table at all.
4. **The name is the implementation.** `[ec.hmac]` configures the `hmac`
   implementation. To run an implementation under a name of your own, add an
   `implementation` line. That is how two Prebid Servers run side by side.
5. **Names are snake_case,** like every key in the file.
6. **Secrets are key names.** A secret setting holds the name of a key in
   `trusted_server_secrets`, never the secret itself.

| Type | Runs | `provider` is | Implementations in this repository |
| --- | --- | --- | --- |
| `ec` | one | a string | `hmac`, `host_signals`, `client_fixed` (demonstration builds), or an integration that supplies identity |
| `geo` | one | a string | `platform`, `none`, or an integration that supplies location |
| `device` | one | a string | `builtin` (the default), `fastly`, or an integration that supplies device signals |
| `permission_signal` | several, in order | a list | `gpc`, `gpp_sale_opt_out`, `us_privacy`, `tcf` |
| `demand` | several | a list | `openrtb`, `prebid_server`, `aps` |
| `adserver` | one | a string | `adserver_mock` |
| `integration` | several | a list | `datadome`, `didomi`, `google_tag_manager`, `gpt`, `gpt_diagnostics`, `js_asset_proxy`, `lockr`, `nextjs`, `osano`, `permutive`, `prebid`, `sourcepoint`, `testlight` |

`openrtb`, `prebid_server`, `aps` and `adserver_mock` supply implementations
only. They are not page integrations and cannot be named in
`[integration] provider`.

A provider that an integration supplies needs that integration named in
`[integration] provider` too, because the module has to be registered before
another type can select what it offers.

A few types keep settings of their own beside `provider`, where the setting
belongs to the job rather than to one provider. `[ec]` holds `ec_store`, the
partner registry and the cluster thresholds, and `[geo]` holds
`assume_single_jurisdiction`. Those keys sit directly in the type's table.

### Leaving `provider` out

| Type | With no `provider` line |
| --- | --- |
| `ec` | no Edge Cookie is created |
| `geo` | no location is resolved and no host geo service is called |
| `device` | `builtin` runs, which reads the User-Agent only |
| `permission_signal` | every linked provider runs, in the order shown above |
| `demand` | no demand source is called |
| `adserver` | the highest bid wins, with no ad server |
| `integration` | no integration runs |

## What is checked before a request is served

Every rule below refuses startup, and the message names the table and the fix.
Some are caught earlier, when the configuration is validated or pushed, and
the two lists say which.

- A `[<type>.<name>]` table that its type's `provider` does not name. A block
  left behind after a provider is switched off is caught, rather than sitting
  unused and misleading the next reader.
- A `provider` entry, or an `implementation` line, that names an
  implementation this build does not have. The message lists the ones it does.
- A selected provider that needs a setting its table does not give, such as
  `hmac` with no `passphrase`.
- A setting a provider does not know. Every provider rejects unknown keys, so
  a misspelt setting fails instead of being ignored.
- A name that is not snake_case, or a name selected twice.
- A key in a type's table that is neither `provider`, one of that type's own
  settings, nor a named provider table.
- A `demand` or `adserver` endpoint that is not HTTPS. Plain HTTP is allowed
  only to `127.0.0.1`, `::1` or `localhost`, so a local test stack runs
  without certificates and nothing leaves the machine unencrypted. An endpoint
  carrying credentials or a fragment is refused in every case.
- A secret setting whose key name is missing from `trusted_server_secrets`.

### Checked when the configuration is validated or pushed

`ts config validate`, `ts config diff` and `ts config push` all run the same
set. It is everything that can be decided from the file and from the
implementations compiled into the CLI.

- The whole auction plan, compiled from `[demand]`, `[adserver]` and
  `[auction.bidders]`. That covers unselected tables, names that are not
  snake_case, an implementation this build does not have, endpoint scheme and
  host, timeouts, routing modes, notification bounds, a bidder route naming a
  demand source `[demand] provider` does not select, and any setting the
  chosen implementation rejects.
- Every integration's own settings, selected or not, so a typo in a block that
  is switched off is still caught.
- Every secret setting holding a non-empty key name rather than a value, with a
  secret store declared to hold it.
- Basic-auth coverage of the admin namespace, and the placeholder values the
  template ships with.

### Checked when the service starts

Everything above runs again, on the configuration the instance actually
loaded, and these join it.

- Which providers `[ec]`, `[geo]`, `[device]` and `[permission_signal]`
  select. Those four are settled where the adapter composes the build, so a
  name this build does not have, a missing settings table, or a table the
  selector does not name, stops the service on its next start rather than the
  push. **A passing `ts config validate` is not proof that a change to those
  four will start.** Start an instance on the new configuration to find out.
- Assembling the integration registry, which is where a module that supplies an
  identity, location or device provider is matched to the type that selected
  it. A module declaring a provider no type selects is reported here.
- The resolved secret values, which a key name alone cannot show. A weak or
  placeholder password fails here.
- The compiled `permissions.yaml` policy, and the acknowledgment an Edge
  Cookie provider needs when no geo provider is selected.
- The checks only the host can make, being backend name prediction and
  collisions, and whether the adapter can call more than one demand source at
  once.

## Settings that are not providers

The other tables configure Trusted Server itself rather than choose a provider.
They keep their own keys and have no `provider` line.

| Table | Configures |
| --- | --- |
| `[publisher]` | the site, its origin and the proxy secret |
| `[auction]` | whether auctions run, the whole-auction timeout and creative handling |
| `[auction.bidders.<code>]` | which demand provider a browser bidder code is sent to |
| `[creative_opportunities]` | server-rendered ad slots |
| `[proxy]`, `[cache]`, `[rewrite]` | first-party proxying, caching and URL rewriting |
| `[request_signing]`, `[trusted_client_ip]`, `[[handlers]]` | signing, client addresses and admin access |
| `[debug]`, `[tinybird]` | diagnostics and telemetry |

## Why the file works this way

**For the people who run it.** There is one pattern to learn. Whether a
section chooses an identity provider or the demand sources for an auction, the
question "what runs, and how is it set up" is answered the same way, in the
same place. A change reads plainly in review, because switching a provider is
a change to one `provider` line. And mistakes are caught when the
configuration is validated or the server starts, not when a visitor's request
takes an unexpected path. A leftover table, a misspelt setting or a name the
build does not know all stop the deployment with a message that names the
fix.

**For the people who write the code.** A provider plugs in through one
registration and inherits the checks above without writing them again. Core
code does not name any vendor, so adding a provider does not mean editing core,
and a vendor's crate can supply its own provider on the same terms as the ones
in this repository.

**For everyone.** A configuration that cannot hold a silent mistake is one
that can be trusted in production and handed from one team to another.

## A complete example

A site with every kind of component the rules cover.

```toml
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "handler_password"              # key name in trusted_server_secrets

[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "publisher_proxy_secret"    # key name

[proxy]
allowed_domains = ["assets.example.com"]

[integration]
provider = ["prebid", "gpt"]

[integration.prebid]
external_bundle_url = "https://assets.example.com/prebid/trusted-prebid.js"
timeout_ms = 1500                          # the browser's Prebid timeout

[integration.gpt]
gam_attribution_enabled = true

[ec]
provider = "hmac"
ec_store = "ec_identity_store"

[ec.hmac]
passphrase = "ec_passphrase"               # key name

[geo]
provider = "platform"

[device]
provider = "builtin"

[permission_signal]
provider = ["gpc", "gpp_sale_opt_out", "us_privacy", "tcf"]

[demand]
provider = ["pbs_main"]

[demand.pbs_main]
implementation = "prebid_server"           # the name is a label of your own
endpoint = "https://prebid.example.com/openrtb2/auction"
timeout_ms = 1200                          # this demand source only
consent_forwarding = "both"

[adserver]
provider = "adserver_mock"

[adserver.adserver_mock]
endpoint = "https://adserver.example.com/decide"
timeout_ms = 500

[auction]
enabled = true
timeout_ms = 2000                          # the whole auction

[auction.bidders.example-server]
provider = "pbs_main"

[creative_opportunities]
enabled = true
gam_network_id = "123456789"
auction_timeout_ms = 500                   # the page's server-side auction

[[creative_opportunities.slot]]
id = "leaderboard"
gam_unit_path = "/123456789/leaderboard"
div_id = "div-gpt-ad-leaderboard"
page_patterns = ["/"]
formats = [{ width = 728, height = 90 }]
```

Two Prebid Servers run side by side by giving each its own name and pointing
both at the same implementation.

```toml
[demand]
provider = ["pbs_main", "pbs_house"]

[demand.pbs_main]
implementation = "prebid_server"
endpoint = "https://prebid.example.com/openrtb2/auction"

[demand.pbs_house]
implementation = "prebid_server"
endpoint = "https://house.example.com/openrtb2/auction"
```

## Moving from the previous layout

| Previous | Now |
| --- | --- |
| `[integrations.<id>]` with `enabled = true` | `<id>` in `[integration] provider`, and `[integration.<id>]` only for settings |
| `[ec.providers.<name>]` | `[ec.<name>]` |
| `[permission_signal] sources` | `[permission_signal] provider` |
| `host-signals`, `client-fixed`, `gpp-sale-opt-out`, `us-privacy` | `host_signals`, `client_fixed`, `gpp_sale_opt_out`, `us_privacy` |
| `[auction.providers.<id>]` with `protocol`, `profile` and `profile_config` | `[demand] provider` and `[demand.<name>]`, with `implementation` and the settings flat in the table |
| `profile = "standard"` | `implementation = "openrtb"` |
| `[auction] mediator = "adserver_mock"` and `[integrations.adserver_mock]` | `[adserver] provider = "adserver_mock"` and `[adserver.adserver_mock]` |
| `[integrations.aps] rendering_mode` | `rendering_mode` in the `[demand.<name>]` table of the `aps` provider |
| `[debug.auction_html_comment_options] include_mediator_response` | `include_adserver_response` |

The word mediator is gone with it. It is "ad server" in prose and `adserver`
in configuration, and the auction response metadata that read
`parallel_mediation` now reads `parallel_adserver`.

`[auction] providers` and `[auction] mediator` are not ignored. A
configuration still carrying either is refused with a message naming where the
setting moved to.

## For developers adding a provider

A provider is registered by its integration builder, and the configuration
rules above apply to it without any extra code. See the
[integration guide](/guide/integration-guide) for the registration itself.
