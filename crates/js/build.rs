#![allow(clippy::unwrap_used, clippy::panic)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use build_print::{error, info, warn};

const UNIFIED_BUNDLE: &str = "tsjs-unified.js";

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
        warn!("tsjs: npm not found; will use existing dist if available");
    }

    // Install deps if node_modules missing
    if !skip {
        if let Some(npm_path) = npm.clone() {
            if !ts_dir.join("node_modules").exists() {
                let status = Command::new(npm_path.clone())
                    .arg("install")
                    .current_dir(&ts_dir)
                    .status();
                if !status
                    .as_ref()
                    .map(std::process::ExitStatus::success)
                    .unwrap_or(false)
                {
                    error!("tsjs: npm install failed; using existing dist if available");
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

    // Build unified bundle
    if !skip {
        if let Some(npm_path) = npm.clone() {
            info!("tsjs: Building unified bundle");
            let js_modules = env::var("TSJS_MODULES").unwrap_or("".to_string());

            let status = Command::new(&npm_path)
                .env("TSJS_MODULES", js_modules)
                .args(["run", "build"])
                .current_dir(&ts_dir)
                .status();
            if !status
                .as_ref()
                .map(std::process::ExitStatus::success)
                .unwrap_or(false)
            {
                panic!("tsjs: npm run build failed - refusing to use stale bundle");
            }
        }
    }

    // Copy bundle into OUT_DIR for include_str!
    copy_bundle(UNIFIED_BUNDLE, true, &crate_dir, &dist_dir, &out_dir);
}

fn copy_bundle(filename: &str, required: bool, crate_dir: &Path, dist_dir: &Path, out_dir: &Path) {
    let primary = dist_dir.join(filename);
    let fallback = crate_dir.join("dist").join(filename);
    let target = out_dir.join(filename);

    for source in [&primary, &fallback] {
        if source.exists() {
            if let Err(e) = fs::copy(source, &target) {
                if required {
                    panic!("tsjs: failed to copy {:?} to {:?}: {}", source, target, e);
                }
            }
            return;
        }
    }

    if required {
        panic!(
            "tsjs: bundle {} not found: {:?} (and fallback {:?}). Ensure Node is installed and `npm run build` succeeds, or commit dist/{}.",
            filename, primary, fallback, filename
        );
    }

    let _ = fs::write(&target, "");
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
