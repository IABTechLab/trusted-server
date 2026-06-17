# Trusted Server CLI

The Trusted Server CLI binary is `ts`. It is a host-target operator tool for
configuration, page audits, and EdgeZero-backed lifecycle commands.

## Install from source

The workspace default target is `wasm32-wasip1`, so build or test the CLI with
your host target:

```bash
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cargo build --package trusted-server-cli --target "$HOST_TARGET"
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

## Audit a public page

`ts audit` loads a public page in a fresh headless Chrome/Chromium session,
collects rendered JavaScript asset evidence, detects known Trusted Server
integrations, and writes local draft artifacts.

Chrome or Chromium must be installed locally. The command checks common PATH
names and standard macOS/Linux install locations.

```bash
ts audit https://publisher.example
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
ts audit https://publisher.example --no-config
```

Use custom output paths when reviewing artifacts first:

```bash
ts audit https://publisher.example \
  --js-assets audit/js-assets.toml \
  --config audit/trusted-server.toml
```

Use `--force` only when replacing existing output files is intentional:

```bash
ts audit https://publisher.example --force
```

`ts audit` is not an EdgeZero adapter command. It has no `--adapter` option and
it does not provision resources, push config, build, deploy, or contact platform
APIs.
