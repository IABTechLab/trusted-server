//! Which app-config blob serves a request.
//!
//! A service reads one blob, at the key its `__KEY` selector names or at the
//! logical store ID. A selector that carries [`HOST_PLACEHOLDER`] names a blob
//! for each host instead. The placeholder is replaced by the host a request
//! names, so one service serves several publishers, each from a blob of its
//! own.
//!
//! A deployment's request addressing may name selector values in place of
//! the host, and the first value whose blob the store holds is read.
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
    /// The request names no host, or a host the store holds no blob for, or
    /// the store holds a blob for none of the selector values a request
    /// addressing named.
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
    exists: impl FnMut(&str) -> Result<bool, E>,
) -> Result<String, Refusal> {
    resolve_key_from(selector, host, exists)
}

/// Resolves the key of the blob that serves a request a deployment's request
/// addressing named, from the selector values it answered.
///
/// A selector without the placeholder is the key itself and the values are
/// not read, because a service that serves one publisher reads its one blob
/// whatever a request names. Otherwise each value takes the placeholder's
/// place in turn, as a host does, and the first whose blob the store holds
/// is the key. A value that is not a host name is passed over without a
/// lookup, so no value can name the logical store ID or a part of a blob.
///
/// # Errors
///
/// Returns [`Refusal::NoConfigForHost`] when the store holds a blob for none
/// of the values, so a request is never served from a key the hook did not
/// name. Returns [`Refusal::StoreUnavailable`] at the first lookup that
/// fails, because a later value must not serve while it is unknown whether
/// an earlier one has a blob.
pub(crate) fn resolve_addressed_key<E: Display>(
    selector: &str,
    values: &[String],
    exists: impl FnMut(&str) -> Result<bool, E>,
) -> Result<String, Refusal> {
    resolve_key_from(selector, values.iter().map(String::as_str), exists)
}

/// The key of the first of `names` whose blob `exists`, or the selector
/// itself when it carries no placeholder.
fn resolve_key_from<'a, E: Display>(
    selector: &str,
    names: impl IntoIterator<Item = &'a str>,
    mut exists: impl FnMut(&str) -> Result<bool, E>,
) -> Result<String, Refusal> {
    if !selector.contains(HOST_PLACEHOLDER) {
        return Ok(selector.to_owned());
    }
    for name in names.into_iter().filter_map(normalized_host) {
        let key = selector.replace(HOST_PLACEHOLDER, &name);
        match exists(&key) {
            Ok(true) => return Ok(key),
            Ok(false) => {}
            Err(error) => return Err(Refusal::StoreUnavailable(error.to_string())),
        }
    }
    Err(Refusal::NoConfigForHost)
}

