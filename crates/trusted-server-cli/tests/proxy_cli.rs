//! Public command availability regression.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
#[cfg(target_os = "linux")]
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

#[test]
fn dev_proxy_binary_starts_with_unified_tls_features() {
    // Run a fresh process: a provider installed by another test must not hide
    // missing initialization in the operator command.
    let directory = tempfile::tempdir().expect("should create an isolated CA directory");
    let reserved = TcpListener::bind("127.0.0.1:0").expect("should reserve a proxy address");
    let address = reserved
        .local_addr()
        .expect("should read the proxy address");
    drop(reserved);
    let mut child = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "dev",
            "proxy",
            "--map",
            "www.example.com=127.0.0.1:9",
            "--listen",
            &address.to_string(),
            "--ca-dir",
        ])
        .arg(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("should run the standalone proxy command");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut listening = false;
    while Instant::now() < deadline {
        if child
            .try_wait()
            .expect("should inspect proxy startup")
            .is_some()
        {
            break;
        }
        if let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("should bound the readiness response");
            if stream
                .write_all(
                    b"GET /proxy.pac HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .is_ok()
            {
                let mut response = String::new();
                if stream.read_to_string(&mut response).is_ok()
                    && response.starts_with("HTTP/1.1 200 ")
                    && response.contains("FindProxyForURL")
                {
                    listening = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let output = child
        .wait_with_output()
        .expect("should reap the proxy process");
    assert!(
        listening,
        "should initialize TLS and listen with unified dependencies: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
