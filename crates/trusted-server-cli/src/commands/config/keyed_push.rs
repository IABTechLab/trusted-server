//! Pushes an app-config blob to Fastly under a key other than the logical
//! store ID.
//!
//! A service that chooses its blob by host keeps a blob under each host, and
//! `EdgeZero` writes no Fastly key but the logical store ID. This writer
//! validates the blob as `ts config validate` does, wraps it in `EdgeZero`'s
//! envelope and splits a long one into `EdgeZero`'s chunk format, so the
//! runtime reads it as it reads any pushed blob. `EdgeZero` keeps that format
//! private to its crate, so the constants below copy it. The writer writes
//! only the local server's store, in the `fastly.toml` the manifest names.

use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use edgezero_cli::args::{ConfigPushArgs, ConfigValidateArgs};
use edgezero_core::app_config::{self, AppConfigLoadOptions};
use edgezero_core::blob_envelope::BlobEnvelope;
use edgezero_core::manifest::{Manifest, ManifestLoader};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use toml_edit::{DocumentMut, Item, Table};
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID;

use crate::app_config::resolve_app_config_path;

/// The adapter whose keys this writer writes.
const ADAPTER: &str = "fastly";
/// The longest value one config store entry holds, counted in bytes.
const ENTRY_LIMIT: usize = 8_000;
/// The most bytes of an envelope one chunk carries.
const CHUNK_LIMIT: usize = 7_000;
/// The longest key a config store entry takes, in characters.
const KEY_LIMIT: usize = 255;
/// Joins a blob's key to a chunk's content address and index.
const CHUNK_KEY_INFIX: &str = ".__edgezero_chunks.";
/// Marks an entry as a pointer to the chunks of one blob.
const POINTER_KIND: &str = "fastly_config_chunks";

/// The pointer to the chunks of one blob, with `EdgeZero`'s fields in
/// `EdgeZero`'s order.
#[derive(Serialize)]
struct ChunkPointer<'a> {
    chunks: Vec<ChunkRef>,
    data_sha256: &'a str,
    edgezero_kind: &'static str,
    envelope_len: usize,
    envelope_sha256: &'a str,
    version: u8,
}

/// One chunk a pointer names.
#[derive(Serialize)]
struct ChunkRef {
    key: String,
    len: usize,
    sha256: String,
}

/// The key this writer pushes for `args`, or `None` when the push is
/// `EdgeZero`'s.
///
/// The writer takes a Fastly push of a `--key` other than the logical store
/// ID, which `EdgeZero` refuses.
#[must_use]
pub(crate) fn own_key(args: &ConfigPushArgs) -> Option<&str> {
    let logical_store = args
        .store
        .as_deref()
        .is_none_or(|store| store == DEFAULT_CONFIG_STORE_ID);
    if !args.adapter.eq_ignore_ascii_case(ADAPTER) || !logical_store {
        return None;
    }
    args.key
        .as_deref()
        .filter(|key| *key != DEFAULT_CONFIG_STORE_ID)
}

/// Pushes the blob `args` names under `key`, into the local server's store.
///
/// Every key is checked before anything is written, and a long blob's
/// chunks are written before the pointer to them. `--dry-run` lists the keys
/// and writes nothing.
///
/// # Errors
///
/// Returns a message when the push is not `--local`, the blob fails
/// validation, a key is refused, or the local manifest cannot be read or
/// written.
pub(crate) fn run(args: &ConfigPushArgs, key: &str, out: &mut dyn Write) -> Result<(), String> {
    if !args.local {
        return Err(format!(
            "`--key {key}` can be pushed only with `--local` on Fastly. Write a live \
             store's entries with `fastly config-store-entry update`."
        ));
    }
    let loader = ManifestLoader::from_path(&args.manifest)
        .map_err(|error| format!("failed to load {}: {error}", args.manifest.display()))?;
    let manifest = loader.manifest();
    let envelope = envelope(args, manifest)?;
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|error| format!("failed to serialize the envelope: {error}"))?;
    let entries = entries(key, &envelope_json, &envelope.sha256)?;
    let fastly_toml = local_manifest(&args.manifest, manifest)?;

    let verb = if args.dry_run { "would write" } else { "wrote" };
    if !args.dry_run {
        write_local(&fastly_toml, &entries)?;
    }
    for (entry_key, value) in &entries {
        writeln!(
            out,
            "{verb} `{entry_key}` ({} bytes) to \
             [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}.contents] in {}",
            value.len(),
            fastly_toml.display()
        )
        .map_err(|error| format!("failed to write command output: {error}"))?;
    }
    Ok(())
}

