# Trusted Server CLI

The Trusted Server CLI binary is `ts`. It is a host-target operator tool for
configuration and EdgeZero-backed lifecycle commands.

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

`config validate` and `config push` use EdgeZero's typed app-config loader. By
default that loader applies `TRUSTED_SERVER__...` environment overlays before
validation and blob creation. Pass `--no-env` for file-only operation.

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
