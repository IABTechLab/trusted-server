//! The request headers a hosting platform sets itself, named by its adapter.
//!
//! Core reads and strips the standard forwarding headers on every platform.
//! A platform's own are declared by its adapter, once, before it serves:
//!
//! - a header the platform sets to `1` on a request that arrived over TLS,
//!   which core reads for the scheme when nothing better says and never
//!   forwards to an origin,
//! - any other header of the platform's that a client could also send, such
//!   as one carrying the client's address,
//! - the platform's internal headers, which are never passed on to a third
//!   party.
//!
//! A header of the first two kinds is removed at entry, with the standard
//! forwarding headers, so only the value the platform set survives. Each
//! list keeps a name declared twice once.

use std::sync::{Mutex, PoisonError};

static TLS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
static SPOOFABLE: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
static INTERNAL: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn declare(list: &Mutex<Vec<&'static str>>, names: &[&'static str]) {
    let mut declared = list.lock().unwrap_or_else(PoisonError::into_inner);
    for name in names {
        if !declared.contains(name) {
            declared.push(name);
        }
    }
}

fn declared(list: &Mutex<Vec<&'static str>>) -> Vec<&'static str> {
    list.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Declares the headers, each in lower case, that the hosting platform sets
/// to `1` on a request that arrived over TLS.
pub fn declare_platform_tls_headers(names: &[&'static str]) {
    declare(&TLS, names);
}

/// The TLS headers the running platform declared, in declaration order.
#[must_use]
pub fn platform_tls_headers() -> Vec<&'static str> {
    declared(&TLS)
}

/// Declares the hosting platform's own headers, each in lower case, that a
/// client could also send, beyond its TLS headers.
pub fn declare_platform_spoofable_headers(names: &[&'static str]) {
    declare(&SPOOFABLE, names);
}

/// The spoofable headers the running platform declared, beyond its TLS
/// headers, in declaration order.
#[must_use]
pub fn platform_spoofable_headers() -> Vec<&'static str> {
    declared(&SPOOFABLE)
}

/// Declares the hosting platform's internal headers, each in lower case,
/// which are never passed on to a third party.
pub fn declare_platform_internal_headers(names: &[&'static str]) {
    declare(&INTERNAL, names);
}

/// Whether the running platform declared `name`, in lower case, as one of
/// its internal headers.
#[must_use]
pub fn is_platform_internal_header(name: &str) -> bool {
    INTERNAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_header_is_listed_once_in_its_own_list() {
        declare_platform_tls_headers(&["example-twice-tls"]);
        declare_platform_tls_headers(&["example-twice-tls"]);
        declare_platform_internal_headers(&["example-twice-internal"]);

        assert_eq!(
            platform_tls_headers()
                .iter()
                .filter(|name| **name == "example-twice-tls")
                .count(),
            1
        );
        assert!(!platform_spoofable_headers().contains(&"example-twice-tls"));
        assert!(is_platform_internal_header("example-twice-internal"));
        assert!(!is_platform_internal_header("example-twice-tls"));
    }
}
