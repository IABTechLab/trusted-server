#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "build script failures should stop Cargo with a clear diagnostic"
)]

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use build_print::{info, warn};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const RELEASE_SENTINEL: &str = "__TSJS_RELEASE_ID_SENTINEL_V1__";
const RELEASE_PREFIX: &[u8] = b"tsjs-release-v1\0";

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReleaseManifest {
    version: u8,
    release_id: String,
    artifacts: Vec<ReleaseArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseArtifact {
    id: String,
    role: String,
    phase: Option<String>,
    trigger: Option<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    file: String,
    bytes: usize,
    hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogManifest {
    version: u8,
    modules: Vec<CatalogModule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogModule {
    id: String,
    phase: String,
    trigger: Option<String>,
    include: String,
}

fn main() {
    println!("cargo:rerun-if-changed=lib");
    watch_dir_recursively(Path::new("lib"));

    let skip = env::var("TSJS_SKIP_BUILD").is_ok_and(|value| value == "1");
    let crate_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("should set CARGO_MANIFEST_DIR for build script"),
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("should set OUT_DIR for build script"));
    let ts_dir = crate_dir.join("lib");
    let dist_dir = crate_dir.join("dist");
    fs::create_dir_all(&dist_dir).expect("should create dist directory");

    if !ts_dir.join("package.json").exists() {
        return;
    }
    let npm = which::which("npm").ok();
    if npm.is_none() {
        warn!("tsjs: npm not found; will use existing dist if available");
    }
    if !skip
        && let Some(npm_path) = npm.as_deref()
        && !ts_dir.join("node_modules").exists()
    {
        let status = Command::new(npm_path)
            .arg("ci")
            .current_dir(&ts_dir)
            .status();
        if !status.as_ref().is_ok_and(ExitStatus::success) {
            warn!("tsjs: npm ci failed; using existing dist if available");
        }
    }
    if !skip
        && env::var("TSJS_TEST").is_ok_and(|value| value == "1")
        && let Some(npm_path) = npm.as_deref()
    {
        Command::new(npm_path)
            .args(["run", "test", "--", "--run"])
            .current_dir(&ts_dir)
            .status()
            .expect("should run requested TSJS tests");
    }
    if !skip && let Some(npm_path) = npm.as_deref() {
        info!("tsjs: Building phase-aware release artifacts");
        let status = Command::new(npm_path)
            .args(["run", "build"])
            .current_dir(&ts_dir)
            .status();
        assert!(
            status.as_ref().is_ok_and(ExitStatus::success),
            "tsjs: npm run build failed - refusing to use stale bundles"
        );
    }

    let manifest = read_and_validate_release(&dist_dir);
    let catalog = read_and_validate_catalog(&dist_dir, &manifest);
    for artifact in &manifest.artifacts {
        copy_bundle(&artifact.file, &crate_dir, &dist_dir, &out_dir);
    }
    generate_metadata(&manifest, &catalog, &out_dir);
    info!(
        "tsjs: Embedded {} canonical release artifacts",
        manifest.artifacts.len()
    );
}

fn read_and_validate_catalog(dist_dir: &Path, release: &ReleaseManifest) -> CatalogManifest {
    let catalog_text = fs::read_to_string(dist_dir.join("tsjs-catalog-v1.json"))
        .expect("should read generated catalog manifest");
    let catalog: CatalogManifest =
        serde_json::from_str(&catalog_text).expect("should parse exact catalog manifest");
    assert_eq!(
        catalog.version, 1,
        "tsjs: catalog manifest version must be one"
    );
    assert_eq!(
        catalog.modules.len(),
        20,
        "tsjs: catalog must contain twenty modules"
    );
    for (module, artifact) in catalog.modules.iter().zip(release.artifacts.iter().skip(2)) {
        assert_eq!(module.id, artifact.id, "tsjs: catalog/release id mismatch");
        assert_eq!(
            Some(module.phase.as_str()),
            artifact.phase.as_deref(),
            "tsjs: catalog/release phase mismatch"
        );
        assert_eq!(
            module.trigger.as_deref(),
            artifact.trigger.as_deref(),
            "tsjs: catalog/release trigger mismatch"
        );
        assert!(
            module.include == "always"
                || module.include == "creative_guard"
                || module.include == "gpt_diagnostics_active"
                || module.include == "diagnostics_presentation"
                || module.include == "prebid_and_gpt"
                || module.include.starts_with("integration:"),
            "tsjs: unknown catalog inclusion predicate"
        );
    }
    catalog
}

fn read_and_validate_release(dist_dir: &Path) -> ReleaseManifest {
    let manifest_text = fs::read_to_string(dist_dir.join("tsjs-release-v1.json"))
        .expect("should read generated release manifest");
    let manifest: ReleaseManifest =
        serde_json::from_str(&manifest_text).expect("should parse exact release manifest");
    assert_eq!(
        manifest.version, 1,
        "tsjs: release manifest version must be one"
    );
    assert!(
        valid_hash(&manifest.release_id),
        "tsjs: generated manifest has invalid release id"
    );
    assert_eq!(
        manifest.artifacts.len(),
        22,
        "tsjs: release must contain bootstrap, core, and twenty integrations"
    );
    assert_eq!(manifest.artifacts[0].id, "bootstrap");
    assert_eq!(manifest.artifacts[0].role, "bootstrap");
    assert_eq!(manifest.artifacts[1].id, "core");
    assert_eq!(manifest.artifacts[1].role, "core");

    let mut canonical = Vec::new();
    canonical.extend_from_slice(RELEASE_PREFIX);
    push_u64(&mut canonical, manifest.artifacts.len());
    let mut ids = std::collections::HashSet::new();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        assert!(ids.insert(&artifact.id), "tsjs: duplicate artifact id");
        if index >= 2 {
            assert_eq!(artifact.role, "integration");
            if index < 16 {
                assert_eq!(artifact.phase.as_deref(), Some("critical"));
                assert!(artifact.trigger.is_none());
            } else {
                assert_eq!(artifact.phase.as_deref(), Some("deferred"));
                assert_eq!(artifact.trigger.as_deref(), Some("first_display_or_idle"));
                assert!(
                    artifact.outputs.is_empty(),
                    "tsjs: deferred provider is forbidden"
                );
            }
        } else {
            assert!(artifact.phase.is_none() && artifact.trigger.is_none());
        }

        let source = fs::read_to_string(dist_dir.join(&artifact.file))
            .unwrap_or_else(|error| panic!("tsjs: failed to read {}: {error}", artifact.file));
        assert_eq!(
            source.len(),
            artifact.bytes,
            "tsjs: artifact byte length mismatch"
        );
        assert_eq!(
            hex_digest(source.as_bytes()),
            artifact.hash,
            "tsjs: artifact content hash mismatch"
        );
        assert_eq!(
            source.matches(&manifest.release_id).count(),
            1,
            "tsjs: artifact must carry the release id exactly once"
        );
        assert!(
            !source.contains(RELEASE_SENTINEL),
            "tsjs: release sentinel remains"
        );
        let normalized = source.replacen(&manifest.release_id, RELEASE_SENTINEL, 1);
        push_frame(&mut canonical, artifact.id.as_bytes());
        push_frame(&mut canonical, artifact.role.as_bytes());
        push_frame(
            &mut canonical,
            artifact.phase.as_deref().unwrap_or_default().as_bytes(),
        );
        push_frame(
            &mut canonical,
            artifact.trigger.as_deref().unwrap_or_default().as_bytes(),
        );
        push_frame(&mut canonical, normalized.as_bytes());
    }
    assert_eq!(
        hex_digest(&canonical),
        manifest.release_id,
        "tsjs: sentinel-normalized release hash mismatch"
    );
    manifest
}

fn generate_metadata(manifest: &ReleaseManifest, catalog: &CatalogManifest, out_dir: &Path) {
    let mut code = String::from("// Auto-generated by build.rs - DO NOT EDIT\n\n");
    let integrations = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.role == "integration")
        .count();
    let critical = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.phase.as_deref() == Some("critical"))
        .count();
    writeln!(
        code,
        "pub(crate) const TSJS_RELEASE_ID: &str = {:?};",
        manifest.release_id
    )
    .expect("should write release id");
    writeln!(
        code,
        "pub(crate) const GENERATED_MAX_CRITICAL_MODULES: usize = {critical};\npub(crate) const GENERATED_MAX_MANIFEST_MODULES: usize = {integrations};"
    )
    .expect("should write generated capacities");
    code.push_str(
        "pub(crate) const GPT_BOOTSTRAP_FALLBACK: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/gpt-bootstrap-fallback.js\"));\n\n",
    );
    writeln!(
        code,
        "pub(crate) const TSJS_ARTIFACTS: [TsjsGeneratedArtifactMeta; {}] = [",
        manifest.artifacts.len()
    )
    .expect("should write generated artifact header");
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let inputs = rust_string_slice(&artifact.inputs);
        let outputs = rust_string_slice(&artifact.outputs);
        let include = index
            .checked_sub(2)
            .and_then(|catalog_index| catalog.modules.get(catalog_index))
            .map(|module| module.include.as_str());
        writeln!(
            code,
            "    TsjsGeneratedArtifactMeta {{ id: {:?}, role: {:?}, phase: {}, trigger: {}, include: {}, inputs: {inputs}, outputs: {outputs}, file: {:?}, hash: {:?}, bundle: include_str!(concat!(env!(\"OUT_DIR\"), {:?})) }},",
            artifact.id,
            artifact.role,
            rust_option(artifact.phase.as_deref()),
            rust_option(artifact.trigger.as_deref()),
            rust_option(include),
            artifact.file,
            artifact.hash,
            format!("/{}", artifact.file),
        )
        .expect("should write generated artifact");
    }
    code.push_str(
        "];\n\npub(crate) struct TsjsGeneratedArtifactMeta {\n    pub bundle: &'static str,\n    pub file: &'static str,\n    pub hash: &'static str,\n    pub id: &'static str,\n    pub include: Option<&'static str>,\n    pub inputs: &'static [&'static str],\n    pub outputs: &'static [&'static str],\n    pub phase: Option<&'static str>,\n    pub role: &'static str,\n    pub trigger: Option<&'static str>,\n}\n",
    );
    fs::write(out_dir.join("tsjs_modules.rs"), code).expect("should write generated TSJS metadata");
}

