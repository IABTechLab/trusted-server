#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "build script failures should stop Cargo with a clear diagnostic"
)]

#[path = "build/bundle_set.rs"]
mod bundle_set;

use std::env;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use build_print::info;
use sha2::{Digest as _, Sha256};

use crate::bundle_set::{bundle_file_name, check_bundle_set, expected_module_ids, scan_bundle_dir};

const PREBUILT_DIR_VAR: &str = "TSJS_PREBUILT_DIR";
const SKIP_BUILD_VAR: &str = "TSJS_SKIP_BUILD";
const TEST_VAR: &str = "TSJS_TEST";

fn main() {
    // Cargo scans directories recursively, so these cover every TS source.
    for path in [
        "lib/src",
        "lib/build-all.mjs",
        "lib/package.json",
        "lib/package-lock.json",
        "lib/tsconfig.json",
        "lib/node_modules/.package-lock.json",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for var in [PREBUILT_DIR_VAR, SKIP_BUILD_VAR, TEST_VAR] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let crate_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("should set CARGO_MANIFEST_DIR for build script"),
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("should set OUT_DIR for build script"));
    let ts_dir = crate_dir.join("lib");
    // Private to this build script run: concurrent builds never share it.
    let bundle_dir = out_dir.join("tsjs-dist");

    let expected = expected_module_ids(&ts_dir.join("src"))
        .unwrap_or_else(|err| panic!("tsjs: failed to discover modules: {err}"));

    if let Some(prebuilt_dir) = env::var_os(PREBUILT_DIR_VAR).map(PathBuf::from) {
        println!("cargo:rerun-if-changed={}", prebuilt_dir.display());
        info!(
            "tsjs: Using prebuilt bundles from {}",
            prebuilt_dir.display()
        );
        validate_bundle_dir(&expected, &prebuilt_dir);
        copy_prebuilt_bundles(&expected, &prebuilt_dir, &bundle_dir);
    } else {
        build_bundles(&ts_dir, &bundle_dir);
    }

    validate_bundle_dir(&expected, &bundle_dir);
    info!(
        "tsjs: Embedding {} module files: {:?}",
        expected.len(),
        expected
    );

    write_module_table(&expected, &bundle_dir, &out_dir);
}

fn build_bundles(ts_dir: &Path, bundle_dir: &Path) {
    let how_to_prebuild = format!(
        "To embed prebuilt bundles instead, run `npm run build` in {} and set \
         {PREBUILT_DIR_VAR} to the directory holding the tsjs-*.js files (by default {}).",
        ts_dir.display(),
        ts_dir.with_file_name("dist").display()
    );

    assert!(
        env::var_os(SKIP_BUILD_VAR).is_none(),
        "tsjs: {SKIP_BUILD_VAR} is no longer supported because it embedded whatever \
         dist/ held. {how_to_prebuild}"
    );
    assert!(
        ts_dir.join("package.json").is_file(),
        "tsjs: {} not found. {how_to_prebuild}",
        ts_dir.join("package.json").display()
    );
    let npm = which::which("npm").unwrap_or_else(|_| {
        panic!("tsjs: npm not found on PATH; Node.js is required to build the tsjs bundles. {how_to_prebuild}")
    });

    install_dependencies_if_missing(&npm, ts_dir);
    ensure_dependencies_fresh(ts_dir);

    if env::var(TEST_VAR).is_ok_and(|value| value == "1") {
        let status = Command::new(&npm)
            .args(["run", "test", "--", "--run"])
            .current_dir(ts_dir)
            .status();
        assert!(
            status.as_ref().is_ok_and(ExitStatus::success),
            "tsjs: {TEST_VAR}=1 requested the tsjs tests and they failed"
        );
    }

    info!(
        "tsjs: Building per-module bundles into {}",
        bundle_dir.display()
    );
    let status = Command::new(&npm)
        .args(["run", "build", "--", "--out-dir"])
        .arg(bundle_dir)
        .current_dir(ts_dir)
        .status();
    assert!(
        status.as_ref().is_ok_and(ExitStatus::success),
        "tsjs: npm run build failed - refusing to embed incomplete bundles"
    );
}

