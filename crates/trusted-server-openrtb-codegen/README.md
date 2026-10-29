# trusted-server-openrtb-codegen

Host-only maintenance tool that regenerates Trusted Server's checked-in
OpenRTB Rust model from the local protobuf schema.

The generator requires `protoc`. It compiles
`trusted-server-openrtb/proto/openrtb.proto`, removes protobuf-only derives and
attributes, adds the repository's serde conventions, and replaces
`trusted-server-openrtb/src/generated.rs`. Ordinary application builds do not
run this package and do not require `protoc`.

Use the owning wrapper from the repository root:

```bash
./crates/trusted-server-openrtb/generate.sh
cargo test --package trusted-server-openrtb-codegen --target "$(rustc -vV | sed -n 's/host: //p')"
```

Review the complete generated diff before committing it. The
[`trusted-server-openrtb` README](../trusted-server-openrtb/README.md) describes
the schema adaptations and public model boundary.