fn rust_string_slice(values: &[String]) -> String {
    format!(
        "&[{}]",
        values
            .iter()
            .map(|value| format!("{value:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn rust_option(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| format!("Some({value:?})"))
}

fn push_u64(target: &mut Vec<u8>, value: usize) {
    let value = u64::try_from(value).expect("should fit release frame length in u64");
    target.extend_from_slice(&value.to_be_bytes());
}

fn push_frame(target: &mut Vec<u8>, bytes: &[u8]) {
    push_u64(target, bytes.len());
    target.extend_from_slice(bytes);
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn copy_bundle(filename: &str, crate_dir: &Path, dist_dir: &Path, out_dir: &Path) {
    let primary = dist_dir.join(filename);
    let fallback = crate_dir.join("dist").join(filename);
    let target = out_dir.join(filename);
    for source in [&primary, &fallback] {
        if source.exists() {
            fs::copy(source, &target).unwrap_or_else(|error| {
                panic!(
                    "tsjs: failed to copy {} to {}: {error}",
                    source.display(),
                    target.display()
                )
            });
            return;
        }
        return;
    }
    panic!("tsjs: bundle {filename} was not generated");
}

fn watch_dir_recursively(root: &Path) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(path_string) = path.to_str() {
                println!("cargo:rerun-if-changed={path_string}");
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
}
