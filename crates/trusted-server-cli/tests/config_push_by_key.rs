//! A Fastly push under a key other than the logical store ID, which `ts`
//! writes itself because `EdgeZero` writes no such key.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use error_stack::Report;
use tempfile::TempDir;
use toml_edit::DocumentMut;
use trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::permissions::{PolicyReference, policy_sha256};
use trusted_server_core::platform::{
    PlatformConfigStore, PlatformError, PlatformPolicySource, PlatformSecretStore, PolicyOrigin,
    StoreId, StoreName, policy_entry_text, policy_key,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings_from_config_store_with;

const HOST: &str = "a.publisher.example";

/// A second host, whose blob names the same policy file as [`HOST`]'s.
const OTHER_HOST: &str = "b.publisher.example";

/// The policy file each push supplies, in the project.
const POLICY_FILE: &str = "sample.yaml";

/// The repository's sample, which the project's app config names.
const POLICY_TEXT: &str = include_str!("../../../config/permissions/sample.yaml");

/// A small valid policy whose digest no other test uses, given a `group` no
/// other test passes, so no earlier read-back in the process holds its rules
/// and a read-back has to read the file from its shared entry.
fn policy_only_this_test_loads(group: &str) -> String {
    format!(
        "groups:
  {group}:
    default: requires_signal
rules:
  group: {group}
  jurisdiction: unknown
"
    )
}

/// A project holding the repository's manifests, with only the Fastly
/// adapter declared, the example app config naming the repository's sample
/// policy, and that policy's file.
fn project() -> TempDir {
    let directory = project_naming_no_policy();
    let path = directory.path().join("trusted-server.toml");
    let app_config = fs::read_to_string(&path).expect("should read the app config");
    fs::write(
        &path,
        format!("{app_config}\n[permissions]\npolicy = \"sample\"\n"),
    )
    .expect("should name the policy");
    fs::write(directory.path().join(POLICY_FILE), POLICY_TEXT)
        .expect("should write the policy file");
    directory
}

/// A project as [`project`] makes, with its policy file holding `policy`.
fn project_with_policy(policy: &str) -> TempDir {
    let directory = project();
    fs::write(directory.path().join(POLICY_FILE), policy).expect("should write the policy file");
    directory
}

/// A project holding the repository's manifests, with only the Fastly
/// adapter declared, and the example app config, which names no policy.
fn project_naming_no_policy() -> TempDir {
    let directory = tempfile::tempdir().expect("should create a temporary project");
    let mut manifest: toml::Value = toml::from_str(include_str!("../../../edgezero.toml"))
        .expect("should parse the repository manifest");
    manifest["adapters"]
        .as_table_mut()
        .expect("should declare adapters")
        .retain(|name, _| name == "fastly");
    fs::write(
        directory.path().join("edgezero.toml"),
        toml::to_string(&manifest).expect("should serialize the Fastly test manifest"),
    )
    .expect("should write the test manifest");
    fs::write(
        directory.path().join("fastly.toml"),
        include_str!("../../../fastly.toml"),
    )
    .expect("should copy the local Fastly manifest");
    fs::write(
        directory.path().join("trusted-server.toml"),
        include_str!("../../../trusted-server.example.toml"),
    )
    .expect("should copy the example app config");
    directory
}

/// Makes the project's app config long enough to be stored as chunks.
fn lengthen_app_config(project: &TempDir) {
    let path = project.path().join("trusted-server.toml");
    let mut document: DocumentMut = fs::read_to_string(&path)
        .expect("should read the app config")
        .parse()
        .expect("should parse the app config");
    let mut domains = toml_edit::Array::new();
    for index in 0..400 {
        domains.push(format!("*.assets-{index}.publisher.example"));
    }
    document["proxy"]["allowed_domains"] = toml_edit::value(domains);
    fs::write(&path, document.to_string()).expect("should write the app config");
}

fn fastly_toml(project: &TempDir) -> PathBuf {
    project.path().join("fastly.toml")
}

/// Runs `ts config push --adapter fastly` in `project` with `args`.
fn push(project: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "config",
            "push",
            "--adapter",
            "fastly",
            "--no-diff",
            "--yes",
        ])
        .args(args)
        .current_dir(project.path())
        // Do not inherit operator configuration or credentials from the test runner.
        .env_clear()
        // The push hands its settings over in a file in the temp folder.
        .envs(["TMPDIR", "TMP", "TEMP"].map(|name| (name, std::env::temp_dir().into_os_string())))
        .env("RUST_LOG", "info")
        .env("TRUSTED_SERVER__PUBLISHER__DOMAIN", HOST)
        .env(
            "TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN",
            format!(".{HOST}"),
        )
        .env(
            "TRUSTED_SERVER__SOURCE__ORIGIN__URL",
            "https://origin.publisher.example",
        )
        .output()
        .expect("should run a config push")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "the push should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The local server's logical config store, in the order its entries sit in
