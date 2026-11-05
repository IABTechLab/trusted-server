//! Differential evidence: producer-normalized manifests against `cargo vendor`.
//!
//! The ordinary CI candidate fixture records the manifests `cargo vendor`
//! wrote for each Git package. They are a comparison fixture, never a vendor
//! input. This module reduces two manifests to the facts dependency
//! resolution reads (package identity, features, `links` and every
//! dependency's source, requirement, rename, optionality, default features
//! and feature set) and lists every fact on which they disagree. Equal
//! facts are evidence that the producer resolved inheritance as Cargo does;
//! they confer no authority.
//!
//! Two build-target facts are compared by their Cargo meaning, not their
//! spelling, because `cargo vendor` makes target auto-discovery explicit
//! while the producer leaves it to Cargo:
//!
//! - `package.build`: an absent key and `build = true` both mean "use
//!   `build.rs` if the package directory has one", so they compare as
//!   `"build.rs"` when that file exists and as `false` when it does not;
//!   `build = false` and an explicit path compare as written.
//! - `lib.proc-macro` (or `proc_macro`): absent means `false`.
//!
//! No other absent/explicit pair is treated as equal.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use toml::{Table, Value};

use super::parse_manifest;
use crate::error::ReleaseError;
use crate::materials::material_error;

/// `[package]` fields that bear on identity or resolution.
const PACKAGE_FACTS: [&str; 5] = ["edition", "links", "name", "rust-version", "version"];
/// Dependency keys compared as written; `features` and defaults are normalized.
const DEPENDENCY_FACTS: [&str; 10] = [
    "branch", "git", "optional", "package", "path", "public", "registry", "rev", "tag", "version",
];

/// Lists every resolution fact on which the two manifests disagree.
///
/// An empty list means they are equivalent for dependency resolution.
/// `build_rs_present` says whether the vendored package directory holds a
/// `build.rs`, which decides what an auto-discovered build script means.
///
/// # Errors
///
/// Returns [`ReleaseError::Material`] when either manifest does not parse
/// or holds a malformed `build` or `proc-macro` value.
pub(crate) fn resolution_differences(
    producer: &[u8],
    cargo_vendor: &[u8],
    build_rs_present: bool,
) -> Result<Vec<String>, Report<ReleaseError>> {
    let producer = resolution_facts(&parse_manifest(producer)?, build_rs_present)?;
    let cargo_vendor = resolution_facts(&parse_manifest(cargo_vendor)?, build_rs_present)?;
    let keys: BTreeSet<&String> = producer.keys().chain(cargo_vendor.keys()).collect();
    Ok(keys
        .into_iter()
        .filter(|key| producer.get(*key) != cargo_vendor.get(*key))
        .map(|key| {
            format!(
                "{key}: producer={} cargo-vendor={}",
                producer.get(key).map_or("<absent>", String::as_str),
                cargo_vendor.get(key).map_or("<absent>", String::as_str),
            )
        })
        .collect())
}

/// Flattens the resolution-relevant facts of one manifest.
fn resolution_facts(
    manifest: &Table,
    build_rs_present: bool,
) -> Result<BTreeMap<String, String>, Report<ReleaseError>> {
    let mut facts = BTreeMap::new();
    let package = manifest
        .get("package")
        .and_then(Value::as_table)
        .ok_or_else(|| material_error("compared manifest has no [package] table"))?;
    for key in PACKAGE_FACTS {
        if let Some(value) = package.get(key) {
            facts.insert(format!("package.{key}"), value.to_string());
        }
    }
    facts.insert(
        "package.build".to_owned(),
        build_fact(package.get("build"), build_rs_present)?,
    );
    let lib = manifest.get("lib").and_then(Value::as_table);
    let proc_macro =
        match lib.and_then(|lib| lib.get("proc-macro").or_else(|| lib.get("proc_macro"))) {
            None => false,
            Some(Value::Boolean(value)) => *value,
            Some(_) => return Err(material_error("compared lib.proc-macro is not a boolean")),
        };
    facts.insert("lib.proc-macro".to_owned(), proc_macro.to_string());
    if let Some(features) = manifest.get("features").and_then(Value::as_table) {
        for (name, members) in features {
            facts.insert(format!("features.{name}"), sorted_strings(members)?);
        }
    }
    dependency_facts(manifest, "", &mut facts)?;
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (cfg, target) in targets {
            if let Some(target) = target.as_table() {
                dependency_facts(target, &format!("target.{cfg}."), &mut facts)?;
            }
        }
    }
    Ok(facts)
}

/// The build script Cargo will run, by meaning rather than spelling.
fn build_fact(
    build: Option<&Value>,
    build_rs_present: bool,
) -> Result<String, Report<ReleaseError>> {
    match build {
        None | Some(Value::Boolean(true)) if build_rs_present => Ok("\"build.rs\"".to_owned()),
        None | Some(Value::Boolean(true)) | Some(Value::Boolean(false)) => Ok("false".to_owned()),
        Some(Value::String(path)) => Ok(format!("{path:?}")),
        Some(_) => Err(material_error("compared package.build is malformed")),
    }
}

