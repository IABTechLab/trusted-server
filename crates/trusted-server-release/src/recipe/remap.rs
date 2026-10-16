//! Deterministic compiler path remapping for the target units of one build.
//!
//! Debug info (`debug = 1`), panic locations and `file!()` embed the paths
//! rustc compiled from. Every root those paths can come from is remapped to a
//! fixed prefix, so the final bytes do not depend on where a build's
//! checkout, vendor tree, Cargo home, Rust tools or target directory live.
//!
//! # Cargo precedence
//!
//! Cargo takes rustflags from the first source that defines any:
//! `CARGO_ENCODED_RUSTFLAGS`, then `RUSTFLAGS`, then every matching
//! `target.<triple>.rustflags` and `target.<cfg>.rustflags` joined, and only
//! then `build.rustflags`. A `--config` value merges with the configuration
//! files and arrays concatenate with the command line last. The remap
//! therefore travels as one `--config target."<triple>".rustflags=[...]`
//! entry: reviewed target flags in `.cargo/config.toml` stay in effect before
//! it. Either environment variable would replace every configured flag, so
//! the audited environment rejects both and the plan never sets them. Any
//! target rustflags supersede `build.rustflags`, so the reviewed
//! configuration must not set it ([`check_cargo_config`]), and the legacy
//! extensionless `.cargo/config` layer is refused outright
//! ([`check_no_extensionless_config`]).
//!
//! # Ordering
//!
//! When several `--remap-path-prefix` flags match one path, rustc applies the
//! last matching one. The remapped roots are required to be pairwise
//! disjoint, so at most one ever matches and the emitted order cannot change
//! the result.
//!
//! # Limitation
//!
//! Remapping rewrites paths rustc knows as source locations. A string built
//! by `env!("CARGO_MANIFEST_DIR")`, `env!("OUT_DIR")` or `concat!` of them is
//! an ordinary literal and keeps the execution path. The final bytes are
//! therefore scanned ([`scan_remapped_bytes`]) so such a literal fails the
//! build evidence instead of passing silently.
//!
//! With an explicit `--target`, rustflags reach only target units, never
//! build scripts or procedural macros: exactly the code in the artifact.

use std::io;
use std::path::Path;

use error_stack::Report;

use super::fresh::disjoint;
use super::recipe_error;
use crate::error::ReleaseError;

/// Tracked Cargo configuration Cargo reads from the checkout root.
pub(super) const CARGO_CONFIG: &str = ".cargo/config.toml";
/// Legacy extensionless configuration Cargo also reads; always refused.
pub(super) const LEGACY_CARGO_CONFIG: &str = ".cargo/config";
/// Fixed prefix of the source checkout.
pub(super) const REMAPPED_CHECKOUT: &str = "/trusted-server";
/// Fixed prefix of the vendored dependency directory.
pub(super) const REMAPPED_VENDOR: &str = "/cargo-vendor";
/// Fixed prefix of the private Cargo home.
pub(super) const REMAPPED_CARGO_HOME: &str = "/cargo-home";
/// Fixed prefix of the Rust tool root, which is the compiler sysroot.
pub(super) const REMAPPED_TOOLS: &str = "/rust-tools";
/// Fixed prefix of the Cargo target directory, which holds every `OUT_DIR`.
pub(super) const REMAPPED_TARGET: &str = "/cargo-target";
/// Every fixed prefix, in emitted order.
pub(super) const REMAPPED_PREFIXES: [&str; 5] = [
    REMAPPED_CHECKOUT,
    REMAPPED_VENDOR,
    REMAPPED_CARGO_HOME,
    REMAPPED_TOOLS,
    REMAPPED_TARGET,
];
/// Execution-namespace prefix of every container mount; never in final bytes.
const EXECUTION_MOUNT_PREFIX: &str = "/boundary/";
/// Most forbidden prefixes one scan accepts.
const MAX_FORBIDDEN: usize = 64;
/// Longest forbidden prefix one scan accepts.
const MAX_PREFIX_BYTES: usize = 4096;

/// Execution paths of every root a compiled path can come from.
///
/// Each must already be plain absolute text without quotes, backslashes,
/// `:` or control characters, as [`super::paths::execution_text`] returns.
pub(super) struct RemapSources<'a> {
    pub(super) checkout: &'a str,
    pub(super) vendor: &'a str,
    pub(super) cargo_home: &'a str,
    pub(super) tools: &'a str,
    pub(super) target: &'a str,
}

