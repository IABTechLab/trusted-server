use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Rebuild if TS sources change (belt-and-suspenders): enumerate every file under ts/
    println!("cargo:rerun-if-changed=ts");
    watch_dir_recursively(Path::new("ts"));

    // Allow opt-out or force via env
    let skip = env::var("TSJS_SKIP_BUILD")
        .map(|v| v == "1")
        .unwrap_or(false);

    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ts_dir = crate_dir.join("ts");
    let dist_dir = crate_dir.join("dist");
    let outfile = dist_dir.join("tsjs.js");

    // Ensure dist exists
    let _ = fs::create_dir_all(&dist_dir);

    // Only try to build if we have a ts project
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
                if status.as_ref().map(|s| s.success()).unwrap_or(false) == false {
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
            if status.as_ref().map(|s| s.success()).unwrap_or(false) == false {
                println!("cargo:warning=tsjs: npm run build failed; will try fallback if allowed");
            }
        }
    }

    // Copy the result into OUT_DIR for include_str!
    let target_js = out_dir.join("tsjs.js");
    if outfile.exists() {
        if let Err(e) = fs::copy(&outfile, &target_js) {
            panic!(
                "tsjs: failed to copy {:?} to {:?}: {}",
                outfile, target_js, e
            );
        }
        return;
    }

    // Fallback: use checked-in dist/tsjs.js if present (default behavior)
    let fallback = crate_dir.join("dist/tsjs.js");
    if fallback.exists() {
        if let Err(e) = fs::copy(&fallback, &target_js) {
            panic!(
                "tsjs: failed to copy fallback {:?} to {:?}: {}",
                fallback, target_js, e
            );
        }
        return;
    }

    // Final hard error to ensure tsjs.js exists somewhere
    panic!(
        "tsjs: build output not found: {:?} and fallback {:?} missing. Ensure Node is installed and `npm run build` succeeds, or commit dist/tsjs.js.",
        outfile, fallback
    );
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