/// Run `npm ci` when `node_modules` is absent, serialized across build scripts.
fn install_dependencies_if_missing(npm: &Path, ts_dir: &Path) {
    let node_modules = ts_dir.join("node_modules");

    // Two build scripts must not run `npm ci` in the same directory at once.
    // Check only while holding the lock: `npm ci` creates node_modules seconds
    // before it finishes, so an unlocked check can see a partial install. The
    // lock is released when `lock_file` drops.
    let lock_path = ts_dir.join(".tsjs-npm-ci.lock");
    let lock_file = File::create(&lock_path)
        .unwrap_or_else(|err| panic!("tsjs: failed to create {}: {err}", lock_path.display()));
    lock_file
        .lock()
        .unwrap_or_else(|err| panic!("tsjs: failed to lock {}: {err}", lock_path.display()));

    if node_modules.exists() {
        return;
    }

    info!("tsjs: node_modules missing; running npm ci");
    let status = Command::new(npm).arg("ci").current_dir(ts_dir).status();
    assert!(
        status.as_ref().is_ok_and(ExitStatus::success),
        "tsjs: npm ci failed in {}",
        ts_dir.display()
    );
}

/// Fail when `node_modules` is older than `package-lock.json`.
///
/// Uses npm's own freshness signal, the hidden lockfile it writes on install.
/// Reinstalling automatically would delete `node_modules` under any other
/// build script that is running, so this only reports the problem.
fn ensure_dependencies_fresh(ts_dir: &Path) {
    let lockfile = ts_dir.join("package-lock.json");
    let hidden_lockfile = ts_dir.join("node_modules").join(".package-lock.json");
    let stale_message = format!(
        "tsjs: node_modules is out of date with package-lock.json; run `npm ci` in {}",
        ts_dir.display()
    );

    let Ok(lockfile_modified) = fs::metadata(&lockfile).and_then(|meta| meta.modified()) else {
        return;
    };
    let hidden_modified = fs::metadata(&hidden_lockfile)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|_| panic!("{stale_message} ({} missing)", hidden_lockfile.display()));
    assert!(hidden_modified >= lockfile_modified, "{stale_message}");
}

fn validate_bundle_dir(expected: &[String], dir: &Path) {
    let found = scan_bundle_dir(dir).unwrap_or_else(|err| panic!("tsjs: {err}"));
    if let Err(err) = check_bundle_set(expected, &found) {
        panic!("tsjs: invalid bundle set in {}: {err}", dir.display());
    }
}

fn copy_prebuilt_bundles(expected: &[String], prebuilt_dir: &Path, bundle_dir: &Path) {
    if bundle_dir.exists() {
        fs::remove_dir_all(bundle_dir)
            .unwrap_or_else(|err| panic!("tsjs: failed to clean {}: {err}", bundle_dir.display()));
    }
    fs::create_dir_all(bundle_dir)
        .unwrap_or_else(|err| panic!("tsjs: failed to create {}: {err}", bundle_dir.display()));
    for id in expected {
        let file_name = bundle_file_name(id);
        let source = prebuilt_dir.join(&file_name);
        let target = bundle_dir.join(&file_name);
        fs::copy(&source, &target).unwrap_or_else(|err| {
            panic!(
                "tsjs: failed to copy {} to {}: {err}",
                source.display(),
                target.display()
            )
        });
    }
}

fn write_module_table(expected: &[String], bundle_dir: &Path, out_dir: &Path) {
    let mut codegen = String::new();
    codegen.push_str("// Auto-generated by build.rs - DO NOT EDIT\n\n");

    writeln!(
        codegen,
        "pub(crate) const TSJS_MODULES: [TsjsModuleMeta; {}] = [",
        expected.len()
    )
    .expect("should write generated module header");
    for id in expected {
        let filename = bundle_file_name(id);
        let sha256 = bundle_sha256(&bundle_dir.join(&filename));
        writeln!(
            codegen,
            "    TsjsModuleMeta {{\n        bundle: include_str!(concat!(env!(\"OUT_DIR\"), \"/tsjs-dist/{filename}\")),\n        id: \"{id}\",\n        sha256: \"{sha256}\",\n    }},\n"
        )
        .expect("should write generated module entry");
    }
    codegen.push_str("];\n");
    codegen.push_str("\npub(crate) struct TsjsModuleMeta {\n");
    codegen.push_str("    pub bundle: &'static str,\n");
    codegen.push_str("    pub id: &'static str,\n");
    codegen.push_str("    pub sha256: &'static str,\n");
    codegen.push_str("}\n");

    let generated_path = out_dir.join("tsjs_modules.rs");
    fs::write(&generated_path, &codegen).unwrap_or_else(|err| {
        panic!(
            "tsjs: failed to write generated code to {}: {err}",
            generated_path.display()
        );
    });
}

fn bundle_sha256(path: &Path) -> String {
    let content = fs::read(path).unwrap_or_else(|err| {
        panic!(
            "tsjs: failed to read bundle {} for hashing: {err}",
            path.display()
        );
    });
    hex::encode(Sha256::digest(&content))
}
