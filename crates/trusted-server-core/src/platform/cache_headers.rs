//! The response headers a hosting platform reads a shared cache policy from.
//!
//! Core reads `Surrogate-Control` and `CDN-Cache-Control` for every platform.
//! A platform's own header is declared by its adapter, once, before it
//! serves, so that core does not keep an origin response whose policy is in
//! a header it does not read. The declaration is read for nothing else.

use std::sync::{Mutex, PoisonError};

static DECLARED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Declares the headers the hosting platform reads a shared cache policy
/// from, each in lower case, beyond the two core reads for every platform.
///
/// An origin response carrying a policy in a declared header is not kept for
/// other readers, because core does not read that policy. A name declared
/// twice is kept once.
pub fn declare_platform_cache_headers(names: &[&'static str]) {
    let mut declared = DECLARED.lock().unwrap_or_else(PoisonError::into_inner);
    for name in names {
        if !declared.contains(name) {
            declared.push(name);
        }
    }
}

/// The headers the running platform declared, in declaration order.
#[must_use]
pub fn platform_cache_headers() -> Vec<&'static str> {
    DECLARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_header_is_listed_once() {
        declare_platform_cache_headers(&["example-twice-cache-control"]);
        declare_platform_cache_headers(&["example-twice-cache-control"]);

        assert_eq!(
            platform_cache_headers()
                .iter()
                .filter(|name| **name == "example-twice-cache-control")
                .count(),
            1
        );
    }
}