/// Records the facts of every dependency table directly inside `scope`.
fn dependency_facts(
    scope: &Table,
    prefix: &str,
    facts: &mut BTreeMap<String, String>,
) -> Result<(), Report<ReleaseError>> {
    for (section, canonical) in [
        ("dependencies", "dependencies"),
        ("dev-dependencies", "dev-dependencies"),
        ("dev_dependencies", "dev-dependencies"),
        ("build-dependencies", "build-dependencies"),
        ("build_dependencies", "build-dependencies"),
    ] {
        let Some(dependencies) = scope.get(section).and_then(Value::as_table) else {
            continue;
        };
        for (name, dependency) in dependencies {
            let base = format!("{prefix}{canonical}.{name}");
            let detailed = match dependency {
                Value::String(version) => {
                    Table::from_iter([("version".to_owned(), Value::String(version.clone()))])
                }
                Value::Table(table) => table.clone(),
                _ => return Err(material_error(format!("{base} is malformed"))),
            };
            for key in DEPENDENCY_FACTS {
                if let Some(value) = detailed.get(key) {
                    facts.insert(format!("{base}.{key}"), value.to_string());
                }
            }
            let defaults = detailed
                .get("default-features")
                .or_else(|| detailed.get("default_features"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            facts.insert(format!("{base}.default-features"), defaults.to_string());
            if let Some(features) = detailed.get("features") {
                facts.insert(format!("{base}.features"), sorted_strings(features)?);
            }
            if detailed.contains_key("workspace") {
                facts.insert(format!("{base}.workspace"), "inherits".to_owned());
            }
        }
    }
    Ok(())
}

/// Renders an array of strings as a sorted, de-duplicated list.
fn sorted_strings(value: &Value) -> Result<String, Report<ReleaseError>> {
    let set: BTreeSet<&str> = value
        .as_array()
        .and_then(|values| values.iter().map(Value::as_str).collect::<Option<_>>())
        .ok_or_else(|| material_error("compared feature list is not an array of strings"))?;
    Ok(set.into_iter().collect::<Vec<_>>().join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRODUCER: &str = "[package]\nname = \"example-core\"\nversion = \"0.4.0\"\nedition = \"2021\"\n\n[dependencies.example-dep]\nversion = \"1\"\nfeatures = [\"alpha\", \"beta\"]\n\n[dependencies.example-sibling]\npath = \"../example-sibling\"\ndefault-features = false\n";

    #[test]
    fn equal_resolution_ignores_order_repeats_and_presentation() {
        let cargo_vendor = "[package]\nedition = \"2021\"\nname = \"example-core\"\nversion = \"0.4.0\"\nreadme = \"README.md\"\nautobins = false\n\n[dependencies]\nexample-dep = { version = \"1\", features = [\"beta\", \"alpha\", \"beta\"] }\nexample-sibling = { path = \"../example-sibling\", default_features = false }\n";
        let differences =
            resolution_differences(PRODUCER.as_bytes(), cargo_vendor.as_bytes(), false)
                .expect("should compare both manifests");
        assert!(
            differences.is_empty(),
            "should treat equal resolution facts as equivalent: {differences:?}"
        );
    }

    #[test]
    fn every_resolution_difference_is_reported() {
        let cargo_vendor = "[package]\nname = \"example-core\"\nversion = \"0.4.1\"\nedition = \"2021\"\n\n[dependencies.example-dep]\nversion = \"1\"\nfeatures = [\"alpha\"]\n\n[dependencies.example-sibling]\npath = \"../example-sibling\"\n\n[dev-dependencies]\nexample-extra = \"2\"\n";
        let differences =
            resolution_differences(PRODUCER.as_bytes(), cargo_vendor.as_bytes(), false)
                .expect("should compare both manifests");
        let keys: Vec<&str> = differences
            .iter()
            .map(|line| line.split(':').next().unwrap_or_default())
            .collect();
        assert_eq!(
            keys,
            [
                "dependencies.example-dep.features",
                "dependencies.example-sibling.default-features",
                "dev-dependencies.example-extra.default-features",
                "dev-dependencies.example-extra.version",
                "package.version",
            ],
            "should report the version, feature, default-features and missing dependency"
        );
    }

    #[test]
    fn build_targets_compare_by_cargo_meaning() {
        let package = "[package]\nname = \"example-core\"\nversion = \"0.4.0\"\n";
        let with = |extra: &str| format!("{package}{extra}");
        let differs = |producer: &str, cargo_vendor: &str, build_rs_present: bool| {
            !resolution_differences(
                producer.as_bytes(),
                cargo_vendor.as_bytes(),
                build_rs_present,
            )
            .expect("should compare both manifests")
            .is_empty()
        };
        let explicit_script = with("build = \"build.rs\"\n");
        let no_script = with("build = false\n");
        let proc_macro = with("\n[lib]\nproc-macro = true\n");
        assert!(
            !differs(package, &explicit_script, true),
            "should equate auto-discovery with the explicit build.rs that exists"
        );
        assert!(
            differs(package, &explicit_script, false),
            "should not equate auto-discovery with a build.rs that is absent"
        );
        assert!(
            !differs(package, &no_script, false),
            "should equate auto-discovery with build = false when no build.rs exists"
        );
        assert!(
            differs(package, &no_script, true),
            "should not equate auto-discovery with build = false when build.rs exists"
        );
        assert!(
            !differs(&with("build = true\n"), package, true),
            "should equate build = true with an absent key"
        );
        assert!(
            differs(
                &with("build = \"tools/build.rs\"\n"),
                &explicit_script,
                true
            ),
            "should compare explicit build paths as written"
        );
        assert!(
            !differs(package, &with("\n[lib]\nproc-macro = false\n"), false),
            "should equate an absent proc-macro with false"
        );
        assert!(
            differs(package, &proc_macro, false),
            "should report a proc-macro difference"
        );
        assert!(
            !differs(&with("\n[lib]\nproc_macro = true\n"), &proc_macro, false),
            "should read the underscore spelling too"
        );
    }

    #[test]
    fn unparsable_manifests_are_errors() {
        assert!(
            resolution_differences(b"[package", PRODUCER.as_bytes(), false).is_err(),
            "should refuse a manifest that does not parse"
        );
        assert!(
            resolution_differences(b"[lib]\n", PRODUCER.as_bytes(), false).is_err(),
            "should refuse a manifest without a package"
        );
    }
}
