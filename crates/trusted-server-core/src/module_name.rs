//! The name a module is selected by, taken from the folder its crate lives in.
//!
//! A crate at `crates/geo/example` supplies the module `geo.example`: the path
//! below the nearest `crates` directory, with `.` between the parts. A crate
//! supplying several modules names each one after itself, as
//! `middleware.example.hide`. A section may leave its own type folder off, so
//! under `[geo]` the name `example` means `geo.example`. Core's own modules
//! take bare names, such as `builtin`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

/// The name of the module the calling crate supplies, or, given an entry, the
/// name of that entry, as `<crate path>.<entry>`.
///
/// Derived from Cargo's `CARGO_MANIFEST_DIR` when the calling crate is built,
/// so a name cannot drift from its folder. A crate built from outside a
/// `crates` directory, such as a published package, is named by its package
/// name.
#[macro_export]
macro_rules! module_name {
    () => {
        $crate::module_name::from_manifest(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), None)
    };
    ($entry:expr) => {
        $crate::module_name::from_manifest(
            env!("CARGO_MANIFEST_DIR"),
            env!("CARGO_PKG_NAME"),
            Some($entry),
        )
    };
}

/// The module name for a crate built at `manifest_dir`, with `entry` after it
/// when the crate supplies several.
///
/// Each distinct name is made once and kept for the life of the process, so
/// a module can hand out `&'static str` without its crate holding a copy.
#[must_use]
pub fn from_manifest(manifest_dir: &str, package: &str, entry: Option<&str>) -> &'static str {
    static NAMES: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut name = crate_path(manifest_dir).unwrap_or_else(|| package.to_owned());
    if let Some(entry) = entry {
        name.push('.');
        name.push_str(entry);
    }
    let mut names = NAMES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = names.get(&name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.clone().into_boxed_str());
    names.insert(name, leaked);
    leaked
}

/// The path below the nearest `crates` directory, joined by `.`, or `None`
/// when the folder is not below one.
fn crate_path(manifest_dir: &str) -> Option<String> {
    let parts: Vec<&str> = manifest_dir
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    let crates = parts.iter().rposition(|part| *part == "crates")?;
    let below = &parts[crates + 1..];
    (!below.is_empty()).then(|| below.join("."))
}

/// The longest module name, in bytes.
const MAX_NAME_BYTES: usize = 255;

/// Whether `name` can name a module: parts joined by `.`, each of lower case
/// letters, digits, `_` or `-`.
///
/// The same rule serves a crate's derived name and a name written in a
/// section, so anything a crate can be called can also be written.
#[must_use]
pub fn is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
}

/// The offered name that `written`, selected in a section of type
/// `type_folder`, means.
///
/// The name as written wins, which is how a bare name reaches one of core's
/// own modules and a full path reaches a module under another type. Otherwise
/// the type folder is put in front, because a section is named exactly as its
/// folder.
#[must_use]
pub fn resolve<'a>(type_folder: &str, written: &str, offered: &[&'a str]) -> Option<&'a str> {
    if let Some(exact) = offered.iter().find(|name| **name == written) {
        return Some(exact);
    }
    offered.iter().copied().find(|name| {
        name.split_once('.')
            .is_some_and(|(folder, rest)| rest == written && folder == type_folder)
    })
}

/// The short form of `name` within a section of type `type_folder`, being the
/// name with the type folder taken off, or the name itself when it lives
/// under another type or is one of core's own.
#[must_use]
pub fn short_form<'a>(type_folder: &str, name: &'a str) -> &'a str {
    match name.split_once('.') {
        Some((folder, rest)) if folder == type_folder => rest,
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crate_is_named_by_its_path_below_crates() {
        for (dir, expected) in [
            (
                "/home/runner/work/trusted-server/crates/geo/example",
                "geo.example",
            ),
            (
                "D:\\Workspace\\trusted-server\\crates\\permission-signal\\example-scheme",
                "permission-signal.example-scheme",
            ),
            (
                "/repo/vendor/trusted-server/crates/middleware/example",
                "middleware.example",
            ),
            ("/repo/crates/demo/example", "demo.example"),
        ] {
            assert_eq!(crate_path(dir).as_deref(), Some(expected), "{dir}");
        }
    }

    #[test]
    fn the_nearest_crates_directory_counts() {
        assert_eq!(
            crate_path("/work/crates/tools/checkout/crates/geo/example").as_deref(),
            Some("geo.example")
        );
    }

    #[test]
    fn a_crate_outside_a_crates_directory_takes_its_package_name() {
        assert_eq!(
            from_manifest("/registry/src/example-geo-0.1.0", "example-geo", None),
            "example-geo"
        );
        assert_eq!(crate_path("/work/crates"), None);
    }

    #[test]
    fn an_entry_follows_the_crate_path_and_each_name_is_kept_once() {
        let first = from_manifest("/repo/crates/middleware/example", "x", Some("hide"));
        let second = from_manifest("/repo/crates/middleware/example", "x", Some("hide"));

        assert_eq!(first, "middleware.example.hide");
        assert!(
            std::ptr::eq(first, second),
            "the same name is not made twice"
        );
    }

    #[test]
    fn this_crate_names_itself() {
        assert_eq!(crate::module_name!(), "trusted-server-core");
    }

    #[test]
    fn a_name_resolves_as_written_then_within_its_type() {
        let offered = [
            "builtin",
            "geo.example",
            "permission-signal.us-privacy",
            "testing.mock",
        ];

        assert_eq!(resolve("device", "builtin", &offered), Some("builtin"));
        assert_eq!(resolve("geo", "example", &offered), Some("geo.example"));
        assert_eq!(resolve("geo", "geo.example", &offered), Some("geo.example"));
        assert_eq!(
            resolve("permission-signal", "us-privacy", &offered),
            Some("permission-signal.us-privacy")
        );
        assert_eq!(
            resolve("permission_signal", "us-privacy", &offered),
            None,
            "a section is named exactly as its folder"
        );
        assert_eq!(
            resolve("ad-server", "testing.mock", &offered),
            Some("testing.mock")
        );
        assert_eq!(
            resolve("device", "example", &offered),
            None,
            "another type's module"
        );
        assert_eq!(resolve("geo", "missing", &offered), None);
    }

    #[test]
    fn a_name_is_lower_case_parts_joined_by_dots() {
        for good in [
            "4example",
            "us-privacy",
            "host_signals",
            "middleware.example.hide",
        ] {
            assert!(is_valid(good), "{good}");
        }
        for bad in [
            "",
            "Geo",
            "geo..example",
            ".geo",
            "geo.",
            "a b",
            "geo/example",
        ] {
            assert!(!is_valid(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_short_form_drops_only_the_sections_own_type() {
        assert_eq!(short_form("geo", "geo.example"), "example");
        assert_eq!(
            short_form("permission-signal", "permission-signal.example-scheme"),
            "example-scheme"
        );
        assert_eq!(short_form("ad-server", "testing.mock"), "testing.mock");
        assert_eq!(short_form("geo", "platform"), "platform");
    }
}
