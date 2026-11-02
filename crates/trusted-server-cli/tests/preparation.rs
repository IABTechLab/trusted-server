//! A fresh CLI process must reach the worker and preserve per-target refusal.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

#[test]
fn fresh_cli_reports_queue_refusal_without_panicking_or_disclosing_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind worker boundary");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("should read address")
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "preparation",
            "warm",
            "--worker-url",
            &url,
            "--target",
            "/a",
            "--register",
        ])
        .env_clear()
        .env("TS_PREPARATION_OPERATOR_SECRET", "operator-boundary-secret")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("should start CLI");
    listener.set_nonblocking(true).expect("should bound accept");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if child.try_wait().expect("should inspect CLI").is_some()
                    || std::time::Instant::now() >= deadline
                {
                    let _ = child.kill();
                    let output = child.wait_with_output().expect("should reap CLI");
                    panic!(
                        "CLI did not reach worker: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("worker accept failed: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("should bound request");
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let read = stream.read(&mut buffer).expect("should read request");
        assert!(read > 0, "request should complete");
        request.extend_from_slice(&buffer[..read]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .expect("should declare length")
                .parse::<usize>()
                .expect("should parse length");
            if request.len() >= end + 4 + length {
                break;
            }
        }
    }
    let body = r#"{"protocol":"encypher-preparation/1","targets":[{"target":{"path":"/a","variant":""},"admission":{"outcome":"queue_full"}}]}"#;
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("should answer");
    drop(stream);
    let output = child.wait_with_output().expect("should reap CLI");
    assert!(
        !output.status.success(),
        "a refused target must fail the command"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("1 target"), "{stderr}");
    assert!(!stderr.contains("operator-boundary-secret"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("queue_full"));
    assert!(!stdout.contains("operator-boundary-secret"));
}
