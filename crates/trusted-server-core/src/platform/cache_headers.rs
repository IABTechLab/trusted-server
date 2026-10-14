//! The response headers a hosting platform reads a shared cache policy from.
//!
//! Core reads `Surrogate-Control` and `CDN-Cache-Control` for every platform.
//! A platform's own header is declared by its adapter, once, before it
//! serves, and joins the headers core strips from every response it makes
//! private.

use std::sync::{Mutex, PoisonError};

static DECLARED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Declares the headers the hosting platform reads a shared cache policy
/// from, each in lower case, beyond the two core reads for every platform.
///
/// Each declared name is stripped from every response core makes private,
/// so a policy an origin or an operator wrote in the platform's own header
/// never reaches a shared cache on a personalized response. A name declared
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
