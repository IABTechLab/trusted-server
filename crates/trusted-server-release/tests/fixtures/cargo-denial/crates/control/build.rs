//! A build script that attempts nothing, so the control still runs one.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
}
