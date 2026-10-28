//! Which app-config blob serves a request.
//!
//! The service's selector is the entry [`SELECTOR_ENTRY`] in the config store
//! linked as `trusted_server_config`, beside the blobs it names. A service
//! without the entry reads one blob, at the logical store ID, and a selector
//! without [`HOST_PLACEHOLDER`] is the key of one blob. A selector that
//! carries the placeholder names a blob for each host instead. The
//! placeholder is replaced by the host a request names, so one service serves
//! several publishers, each from a blob of its own.
//!
//! A deployment's request addressing may name selector values in place of
//! the host, and the first value whose blob the store holds is read.
//!
//! A host that has no blob is refused. It is never answered from another key.

use core::fmt::Display;

/// The config store entry that holds the service's selector.
pub(crate) const SELECTOR_ENTRY: &str = "TS__CONFIG__KEY";

/// The part of a selector that a request's host replaces.
pub(crate) const HOST_PLACEHOLDER: &str = "{host}";

/// The longest name DNS carries.
const MAX_HOST_LEN: usize = 253;

/// The longest key a Fastly config store entry takes, in characters.
const MAX_KEY_LEN: usize = 255;

/// Why a request is not served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The request names no host, or a host the store holds no blob for, or
    /// the store holds a blob for none of the selector values a request
    /// addressing named.
    NoConfigForHost,
    /// The selector, or whether the store holds the host's blob, could not
    /// be read.
    StoreUnavailable(String),
    /// The selector is malformed, as the message says.
    MalformedSelector(String),
}

/// The selector `lookup` finds at [`SELECTOR_ENTRY`], or `logical_key` when
/// there is no entry.
///
/// # Errors
///
/// Returns [`Refusal::StoreUnavailable`] when the lookup fails and
/// [`Refusal::MalformedSelector`] when the selector is malformed, so a
/// service whose selector cannot be used serves no request rather than
/// reading the logical key for every host.
pub(crate) fn read_selector<E: Display>(
    logical_key: &str,
    lookup: impl FnOnce(&str) -> Result<Option<String>, E>,
) -> Result<String, Refusal> {
    match lookup(SELECTOR_ENTRY) {
        Ok(None) => Ok(logical_key.to_owned()),
        Ok(Some(selector)) => match malformed(&selector) {
            None => Ok(selector),
            Some(rule) => Err(Refusal::MalformedSelector(format!(
                "selector {selector:?} in `{SELECTOR_ENTRY}` {rule}"
            ))),
        },
        Err(error) => Err(Refusal::StoreUnavailable(error.to_string())),
    }
}

/// The rule `selector` breaks, or `None` when it is well formed.
///
/// A brace outside the one placeholder catches `{Host}`, `{hostname}` and an
/// unclosed brace, each of which would otherwise be one key for every host.
fn malformed(selector: &str) -> Option<&'static str> {
    if selector.is_empty() {
        Some("is empty")
    } else if selector
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        Some("carries whitespace or a control character")
    } else if selector.matches(HOST_PLACEHOLDER).count() > 1 {
        Some("carries `{host}` more than once")
    } else if selector
        .replacen(HOST_PLACEHOLDER, "", 1)
        .contains(['{', '}'])
    {
        Some("carries a brace that is not part of `{host}`")
    } else if selector.chars().count() > MAX_KEY_LEN {
        Some("is longer than 255 characters")
    } else {
        None
    }
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
/// `selector` is the service's selector, or the logical store ID
/// where none is set. A selector without the placeholder is the key itself
/// and nothing is looked up, which is every service that serves one
/// publisher. Otherwise the key is the selector with the request's host in
/// the placeholder's place, and `exists` is asked whether the store holds it.
/// A key longer than the store takes is never looked up.
///
/// # Errors
///
/// Returns [`Refusal::NoConfigForHost`] when the request names no host name,
/// the host's key is too long, or the store holds no blob at it. Returns
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
/// is the key. A value that is not a host name, or whose key would be
/// longer than the store takes, is passed over without a lookup, so no value
/// can name the logical store ID or a part of a blob.
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
        if key.chars().count() > MAX_KEY_LEN {
            continue;
        }
        match exists(&key) {
            Ok(true) => return Ok(key),
            Ok(false) => {}
            Err(error) => return Err(Refusal::StoreUnavailable(error.to_string())),
        }
    }
    Err(Refusal::NoConfigForHost)
}