/// Whether the config store linked as `store_name` holds an entry at `key`.
///
/// Reads the entry at the key alone and not the chunks a large blob is split
/// into, because the question is only whether the host has a blob.
///
/// # Errors
///
/// Returns a message naming the store when it cannot be opened or the lookup
/// fails. The message leaves the key out, because a key built from a value
/// a request named is not logged. Uses [`fastly::ConfigStore::try_get`],
/// because `get` panics on a lookup error.
pub(crate) fn blob_exists(store_name: &str, key: &str) -> Result<bool, String> {
    let store = fastly::ConfigStore::try_open(store_name)
        .map_err(|error| format!("failed to open config store `{store_name}`: {error}"))?;
    store
        .try_get(key)
        .map(|value| value.is_some())
        .map_err(|error| {
            format!("failed to look up an entry in config store `{store_name}`: {error}")
        })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Stands in for a store lookup that must not happen.
    fn no_lookup(key: &str) -> Result<bool, String> {
        panic!("the store must not be asked about `{key}`")
    }

    /// A store holding exactly `keys`.
    fn holding<'a>(keys: &'a [&'a str]) -> impl FnMut(&str) -> Result<bool, String> + 'a {
        move |key| Ok(keys.contains(&key))
    }

    /// A store holding exactly `keys` that records each key it is asked
    /// about.
    fn recording<'a>(
        keys: &'a [&'a str],
        asked: &'a RefCell<Vec<String>>,
    ) -> impl FnMut(&str) -> Result<bool, String> + 'a {
        move |key| {
            asked.borrow_mut().push(key.to_owned());
            Ok(keys.contains(&key))
        }
    }

    fn values(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
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

    #[test]
    fn a_selector_value_takes_the_placeholder_s_place() {
        for (selector, key) in [
            ("{host}", "publisher.example"),
            ("{host}_staging", "publisher.example_staging"),
            ("sites.{host}", "sites.publisher.example"),
        ] {
            assert_eq!(
                resolve_addressed_key(selector, &values(&["publisher.example"]), holding(&[key])),
                Ok(key.to_owned()),
                "should key the blob by the selector value under `{selector}`"
            );
        }
        assert_eq!(
            resolve_addressed_key(
                "{host}",
                &values(&["Publisher.EXAMPLE."]),
                holding(&["publisher.example"])
            ),
            Ok("publisher.example".to_owned()),
            "a selector value should be keyed as a host is"
        );
    }

    #[test]
    fn the_first_selector_value_whose_blob_exists_is_read() {
        let asked = RefCell::new(Vec::new());

        let resolved = resolve_addressed_key(
            "{host}",
            &values(&["absent.example", "b.example", "a.example"]),
            recording(&["a.example", "b.example"], &asked),
        );

        assert_eq!(
            resolved,
            Ok("b.example".to_owned()),
            "should read the first value the store holds a blob for"
        );
        assert_eq!(
            *asked.borrow(),
            ["absent.example", "b.example"],
            "should ask in order and stop at the first blob found"
        );
    }

    #[test]
    fn every_selector_value_missing_is_refused_and_not_served_from_another_key() {
        // The store holds a blob at the logical store ID, one for the host
        // the request arrived on and one for another publisher. None of them
        // may answer for values that have none.
        let stored = ["trusted_server_config", "front.example", "a.example"];

        for names in [&[][..], &["unknown.example"], &["x.example", "y.example"]] {
            assert_eq!(
                resolve_addressed_key("{host}", &values(names), holding(&stored)),
                Err(Refusal::NoConfigForHost),
                "values {names:?} with no blob should be refused"
            );
        }
    }

    #[test]
    fn a_selector_value_that_is_not_a_host_name_is_passed_over_without_a_lookup() {
        let asked = RefCell::new(Vec::new());

        let resolved = resolve_addressed_key(
            "{host}",
            &values(&[
                "trusted_server_config",
                "publisher.example.__edgezero_chunks.abc.0",
                "",
                "a.example/path",
                "a.example",
            ]),
            recording(&["trusted_server_config", "a.example"], &asked),
        );

        assert_eq!(
            resolved,
            Ok("a.example".to_owned()),
            "should read the one value that is a host name"
        );
        assert_eq!(
            *asked.borrow(),
            ["a.example"],
            "should look up nothing a host could not name"
        );
    }

    #[test]
    fn a_selector_without_the_placeholder_ignores_the_selector_values() {
        assert_eq!(
            resolve_addressed_key("trusted_server_config", &values(&["a.example"]), no_lookup),
            Ok("trusted_server_config".to_owned()),
            "a service that serves one publisher should read its one key"
        );
    }

    #[test]
    fn a_failed_lookup_of_a_selector_value_refuses_and_tries_no_later_value() {
        let asked = RefCell::new(Vec::new());

        let refused = resolve_addressed_key(
            "{host}",
            &values(&["a.example", "b.example"]),
            |key: &str| {
                asked.borrow_mut().push(key.to_owned());
                Err::<bool, _>("lookup failed")
            },
        );

        assert_eq!(
            refused,
            Err(Refusal::StoreUnavailable("lookup failed".to_owned())),
            "should not serve while an earlier value's blob is unknown"
        );
        assert_eq!(*asked.borrow(), ["a.example"], "should try no later value");
    }
}
