#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "Cargo directives require stdout and build script failures must stop the build"
)]

#[path = "build/bundle_set.rs"]
mod bundle_set;
#[allow(dead_code, reason = "only the shared release guard is needed here")]
#[path = "../trusted-server-core/build_support/git_version.rs"]
mod git_version;
#[path = "build/inventory.rs"]
mod inventory;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "build/prebuilt.rs"]
mod prebuilt;
#[path = "build/trace_sources.rs"]
pub(crate) mod trace_sources;

use std::env;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use build_print::info;
use sha2::{Digest as _, Sha256};

use self::bundle_set::{bundle_file_name, check_bundle_set, expected_module_ids, scan_bundle_dir};
use self::trace_sources::trace_source_digest;

const PREBUILT_DIR_VAR: &str = "TSJS_PREBUILT_DIR";
const INVENTORY_VAR: &str = "TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY";
const RELEASE_VAR: &str = "TRUSTED_SERVER__RELEASE_BUILD";
const SOURCE_VAR: &str = "TRUSTED_SERVER__GIT_VERSION";
const SKIP_BUILD_VAR: &str = "TSJS_SKIP_BUILD";
const TEST_VAR: &str = "TSJS_TEST";

fn main() {
    run_with_hook(|_, _| {});
}

/// Run the actual build script with a crate-private post-copy race fixture.
pub(crate) fn run_with_hook(hook: impl FnMut(&Path, &Path)) {
    // Cargo scans directories recursively, so these cover every TS source.
    // Every path must exist: a missing entry makes Cargo rerun this script on
    // every build, the opposite of the intent.
    for path in [
        "lib/src",
        "lib/build-all.mjs",
        "lib/build-module-options.mjs",
        "lib/trace-asset-sources.mjs",
        "lib/build-trace.mjs",
        "lib/trace-build-inputs.json",
        "../../.tool-versions",
        "lib/trace-assets-manifest.json",
        "lib/trace-assets",
        "lib/build-prebid-external.mjs",
        "lib/release-composition.mjs",
        "lib/vitest.config.ts",
        "lib/package.json",
        "lib/package-lock.json",
        "lib/tsconfig.json",
        "lib/vite.config.ts",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for var in [
        PREBUILT_DIR_VAR,
        INVENTORY_VAR,
        SKIP_BUILD_VAR,
        TEST_VAR,
        RELEASE_VAR,
        SOURCE_VAR,
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    assert!(
        env::var_os(SKIP_BUILD_VAR).is_none(),
        "tsjs: {SKIP_BUILD_VAR} is no longer supported whenever present; run `npm run build` \
         in crates/trusted-server-js/lib and set {PREBUILT_DIR_VAR} to embed prebuilt bundles \
         instead"
    );
    let source = env::var_os(SOURCE_VAR);
    let release =
        git_version::release_source(env::var_os(RELEASE_VAR).as_deref(), source.as_deref())
            .unwrap_or_else(|error| panic!("tsjs: {error}"));
    let prebuilt_dir = env::var_os(PREBUILT_DIR_VAR).map(PathBuf::from);
    let inventory_path = env::var_os(INVENTORY_VAR).map(PathBuf::from);
    // Ordinary releases build fresh private bundles. Selecting prebuilt release
    // inputs instead requires the complete checked pair, never a loose folder.
    if release.is_some() && (prebuilt_dir.is_some() || inventory_path.is_some()) {
        assert!(
            prebuilt_dir.is_some(),
            "tsjs: release builds require {PREBUILT_DIR_VAR}"
        );
        assert!(
            inventory_path.is_some(),
            "tsjs: release builds require {INVENTORY_VAR}"
        );
    }
    assert!(
        inventory_path.is_none() || prebuilt_dir.is_some(),
        "tsjs: {INVENTORY_VAR} requires {PREBUILT_DIR_VAR}"
    );
    if let Some(path) = &inventory_path {
        assert!(
            path.is_absolute(),
            "tsjs: {INVENTORY_VAR} must be an absolute path"
        );
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let crate_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("should set CARGO_MANIFEST_DIR for build script"),
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("should set OUT_DIR for build script"));
    let ts_dir = crate_dir.join("lib");
    // Private to this build script run: concurrent builds never share it.
    let bundle_dir = out_dir.join("tsjs-dist");
    let trace_dir = out_dir.join("tsjs-trace-dist");

    let expected = expected_module_ids(&ts_dir.join("src"))
        .unwrap_or_else(|err| panic!("tsjs: failed to discover modules: {err}"));

    if let Some(prebuilt_dir) = prebuilt_dir {
        assert!(
            prebuilt_dir.is_absolute(),
            "tsjs: {PREBUILT_DIR_VAR} must be an absolute path because Cargo runs build \
             scripts from {}; got {prebuilt_dir:?}",
            crate_dir.display()
        );
        println!("cargo:rerun-if-changed={}", prebuilt_dir.display());
        info!(
            "tsjs: Using prebuilt bundles from {}",
            prebuilt_dir.display()
        );
        if let Some(path) = inventory_path {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                prebuilt::inventory_outside(&prebuilt_dir, &path).unwrap_or_else(|error| {
                    panic!("tsjs: invalid inventory path {}: {error}", path.display())
                });
                let inventory = inventory::read_inventory(&path, &expected, release.as_deref())
                    .unwrap_or_else(|error| {
                        panic!("tsjs: invalid inventory {}: {error}", path.display())
                    });
                prebuilt::copy_checked(
                    &expected,
                    &prebuilt_dir,
                    &bundle_dir,
                    &inventory.inventory,
                    hook,
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "tsjs: invalid prebuilt inventory inputs in {}: {error}",
                        prebuilt_dir.display()
                    )
                });
                // Cargo JSON build-script messages expose the prepared input
                // identity to the later release evidence collector.
                println!(
                    "cargo:rustc-env=TRUSTED_SERVER__TSJS_INVENTORY_SHA256={}",
                    inventory.raw_sha256
                );
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            panic!("tsjs: checked prebuilt copying requires a supported Linux or macOS build host");
        } else {
            copy_prebuilt(&expected, &prebuilt_dir, &bundle_dir);
        }
    } else {
        build_bundles(&ts_dir, &bundle_dir);
        move_rebuilt_trace(&bundle_dir, &trace_dir);
    }

    validate_bundle_dir(&expected, &bundle_dir);
    info!(
        "tsjs: Embedding {} module files: {:?}",
        expected.len(),
        expected
    );

    write_module_table(&expected, &bundle_dir, &out_dir);
    embed_trace_assets(
        &ts_dir,
        &trace_dir,
        &out_dir,
        env::var_os(PREBUILT_DIR_VAR).is_none(),
    );
}

