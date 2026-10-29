//! Public process tests for the `boundary-probe connect` subcommand.
//!
//! The probe makes one TCP connect to a literal loopback `IP:PORT`, prints
//! `connected` (exit 0) or `denied errno=<n>` (exit 3) with the real errno,
//! and does nothing else.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::{Command, Output};

const RELEASE_BIN: &str = env!("CARGO_BIN_EXE_trusted-server-release");
const DENIED_STATUS: i32 = 3;

fn probe(address: &str) -> Output {
    Command::new(RELEASE_BIN)
        .args(["boundary-probe", "connect", address])
        .env_clear()
        .output()
        .expect("should run the release binary")
}

fn listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind listener");
    listener
        .set_nonblocking(true)
        .expect("should make listener non-blocking");
    listener
}

fn accepted(listener: &TcpListener) -> usize {
    let mut count = 0;
    loop {
        match listener.accept() {
            Ok(_) => count += 1,
            Err(error) if error.kind() == ErrorKind::WouldBlock => return count,
            Err(error) => {
                assert!(
                    error.kind() == ErrorKind::Interrupted,
                    "should accept: {error}"
                );
            }
        }
    }
}

#[test]
fn probe_connects_to_a_loopback_listener() {
    let listener = listener();
    let address = listener
        .local_addr()
        .expect("should read address")
        .to_string();
    let output = probe(&address);
    assert_eq!(
        output.status.code(),
        Some(0),
        "should exit 0 when connected"
    );
    assert_eq!(
        output.stdout, b"connected\n",
        "should print exactly one report"
    );
    assert_eq!(
        accepted(&listener),
        1,
        "should complete exactly one handshake"
    );
}

#[test]
fn probe_reports_the_real_refusal_errno() {
    let address = {
        let closed = TcpListener::bind("127.0.0.1:0").expect("should bind");
        closed
            .local_addr()
            .expect("should read address")
            .to_string()
    };
    let output = probe(&address);
    assert_eq!(
        output.status.code(),
        Some(DENIED_STATUS),
        "should exit 3 when refused"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("denied errno={}\n", libc::ECONNREFUSED),
        "should report the operating system's errno"
    );
}

#[test]
fn probe_rejects_non_loopback_and_malformed_addresses() {
    for address in ["192.0.2.1:80", "localhost:80", "127.0.0.1:0", "127.0.0.1"] {
        let output = probe(address);
        assert_eq!(output.status.code(), Some(1), "should reject {address}");
        assert!(output.stdout.is_empty(), "should not report for {address}");
    }
    let extra = Command::new(RELEASE_BIN)
        .args(["boundary-probe", "connect", "127.0.0.1:80", "extra"])
        .env_clear()
        .output()
        .expect("should run the release binary");
    assert_eq!(
        extra.status.code(),
        Some(1),
        "should reject extra arguments"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn probe_is_denied_with_eperm_under_sandbox_exec() {
    let listener = listener();
    let address = listener
        .local_addr()
        .expect("should read address")
        .to_string();
    let output = Command::new("/usr/bin/sandbox-exec")
        .args([
            "-p",
            "(version 1)(allow default)(deny network*)",
            RELEASE_BIN,
            "boundary-probe",
            "connect",
            &address,
        ])
        .env_clear()
        .output()
        .expect("should run the sandboxed release binary");
    assert_eq!(
        output.status.code(),
        Some(DENIED_STATUS),
        "should exit 3 when denied"
    );
    assert_eq!(
        output.stdout, b"denied errno=1\n",
        "should report real EPERM"
    );
    assert_eq!(accepted(&listener), 0, "should observe no accept");
}
