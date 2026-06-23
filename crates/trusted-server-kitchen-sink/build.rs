#![allow(
    clippy::print_stdout,
    clippy::panic,
    reason = "build scripts communicate with Cargo via stdout and fail the build on generation errors"
)]

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

struct AssetInput {
    source_path: PathBuf,
    site_path: String,
    content_type: &'static str,
    etag: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let site_dir = manifest_dir.join("site");
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let generated_path = out_dir.join("kitchen_sink_assets.rs");

    println!("cargo:rerun-if-changed={}", site_dir.display());

    let mut assets = Vec::new();
    collect_assets(&site_dir, &site_dir, &mut assets)?;
    assets.sort_by(|left, right| left.site_path.cmp(&right.site_path));

    let mut generated = String::from("pub static ASSETS: &[KitchenSinkAsset] = &[\n");
    for asset in assets {
        generated.push_str("    KitchenSinkAsset {\n");
        generated.push_str(&format!("        path: {:?},\n", asset.site_path));
        generated.push_str(&format!(
            "        body: include_bytes!(r#\"{}\"#),\n",
            asset.source_path.display()
        ));
        generated.push_str(&format!(
            "        content_type: {:?},\n",
            asset.content_type
        ));
        generated.push_str(&format!("        etag: {:?},\n", asset.etag));
        generated.push_str("    },\n");
    }
    generated.push_str("];\n");

    fs::write(generated_path, generated)?;
    Ok(())
}

fn collect_assets(
    site_dir: &Path,
    current_dir: &Path,
    assets: &mut Vec<AssetInput>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(current_dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        if is_dot_name(&file_name) {
            continue;
        }

        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            collect_assets(site_dir, &path, assets)?;
            continue;
        }

        if !path.is_file() {
            continue;
        }

        println!("cargo:rerun-if-changed={}", path.display());
        let relative_path = path
            .strip_prefix(site_dir)
            .expect("should collect only files under site directory");
        let site_path = relative_path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let content_type = content_type_for_path(relative_path);
        let body = fs::read(&path)?;
        let digest = Sha256::digest(&body);
        let etag = format!("\"sha256-{}\"", hex::encode(digest));
        assets.push(AssetInput {
            source_path: path,
            site_path,
            content_type,
            etag,
        });
    }

    Ok(())
}

fn is_dot_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}
