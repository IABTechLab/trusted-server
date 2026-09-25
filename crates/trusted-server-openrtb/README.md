# trusted-server-openrtb

Portable Rust representation of the OpenRTB 2.6 JSON model used by Trusted
Server auctions. The generated types are ordinary serde values; protobuf binary
encoding is deliberately absent.

`src/generated.rs` is checked in, so normal builds on native,
`wasm32-wasip1`, and `wasm32-unknown-unknown` do not require `protoc` or execute
code generation. All generated model types are re-exported at the crate root.
The hand-written API also provides `bool_as_int` for OpenRTB's integer boolean
encoding and `ToExt` for converting selected extension values into omit-when-
empty JSON maps.

## Regenerate

The local protobuf source uses `proto2` and explicit optional fields so the
generated Rust model preserves OpenRTB omission semantics. The generator then
removes `prost::Message` concerns, adds serde derives and skip rules, and
injects extension maps.

From the repository root, with `protoc` installed:

```bash
./crates/trusted-server-openrtb/generate.sh
```

Review `proto/openrtb.proto`, `src/generated.rs`, and the generator changes as
one unit. Run the shared target suite with `cargo test-fastly`; run the host-only
generator tests with the command in the
[`trusted-server-openrtb-codegen` README](../trusted-server-openrtb-codegen/README.md).
