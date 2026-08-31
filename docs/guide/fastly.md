# Fastly Setup

This guide covers setting up your Fastly account and Compute service for Trusted Server.

## Create a Fastly Account

1. Go to [manage.fastly.com](https://manage.fastly.com) and create an account if you don't have one

## Create an API Token

1. Log in to the Fastly control panel
2. Go to **Account > API tokens > Personal tokens**
3. Click **Create token**
4. Configure the token:
   - Name the token (e.g., "Trusted Server Deploy")
   - Choose **User Token**
   - Choose **Global API Access**
   - Choose what makes sense for your organization in terms of Service Access
5. Click **Create Token**
6. Copy the key to a secure location - you will not be able to see it again

## Create a Compute Service

1. Click **Compute** in the navigation
2. Click **Create Service**
3. Click **Create Empty Service** (below the main options)
4. Configure the service:
   - Add your domain of the website you'll be testing or using
   - Click **Update**

## Configure Origins

Origins are the backend servers that Trusted Server will communicate with (ad servers, SSPs, etc.).

1. In your Compute service, click on the **Origins** section
2. For each backend you need to add:
   - Enter the FQDN or IP address
   - Click **Add**
   - Enter a **Name** in the first field - this name will be referenced in your code (e.g., `my_ad_integration_1`)
   - Configure port numbers and TLS settings as needed

::: tip
After saving origin information, you can select port numbers and toggle TLS on/off.
:::

## Configure Fastly CLI Profile

After installing the Fastly CLI, create a profile with your API token:

```bash
fastly profile create
```

Follow the interactive prompts to paste your API token.

## Domain Configuration

::: tip
With a dev account, Fastly gives you a test domain by default (e.g., `xxx.edgecompute.app`). You can use this for testing before configuring your own domain.
:::

### Using Your Own Domain

When you're ready to use your own domain:

1. In the Fastly control panel, add your domain to the service
2. Create a CNAME record at your DNS provider pointing to your Fastly domain
3. Fastly provides 2 free TLS certificates (non-wildcard) per account

### TLS Requirements

- Fastly Compute **only accepts client traffic via TLS** (HTTPS)
- Origins and backends can be non-TLS if needed

## CDN-fronted Client IP

When another CDN or Fastly service fronts Trusted Server, the Fastly Compute
client address identifies the immediate edge node rather than the original
reader. Geolocation, EC identity derivation, and consent jurisdiction all read
that single value, so all three describe the fronting POP instead of the reader.
Nothing errors: pages render and ads serve while the derived values are wrong.

Trusted Server can instead consume a reader-IP header, but only when the same
request carries a shared secret that only the front door knows. Trusting the
header on its own would be worse than the problem it solves, because any caller
could then choose the address used for geolocation, EC identity derivation, and
bot protection.

This is opt-in. With no `[trusted_client_ip]` section, Trusted Server keeps
using the immediate peer address.

### Choose dedicated header names

Prefer header names that nothing else in the fronting service uses:

```toml
[trusted_client_ip]
ip_header = "x-ts-client-ip"
auth_header = "x-ts-client-ip-auth"
shared_secret = "replace-with-a-random-shared-secret"
```

`ip_header` may also be
[`fastly-client-ip`](https://www.fastly.com/documentation/reference/http/http-headers/Fastly-Client-IP/),
which suits a VCL service dedicated to Trusted Server. On a service that also
carries other traffic, a dedicated `x-` name is safer: the front-door VCL then
never modifies `Fastly-Client-IP`, so security rules, rate limiters, logging
formats, and vendor snippets that read it keep working unchanged.

Both names are validated at startup. `ip_header` must be `fastly-client-ip` or
begin with `x-`, `auth_header` must begin with `x-`, the two must differ, and
neither may reuse a Trusted Server internal header name such as
`x-forwarded-for` or `x-ts-ec`. Use lowercase in the TOML.

### Configure the front door

Set both headers in the fronting VCL service, reading the secret from a private
(write-only) edge dictionary rather than from VCL source:

```vcl
sub vcl_recv {
  # Trusted Server reader-IP handoff.
  if (fastly.ff.visits_this_service == 0) {
    # Client-supplied copies never survive, on any route.
    unset req.http.X-TS-Client-IP;
    unset req.http.X-TS-Client-IP-Auth;

    # Stamp only on the Trusted Server route, so the secret never reaches
    # another backend.
    if (req.http.host == "www.example.com") {
      set req.http.X-TS-Client-IP = client.ip;
      set req.http.X-TS-Client-IP-Auth =
        table.lookup(ts_private_config, "trusted_client_ip_secret");
    }
  }
}
```

Attach an edge dictionary named `ts_private_config` to the fronting service and
store the secret under the key `trusted_client_ip_secret`. Create the dictionary
as write-only so the value cannot be read back through the API or the web
interface. If the key is absent, the lookup yields no matching value, the
authentication check fails, and Trusted Server falls back to the peer address.

Four details in that example carry weight:

- **`fastly.ff.visits_this_service == 0`** restricts the whole block to the
  first entry into this service. A [shielded](https://www.fastly.com/documentation/guides/concepts/shielding/)
  request enters the same service twice, and on the second entry
  [`client.ip`](https://www.fastly.com/documentation/reference/vcl/variables/client-connection/client-ip/)
  is the first POP rather than the reader. Keeping the `unset` lines inside this
  guard also lets the values stamped on the first entry survive the second.
- **`unset` before `set`** removes every copy of each header, including one a
  client sent. Trusted Server ignores a forwarded address whenever either
  header carries more than one value, so without the `unset` a reader could
  send its own copy, force the fallback, and keep its real address out of
  geolocation and bot protection.
- **The route condition wraps only the `set` lines.** Client-supplied values
  are removed on every route, while the secret is added only on the route that
  reaches Trusted Server. Match on `req.http.host` or `req.url` rather than on
  the selected backend: those are available from the start of `vcl_recv` and do
  not change when other VCL restarts the request.
- **There is no `req.restarts` guard.** A restart can change which backend a
  request reaches. Re-running the block on each pass re-evaluates the route
  condition, so a stamp made before a restart cannot follow the request to a
  different backend. On the ordinary path the host is unchanged and re-running
  writes the same values.

Configure the identical header names and secret in Trusted Server. The
`shared_secret` shown above is an intentionally invalid placeholder that
Trusted Server rejects at startup; replace it with the exact value stored in
the dictionary. Use a cryptographically random value of at least 32 ASCII
graphic bytes, encoded as hex or base64url with no whitespace.

### What Trusted Server does with the result

Trusted Server accepts the forwarded address only when the request carries
exactly one authentication value matching `shared_secret` byte for byte and
exactly one IP value that parses directly as IPv4 or IPv6. Values are not
trimmed. Both headers are removed before routing.

| Request state                                                     | Address used     |
| ----------------------------------------------------------------- | ---------------- |
| One matching auth value and one bare IP value                     | Forwarded reader |
| Auth value missing, empty, wrong, duplicated, or not UTF-8        | Immediate peer   |
| IP value missing, duplicated, not UTF-8, or not a bare IP address | Immediate peer   |
| Request bypassed the front door                                   | Immediate peer   |
| No `[trusted_client_ip]` section configured                       | Immediate peer   |

No combination rejects the request. A misconfigured front door, a rotated
secret, or a renamed header degrades to the current behavior rather than
causing an outage.

### Confirm the topology first

The VCL example assumes the reader connects directly to the fronting Fastly
service.

| Topology                                            | `client.ip` at the front door | Result                                    |
| --------------------------------------------------- | ----------------------------- | ----------------------------------------- |
| Reader to fronting Fastly service to Trusted Server | The reader                    | Correct reader address                    |
| Reader to another CDN to Fastly to Trusted Server   | That CDN's node               | Wrong address, and authenticated as valid |
| Reader directly to Trusted Server                   | Not applicable                | Falls back to the immediate peer          |
| Fastly no-code request routing                      | No injection point            | Mechanism unavailable                     |

The second row is the one topology that fails with a wrong value instead of
falling back. If another CDN precedes Fastly, restrict direct access to the
Fastly front door and derive `ip_header` from that CDN's protected reader-IP
value instead of from `client.ip`.

::: warning No-code request routing limitation
Fastly no-code request routing does not provide a point to inject these headers.
If that routing path does not preserve the original reader IP, Trusted Server
cannot recover it with this mechanism. Use a fronting service that can set both
headers before forwarding the request.
:::

### Verify before trusting it

Geolocation response headers are the observable signal. From a client whose
real location differs from the fronting POP, compare a request through the
front door against one sent directly to the Compute service, and confirm
`x-geo-city` and `x-geo-coordinates` agree.

Then confirm the anti-evasion path holds by sending duplicate and junk trust
headers through the front door:

```bash
curl -sD - -o /dev/null 'https://www.example.com/' \
  -H 'X-TS-Client-IP: 198.51.100.7' \
  -H 'X-TS-Client-IP: 203.0.113.10' \
  -H 'X-TS-Client-IP-Auth: not-the-secret'
```

The geolocation headers should still describe the reader. If they describe the
fronting POP, something upstream is leaving a client-supplied copy in place.
IPv6 readers are worth testing separately, because Trusted Server requires a
bare address and rejects bracketed, ported, or zone-suffixed forms.

### Effect on origin requests

Trusted Server treats `fastly-client-ip` as client-spoofable and strips it at
request entry whether or not `[trusted_client_ip]` is configured, so it no
longer forwards an inbound `Fastly-Client-IP` to the publisher origin. Check
whether the origin reads that header for geolocation, fraud checks, or logging
before deploying. Reconstructing a trustworthy client-address header for origin
requests is tracked separately from this mechanism.

## Create Config and Secret Stores

For features like request signing, you'll need to create Fastly stores:

### Config Store

Used for storing public configuration (e.g., public keys, key metadata):

```bash
fastly config-store create --name jwks_store
```

### Secret Store

Used for storing sensitive data (e.g., private signing keys):

```bash
fastly secret-store create --name signing_keys
```

Note the store IDs - you'll need them for your `trusted-server.toml` configuration.

## Create EC KV Store

Edge Cookie flows require one KV store:

- Identity graph store (`ec_store`) - EC identity graph, source-domain keyed partner UIDs, minimal consent metadata, and withdrawal tombstones

Partners are configured statically in `[[ec.partners]]` and loaded into an in-memory registry at startup. There is no separate consent KV store. Consent is interpreted from live request cookies, headers, geolocation, and policy defaults.

Create it:

```bash
fastly kv-store create --name ec_identity_store
```

Configure in `trusted-server.toml`:

```toml
[ec]
passphrase = "replace-with-32-plus-byte-random-secret"
ec_store = "ec_identity_store"
```

Verify stores exist:

```bash
fastly kv-store list
```

Verify stores are linked to your active service version:

```bash
fastly resource-link list --service-id <service-id> --version <active-version>
```

If EC sync returns `kv_unavailable` or identify responses are degraded, first check that the identity store is present and linked to the active version. Legacy partner/consent KV bindings can be removed once no deployment-specific tooling depends on them.

## Next Steps

- Return to [Getting Started](/guide/getting-started) to continue setup
- See [Configuration](/guide/configuration) for detailed configuration options
- See [EC Setup Guide](/guide/ec-setup-guide) for end-to-end EC verification
- See [Request Signing](/guide/request-signing) for setting up cryptographic signing
