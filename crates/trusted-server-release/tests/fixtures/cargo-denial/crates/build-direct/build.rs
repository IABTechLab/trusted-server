//! Attempts one loopback connection while Cargo runs this build script.

use std::net::TcpStream;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let report = match TcpStream::connect(("127.0.0.1", 9)) {
        Ok(_) => "connected".to_owned(),
        Err(error) => format!("errno=[{}]", error.raw_os_error().unwrap_or(-1)),
    };
    eprintln!("TS_DENIAL case=build-direct {report}");
    std::process::exit(1);
}
