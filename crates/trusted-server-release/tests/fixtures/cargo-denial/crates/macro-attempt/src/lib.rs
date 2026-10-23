//! A procedural macro that attempts one loopback connection while it expands.

use std::net::TcpStream;

use proc_macro::TokenStream;

/// Expands to a compile error reporting the connection attempt.
#[proc_macro]
pub fn attempt(_input: TokenStream) -> TokenStream {
    let report = match TcpStream::connect(("127.0.0.1", 9)) {
        Ok(_) => "connected".to_owned(),
        Err(error) => format!("errno=[{}]", error.raw_os_error().unwrap_or(-1)),
    };
    format!("compile_error!(\"TS_DENIAL case=macro-attempt {report}\");")
        .parse()
        .expect("should parse the generated report")
}