/// The blob `args` names, validated as `ts config validate` validates it and
/// wrapped in `EdgeZero`'s envelope.
fn envelope(args: &ConfigPushArgs, manifest: &Manifest) -> Result<BlobEnvelope, String> {
    let mut validate = ConfigValidateArgs::default();
    validate.app_config.clone_from(&args.app_config);
    validate.manifest.clone_from(&args.manifest);
    validate.no_env = args.no_env;
    edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>(&validate)?;

    let app_name = manifest.app.name.as_deref().ok_or_else(|| {
        format!(
            "{} has no [app].name; cannot resolve the app config",
            args.manifest.display()
        )
    })?;
    let path = resolve_app_config_path(args.app_config.as_deref(), &args.manifest, app_name);
    let mut options = AppConfigLoadOptions::default();
    options.env_overlay = !args.no_env;
    let typed = app_config::deserialize_app_config_with_options::<TrustedServerAppConfig>(
        &path, app_name, &options,
    )
    .map_err(|error| format!("failed to load {}: {error}", path.display()))?;
    let data = serde_json::to_value(&typed)
        .map_err(|error| format!("failed to serialize the app config: {error}"))?;
    let generated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(BlobEnvelope::new(data, generated_at))
}

/// The entries that hold `envelope_json` under `key`, in the order they are
/// written.
///
/// An envelope that fits one entry is that entry. A longer one is split into
/// chunks at `EdgeZero`'s boundaries, followed by the pointer to them, so the
/// chunks are always in place before the pointer that names them.
///
/// # Errors
///
/// Returns a message, before anything is written, when `key` is empty or
/// holds the chunk infix, when it or a chunk key derived from it is longer
/// than [`KEY_LIMIT`] characters, or when the pointer is too long for an
/// entry.
fn entries(
    key: &str,
    envelope_json: &str,
    data_sha256: &str,
) -> Result<Vec<(String, String)>, String> {
    if key.is_empty() || key.contains(CHUNK_KEY_INFIX) {
        return Err(format!(
            "config key {key:?} is empty or holds `{CHUNK_KEY_INFIX}`, which only chunk keys \
             carry"
        ));
    }
    check_key_length(key)?;
    if envelope_json.len() <= ENTRY_LIMIT {
        return Ok(vec![(key.to_owned(), envelope_json.to_owned())]);
    }

    let envelope_sha256 = sha256_hex(envelope_json);
    let mut entries = Vec::new();
    let mut chunks = Vec::new();
    for (index, chunk) in chunks_of(envelope_json).enumerate() {
        let chunk_key = format!("{key}{CHUNK_KEY_INFIX}{envelope_sha256}.{index}");
        check_key_length(&chunk_key)?;
        chunks.push(ChunkRef {
            key: chunk_key.clone(),
            len: chunk.len(),
            sha256: sha256_hex(chunk),
        });
        entries.push((chunk_key, chunk.to_owned()));
    }
    let pointer = serde_json::to_string(&ChunkPointer {
        chunks,
        data_sha256,
        edgezero_kind: POINTER_KIND,
        envelope_len: envelope_json.len(),
        envelope_sha256: &envelope_sha256,
        version: 1,
    })
    .map_err(|error| format!("failed to serialize the chunk pointer for `{key}`: {error}"))?;
    if pointer.len() > ENTRY_LIMIT {
        return Err(format!(
            "the chunk pointer for `{key}` is {} bytes, over the {ENTRY_LIMIT}-byte entry limit",
            pointer.len()
        ));
    }
    entries.push((key.to_owned(), pointer));
    Ok(entries)
}

/// Refuses a key longer than a config store entry takes.
fn check_key_length(key: &str) -> Result<(), String> {
    let length = key.chars().count();
    if length > KEY_LIMIT {
        return Err(format!(
            "config key `{key}` is {length} characters, over the {KEY_LIMIT}-character limit. \
             A long blob's chunk keys add 85 or more characters to its key, so use a shorter \
             --key."
        ));
    }
    Ok(())
}