/// the file.
fn stored_entries(project: &TempDir) -> Vec<(String, String)> {
    let document: DocumentMut = fs::read_to_string(fastly_toml(project))
        .expect("should read the local Fastly manifest")
        .parse()
        .expect("should parse the local Fastly manifest");
    document["local_server"]["config_stores"][DEFAULT_CONFIG_STORE_ID]["contents"]
        .as_table()
        .expect("should hold the store's contents")
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().expect("should store each entry as a string");
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

struct MemoryConfigStore(BTreeMap<String, String>);

impl MemoryConfigStore {
    fn entry(&self, key: &str) -> Result<String, Report<PlatformError>> {
        self.0.get(key).cloned().ok_or_else(|| {
            Report::new(PlatformError::ConfigStore).attach(format!("missing key `{key}`"))
        })
    }
}

impl PlatformConfigStore for MemoryConfigStore {
    /// The entry at `key`, its chunks joined in order when it is a chunk
    /// pointer, as the Fastly config store returns a long entry whole.
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let value = self.entry(key)?;
        let pointer: serde_json::Value =
            serde_json::from_str(&value).unwrap_or(serde_json::Value::Null);
        if pointer["edgezero_kind"] != "fastly_config_chunks" {
            return Ok(value);
        }
        pointer["chunks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|chunk| self.entry(chunk["key"].as_str().unwrap_or_default()))
            .collect()
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Ok(())
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Ok(())
    }
}

/// Answers every secret reference with one value, long enough for each.
struct FixedSecretStore;

impl PlatformSecretStore for FixedSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Ok(b"read-back-secret-value-of-at-least-32-bytes".to_vec())
    }

    fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Ok(())
    }

    fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Ok(())
    }
}

thread_local! {
    /// The policy file reads [`SharedPolicyEntries`] has answered on this
    /// thread, which runs one test at a time.
    static POLICY_READS: Cell<usize> = const { Cell::new(0) };
}

/// Reads a host's policy file from its shared entry, as the Fastly adapter
/// does, chunks included.
struct SharedPolicyEntries<'a>(&'a MemoryConfigStore);

impl PlatformPolicySource for SharedPolicyEntries<'_> {
    fn policy_text(&self, reference: &PolicyReference) -> Result<String, Report<PlatformError>> {
        POLICY_READS.with(|reads| reads.set(reads.get() + 1));
        let store_name = StoreName::from(DEFAULT_CONFIG_STORE_ID);
        let value = self.0.get(&store_name, &policy_key(&reference.sha256))?;
        policy_entry_text(&value)
    }
}

/// Reads the blob at `key` the way the runtime does, chunks included, with a
/// host's policy file read from its shared entry.
fn read_back(
    entries: &[(String, String)],
    key: &str,
) -> Result<Settings, Report<TrustedServerError>> {
    let store = MemoryConfigStore(entries.iter().cloned().collect());
    let shared = SharedPolicyEntries(&store);
    let origin = if key == DEFAULT_CONFIG_STORE_ID {
        PolicyOrigin::Blob
    } else {
        PolicyOrigin::Source(&shared)
    };
    get_settings_from_config_store_with(
        &store,
        &FixedSecretStore,
        &StoreName::from(DEFAULT_CONFIG_STORE_ID),
        key,
        &StoreName::from("trusted_server_secrets"),
        origin,
        &trusted_server_cli::linked_builders(),
    )
}

