//! Which app-config blob serves a request.
//!
//! A service reads one blob, at the key its `__KEY` selector names or at the
//! logical store ID. A selector that carries [`HOST_PLACEHOLDER`] names a blob
//! for each host instead. The placeholder is replaced by the host a request
//! names, so one service serves several publishers, each from a blob of its
//! own.
//!
//! A host that has no blob is refused. It is never answered from another key.

use core::fmt::Display;

/// The part of a `__KEY` selector that a request's host replaces.
pub(crate) const HOST_PLACEHOLDER: &str = "{host}";

/// The longest name DNS carries.
const MAX_HOST_LEN: usize = 253;

/// Why a request is not served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The request names no host, or a host the store holds no blob for.
    NoConfigForHost,
    /// Whether the store holds the host's blob could not be read.
    StoreUnavailable(String),
}

/// The host as a blob is keyed by it, or `None` when `host` is not a host
/// name.
///
/// Lowercased, with a port and one trailing dot removed. Only letters, digits
/// and hyphens pass, in labels joined by single dots, and no label starts or
/// ends with a hyphen.
pub(crate) fn normalized_host(host: &str) -> Option<String> {
    let without_port = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    let name = without_port.strip_suffix('.').unwrap_or(without_port);
    let is_host_name = !name.is_empty()
        && name.len() <= MAX_HOST_LEN
        && name.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    is_host_name.then(|| name.to_ascii_lowercase())
}

/// Resolves the key of the blob that serves a request.
///
/// `selector` is the service's `__KEY` selector, or the logical store ID
/// where none is set. A selector without the placeholder is the key itself
/// and nothing is looked up, which is every service that serves one
/// publisher. Otherwise the key is the selector with the request's host in
/// the placeholder's place, and `exists` is asked whether the store holds it.
///
/// # Errors
///
/// Returns [`Refusal::NoConfigForHost`] when the request names no host name
/// or the store holds no blob at the host's key. Returns
/// [`Refusal::StoreUnavailable`] when `exists` fails, because a request is
/// not served while it is unknown which blob is its own.
pub(crate) fn resolve_key<E: Display>(
    selector: &str,
    host: Option<&str>,
    exists: impl FnOnce(&str) -> Result<bool, E>,
) -> Result<String, Refusal> {
    if !selector.contains(HOST_PLACEHOLDER) {
        return Ok(selector.to_owned());
    }
    let key = host
        .and_then(normalized_host)
        .map(|host| selector.replace(HOST_PLACEHOLDER, &host))
        .ok_or(Refusal::NoConfigForHost)?;
    match exists(&key) {
        Ok(true) => Ok(key),
        Ok(false) => Err(Refusal::NoConfigForHost),
        Err(error) => Err(Refusal::StoreUnavailable(error.to_string())),
    }
}

