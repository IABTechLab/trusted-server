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
//! C code those build scripts compile into a native artifact gets the same
//! maps through [`c_prefix_map_environment`].

use std::io;
use std::path::Path;

use error_stack::Report;

use super::apple::REMAPPED_DEVELOPER;
use super::fresh::disjoint;
use super::recipe_error;
use crate::error::ReleaseError;
use crate::policy::ReleaseTarget;

/// Tracked Cargo configuration Cargo reads from the checkout root.
pub(super) const CARGO_CONFIG: &str = ".cargo/config.toml";
/// Legacy extensionless configuration Cargo also reads; always refused.
pub(super) const LEGACY_CARGO_CONFIG: &str = ".cargo/config";
/// Fixed prefix of the source checkout.
pub(crate) const REMAPPED_CHECKOUT: &str = "/trusted-server";
/// Fixed prefix of the vendored dependency directory.
pub(super) const REMAPPED_VENDOR: &str = "/cargo-vendor";
/// Fixed prefix of the private Cargo home.
pub(super) const REMAPPED_CARGO_HOME: &str = "/cargo-home";
/// Fixed prefix of the Rust tool root, which is the compiler sysroot.
pub(super) const REMAPPED_TOOLS: &str = "/rust-tools";
/// Fixed prefix of the Cargo target directory, which holds every `OUT_DIR`.
pub(crate) const REMAPPED_TARGET: &str = "/cargo-target";
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
    remap_arguments_with(triple, sources, &[])
}

/// Returns the same `--config` pair with `extra` rustflags appended inside
/// the one target rustflags entry, such as the Darwin linker arguments.
///
/// # Errors
///
/// As [`remap_arguments`], or [`ReleaseError::Recipe`] when an extra flag
/// holds a quote, backslash or control character a TOML basic string would
/// need to escape.
pub(super) fn remap_arguments_with(
    triple: &str,
    sources: &RemapSources<'_>,
    extra: &[String],
) -> Result<Vec<String>, Report<ReleaseError>> {
    if extra
        .iter()
        .any(|flag| flag.contains(['"', '\\']) || flag.chars().any(char::is_control))
    {
        return Err(recipe_error(
            "an extra rustflag holds a quote, backslash or control character",
        ));
    }
    let flags: Vec<String> = remapped_pairs(sources)?
        .map(|(from, to)| format!("--remap-path-prefix={from}={to}"))
        .chain(extra.iter().cloned())
        .collect();
    rustflags_arguments(triple, &flags)
}

/// Encodes one target-scoped configuration pair without replacing source flags.
///
/// # Errors
///
/// Rejects unsafe target names or flags requiring unsupported TOML escaping.
pub(crate) fn rustflags_arguments(
    triple: &str,
    flags: &[String],
) -> Result<Vec<String>, Report<ReleaseError>> {
    if triple.is_empty()
        || !triple
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || flags
            .iter()
            .any(|flag| flag.contains(['"', '\\']) || flag.chars().any(char::is_control))
    {
        return Err(recipe_error("target rustflags cannot be safely encoded"));
    }
    let flags: Vec<_> = flags.iter().map(|flag| format!("\"{flag}\"")).collect();
    Ok(vec![
        "--config".to_owned(),
        format!("target.\"{triple}\".rustflags=[{}]", flags.join(",")),
    ])
}

/// Pairs every source root with its fixed prefix after requiring the roots
/// to be pairwise disjoint.
fn remapped_pairs<'a>(
    sources: &RemapSources<'a>,
) -> Result<impl Iterator<Item = (&'a str, &'static str)>, Report<ReleaseError>> {
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
    Ok(pairs
        .into_iter()
        .map(|(_, from)| from)
        .zip(REMAPPED_PREFIXES))
}

/// Returns the target-scoped `CFLAGS` variable that applies the same remap to
/// C code, or `None` for Wasm.
///
/// Native assets link C dependencies that `cc` compiles from build scripts
/// (bundled `SQLite`, `ring`, `aws-lc-sys`). With the profile's debug info,
/// Cargo tells those build scripts to emit DWARF, and `__FILE__` strings and
/// DWARF directories then hold execution paths rustc's remap never sees.
/// `-ffile-prefix-map` rewrites both in GCC 8+ and Clang 10+;
/// `-fdebug-prefix-map` is repeated because the GCC driver forwards only that
/// one to the assembler for `.S` sources. `cc` reads
/// `CFLAGS_<triple with underscores>` first, the plan sets it (the audited
/// environment rejects every inherited `CFLAGS` form), and it is bound by the
/// plan digest. `cc` splits the value on whitespace, so a root containing any
/// is refused instead of silently split.
///
/// # Errors
///
/// Returns [`ReleaseError::Recipe`] when two roots overlap or a root holds
/// whitespace.
pub(super) fn c_prefix_map_environment(
    target: ReleaseTarget,
    sources: &RemapSources<'_>,
) -> Result<Option<(String, String)>, Report<ReleaseError>> {
    c_prefix_map_environment_with(target, sources, None)
}

/// Returns the same variable, also mapping the Apple developer directory to
/// [`REMAPPED_DEVELOPER`] when `developer` is given, so SDK header paths in
/// C debug info and `__FILE__` strings do not name the host Xcode.
///
/// # Errors
///
/// As [`c_prefix_map_environment`], with the developer directory included in
/// the overlap and whitespace checks.
pub(super) fn c_prefix_map_environment_with(
    target: ReleaseTarget,
    sources: &RemapSources<'_>,
    developer: Option<&str>,
) -> Result<Option<(String, String)>, Report<ReleaseError>> {
    if target == ReleaseTarget::Wasm32Wasip1 {
        return Ok(None);
    }
    let mut pairs: Vec<(&str, &str)> = remapped_pairs(sources)?.collect();
    if let Some(developer) = developer {
        let mut labelled: Vec<(&str, &Path)> = pairs
            .iter()
            .map(|(from, _)| ("remapped root", Path::new(*from)))
            .collect();
        labelled.push(("remapped Apple developer directory", Path::new(developer)));
        disjoint(&labelled)?;
        pairs.push((developer, REMAPPED_DEVELOPER));
    }
    let mut flags = Vec::new();
    for (from, to) in pairs {
        if from.chars().any(char::is_whitespace) {
            return Err(recipe_error(
                "a remapped root holds whitespace, which cc would split",
            ));
        }
        flags.push(format!("-ffile-prefix-map={from}={to}"));
        flags.push(format!("-fdebug-prefix-map={from}={to}"));
    }
    Ok(Some((
        format!("CFLAGS_{}", target.triple().replace('-', "_")),
        flags.join(" "),
    )))
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
pub(crate) fn check_cargo_config(bytes: &[u8]) -> Result<(), Report<ReleaseError>> {
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
pub(crate) fn check_no_extensionless_config(
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
    reject_execution_prefixes(bytes, forbidden)?;
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

/// Rejects actual execution prefixes anywhere in final bytes.
///
/// # Errors
///
/// Rejects unsupported prefix bounds or any surviving raw/container prefix.
pub(crate) fn reject_execution_prefixes(
    bytes: &[u8],
    forbidden: &[&str],
) -> Result<(), Report<ReleaseError>> {
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
    Ok(())
}
