//! Startup refusal tests for the Axum dev server binary.

use std::process::Command;

#[test]
fn invalid_bind_address_is_reported_when_logging_is_off() {
    let output = Command::new(env!("CARGO_BIN_EXE_trusted-server-axum"))
        .env("RUST_LOG", "off")
        .env("PORT", "invalid")
        .env_remove("EDGEZERO__ADAPTER__HOST")
        .env_remove("EDGEZERO__ADAPTER__PORT")
        .output()
        .expect("should run the Axum binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "should refuse startup");
    assert!(
        stderr.contains("PORT=\"invalid\"") && stderr.contains("startup refused"),
        "should name the invalid variable with logging disabled: {stderr}"
    );
}
