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
//!
//! A host's blob names its permission policy file and pins the file's
//! SHA-256, and carries no copy of the file. Each file `--permissions`
//! supplies is kept once, in a shared entry under its digest, written before
//! the blob, and an entry that already holds the file is not written again.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use edgezero_cli::args::{ConfigPushArgs, ConfigValidateArgs};
use edgezero_core::app_config::{self, AppConfigLoadOptions};
use edgezero_core::blob_envelope::BlobEnvelope;
use edgezero_core::manifest::{Manifest, ManifestLoader};
use error_stack::Report;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use toml_edit::{DocumentMut, Item, Table};
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID;
use trusted_server_core::permissions::policy_sha256;
use trusted_server_core::platform::{
    PlatformConfigStore, PlatformError, PolicyEntry, StoreId, StoreName, policy_entry_text,
    policy_key,
};
use trusted_server_core::settings_data::resolve_fastly_chunk_pointer;

use super::policy::{self, PolicyText, PushArgs};

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

/// Pushes the blob `args` names under `key`, into the local server's store,
/// with each policy file `--permissions` supplies in its shared entry.
///
/// The settings are checked as the plain push checks them, and the policy
/// file they name must be among the files supplied. Every key is checked
/// before anything is written. Each policy entry is written before the blob,
/// and a long value's chunks before the pointer to them. `--dry-run` lists
/// the keys and writes nothing.
///
/// # Errors
///
/// Returns a message when the push is not `--local`, the settings or the
/// policy files are refused, the blob fails validation, a key is refused, or
/// the local manifest cannot be read or written.
pub(crate) fn run(args: &PushArgs, key: &str, out: &mut dyn Write) -> Result<(), String> {
    let push = &args.edgezero;
    if !push.local {
        return Err(format!(
            "`--key {key}` can be pushed only with `--local` on Fastly. Write a live \
             store's entries with `fastly config-store-entry update`."
        ));
    }
    let loader = ManifestLoader::from_path(&push.manifest)
        .map_err(|error| format!("failed to load {}: {error}", push.manifest.display()))?;
    let manifest = loader.manifest();
    // The blob pins the file it names and carries no copy, because the file
    // is kept once, in its shared entry.
    let prepared = policy::prepare(
        &push.manifest,
        push.app_config.as_deref(),
        push.no_env,
        &args.policy.files,
        PolicyText::Omitted,
    )?;
    let envelope = envelope(&push.manifest, &prepared.file, manifest)?;
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|error| format!("failed to serialize the envelope: {error}"))?;
    let blob = entries(key, &envelope_json, &envelope.sha256)?;
    let policies = prepared
        .files
        .iter()
        .map(|file| policy_entries(&file.name, &file.text))
        .collect::<Result<Vec<_>, _>>()?;
    let fastly_toml = local_manifest(&push.manifest, manifest)?;
    let stored = local_entries(&fastly_toml)?;

    let (kept, missing): (Vec<_>, Vec<_>) = policies
        .iter()
        .partition(|policy| holds_policy(&stored, &policy.key, &policy.sha256));
    let written: Vec<(String, String)> = missing
        .iter()
        .flat_map(|policy| policy.entries.iter().cloned())
        .chain(blob)
        .collect();

    let verb = if push.dry_run { "would write" } else { "wrote" };
    if !push.dry_run {
        write_local(&fastly_toml, &written)?;
    }
    let output_failed = |error: std::io::Error| format!("failed to write command output: {error}");
    for policy in kept {
        writeln!(
            out,
            "kept `{}`, which already holds the policy `{}`",
            policy.key, policy.name
        )
        .map_err(output_failed)?;
    }
    for (entry_key, value) in &written {
        writeln!(
            out,
            "{verb} `{entry_key}` ({} bytes) to \
             [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}.contents] in {}",
            value.len(),
            fastly_toml.display()
        )
        .map_err(output_failed)?;
    }
    Ok(())
}

/// The blob in the settings file `app_config`, validated as
/// `ts config validate` validates it and wrapped in `EdgeZero`'s envelope.
///
/// The file is one [`policy::prepare`] wrote, with the `TRUSTED_SERVER__`
/// overlay already applied, so it is read without the overlay.
fn envelope(
    manifest_path: &Path,
    app_config: &Path,
    manifest: &Manifest,
) -> Result<BlobEnvelope, String> {
    let mut validate = ConfigValidateArgs::default();
    validate.app_config = Some(app_config.to_path_buf());
    validate.manifest = manifest_path.to_path_buf();
    validate.no_env = true;
    edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>(&validate)?;

    let app_name = manifest.app.name.as_deref().ok_or_else(|| {
        format!(
            "{} has no [app].name; cannot resolve the app config",
            manifest_path.display()
        )
    })?;
    let mut options = AppConfigLoadOptions::default();
    options.env_overlay = false;
    let typed = app_config::deserialize_app_config_with_options::<TrustedServerAppConfig>(
        app_config, app_name, &options,
    )
    .map_err(|error| format!("failed to load {}: {error}", app_config.display()))?;
    let data = serde_json::to_value(&typed)
        .map_err(|error| format!("failed to serialize the app config: {error}"))?;
    Ok(BlobEnvelope::new(data, generated_at()))
}

