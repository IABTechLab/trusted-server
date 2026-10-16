//! Builds without any connection attempt.

// The absolute include gives rustc a real source location to remap and retain.
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/control.rs"));
