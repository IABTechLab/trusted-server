//! Re-executes this build script; the child attempts one loopback connection.

use std::net::TcpStream;
use std::process::Command;

/// Marks the re-executed child.
const CHILD: &str = "TS_DENIAL_GRANDCHILD";

fn main() {
    if std::env::var_os(CHILD).is_some() {
        let report = match TcpStream::connect(("127.0.0.1", 9)) {
            Ok(_) => "connected".to_owned(),
            Err(error) => format!("errno=[{}]", error.raw_os_error().unwrap_or(-1)),
        };
        println!("{report}");
        return;
    }
    println!("cargo::rerun-if-changed=build.rs");
    let program = std::env::current_exe().expect("should locate the build script");
    let report = match Command::new(program).env(CHILD, "1").output() {
        Ok(output) if output.status.success() => {
            format!("child {}", String::from_utf8_lossy(&output.stdout).trim())
        }
        Ok(output) => format!("child failed with {}", output.status),
        Err(error) => format!("child could not start: {error}"),
    };
    eprintln!("TS_DENIAL case=build-grandchild {report}");
    std::process::exit(1);
}