/// The time an envelope is made, as `EdgeZero` stamps one.
fn generated_at() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// One policy file, as the entries that hold it under its shared key.
struct PolicyEntries {
    /// The file's stem.
    name: String,
    /// The file's SHA-256.
    sha256: String,
    /// The shared key, `permissions_` and the file's SHA-256.
    key: String,
    /// The envelope, or its chunks followed by the pointer to them.
    entries: Vec<(String, String)>,
}

/// The entries that hold the policy file `name` with the contents `text`
/// under its shared key, an envelope whose data is the file's name and the
/// file, split as a long blob is.
fn policy_entries(name: &str, text: &str) -> Result<PolicyEntries, String> {
    let sha256 = policy_sha256(text);
    let key = policy_key(&sha256);
    let data = serde_json::to_value(PolicyEntry {
        name: name.to_owned(),
        text: text.to_owned(),
    })
    .map_err(|error| format!("failed to serialize the policy entry for `{name}`: {error}"))?;
    let envelope = BlobEnvelope::new(data, generated_at());
    let envelope_json = serde_json::to_string(&envelope)
        .map_err(|error| format!("failed to serialize the policy entry for `{name}`: {error}"))?;
    let entries = entries(&key, &envelope_json, &envelope.sha256)?;
    Ok(PolicyEntries {
        name: name.to_owned(),
        sha256,
        key,
        entries,
    })
}

/// Whether `stored` holds the policy file whose SHA-256 is `sha256` under
/// `key`, read back as the runtime reads it, chunks included. An entry that
/// cannot be read back, or holds another file, does not.
fn holds_policy(stored: &BTreeMap<String, String>, key: &str, sha256: &str) -> bool {
    let store = StoredEntries(stored);
    let store_name = StoreName::from(DEFAULT_CONFIG_STORE_ID);
    stored
        .get(key)
        .and_then(|value| resolve_fastly_chunk_pointer(&store, &store_name, value).ok())
        .and_then(|value| policy_entry_text(&value).ok())
        .is_some_and(|text| policy_sha256(&text) == sha256)
}

/// Entries already read from a store, for the chunk reader to read chunks
/// from.
struct StoredEntries<'a>(&'a BTreeMap<String, String>);

impl PlatformConfigStore for StoredEntries<'_> {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        self.0
            .get(key)
            .cloned()
            .ok_or_else(|| Report::new(PlatformError::ConfigStore).attach("not found"))
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
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

/// The entries the local server's logical config store holds in
/// `fastly_toml`, none when it holds none yet.
fn local_entries(fastly_toml: &Path) -> Result<BTreeMap<String, String>, String> {
    let raw = fs::read_to_string(fastly_toml)
        .map_err(|error| format!("failed to read {}: {error}", fastly_toml.display()))?;
    // The parse error is left out, because it can quote a stored value.
    let document: DocumentMut = raw
        .parse()
        .map_err(|_error| format!("failed to parse {} as TOML", fastly_toml.display()))?;
    let contents = document
        .get("local_server")
        .and_then(|item| item.get("config_stores"))
        .and_then(|item| item.get(DEFAULT_CONFIG_STORE_ID))
        .and_then(|item| item.get("contents"))
        .and_then(Item::as_table_like);
    Ok(contents
        .map(|table| {
            table
                .iter()
                .filter_map(|(key, value)| Some((key.to_owned(), value.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default())
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

    /// A valid policy whose top node grants everything.
    const OPEN_POLICY: &str = "groups:\n  open:\n    default: granted\nrules:\n  group: open\n  jurisdiction: non-regulated\n";

    /// A valid policy whose top node grants nothing without a signal.
    const STRICT_POLICY: &str = "groups:\n  strict:\n    default: requires_signal\nrules:\n  group: strict\n  jurisdiction: gdpr\n";

    /// The repetitions of a comment line that make a policy file too long for
    /// one entry.
    const PADDING_LINES: usize = 1_200;

    #[test]
    fn a_policy_file_is_kept_under_its_digest_and_read_back_whole() {
        let text = format!("{OPEN_POLICY}{}", "# padding\n".repeat(PADDING_LINES));

        let policy = policy_entries("open", &text).expect("should build the policy's entries");

        assert_eq!(policy.key, format!("permissions_{}", policy_sha256(&text)));
        assert!(
            policy.entries.len() > 2,
            "a long file should be kept as chunks and a pointer"
        );
        assert_eq!(
            policy.entries.last().map(|(key, _)| key.as_str()),
            Some(policy.key.as_str()),
            "the pointer should be written last"
        );
        let stored: BTreeMap<String, String> = policy.entries.iter().cloned().collect();
        assert!(
            holds_policy(&stored, &policy.key, &policy.sha256),
            "the entries should read back as the file"
        );
    }

    #[test]
    fn an_entry_missing_unreadable_or_holding_another_file_is_written_again() {
        let open = policy_entries("open", OPEN_POLICY).expect("should build the open entries");
        let strict =
            policy_entries("strict", STRICT_POLICY).expect("should build the strict entries");
        let garbled = BTreeMap::from([(open.key.clone(), "not an envelope".to_owned())]);
        let other: BTreeMap<String, String> = strict
            .entries
            .iter()
            .map(|(_, value)| (open.key.clone(), value.clone()))
            .collect();

        for (stored, case) in [
            (BTreeMap::new(), "a missing entry"),
            (garbled, "an entry that cannot be read back"),
            (other, "an entry holding another file"),
        ] {
            assert!(
                !holds_policy(&stored, &open.key, &open.sha256),
                "{case} should be written again"
            );
        }
    }

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
