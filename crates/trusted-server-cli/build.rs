//! Compile immutable facts owned by the CLI product. Release source inputs are
//! validated once by `trusted-server-core`'s build script.

#![allow(
    clippy::print_stdout,
    reason = "Cargo build script directives use stdout"
)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").expect("should set the owning Cargo target");
    println!("cargo:rustc-env=TRUSTED_SERVER__BUILD_TARGET={target}");
}
