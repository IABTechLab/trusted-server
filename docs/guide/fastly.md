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
reader. Trusted Server can instead consume an authenticated reader-IP header,
but only after the public front door is configured to overwrite both the IP and
authentication headers on every backend request. Preserving values supplied by
the browser is unsafe because a caller could choose the IP used for geolocation
and other request processing.

For a VCL-to-Compute [service chain](https://www.fastly.com/documentation/guides/getting-started/services/service-chaining/),
overwrite [`Fastly-Client-IP`](https://www.fastly.com/documentation/reference/http/http-headers/Fastly-Client-IP/)
from the initial [`client.ip`](https://www.fastly.com/documentation/reference/vcl/variables/client-connection/client-ip/)
and set a dedicated authentication header in the fronting service. For example:

```vcl
sub vcl_recv {
  if (fastly.ff.visits_this_service == 0 && req.restarts == 0) {
    set req.http.Fastly-Client-IP = client.ip;
  }
  set req.http.X-TS-Client-IP-Auth = "replace-with-a-random-shared-secret";
}
```

Use a cryptographically random secret in production; the value above is only a
placeholder. Configure the identical header names and secret in Trusted Server:

```toml
[trusted_client_ip]
ip_header = "fastly-client-ip"
auth_header = "x-ts-client-ip-auth"
shared_secret = "replace-with-a-random-shared-secret"
```

`Fastly-Client-IP` is not protected from modification when it first enters
Fastly, which is why overwriting it and authenticating the handoff are both
required. Trusted Server removes both trust headers before routing. Direct
requests and requests with missing, invalid, or duplicated trust headers remain
available and use the immediate peer address instead.

::: warning No-code request routing limitation
Fastly no-code request routing does not provide a point to inject these headers.
If that routing path does not preserve the original reader IP, Trusted Server
cannot recover it with this mechanism. Use a fronting service that can overwrite
both headers before forwarding the request.
:::

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