/// Whether the config store linked as `store_name` holds an entry at `key`.
///
/// Reads the entry at the key alone and not the chunks a large blob is split
/// into, because the question is only whether the host has a blob.
///
/// # Errors
///
/// Returns a message naming the store when it cannot be opened or the lookup
/// fails. Uses [`fastly::ConfigStore::try_get`], because `get` panics on a
/// lookup error.
pub(crate) fn blob_exists(store_name: &str, key: &str) -> Result<bool, String> {
    let store = fastly::ConfigStore::try_open(store_name)
        .map_err(|error| format!("failed to open config store `{store_name}`: {error}"))?;
    store
        .try_get(key)
        .map(|value| value.is_some())
        .map_err(|error| {
            format!("failed to look up `{key}` in config store `{store_name}`: {error}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for a store lookup that must not happen.
    fn no_lookup(key: &str) -> Result<bool, String> {
        panic!("the store must not be asked about `{key}`")
    }

    /// A store holding exactly `keys`.
    fn holding<'a>(keys: &'a [&'a str]) -> impl FnOnce(&str) -> Result<bool, String> + 'a {
        move |key| Ok(keys.contains(&key))
    }

    #[test]
    fn a_selector_without_the_placeholder_is_the_key_and_nothing_is_looked_up() {
        for selector in ["trusted_server_config", "trusted_server_config_staging"] {
            assert_eq!(
                resolve_key(selector, Some("publisher.example"), no_lookup),
                Ok(selector.to_owned()),
                "a service that serves one publisher should read its one key"
            );
        }
        assert_eq!(
            resolve_key("trusted_server_config", None, no_lookup),
            Ok("trusted_server_config".to_owned()),
            "one key should serve a request that names no host"
        );
    }

    #[test]
    fn the_host_takes_the_placeholder_s_place() {
        for (selector, key) in [
            ("{host}", "publisher.example"),
            ("{host}_staging", "publisher.example_staging"),
            ("sites.{host}", "sites.publisher.example"),
        ] {
            assert_eq!(
                resolve_key(selector, Some("publisher.example"), holding(&[key])),
                Ok(key.to_owned()),
                "should key the blob by the host under `{selector}`"
            );
        }
    }

    #[test]
    fn each_host_reads_a_key_of_its_own() {
        let stored = ["a.example", "b.example"];

        assert_eq!(
            resolve_key("{host}", Some("a.example"), holding(&stored)),
            Ok("a.example".to_owned()),
            "should read the first publisher's blob for its host"
        );
        assert_eq!(
            resolve_key("{host}", Some("b.example"), holding(&stored)),
            Ok("b.example".to_owned()),
            "should read the second publisher's blob for its host"
        );
    }

    #[test]
    fn a_host_is_keyed_without_case_port_or_trailing_dot() {
        for host in [
            "publisher.example",
            "Publisher.EXAMPLE",
            "publisher.example:443",
            "publisher.example.",
            "PUBLISHER.example.:8443",
        ] {
            assert_eq!(
                normalized_host(host).as_deref(),
                Some("publisher.example"),
                "`{host}` should name the same site"
            );
        }
        assert_eq!(
            normalized_host("127.0.0.1:7676").as_deref(),
            Some("127.0.0.1"),
            "should key a local address without its port"
        );
        assert_eq!(
            normalized_host("xn--bcher-kva.example").as_deref(),
            Some("xn--bcher-kva.example"),
            "should keep an internationalized name as it is sent"
        );
    }

    #[test]
    fn two_names_of_one_publisher_are_two_keys() {
        assert_ne!(
            normalized_host("www.publisher.example"),
            normalized_host("publisher.example"),
            "a publisher decides what each of its names serves"
        );
    }

    #[test]
    fn what_is_not_a_host_name_is_refused_without_a_lookup() {
        let too_long = "a".repeat(MAX_HOST_LEN + 1);
        for host in [
            "",
            ".",
            "publisher..example",
            ".publisher.example",
            "publisher.example..",
            "publisher.example:",
            "publisher.example:80:80",
            " publisher.example",
            "publisher.example/path",
            "[::1]",
            "[::1]:7676",
            "publisher_example.com",
            "trusted_server_config",
            "publisher.example.__edgezero_chunks.abc.0",
            "{host}",
            "b\u{fc}cher.example",
            too_long.as_str(),
        ] {
            assert_eq!(
                resolve_key("{host}", Some(host), no_lookup),
                Err(Refusal::NoConfigForHost),
                "`{host}` is not a host name and should be refused"
            );
        }
        assert_eq!(
            resolve_key("{host}", None, no_lookup),
            Err(Refusal::NoConfigForHost),
            "a request that names no host should be refused"
        );
    }

    #[test]
    fn a_label_that_starts_or_ends_with_a_hyphen_is_refused() {
        for host in [
            "-a.example",
            "a-.example",
            "publisher.-example",
            "publisher.example-",
            "-",
        ] {
            assert_eq!(
                resolve_key("{host}", Some(host), no_lookup),
                Err(Refusal::NoConfigForHost),
                "`{host}` is not a host name and should be refused"
            );
        }
        assert_eq!(
            normalized_host("pub-lisher.example").as_deref(),
            Some("pub-lisher.example"),
            "a hyphen inside a label should pass"
        );
    }

    #[test]
    fn the_longest_host_name_is_keyed() {
        let label = "a".repeat(63);
        let host = format!("{label}.{label}.{label}.{}", "a".repeat(61));
        assert_eq!(host.len(), MAX_HOST_LEN, "should build the longest name");

        assert_eq!(
            normalized_host(&host),
            Some(host.clone()),
            "should key a name of the greatest length DNS carries"
        );
    }

    #[test]
    fn a_host_with_no_blob_is_refused_and_not_served_from_another_key() {
        // The store holds a blob at the logical store ID and one for another
        // publisher. Neither may answer for a host that has none.
        let stored = ["trusted_server_config", "a.example"];

        assert_eq!(
            resolve_key("{host}", Some("unknown.example"), holding(&stored)),
            Err(Refusal::NoConfigForHost),
            "a host with no blob should be refused"
        );
    }

    #[test]
    fn a_failed_lookup_refuses_and_does_not_serve() {
        let refused = resolve_key("{host}", Some("publisher.example"), |_key| {
            Err::<bool, _>("lookup failed")
        });

        assert_eq!(
            refused,
            Err(Refusal::StoreUnavailable("lookup failed".to_owned())),
            "should not serve while the host's blob is unknown"
        );
    }
}
