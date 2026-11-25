//! Compile immutable facts owned by the CLI product. Release source inputs are
//! validated once by `trusted-server-core`'s build script.

#![allow(
    clippy::print_stdout,
    reason = "Cargo build script directives use stdout"
)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TRUSTED_SERVER__RELEASE_BUILD");
    println!("cargo:rustc-check-cfg=cfg(trusted_server_release_build)");
    // Core's build script rejects an invalid release control for every product
    // build; this only selects release-only resource discovery.
    if std::env::var_os("TRUSTED_SERVER__RELEASE_BUILD").is_some() {
        println!("cargo:rustc-cfg=trusted_server_release_build");
    }
    let target = std::env::var("TARGET").expect("should set the owning Cargo target");
    println!("cargo:rustc-env=TRUSTED_SERVER__BUILD_TARGET={target}");
}
