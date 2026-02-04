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

## Next Steps

- Return to [Getting Started](/guide/getting-started) to continue setup
- See [Configuration](/guide/configuration) for detailed configuration options
- See [Request Signing](/guide/request-signing) for setting up cryptographic signing
