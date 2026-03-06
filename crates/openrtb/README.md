# trusted-server-openrtb

OpenRTB 2.6 data model generated from the [IAB Tech Lab protobuf schema](https://github.com/nicoboss/openrtb/blob/master/proto/openrtb.proto). Types are used exclusively with JSON serde — protobuf binary encoding is stripped at build time.

## Build dependency

This crate requires `protoc` at compile time (invoked by `prost-build`):

```sh
# macOS
brew install protobuf

# Debian / Ubuntu
apt install protobuf-compiler
```

## How types are generated

The `build.rs` pipeline has three phases:

1. **Proto compilation** — `prost-build` compiles `proto/openrtb.proto` into Rust structs.
2. **Strip protobuf concerns** — `prost::Message` derives and `#[prost(...)]` attributes are removed since we only use JSON encoding.
3. **Add serde + OpenRTB support** — `Serialize`/`Deserialize` derives are injected along with `skip_serializing_if` for `Option` and `Vec` fields. Extensible structs receive an `ext: Option<Map<String, Value>>` field, and `Option<bool>` fields get the `bool_as_int` serde adapter for the OpenRTB `0`/`1` convention.

### Proto modifications from upstream

The IAB proto uses `edition = "2023"` which generates non-optional scalars. The local copy converts to `proto2` with explicit `optional` on every field so prost generates `Option<T>`, matching OpenRTB's "omit if not set" semantics. `Ext` messages are removed from the proto and re-injected by the build script as `Option<serde_json::Map>`. See the header comment in `proto/openrtb.proto` for the full list of changes.

## Crate API

All generated types are re-exported at the crate root for flat access:

```rust
use trusted_server_openrtb::{BidRequest, BidResponse, Imp, Banner, Device, User};
```

### `bool_as_int`

Serde helper module that transparently converts `Option<bool>` to/from `0`/`1` integers on the wire. Applied automatically to generated boolean fields.

### `ToExt`

Trait for converting any `Serialize` type into an `Option<Map<String, Value>>` suitable for an `ext` field. Returns `None` for empty maps so `ext` is omitted from JSON output rather than serialized as `"ext": {}`.

```rust
use trusted_server_openrtb::ToExt;

let ext_value = my_custom_ext.to_ext(); // Option<Map<String, Value>>
```
