//! Public command availability regression.
#![cfg(target_os = "linux")]

#[test]
fn linux_dev_proxy_help_is_available() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["dev", "proxy", "--help"])
        .output()
        .expect("should run ts");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("--map"));
}
