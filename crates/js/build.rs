use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const REQUIRED_BUNDLES: &[&str] = &["tsjs-core.js", "tsjs-ext.js", "tsjs-creative.js"];

fn main() {
    // Rebuild if TS sources change (belt-and-suspenders): enumerate every file under ts/
    println!("cargo:rerun-if-changed=lib");
    watch_dir_recursively(Path::new("lib"));

    // Allow opt-out or force via env
    let skip = env::var("TSJS_SKIP_BUILD")
        .map(|v| v == "1")
        .unwrap_or(false);

    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ts_dir = crate_dir.join("lib");
    let dist_dir = crate_dir.join("dist");

    // Ensure dist exists
    let _ = fs::create_dir_all(&dist_dir);

    // Only try to build if we have a library project
    if !ts_dir.join("package.json").exists() {
        // No TS project; rely on prebuilt dist if present
        return;
    }

    // If Node/npm is absent, keep going if dist exists
    let npm = which::which("npm").ok();
    if npm.is_none() {
        println!("cargo:warning=tsjs: npm not found; will use existing dist if available");
    }

    // Install deps if node_modules missing
    if !skip {
        if let Some(npm_path) = npm.clone() {
            if !ts_dir.join("node_modules").exists() {
                let status = Command::new(npm_path.clone())
                    .arg("install")
                    .current_dir(&ts_dir)
                    .status();
                if !status.as_ref().map(|s| s.success()).unwrap_or(false) {
                    println!(
                        "cargo:warning=tsjs: npm install failed; using existing dist if available"
                    );
                }
            }
        }
    }

    // Run tests if requested
    if !skip && npm.is_some() && env::var("TSJS_TEST").map(|v| v == "1").unwrap_or(false) {
        let _ = Command::new(npm.clone().unwrap())
            .args(["run", "test", "--", "--run"]) // ensure non-watch
            .current_dir(&ts_dir)
            .status();
    }

    // Build bundle
    if !skip {
        if let Some(npm_path) = npm.clone() {
            let status = Command::new(npm_path)
                .args(["run", "build"])
                .current_dir(&ts_dir)
                .status();
            if !status.as_ref().map(|s| s.success()).unwrap_or(false) {
                println!("cargo:warning=tsjs: npm run build failed; will try fallback if allowed");
            }
        }
    }

    // Copy the result into OUT_DIR for include_str!
    let bundle_files = discover_bundles(&dist_dir);
    ensure_required_bundles(&bundle_files);
    copy_bundles(&bundle_files, &dist_dir, &out_dir);
    generate_manifest(&bundle_files, &out_dir);
}

fn discover_bundles(dist_dir: &Path) -> Vec<String> {
    let mut bundles = Vec::new();
    let entries = match fs::read_dir(dist_dir) {
        Ok(entries) => entries,
        Err(_) => return bundles,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("js") {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                bundles.push(name.to_string());
            }
        }
    }
    bundles.sort();
    bundles
}

fn watch_dir_recursively(root: &Path) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            // Always ask Cargo to rerun if this path changes
            if let Some(p) = path.to_str() {
                println!("cargo:rerun-if-changed={}", p);
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
}

fn ensure_required_bundles(bundles: &[String]) {
    for required in REQUIRED_BUNDLES {
        if !bundles.iter().any(|bundle| bundle == required) {
            panic!("tsjs: required bundle {} not found in dist/", required);
        }
    }
}

fn copy_bundles(bundles: &[String], dist_dir: &Path, out_dir: &Path) {
    for bundle in bundles {
        let source = dist_dir.join(bundle);
        let target = out_dir.join(bundle);
        if let Err(e) = fs::copy(&source, &target) {
            panic!(
                "tsjs: failed to copy bundle {:?} to {:?}: {}",
                source, target, e
            );
        }
    }
}

fn generate_manifest(bundles: &[String], out_dir: &Path) {
    let manifest_path = out_dir.join("bundle_manifest.rs");
    let mut file = File::create(&manifest_path)
        .unwrap_or_else(|e| panic!("tsjs: failed to create manifest: {}", e));
    writeln!(&mut file, "pub const BUNDLES: &[(&str, &str)] = &[").unwrap();
    for bundle in bundles {
        writeln!(
            &mut file,
            "    (\"{name}\", include_str!(concat!(env!(\"OUT_DIR\"), \"/{name}\"))),",
            name = bundle
        )
        .unwrap();
    }
    writeln!(&mut file, "];").unwrap();
}
