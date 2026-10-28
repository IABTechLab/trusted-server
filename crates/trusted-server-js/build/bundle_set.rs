//! Expected tsjs bundle set: discovery and validation.
//!
//! Shared by `build.rs` and the crate's unit tests through `#[path]`, so it
//! depends only on `std`.

use std::fs;
use std::path::{Path, PathBuf};

/// Module ID of the core bundle, which is always expected and ordered first.
pub(crate) const CORE_MODULE_ID: &str = "core";

/// Return the bundle file name for a module ID, for example `tsjs-core.js`.
pub(crate) fn bundle_file_name(id: &str) -> String {
    format!("tsjs-{id}.js")
}

/// Return the module IDs `build-all.mjs` builds.
///
/// Finds module folders by the rule of `moduleSourceDirs` in
/// `build-module-options.mjs`, `integrations/<name>` under `lib_src` and
/// `<type>/<crate>/js/src/<name>` under `crates_dir`, and keeps those with an
/// `index.ts` file, as `build-all.mjs` does. `core` comes first, the rest
/// follow in alphabetical order.
///
/// # Errors
///
/// Returns a message when a directory cannot be read, when two folders supply
/// a module of the same name, or when a folder is named `core`, which is the
/// shared core's bundle.
pub(crate) fn expected_module_ids(
    lib_src: &Path,
    crates_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut roots = vec![lib_src.join("integrations")];
    for type_dir in folders_in(crates_dir)? {
        for crate_dir in folders_in(&type_dir)? {
            roots.push(crate_dir.join("js").join("src"));
        }
    }

    let mut folders = Vec::new();
    for root in roots {
        for dir in folders_in(&root)? {
            let has_entry = dir.join("index.ts").is_file();
            folders.push((dir, has_entry));
        }
    }
    module_ids(&folders)
}

/// Return the module IDs for module folders in the order they were found,
/// each with whether it holds an `index.ts` file.
///
/// Kept apart from the folder walk so its rules are tested without a file
/// system, which the WebAssembly test run does not have.
///
/// # Errors
///
/// Returns a message when two folders supply a module of the same name, or
/// when a folder is named `core`, which is the shared core's bundle.
pub(crate) fn module_ids(folders: &[(PathBuf, bool)]) -> Result<Vec<String>, String> {
    let mut seen: Vec<(String, &Path)> = Vec::new();
    for (dir, _) in folders {
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == CORE_MODULE_ID {
            return Err(format!(
                "module name {CORE_MODULE_ID}, used by {}, is the shared core's bundle",
                dir.display()
            ));
        }
        if let Some((_, earlier)) = seen.iter().find(|(known, _)| *known == name) {
            return Err(format!(
                "module {name} is in both {} and {}",
                earlier.display(),
                dir.display()
            ));
        }
        seen.push((name, dir.as_path()));
    }

    let mut integrations: Vec<String> = seen
        .into_iter()
        .zip(folders)
        .filter(|(_, (_, has_entry))| *has_entry)
        .map(|((name, _), _)| name)
        .collect();
    integrations.sort();

    let mut ids = Vec::with_capacity(integrations.len() + 1);
    ids.push(CORE_MODULE_ID.to_owned());
    ids.extend(integrations);
    Ok(ids)
}

/// The folders directly inside `dir`, or none when it is not a folder.
///
/// # Errors
///
/// Returns a message when `dir` is a folder but cannot be read.
pub(crate) fn folders_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(dir)
        .map_err(|err| format!("failed to read directory {}: {err}", dir.display()))?;
    let mut folders = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|err| format!("failed to read an entry in {}: {err}", dir.display()))?;
        if entry.path().is_dir() {
            folders.push(entry.path());
        }
    }
    folders.sort();
    Ok(folders)
}

/// List the `tsjs-<id>.js` bundles in `dir` as `(id, byte length)` pairs.
///
/// # Errors
///
/// Returns a message when the directory or a bundle's metadata cannot be read.
pub(crate) fn scan_bundle_dir(dir: &Path) -> Result<Vec<(String, u64)>, String> {
    let entries = fs::read_dir(dir)
        .map_err(|err| format!("failed to read bundle directory {}: {err}", dir.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|err| format!("failed to read an entry in {}: {err}", dir.display()))?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = file_name
            .strip_prefix("tsjs-")
            .and_then(|stem| stem.strip_suffix(".js"))
        else {
            continue;
        };
        let len = entry
            .metadata()
            .map_err(|err| format!("failed to read metadata for {file_name}: {err}"))?
            .len();
        found.push((id.to_owned(), len));
    }
    Ok(found)
}