/// Reads the blob at `key` back as [`read_back`] does, and returns the result
/// with the number of times a shared policy entry was read, which is none
/// when the process already holds the rules of the digest the blob pins.
fn read_back_counting_reads(
    entries: &[(String, String)],
    key: &str,
) -> (Result<Settings, Report<TrustedServerError>>, usize) {
    let before = POLICY_READS.with(Cell::get);
    let settings = read_back(entries, key);
    (settings, POLICY_READS.with(Cell::get) - before)
}

#[test]
fn a_local_push_of_a_host_key_writes_the_local_store() {
    let project = project_with_policy(&policy_only_this_test_loads("host-key"));

    let output = push(
        &project,
        &["--local", "--key", HOST, "--permissions", POLICY_FILE],
    );

    assert_success(&output);
    let entries = stored_entries(&project);
    let (settings, reads) = read_back_counting_reads(&entries, HOST);
    let settings = settings.expect("should read the host's blob back");
    assert_eq!(
        reads, 1,
        "the read-back should read the policy file from its shared entry"
    );
    assert_eq!(
        settings.publisher.domain, HOST,
        "should store the pushed app config"
    );
    assert_eq!(
        settings.permission_policy().map(|policy| policy.name()),
        Some("sample"),
        "the host should run the policy its blob names"
    );
    assert!(
        entries
            .iter()
            .all(|(key, _)| key != DEFAULT_CONFIG_STORE_ID),
        "a host's push should not write the logical store ID"
    );

    let logical = push(
        &project,
        &[
            "--local",
            "--key",
            DEFAULT_CONFIG_STORE_ID,
            "--permissions",
            POLICY_FILE,
        ],
    );

    assert_success(&logical);
    assert!(
        String::from_utf8_lossy(&logical.stdout)
            .contains(&format!("pushed-key={DEFAULT_CONFIG_STORE_ID}")),
        "a push of the logical store ID should still go through EdgeZero: {}",
        String::from_utf8_lossy(&logical.stdout)
    );
}

#[test]
fn a_long_document_is_written_as_chunks_then_a_pointer() {
    let project = project_with_policy(&policy_only_this_test_loads("chunked-blob"));
    lengthen_app_config(&project);
    let before = fs::read_to_string(fastly_toml(&project)).expect("should read the manifest");

    let preview = push(
        &project,
        &[
            "--local",
            "--key",
            HOST,
            "--dry-run",
            "--permissions",
            POLICY_FILE,
        ],
    );

    assert_success(&preview);
    assert!(
        String::from_utf8_lossy(&preview.stdout).contains("would write"),
        "a dry run should list the keys it would write"
    );
    assert_eq!(
        fs::read_to_string(fastly_toml(&project)).expect("should read the manifest"),
        before,
        "a dry run should write nothing"
    );

    assert_success(&push(
        &project,
        &["--local", "--key", HOST, "--permissions", POLICY_FILE],
    ));

    let entries = stored_entries(&project);
    let chunk_prefix = format!("{HOST}.__edgezero_chunks.");
    let chunk_count = entries
        .iter()
        .filter(|(key, _)| key.starts_with(&chunk_prefix))
        .count();
    assert!(chunk_count > 1, "a long blob should be split into chunks");
    let pointer_at = entries
        .iter()
        .position(|(key, _)| key == HOST)
        .expect("should write the pointer");
    let written: Vec<&str> = entries[..pointer_at]
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| key.starts_with(&chunk_prefix))
        .collect();
    assert_eq!(
        written.len(),
        chunk_count,
        "every chunk should be written before the pointer"
    );
    // The Fastly adapter reads a written document back whole, which
    // `config_chunks.rs` tests against a fixture this writer produced.
    let pointer: serde_json::Value =
        serde_json::from_str(&entries[pointer_at].1).expect("should write the pointer as JSON");
    let named: Vec<&str> = pointer["chunks"]
        .as_array()
        .expect("should name the chunks")
        .iter()
        .map(|chunk| chunk["key"].as_str().expect("should name each chunk's key"))
        .collect();
    assert_eq!(
        named, written,
        "the pointer should name the chunks written, in order"
    );
}