/// Returns the `--config` pair remapping every source root for `triple`.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when two remapped roots are equal or
/// nested, since rustc's last-match-wins rule would then make the order of
/// the flags decide the compiled path.
pub(super) fn remap_arguments(
    triple: &str,
    sources: &RemapSources<'_>,
) -> Result<Vec<String>, Report<ReleaseError>> {
    let pairs = [
        ("remapped checkout", sources.checkout),
        ("remapped vendor directory", sources.vendor),
        ("remapped Cargo home", sources.cargo_home),
        ("remapped Rust tools", sources.tools),
        ("remapped target directory", sources.target),
    ];
    let labelled: Vec<(&str, &Path)> = pairs
        .iter()
        .map(|(label, from)| (*label, Path::new(*from)))
        .collect();
    disjoint(&labelled)?;
    let flags: Vec<String> = pairs
        .iter()
        .zip(REMAPPED_PREFIXES)
        .map(|((_, from), to)| format!("\"--remap-path-prefix={from}={to}\""))
        .collect();
    Ok(vec![
        "--config".to_owned(),
        format!("target.\"{triple}\".rustflags=[{}]", flags.join(",")),
    ])
}

/// Requires the reviewed Cargo configuration not to set `build.rustflags`.
///
/// The remap is a target rustflags entry, and Cargo ignores `build.rustflags`
/// whenever any target rustflags exist, so such a setting would be silently
/// dropped instead of reviewed.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the bytes are not UTF-8 TOML or set
/// `build.rustflags`.
pub(super) fn check_cargo_config(bytes: &[u8]) -> Result<(), Report<ReleaseError>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| recipe_error(format!("{CARGO_CONFIG} is not UTF-8: {error}")))?;
    let table: toml::Table = toml::from_str(text)
        .map_err(|error| recipe_error(format!("{CARGO_CONFIG} is not TOML: {error}")))?;
    let sets_build_rustflags = table
        .get("build")
        .and_then(toml::Value::as_table)
        .is_some_and(|build| build.contains_key("rustflags"));
    if sets_build_rustflags {
        return Err(recipe_error(format!(
            "{CARGO_CONFIG} sets build.rustflags, which the target path remap would supersede"
        )));
    }
    Ok(())
}

/// Refuses the legacy extensionless `.cargo/config` at the checkout root.
///
/// Cargo reads it beside `.cargo/config.toml`, so an unreviewed layer could
/// carry rustflags the remap check never sees.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when the approved source tracks it, it
/// exists on disk in any form, or its absence cannot be established.
pub(super) fn check_no_extensionless_config(
    checkout: &Path,
    tracked: bool,
) -> Result<(), Report<ReleaseError>> {
    if tracked {
        return Err(recipe_error(format!(
            "the approved source tracks {LEGACY_CARGO_CONFIG}, which is refused"
        )));
    }
    match std::fs::symlink_metadata(checkout.join(LEGACY_CARGO_CONFIG)) {
        Ok(_) => Err(recipe_error(format!(
            "the checkout holds {LEGACY_CARGO_CONFIG}, which is refused"
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(recipe_error(format!(
            "{LEGACY_CARGO_CONFIG} cannot be inspected: {error}"
        ))),
    }
}

/// Fixed prefixes found in final bytes that hold no forbidden path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RemapScan {
    found: Vec<&'static str>,
}

impl RemapScan {
    /// The fixed prefixes, each followed by `/`, found in the bytes.
    pub(crate) fn found(&self) -> &[&'static str] {
        &self.found
    }
}

/// Returns whether `needle` occurs in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Proves remapping on final bytes by substring search.
///
/// The bytes must contain none of `forbidden` (host roots of the build) nor
/// the container mount prefix `/boundary/`, and at least one fixed prefix
/// followed by `/`.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] for more than 64 forbidden prefixes, an
/// empty or longer than 4096-byte prefix, any forbidden prefix found, or no
/// fixed prefix found.
pub(crate) fn scan_remapped_bytes(
    bytes: &[u8],
    forbidden: &[&str],
) -> Result<RemapScan, Report<ReleaseError>> {
    if forbidden.len() > MAX_FORBIDDEN {
        return Err(recipe_error("too many forbidden prefixes for one scan"));
    }
    for prefix in forbidden.iter().copied().chain([EXECUTION_MOUNT_PREFIX]) {
        if prefix.is_empty() || prefix.len() > MAX_PREFIX_BYTES {
            return Err(recipe_error("forbidden prefix is empty or too long"));
        }
        if contains(bytes, prefix.as_bytes()) {
            return Err(recipe_error(format!(
                "final bytes still hold the unremapped path prefix {prefix:?}"
            )));
        }
    }
    let found: Vec<&'static str> = REMAPPED_PREFIXES
        .into_iter()
        .filter(|prefix| contains(bytes, format!("{prefix}/").as_bytes()))
        .collect();
    if found.is_empty() {
        return Err(recipe_error(
            "final bytes hold no remapped path prefix, so remapping is unproven",
        ));
    }
    Ok(RemapScan { found })
}