/// Check that `found` holds exactly the `expected` modules, each non-empty.
///
/// # Errors
///
/// Returns a message listing every missing, empty and unexpected bundle.
pub(crate) fn check_bundle_set(expected: &[String], found: &[(String, u64)]) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut empty = Vec::new();
    for id in expected {
        match found.iter().find(|(found_id, _)| found_id == id) {
            None => missing.push(id.as_str()),
            Some((_, 0)) => empty.push(id.as_str()),
            Some(_) => {}
        }
    }

    let mut unexpected = found
        .iter()
        .map(|(id, _)| id.as_str())
        .filter(|id| !expected.iter().any(|expected_id| expected_id == id))
        .collect::<Vec<_>>();
    unexpected.sort_unstable();

    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!("missing bundles: {missing:?}"));
    }
    if !empty.is_empty() {
        problems.push(format!("empty bundles: {empty:?}"));
    }
    if !unexpected.is_empty() {
        problems.push(format!("unexpected bundles: {unexpected:?}"));
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} (expected {} modules: {expected:?})",
            problems.join("; "),
            expected.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected() -> Vec<String> {
        ["core", "gpt", "prebid"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn found(entries: &[(&str, u64)]) -> Vec<(String, u64)> {
        entries
            .iter()
            .map(|(id, len)| ((*id).to_owned(), *len))
            .collect()
    }

    #[test]
    fn accepts_complete_non_empty_set() {
        let found = found(&[("prebid", 30), ("core", 10), ("gpt", 20)]);

        let result = check_bundle_set(&expected(), &found);

        assert!(result.is_ok(), "should accept a complete set: {result:?}");
    }

    #[test]
    fn rejects_set_missing_one_bundle() {
        let found = found(&[("core", 10), ("prebid", 30)]);

        let err = check_bundle_set(&expected(), &found).expect_err("should reject missing bundle");

        assert!(
            err.contains(r#"missing bundles: ["gpt"]"#),
            "should name the missing bundle: {err}"
        );
    }

    #[test]
    fn rejects_set_missing_core() {
        let found = found(&[("gpt", 20), ("prebid", 30)]);

        let err = check_bundle_set(&expected(), &found).expect_err("should reject missing core");

        assert!(
            err.contains(r#"missing bundles: ["core"]"#),
            "should name core as missing: {err}"
        );
    }

    #[test]
    fn rejects_set_with_one_empty_bundle() {
        let found = found(&[("core", 10), ("gpt", 0), ("prebid", 30)]);

        let err = check_bundle_set(&expected(), &found).expect_err("should reject empty bundle");

        assert!(
            err.contains(r#"empty bundles: ["gpt"]"#),
            "should name the empty bundle: {err}"
        );
    }

    #[test]
    fn rejects_set_with_unexpected_bundle() {
        let found = found(&[("core", 10), ("gpt", 20), ("prebid", 30), ("removed", 5)]);

        let err =
            check_bundle_set(&expected(), &found).expect_err("should reject unexpected bundle");

        assert!(
            err.contains(r#"unexpected bundles: ["removed"]"#),
            "should name the unexpected bundle: {err}"
        );
    }

    #[test]
    fn reports_every_problem_at_once() {
        let found = found(&[("core", 0), ("extra", 1)]);

        let err = check_bundle_set(&expected(), &found).expect_err("should reject bad set");

        assert!(
            err.contains(r#"missing bundles: ["gpt", "prebid"]"#)
                && err.contains(r#"empty bundles: ["core"]"#)
                && err.contains(r#"unexpected bundles: ["extra"]"#),
            "should list missing, empty and unexpected bundles together: {err}"
        );
    }

    /// Module folders as the folder walk finds them, each with whether it
    /// holds an `index.ts` file.
    fn folders(entries: &[(&str, bool)]) -> Vec<(PathBuf, bool)> {
        entries
            .iter()
            .map(|(dir, has_entry)| (PathBuf::from(dir), *has_entry))
            .collect()
    }

    #[test]
    fn lists_core_then_each_folder_with_an_entry_in_name_order() {
        let found = folders(&[
            ("lib/src/integrations/zeta", true),
            ("crates/kind/vendor/js/src/alpha", true),
            ("crates/kind/vendor/js/src/helpers", false),
        ]);

        let ids = module_ids(&found).expect("should list the modules");

        assert_eq!(
            ids,
            ["core", "alpha", "zeta"],
            "should list core first, then every folder with an index.ts in name order"
        );
    }

    #[test]
    fn rejects_a_module_name_two_folders_supply() {
        let found = folders(&[
            ("lib/src/integrations/alpha", true),
            ("crates/kind/vendor/js/src/alpha", true),
        ]);

        let err = module_ids(&found).expect_err("should refuse a module name found twice");

        assert!(
            err.contains("module alpha is in both"),
            "should name the module and both folders: {err}"
        );
    }

    #[test]
    fn rejects_a_module_named_core() {
        let found = folders(&[("crates/kind/vendor/js/src/core", true)]);

        let err = module_ids(&found).expect_err("should refuse a module named core");

        assert!(
            err.contains("module name core"),
            "should name the reserved name: {err}"
        );
    }

    #[test]
    fn bundle_file_name_matches_build_all_output() {
        assert_eq!(
            bundle_file_name("gpt_diagnostics"),
            "tsjs-gpt_diagnostics.js",
            "should match the tsjs-<id>.js name build-all.mjs writes"
        );
    }
}