#[test]
fn a_key_whose_chunk_keys_are_too_long_is_refused_before_writing() {
    let project = project();
    lengthen_app_config(&project);
    let before = fs::read_to_string(fastly_toml(&project)).expect("should read the manifest");
    // Short enough for the pointer, too long once a chunk key adds its
    // infix, the envelope's hash and an index.
    let key = "k".repeat(200);

    let output = push(
        &project,
        &["--local", "--key", &key, "--permissions", POLICY_FILE],
    );

    assert!(!output.status.success(), "the push should be refused");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("255-character limit"),
        "should say why: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fastly_toml(&project)).expect("should read the manifest"),
        before,
        "a refused push should write nothing"
    );
}

/// Two hosts whose blobs name one file share one entry, so the file is kept
/// once however many hosts name it.
#[test]
fn two_hosts_naming_one_policy_file_keep_it_once() {
    let policy_text = policy_only_this_test_loads("two-hosts");
    let project = project_with_policy(&policy_text);
    let policy_entry = policy_key(&policy_sha256(&policy_text));

    assert_success(&push(
        &project,
        &["--local", "--key", HOST, "--permissions", POLICY_FILE],
    ));
    let first = stored_entries(&project);
    let second = push(
        &project,
        &["--local", "--key", OTHER_HOST, "--permissions", POLICY_FILE],
    );

    assert_success(&second);
    assert!(
        String::from_utf8_lossy(&second.stdout).contains(&format!("kept `{policy_entry}`")),
        "the second push should keep the entry the first wrote: {}",
        String::from_utf8_lossy(&second.stdout)
    );
    let entries = stored_entries(&project);
    let held = |entries: &[(String, String)]| {
        entries
            .iter()
            .filter(|(key, _)| key.starts_with(&policy_entry))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(
        entries.iter().any(|(key, _)| *key == policy_entry),
        "the file should be kept under its digest"
    );
    assert_eq!(
        held(&entries),
        held(&first),
        "the second push should write no entry of the file again"
    );
    let pointer_at = entries
        .iter()
        .position(|(key, _)| *key == policy_entry)
        .expect("should keep the policy entry");
    let blob_at = entries
        .iter()
        .position(|(key, _)| key == HOST)
        .expect("should write the first host's blob");
    assert!(
        pointer_at < blob_at,
        "the policy entry should be written before the blob that names it"
    );
    // The first host's read-back reads the shared entry, and the second
    // host's is answered by the rules the first one loaded.
    for (host, expected_reads) in [(HOST, 1), (OTHER_HOST, 0)] {
        let (settings, reads) = read_back_counting_reads(&entries, host);
        let settings = settings.expect("each host's blob should load");
        assert_eq!(
            reads, expected_reads,
            "only the first host's read-back should read the shared entry"
        );
        assert_eq!(
            settings.permission_policy().map(|policy| policy.sha256()),
            Some(policy_sha256(&policy_text).as_str()),
            "{host} should run the file both pushes supplied"
        );
    }
}

/// The keyed push refuses what the plain push refuses, so a host's blob
/// always names a supplied policy file.
#[test]
fn a_keyed_push_refuses_settings_that_name_no_policy_and_writes_nothing() {
    let project = project_naming_no_policy();
    fs::write(project.path().join(POLICY_FILE), POLICY_TEXT).expect("should write the policy file");
    let before = fs::read_to_string(fastly_toml(&project)).expect("should read the manifest");

    let output = push(
        &project,
        &["--local", "--key", HOST, "--permissions", POLICY_FILE],
    );

    assert!(!output.status.success(), "the push should be refused");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("the settings name no permission policy"),
        "should say what is missing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fastly_toml(&project)).expect("should read the manifest"),
        before,
        "a refused push should write nothing"
    );
}

#[test]
fn a_remote_dry_run_names_the_live_store_and_writes_nothing() {
    let project = project();
    let before = fs::read_to_string(fastly_toml(&project)).expect("should read the manifest");

    // The runner's environment is cleared, so no `fastly` CLI can be found
    // and a dry run that asked it anything would fail.
    let output = push(
        &project,
        &["--key", HOST, "--dry-run", "--permissions", POLICY_FILE],
    );

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("would write `{HOST}` ("))
            && stdout.contains(&format!(
                "the Fastly config store `{DEFAULT_CONFIG_STORE_ID}`"
            )),
        "should list the key for the store the selector names: {stdout}"
    );
    assert_eq!(
        fs::read_to_string(fastly_toml(&project)).expect("should read the manifest"),
        before,
        "a push without --local should not write the local store"
    );
}