/// `envelope_json` in pieces of at most [`CHUNK_LIMIT`] bytes, each ending on
/// a character boundary, which are the boundaries `EdgeZero` splits at.
fn chunks_of(envelope_json: &str) -> impl Iterator<Item = &str> {
    let mut rest = envelope_json;
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = CHUNK_LIMIT.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, tail) = rest.split_at(end);
        rest = tail;
        Some(chunk)
    })
}

/// The lowercase hex SHA-256 of `text`.
fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("should write hex digit to string");
    }
    hex
}

/// The `fastly.toml` the manifest names for the Fastly adapter, beside the
/// manifest.
fn local_manifest(manifest_path: &Path, manifest: &Manifest) -> Result<PathBuf, String> {
    let named = manifest
        .adapter_entry(ADAPTER)
        .and_then(|(_, adapter)| adapter.adapter.manifest.as_deref())
        .ok_or_else(|| {
            format!(
                "{} names no [adapters.fastly.adapter].manifest for a local push",
                manifest_path.display()
            )
        })?;
    let root = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(root.join(named))
}

/// Writes `entries` into the local server's logical config store in
/// `fastly_toml`, keeping every other entry.
fn write_local(fastly_toml: &Path, entries: &[(String, String)]) -> Result<(), String> {
    let raw = fs::read_to_string(fastly_toml)
        .map_err(|error| format!("failed to read {}: {error}", fastly_toml.display()))?;
    // The parse error is left out, because it can quote a stored value.
    let mut document: DocumentMut = raw
        .parse()
        .map_err(|_error| format!("failed to parse {} as TOML", fastly_toml.display()))?;
    let contents = local_contents(&mut document).ok_or_else(|| {
        format!(
            "{} holds [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}] in a form other \
             than an inline-toml table",
            fastly_toml.display()
        )
    })?;
    for (key, value) in entries {
        contents.insert(key, toml_edit::value(value.as_str()));
    }
    fs::write(fastly_toml, document.to_string())
        .map_err(|error| format!("failed to write {}: {error}", fastly_toml.display()))
}

/// The `contents` table of the local server's logical config store, made
/// when absent, or `None` when the manifest holds the store in another form.
fn local_contents(document: &mut DocumentMut) -> Option<&mut Table> {
    let store = document
        .entry("local_server")
        .or_insert(toml_edit::table())
        .as_table_mut()?
        .entry("config_stores")
        .or_insert(toml_edit::table())
        .as_table_mut()?
        .entry(DEFAULT_CONFIG_STORE_ID)
        .or_insert_with(|| {
            let mut store = Table::new();
            store.insert("format", toml_edit::value("inline-toml"));
            Item::Table(store)
        })
        .as_table_mut()?;
    if store.get("format").and_then(Item::as_str) != Some("inline-toml") {
        return None;
    }
    store
        .entry("contents")
        .or_insert(toml_edit::table())
        .as_table_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_envelope_is_split_at_character_boundaries_with_the_pointer_last() {
        // After one byte, two-byte characters straddle the chunk limit, so a
        // split by bytes alone would cut one in half.
        let envelope_json = format!("x{}", "\u{e9}".repeat(ENTRY_LIMIT));

        let written = entries("a.example", &envelope_json, "data-sha").expect("should split");

        let (pointer_key, pointer) = written.last().expect("should write a pointer");
        assert_eq!(pointer_key, "a.example", "should write the pointer last");
        let chunks: Vec<&str> = written[..written.len() - 1]
            .iter()
            .map(|(_, chunk)| chunk.as_str())
            .collect();
        assert_eq!(
            chunks.concat(),
            envelope_json,
            "the chunks should join back into the envelope"
        );
        assert_eq!(
            chunks[0].len(),
            CHUNK_LIMIT - 1,
            "should end the first chunk before the character the limit cuts"
        );
        assert!(
            chunks.iter().all(|chunk| chunk.len() <= CHUNK_LIMIT),
            "no chunk should pass the chunk limit"
        );
        let pointer: serde_json::Value =
            serde_json::from_str(pointer).expect("should write the pointer as JSON");
        assert_eq!(pointer["edgezero_kind"], POINTER_KIND);
        assert_eq!(pointer["envelope_len"], envelope_json.len());
        assert_eq!(
            pointer["chunks"][0]["key"],
            format!("a.example{CHUNK_KEY_INFIX}{}.0", sha256_hex(&envelope_json)),
            "should key each chunk by the envelope's hash and its place"
        );
    }
}
