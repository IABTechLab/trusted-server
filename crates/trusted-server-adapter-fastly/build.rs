//! Compile immutable facts owned by the Fastly product.

#![allow(
    clippy::print_stdout,
    reason = "Cargo build script directives use stdout"
)]

#[allow(
    dead_code,
    reason = "This owner uses only release validation from the shared resolver"
)]
#[path = "../trusted-server-core/build_support/git_version.rs"]
mod git_version;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../trusted-server-core/build_support/git_version.rs");
    println!("cargo:rerun-if-env-changed=TRUSTED_SERVER__RELEASE_BUILD");
    println!("cargo:rerun-if-env-changed=TRUSTED_SERVER__GIT_VERSION");
    let control = std::env::var_os("TRUSTED_SERVER__RELEASE_BUILD");
    let source = std::env::var_os("TRUSTED_SERVER__GIT_VERSION");
    git_version::release_source(control.as_deref(), source.as_deref())
        .expect("should provide valid release build inputs");
    let target = std::env::var("TARGET").expect("should set the owning Cargo target");
    println!("cargo:rustc-env=TRUSTED_SERVER__BUILD_TARGET={target}");
}
