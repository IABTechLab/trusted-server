//! Build script that compiles the `OpenRTB` proto file with prost-build, then
//! post-processes the generated Rust code to:
//!
//! 1. Strip prost runtime derives and attributes (we only use JSON, not
//!    protobuf binary encoding).
//! 2. Add serde `Serialize` / `Deserialize` derives with `skip_serializing_if`
//!    annotations for `Option` and `Vec` fields.
//! 3. Inject `ext: Option<serde_json::Map<String, Value>>` into structs that
//!    correspond to extensible `OpenRTB` objects.

use std::fs;
use std::path::PathBuf;

// Include the codegen module directly so it shares source with lib.rs tests.
include!("src/codegen.rs");

fn main() {
    println!("cargo:rerun-if-changed=proto/openrtb.proto");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("should have OUT_DIR"));

    // Phase 1: Compile proto with prost-build.
    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_protos(&["proto/openrtb.proto"], &["proto/"])
        .expect("should compile openrtb.proto");

    // Phase 2: Post-process the generated file.
    let generated_path = out_dir.join("com.iabtechlab.openrtb.v2.rs");
    let code = fs::read_to_string(&generated_path).expect("should read generated file");
    let processed = postprocess(&code);
    fs::write(&generated_path, processed).expect("should write processed file");
}