/// The selector in the config store linked as `store_name`, as
/// [`read_selector`] reads it.
///
/// # Errors
///
/// Returns [`Refusal::StoreUnavailable`] when the store cannot be opened or
/// the lookup fails, and [`Refusal::MalformedSelector`] as [`read_selector`]
/// does. Uses [`fastly::ConfigStore::try_get`], because `get` panics on a
/// lookup error.
pub(crate) fn selector_in_store(store_name: &str, logical_key: &str) -> Result<String, Refusal> {
    let store = fastly::ConfigStore::try_open(store_name).map_err(|error| {
        Refusal::StoreUnavailable(format!(
            "failed to open config store `{store_name}`: {error}"
        ))
    })?;
    read_selector(logical_key, |entry| {
        store.try_get(entry).map_err(|error| {
            format!("failed to look up `{entry}` in config store `{store_name}`: {error}")
        })
    })
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

    /// A store whose selector entry holds `selector`, or nothing.
    fn selector_entry(
        selector: Option<&str>,
    ) -> impl FnOnce(&str) -> Result<Option<String>, String> + '_ {
        move |entry| {
            assert_eq!(entry, SELECTOR_ENTRY, "should read the selector entry");
            Ok(selector.map(str::to_owned))
        }
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
    fn a_host_key_over_255_characters_is_not_looked_up() {
        let label = "a".repeat(63);
        let host = format!("{label}.{label}.{label}.{}", "a".repeat(61));
        assert_eq!(host.len(), MAX_HOST_LEN, "should build the longest name");

        assert_eq!(
            resolve_key("{host}_v2", Some(&host), no_lookup),
            Err(Refusal::NoConfigForHost),
            "a key of 256 characters should count as no blob, with no lookup"
        );
        let longest_key = format!("{host}_v");
        assert_eq!(
            resolve_key("{host}_v", Some(&host), holding(&[longest_key.as_str()])),
            Ok(longest_key.clone()),
            "a key of 255 characters should still be looked up"
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
    fn an_absent_entry_selects_the_logical_key() {
        assert_eq!(
            read_selector("trusted_server_config", selector_entry(None)),
            Ok("trusted_server_config".to_owned()),
            "a store with no selector should read the logical key"
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

    #[test]
    fn a_well_formed_entry_is_the_selector_as_written() {
        for selector in ["{host}", "{host}_v2", "site_a"] {
            assert_eq!(
                read_selector("trusted_server_config", selector_entry(Some(selector))),
                Ok(selector.to_owned()),
                "`{selector}` should be read as written"
            );
        }
    }

    #[test]
    fn a_malformed_entry_is_refused() {
        let too_long = "a".repeat(MAX_KEY_LEN + 1);
        for selector in [
            "",
            " {host}",
            "{host}\t",
            "{host}\u{7}",
            "{host}{host}",
            "{Host}",
            "{hostname}",
            "{host",
            "host}",
            too_long.as_str(),
        ] {
            let read = read_selector("trusted_server_config", selector_entry(Some(selector)));
            assert!(
                matches!(read, Err(Refusal::MalformedSelector(_))),
                "{selector:?} should be malformed and never one key for every host, got {read:?}"
            );
        }
    }

    #[test]
    fn a_failed_lookup_of_the_entry_is_store_unavailable() {
        let read = read_selector("trusted_server_config", |_entry| {
            Err::<Option<String>, _>("lookup failed")
        });

        assert_eq!(
            read,
            Err(Refusal::StoreUnavailable("lookup failed".to_owned())),
            "should refuse rather than fall back to the logical key"
        );
    }

    #[test]
    fn an_addressed_value_whose_key_is_too_long_is_passed_over() {
        let label = "a".repeat(63);
        let longest_host = format!("{label}.{label}.{label}.{}", "a".repeat(61));
        let asked = RefCell::new(Vec::new());

        let resolved = resolve_addressed_key(
            "{host}_v2",
            &values(&[longest_host.as_str(), "a.example"]),
            recording(&["a.example_v2"], &asked),
        );

        assert_eq!(
            resolved,
            Ok("a.example_v2".to_owned()),
            "should go on to the next value"
        );
        assert_eq!(
            *asked.borrow(),
            ["a.example_v2"],
            "should never look up a key longer than the store takes"
        );
    }
}
