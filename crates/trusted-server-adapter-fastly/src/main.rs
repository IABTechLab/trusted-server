//! The Fastly Compute binary.
//!
//! A shell over the library, which holds everything of substance. A
//! deployment that ships a vendor crate writes its own binary of this shape
//! and calls `run_with` with that crate's builder.

fn main() {
    trusted_server_adapter_fastly::run();
}