fn copy_prebuilt(expected: &[String], source: &Path, destination: &Path) {
    let found =
        bundle_set::scan_legacy_bundle_dir(source).unwrap_or_else(|error| panic!("tsjs: {error}"));
    check_bundle_set(expected, &found)
        .unwrap_or_else(|error| panic!("tsjs: invalid prebuilt bundle set: {error}"));
    if destination.exists() {
        fs::remove_dir_all(destination).expect("should clear private prebuilt destination");
    }
    fs::create_dir_all(destination).expect("should create private prebuilt destination");
    for id in expected {
        let filename = bundle_file_name(id);
        fs::copy(source.join(&filename), destination.join(&filename))
            .unwrap_or_else(|error| panic!("tsjs: failed to copy {filename}: {error}"));
    }
}

fn build_bundles(ts_dir: &Path, bundle_dir: &Path) {
    let how_to_prebuild = format!(
        "To embed prebuilt bundles instead, run `npm run build` in {} and set \
         {PREBUILT_DIR_VAR} to the directory holding the tsjs-*.js files (by default {}).",
        ts_dir.display(),
        ts_dir.with_file_name("dist").display()
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
    // Watched only once it exists: Cargo reruns a build script on every
    // invocation while a watched path is missing.
    println!(
        "cargo:rerun-if-changed={}",
        ts_dir.join("node_modules/.package-lock.json").display()
    );

    if env::var(TEST_VAR).is_ok_and(|value| value == "1") {
        for path in ["test", "vitest.config.ts", "build-prebid-external.mjs"] {
            println!("cargo:rerun-if-changed={}", ts_dir.join(path).display());
        }
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
///
/// Removes a partial `node_modules` left by a failed install so the next build
/// retries it.
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
    if status.as_ref().is_ok_and(ExitStatus::success) {
        return;
    }

    // A failed `npm ci` can leave a partial node_modules behind. Remove it
    // while still holding the lock so the next build retries the install
    // instead of skipping it and failing the freshness check.
    if node_modules.exists()
        && let Err(err) = fs::remove_dir_all(&node_modules)
    {
        panic!(
            "tsjs: npm ci failed in {} and removing the partial {} also failed: {err}",
            ts_dir.display(),
            node_modules.display()
        );
    }
    panic!("tsjs: npm ci failed in {}", ts_dir.display());
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

/// Move the trace viewer `npm run build` writes beside the bundles.
///
/// The checked bundle directory must hold only the bundles it embeds.
fn move_rebuilt_trace(bundle_dir: &Path, destination: &Path) {
    if destination.exists() {
        fs::remove_dir_all(destination).expect("should clear private rebuilt trace destination");
    }
    fs::create_dir_all(destination).expect("should create private rebuilt trace destination");
    fs::rename(bundle_dir.join("trace"), destination.join("trace"))
        .unwrap_or_else(|error| panic!("tsjs: failed to move the rebuilt trace viewer: {error}"));
}

fn validate_bundle_dir(expected: &[String], dir: &Path) {
    let found = scan_bundle_dir(dir).unwrap_or_else(|err| panic!("tsjs: {err}"));
    if let Err(err) = check_bundle_set(expected, &found) {
        panic!("tsjs: invalid bundle set in {}: {err}", dir.display());
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
    writeln!(
        codegen,
        "pub(crate) const ALL_MODULE_IDS: [&str; {0}] = {{\n    let mut ids = [\"\"; {0}];\n    let mut index = 0;\n    while index < {0} {{\n        ids[index] = TSJS_MODULES[index].id;\n        index += 1;\n    }}\n    ids\n}};",
        expected.len()
    )
    .expect("should write generated module IDs");
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

fn embed_trace_assets(ts_dir: &Path, dist_dir: &Path, out_dir: &Path, rebuilt: bool) {
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(ts_dir.join("trace-assets-manifest.json"))
            .expect("should read the committed trace asset manifest"),
    )
    .expect("should parse the trace asset manifest");
    let object = manifest
        .as_object()
        .expect("should use a trace manifest object");
    assert!(
        object.len() == 3
            && object.contains_key("schema_version")
            && object.contains_key("assets")
            && object.contains_key("source_sha256"),
        "tsjs: trace manifest must contain exactly the documented fields"
    );
    assert_eq!(
        manifest["schema_version"].as_u64(),
        Some(1),
        "tsjs: trace manifest schema version must be one"
    );
    if !rebuilt {
        assert_eq!(
            manifest["source_sha256"].as_str(),
            Some(trace_source_digest(ts_dir).as_str()),
            "tsjs: trace build inputs changed; run `npm run build:trace-assets` in crates/trusted-server-js/lib and review unpublished assets, or add a version after publication"
        );
    }
    let assets = manifest["assets"]
        .as_array()
        .expect("should list the versioned trace assets");
    assert_eq!(
        assets.len(),
        2,
        "tsjs: version one requires its JS and CSS assets"
    );
    let mut generated = String::from("const TRACE_ASSETS: [TraceAsset; 2] = [\n");
    for (asset, file) in assets.iter().zip(["v1.js", "v1.css"]) {
        let fields = asset
            .as_object()
            .expect("should represent trace asset metadata as an object");
        assert!(
            fields.len() == 3
                && fields.contains_key("path")
                && fields.contains_key("file")
                && fields.contains_key("sha256"),
            "tsjs: trace asset metadata must contain exactly path, file and sha256"
        );
        let path = format!("/_ts/trace/assets/{file}");
        assert_eq!(
            asset["path"].as_str(),
            Some(path.as_str()),
            "tsjs: trace asset path must be exact"
        );
        assert_eq!(
            asset["file"].as_str(),
            Some(file),
            "tsjs: trace asset filename must be exact"
        );
        let content = fs::read(ts_dir.join("trace-assets").join(file))
            .expect("should read the committed versioned trace asset bytes");
        let digest = hex::encode(Sha256::digest(&content));
        assert_eq!(
            asset["sha256"].as_str(),
            Some(digest.as_str()),
            "tsjs: committed trace asset bytes do not match their manifest digest"
        );
        if rebuilt {
            assert_eq!(
                fs::read(dist_dir.join("trace").join(file))
                    .expect("should rebuild both versioned trace assets"),
                content,
                "tsjs: rebuilt trace bytes differ from committed assets; run `npm run build:trace-assets` in crates/trusted-server-js/lib and review the unpublished draft or add a version"
            );
        }
        fs::write(out_dir.join(format!("trace-{file}")), &content)
            .expect("should copy verified trace bytes into the build output");
        writeln!(generated,
            "TraceAsset {{ path: \"{path}\", bytes: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/trace-{file}\")), sha256: \"{digest}\" }},")
            .expect("should generate the verified trace asset entry");
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("trace_assets.rs"), generated)
        .expect("should generate the trace asset lookup table");
}
