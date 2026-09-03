use std::process::Command;

fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Reads the workspace version from the repository root manifest, so the page
/// reports the same version as the trusted-server crates it runs.
fn workspace_version() -> String {
    let root = std::fs::read_to_string("../../../Cargo.toml").unwrap_or_default();
    let mut in_package = false;
    for line in root.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[workspace.package]";
        } else if in_package && line.starts_with("version") {
            if let Some(version) = line.split('"').nth(1) {
                return version.to_string();
            }
        }
    }
    String::from("unknown")
}

fn main() {
    println!("cargo:rustc-env=TS_CORE_VERSION={}", workspace_version());
    println!("cargo:rustc-env=TS_CORE_COMMIT={}", git(&["rev-parse", "--short=9", "HEAD"]));
    println!("cargo:rustc-env=TS_CORE_DATE={}", git(&["show", "-s", "--format=%cs", "HEAD"]));
    println!("cargo:rustc-env=TS_CORE_BRANCH={}", git(&["rev-parse", "--abbrev-ref", "HEAD"]));
    println!("cargo:rerun-if-changed=../../../Cargo.toml");
}
