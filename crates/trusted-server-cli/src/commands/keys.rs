//! `ts keys`, the request signing key commands.
//!
//! An operator holding a Fastly API token rotates and retires the keys the
//! service signs with. The commands write the stores through the `fastly`
//! CLI, with the token taken from `FASTLY_API_TOKEN` and passed to the
//! `fastly` process in its environment, and a private key reaches the
//! `fastly` process on its standard input, never in its arguments.
//!
//! The rotation library in core takes a read that fails for an entry that
//! does not exist. Every read here is a call to the platform, which can fail
//! for other reasons, so the stores tell the two apart and write nothing once
//! a read has failed.

use std::io::Write as _;
use std::sync::{Arc, OnceLock};

use error_stack::Report;
use trusted_server_core::permissions::PermissionPolicy;
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, DisabledGeo, PlatformBackend, PlatformBackendSpec,
    PlatformConfigStore, PlatformError, PlatformSecretStore, RuntimeServices, StoreId, StoreName,
    UnavailableHttpClient, UnavailableKvStore,
};
use trusted_server_core::request_signing::rotation::KeyRotationManager;
use trusted_server_core::request_signing::{kid_is_creatable, kid_is_well_formed};

use crate::fastly_cli::{FastlyProcess, FastlyRunner};

/// How the `fastly` CLI begins its report that the API found nothing at the
/// address it was asked for. Versions 15.1.0 and 16.1.0 print it for a missing
/// config store, config store entry, secret store and secret store entry.
const API_NOT_FOUND: &str = "the Fastly API returned 404";

/// The entries of the config store that are not keys. A key named as one of
/// them would be written over, or would take the entry with it when deleted.
const BOOKKEEPING_ENTRIES: [&str; 2] = ["current-kid", "active-kids"];

/// Request signing key commands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum KeysCommand {
    /// Create a signing key and make it the one the service signs with.
    Rotate(RotateArgs),
    /// Take a key out of the active set, and delete it with `--delete`.
    Deactivate(DeactivateArgs),
}

/// The two stores request signing keeps its keys in.
#[derive(Debug, clap::Args)]
pub(crate) struct StoreArgs {
    /// The Fastly config store the service links as `jwks_store`, holding the
    /// public keys, `current-kid` and `active-kids`.
    #[arg(long, value_name = "ID")]
    config_store_id: String,
    /// The Fastly secret store the service links as `signing_keys`, holding
    /// the private keys.
    #[arg(long, value_name = "ID")]
    secret_store_id: String,
}

/// Arguments for `ts keys rotate`.
#[derive(Debug, clap::Args)]
pub(crate) struct RotateArgs {
    #[command(flatten)]
    stores: StoreArgs,
    /// The new key's id, `ts-<date>` when not given. It starts with a lower
    /// case letter, holds only letters, digits, `-`, `_`, `.` and `:`, and is
    /// at most 128 characters.
    #[arg(long)]
    kid: Option<String>,
}

/// Arguments for `ts keys deactivate`.
#[derive(Debug, clap::Args)]
pub(crate) struct DeactivateArgs {
    #[command(flatten)]
    stores: StoreArgs,
    /// The key to take out of the active set. The current key is refused, so
    /// rotate first.
    #[arg(long)]
    kid: String,
    /// Also delete the key from both stores.
    #[arg(long)]
    delete: bool,
}

/// The `fastly` CLI as the two stores of one command use it.
struct Platform {
    runner: Arc<dyn FastlyRunner>,
    /// The first read that failed for a reason other than a missing entry.
    failed_read: OnceLock<String>,
}

impl Platform {
    /// Runs `fastly <command> <action>` on the store `store_id`, with `flags`
    /// after the store id.
    ///
    /// A value is joined to its flag with `=`, so that `fastly` never takes a
    /// value beginning with `-` for a flag of its own.
    fn run(
        &self,
        command: &str,
        action: &str,
        store_id: &str,
        flags: &[String],
        stdin: Option<&str>,
    ) -> Result<String, String> {
        let mut arguments = vec![
            command.to_owned(),
            action.to_owned(),
            format!("--store-id={store_id}"),
        ];
        arguments.extend_from_slice(flags);
        arguments.push("--non-interactive".to_owned());
        self.runner.run(&arguments, stdin)
    }

    /// Refuses a write once a read has failed.
    ///
    /// The rotation library would otherwise take the failed read for an empty
    /// store, and write a key list that drops the keys already active or
    /// retire the key the service signs with.
    fn writable(&self, kind: PlatformError) -> Result<(), Report<PlatformError>> {
        match self.failed_read.get() {
            Some(message) => Err(Report::new(kind).attach(format!(
                "nothing is written, because the config store could not be read: {message}"
            ))),
            None => Ok(()),
        }
    }

    /// Deletes one entry of a store, and takes an entry that is already gone
    /// for deleted, so that a delete tried again after a partial failure
    /// finishes.
    ///
    /// The API answers that it found nothing for a missing store as well, so
    /// the store is asked for before a missing entry is taken for deleted.
    fn delete(&self, store: &str, store_id: &str, entry: String) -> Result<(), String> {
        let deleted = self.run(
            &format!("{store}-entry"),
            "delete",
            store_id,
            &[entry, "--auto-yes".to_owned()],
            None,
        );
        match deleted {
            Ok(_) => Ok(()),
            Err(message) if message.contains(API_NOT_FOUND) => self
                .run(store, "describe", store_id, &["--json".to_owned()], None)
                .map(drop)
                .map_err(|_| message),
            Err(message) => Err(message),
        }
    }
}

/// The value in what `fastly config-store-entry describe --json` printed.
fn item_value(output: &str, key: &str) -> Result<String, String> {
    let entry: serde_json::Value = serde_json::from_str(output).map_err(|error| {
        format!("`fastly config-store-entry describe` did not answer JSON: {error}")
    })?;
    entry
        .get("item_value")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("the entry `{key}` has no value"))
}

/// The config store, read and written through the `fastly` CLI.
///
/// Reads name the store the service links, which is not an id the API
/// knows, so every read goes to the store the command was given.
struct FastlyConfigStore {
    platform: Arc<Platform>,
    store_id: String,
}

impl PlatformConfigStore for FastlyConfigStore {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        self.platform
            .run(
                "config-store-entry",
                "describe",
                &self.store_id,
                &[format!("--key={key}"), "--json".to_owned()],
                None,
            )
            .and_then(|output| item_value(&output, key))
            .map_err(|message| {
                if !message.contains(API_NOT_FOUND) {
                    let _ = self.platform.failed_read.set(message.clone());
                }
                Report::new(PlatformError::ConfigStore).attach(message)
            })
    }

    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>> {
        self.platform.writable(PlatformError::ConfigStore)?;
        self.platform
            .run(
                "config-store-entry",
                "update",
                store_id.as_ref(),
                &[
                    format!("--key={key}"),
                    "--stdin".to_owned(),
                    "--upsert".to_owned(),
                ],
                Some(value),
            )
            .map(drop)
            .map_err(|message| Report::new(PlatformError::ConfigStore).attach(message))
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        self.platform.writable(PlatformError::ConfigStore)?;
        self.platform
            .delete("config-store", store_id.as_ref(), format!("--key={key}"))
            .map_err(|message| Report::new(PlatformError::ConfigStore).attach(message))
    }
}

/// The secret store, written through the `fastly` CLI and never read, because
/// the API does not give a secret back.
struct FastlySecretStore {
    platform: Arc<Platform>,
}

impl PlatformSecretStore for FastlySecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported)
            .attach("a secret cannot be read back from the platform"))
    }

    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        self.platform.writable(PlatformError::SecretStore)?;
        self.platform
            .run(
                "secret-store-entry",
                "create",
                store_id.as_ref(),
                &[format!("--name={name}"), "--stdin".to_owned()],
                Some(value),
            )
            .map(drop)
            .map_err(|message| Report::new(PlatformError::SecretStore).attach(message))
    }

    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        self.platform.writable(PlatformError::SecretStore)?;
        self.platform
            .delete("secret-store", store_id.as_ref(), format!("--name={name}"))
            .map_err(|message| Report::new(PlatformError::SecretStore).attach(message))
    }
}

/// A backend nothing here asks for.
struct NoBackend;

impl PlatformBackend for NoBackend {
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Axum
    }

    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

/// The services the rotation library writes through, for these stores.
fn services(runner: Arc<dyn FastlyRunner>, stores: &StoreArgs) -> RuntimeServices {
    let platform = Arc::new(Platform {
        runner,
        failed_read: OnceLock::new(),
    });
    RuntimeServices::builder()
        .config_store(Arc::new(FastlyConfigStore {
            platform: Arc::clone(&platform),
            store_id: stores.config_store_id.clone(),
        }))
        .secret_store(Arc::new(FastlySecretStore { platform }))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(NoBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(DisabledGeo))
        .client_info(ClientInfo::default())
        // The commands decide no request's permissions, and every services
        // graph carries a policy, so they carry the one that grants nothing.
        .permission_policy(PermissionPolicy::granting_nothing())
        .build()
}

/// Refuses a key id that names one of the store's bookkeeping entries.
fn refuse_bookkeeping_entry(kid: &str) -> Result<(), String> {
    if BOOKKEEPING_ENTRIES.contains(&kid) {
        return Err(format!(
            "kid `{kid}` names an entry the store keeps its key list in, and cannot name a key"
        ));
    }
    Ok(())
}

/// Runs a `ts keys` command with the token from
/// [`crate::fastly_cli::TOKEN_VARIABLE`].
///
/// # Errors
///
/// A message when the token is not set or the command fails.
pub(crate) fn run(command: &KeysCommand) -> Result<(), String> {
    let report = run_with(command, Arc::new(FastlyProcess::from_environment()?))?;
    writeln!(std::io::stdout().lock(), "{report}")
        .map_err(|error| format!("the report could not be written: {error}"))
}

/// Runs a `ts keys` command through `runner` and returns what it reports, as
/// JSON.
///
/// # Errors
///
/// A message when the key id is refused, a store cannot be read or a store
/// write fails.
pub(crate) fn run_with(
    command: &KeysCommand,
    runner: Arc<dyn FastlyRunner>,
) -> Result<String, String> {
    match command {
        KeysCommand::Rotate(rotate) => {
            if let Some(kid) = rotate.kid.as_deref() {
                if !kid_is_creatable(kid) {
                    return Err(format!(
                        "kid `{kid}` must start with a lower case letter, hold only letters, \
                         digits, `-`, `_`, `.` and `:`, and be at most 128 characters"
                    ));
                }
                refuse_bookkeeping_entry(kid)?;
            }
            let services = services(runner, &rotate.stores);
            let manager = KeyRotationManager::new(
                &rotate.stores.config_store_id,
                &rotate.stores.secret_store_id,
            );
            let result = manager
                .rotate_key(&services, rotate.kid.clone())
                .map_err(|report| format!("the key was not rotated: {report:?}"))?;
            let jwk = serde_json::to_value(&result.jwk)
                .map_err(|error| format!("the new public key could not be written: {error}"))?;
            Ok(serde_json::json!({
                "new_kid": result.new_kid,
                "previous_kid": result.previous_kid,
                "active_kids": result.active_kids,
                "jwk": jwk,
            })
            .to_string())
        }
        KeysCommand::Deactivate(deactivate) => {
            if !kid_is_well_formed(&deactivate.kid) {
                return Err(format!(
                    "kid `{}` must hold only letters, digits, `-`, `_`, `.` and `:`, and be 1 \
                     to 128 characters",
                    deactivate.kid
                ));
            }
            refuse_bookkeeping_entry(&deactivate.kid)?;
            let services = services(runner, &deactivate.stores);
            let manager = KeyRotationManager::new(
                &deactivate.stores.config_store_id,
                &deactivate.stores.secret_store_id,
            );
            if deactivate.delete {
                manager.delete_key(&services, &deactivate.kid)
            } else {
                manager.deactivate_key(&services, &deactivate.kid)
            }
            .map_err(|report| format!("the key was not deactivated: {report:?}"))?;
            let remaining = manager.list_active_keys(&services).map_err(|report| {
                format!(
                    "the key was retired, and the active keys could not then be read: {report:?}"
                )
            })?;
            Ok(serde_json::json!({
                "deactivated_kid": deactivate.kid,
                "deleted": deactivate.delete,
                "remaining_active_kids": remaining,
            })
            .to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    use super::*;

    const CONFIG_STORE: &str = "config-store-id";
    const SECRET_STORE: &str = "secret-store-id";

    /// One `fastly` invocation as the fake saw it.
    #[derive(Debug, Clone)]
    struct Call {
        args: Vec<String>,
        stdin: Option<String>,
    }

    impl Call {
        /// Whether the invocation changes a store.
        fn writes(&self) -> bool {
            matches!(self.args[1].as_str(), "update" | "create" | "delete")
        }
    }

    /// Plays the `fastly` CLI against two stores held in memory, recording
    /// every invocation. It answers a missing store or entry as the real one
    /// does, with the API's 404.
    #[derive(Debug, Default)]
    struct FakeFastly {
        config: Mutex<BTreeMap<String, String>>,
        secrets: Mutex<BTreeMap<String, String>>,
        calls: Mutex<Vec<Call>>,
        /// Config keys whose next read fails as a network fault would.
        failing_reads: Mutex<BTreeSet<String>>,
    }

    impl FakeFastly {
        fn with_config(entries: &[(&str, &str)]) -> Self {
            let fake = Self::default();
            fake.config.lock().expect("should lock").extend(
                entries
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
            );
            fake
        }

        fn config(&self, key: &str) -> Option<String> {
            self.config.lock().expect("should lock").get(key).cloned()
        }

        fn has_secret(&self, name: &str) -> bool {
            self.secrets.lock().expect("should lock").contains_key(name)
        }

        fn add_secret(&self, name: &str) {
            self.secrets
                .lock()
                .expect("should lock")
                .insert(name.to_owned(), "private".to_owned());
        }

        /// Makes the next read of `key` fail for a reason other than the
        /// entry being absent.
        fn fail_next_read_of(&self, key: &str) {
            self.failing_reads
                .lock()
                .expect("should lock")
                .insert(key.to_owned());
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("should lock").clone()
        }

        fn writes(&self) -> Vec<Call> {
            self.calls().into_iter().filter(Call::writes).collect()
        }
    }

    /// The value joined to `name` with `=`, which is the only form the
    /// command may pass a value in.
    fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        args.iter()
            .find_map(|arg| arg.strip_prefix(name)?.strip_prefix('='))
    }

    /// What `fastly` 15.1.0 and 16.1.0 print on standard error when the API
    /// finds no config store or entry, and no secret store or secret.
    const RECORDED_CONFIG_404: &str = "ERROR: the Fastly API returned 404 Not Found: Record \
                                       not found.\n\nIf you believe this error is the result \
                                       of a bug, please file an issue: \
                                       https://github.com/fastly/cli/issues/new?labels=bug&\
                                       template=bug_report.md";
    const RECORDED_SECRET_404: &str = "ERROR: the Fastly API returned 404 Not Found: Not \
                                       Found (not found).\n\nIf you believe this error is the \
                                       result of a bug, please file an issue: \
                                       https://github.com/fastly/cli/issues/new?labels=bug&\
                                       template=bug_report.md";

    /// The failure the runner reports for a recorded 404, `what` saying
    /// which store was asked.
    fn not_found(what: &str) -> String {
        let recorded = if what.contains("secret") {
            RECORDED_SECRET_404
        } else {
            RECORDED_CONFIG_404
        };
        format!("`fastly` failed: {recorded}")
    }

    impl FastlyRunner for FakeFastly {
        fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
            self.calls.lock().expect("should lock").push(Call {
                args: args.to_vec(),
                stdin: stdin.map(str::to_owned),
            });
            let command = (args[0].as_str(), args[1].as_str());
            let store = flag(args, "--store-id").unwrap_or_default();
            match command {
                ("config-store-entry" | "config-store", _) if store != CONFIG_STORE => {
                    Err(not_found("no such config store"))
                }
                ("secret-store-entry" | "secret-store", _) if store != SECRET_STORE => {
                    Err(not_found("no such secret store"))
                }
                ("config-store" | "secret-store", "describe") => Ok("{}".to_owned()),
                ("config-store-entry", "describe") => {
                    let key = flag(args, "--key").unwrap_or_default();
                    if self.failing_reads.lock().expect("should lock").remove(key) {
                        return Err("`fastly` failed: ERROR: network is unreachable".to_owned());
                    }
                    self.config(key)
                        .map(|value| serde_json::json!({ "item_value": value }).to_string())
                        .ok_or_else(|| not_found("no such entry"))
                }
                ("config-store-entry", "update") => {
                    let key = flag(args, "--key").unwrap_or_default();
                    self.config
                        .lock()
                        .expect("should lock")
                        .insert(key.to_owned(), stdin.unwrap_or_default().to_owned());
                    Ok(String::new())
                }
                ("config-store-entry", "delete") => {
                    let key = flag(args, "--key").unwrap_or_default();
                    self.config
                        .lock()
                        .expect("should lock")
                        .remove(key)
                        .map(|_| String::new())
                        .ok_or_else(|| not_found("no such entry"))
                }
                ("secret-store-entry", "create") => {
                    // The API refuses to create a secret under a name the
                    // store holds, and replaces one only when asked to with
                    // `--recreate` or `--recreate-allow`.
                    let name = flag(args, "--name").unwrap_or_default();
                    let mut secrets = self.secrets.lock().expect("should lock");
                    let replace = args
                        .iter()
                        .any(|arg| arg == "--recreate" || arg == "--recreate-allow");
                    if secrets.contains_key(name) && !replace {
                        return Err(format!("`fastly` failed: ERROR: secret `{name}` exists"));
                    }
                    secrets.insert(name.to_owned(), stdin.unwrap_or_default().to_owned());
                    Ok(String::new())
                }
                ("secret-store-entry", "delete") => {
                    let name = flag(args, "--name").unwrap_or_default();
                    self.secrets
                        .lock()
                        .expect("should lock")
                        .remove(name)
                        .map(|_| String::new())
                        .ok_or_else(|| not_found("no such secret"))
                }
                _ => Err(format!("unexpected fastly {args:?}")),
            }
        }
    }

    fn stores() -> StoreArgs {
        StoreArgs {
            config_store_id: CONFIG_STORE.to_owned(),
            secret_store_id: SECRET_STORE.to_owned(),
        }
    }

    fn rotate(fake: &Arc<FakeFastly>, kid: Option<&str>) -> Result<serde_json::Value, String> {
        let command = KeysCommand::Rotate(RotateArgs {
            stores: stores(),
            kid: kid.map(str::to_owned),
        });
        run_with(&command, Arc::clone(fake) as Arc<dyn FastlyRunner>)
            .map(|report| serde_json::from_str(&report).expect("should report JSON"))
    }

    fn deactivate_in(
        fake: &Arc<FakeFastly>,
        stores: StoreArgs,
        kid: &str,
        delete: bool,
    ) -> Result<serde_json::Value, String> {
        let command = KeysCommand::Deactivate(DeactivateArgs {
            stores,
            kid: kid.to_owned(),
            delete,
        });
        run_with(&command, Arc::clone(fake) as Arc<dyn FastlyRunner>)
            .map(|report| serde_json::from_str(&report).expect("should report JSON"))
    }

    fn deactivate(
        fake: &Arc<FakeFastly>,
        kid: &str,
        delete: bool,
    ) -> Result<serde_json::Value, String> {
        deactivate_in(fake, stores(), kid, delete)
    }

    /// Two active keys, `ts-new` the current one, with `ts-old` held in both
    /// stores.
    fn two_keys() -> Arc<FakeFastly> {
        let fake = Arc::new(FakeFastly::with_config(&[
            ("current-kid", "ts-new"),
            ("active-kids", "ts-old,ts-new"),
            ("ts-old", "{}"),
        ]));
        fake.add_secret("ts-old");
        fake
    }

    #[test]
    fn rotate_stores_a_new_key_and_makes_it_current() {
        let fake = Arc::new(FakeFastly::with_config(&[
            ("current-kid", "ts-old"),
            ("active-kids", "ts-old"),
            ("ts-old", "{}"),
        ]));

        let report = rotate(&fake, Some("ts-new")).expect("should rotate");

        assert_eq!(report["new_kid"], "ts-new");
        assert_eq!(report["previous_kid"], "ts-old");
        assert_eq!(fake.config("current-kid").as_deref(), Some("ts-new"));
        assert_eq!(fake.config("active-kids").as_deref(), Some("ts-old,ts-new"));
        let jwk: serde_json::Value =
            serde_json::from_str(&fake.config("ts-new").expect("should store the public key"))
                .expect("should store the public key as JSON");
        assert_eq!(jwk["kid"], "ts-new");
        assert_eq!(report["jwk"], jwk, "should report the stored public key");
        assert!(
            fake.secrets
                .lock()
                .expect("should lock")
                .get("ts-new")
                .is_some_and(|secret| !secret.is_empty()),
            "should store the private key in the secret store"
        );
    }

    #[test]
    fn the_first_key_of_an_empty_store_has_no_previous_key() {
        let fake = Arc::new(FakeFastly::default());

        let report = rotate(&fake, Some("ts-new")).expect("should rotate");

        assert_eq!(report["previous_kid"], serde_json::Value::Null);
        assert_eq!(fake.config("current-kid").as_deref(), Some("ts-new"));
        assert_eq!(fake.config("active-kids").as_deref(), Some("ts-new"));
    }

    #[test]
    fn the_private_key_reaches_fastly_only_on_its_standard_input() {
        let fake = Arc::new(FakeFastly::default());

        rotate(&fake, Some("ts-new")).expect("should rotate");

        let secret = fake
            .secrets
            .lock()
            .expect("should lock")
            .get("ts-new")
            .cloned()
            .expect("should store the private key");
        for call in fake.calls() {
            assert!(
                call.args.iter().all(|arg| !arg.contains(&secret)),
                "a private key in the arguments is visible to every process: {:?}",
                call.args
            );
            assert!(
                call.args.iter().any(|arg| arg == "--non-interactive"),
                "fastly must never wait on a prompt: {:?}",
                call.args
            );
        }
        let create = fake
            .calls()
            .into_iter()
            .find(|call| call.args[..2] == ["secret-store-entry", "create"])
            .expect("should create the secret");
        assert_eq!(create.stdin.as_deref(), Some(secret.as_str()));
    }

    #[test]
    fn rotate_names_a_key_by_date_when_given_no_kid() {
        let fake = Arc::new(FakeFastly::default());

        let report = rotate(&fake, None).expect("should rotate");

        let kid = report["new_kid"].as_str().expect("should name the key");
        assert!(kid.starts_with("ts-"), "{kid}");
        assert!(kid_is_creatable(kid), "{kid}");
    }

    #[test]
    fn rotate_refuses_a_kid_no_platform_can_store_before_writing_anything() {
        let too_long = "a".repeat(129);
        for kid in ["Kid", "_kid", "-kid", "kid with space", "", &too_long] {
            let fake = Arc::new(FakeFastly::default());

            let error = rotate(&fake, Some(kid)).expect_err("should refuse the kid");

            assert!(
                error.contains("must start with a lower case letter"),
                "{error}"
            );
            assert!(fake.calls().is_empty(), "nothing should be asked of fastly");
        }
    }

    #[test]
    fn rotate_refuses_a_kid_that_is_already_active() {
        let fake = Arc::new(FakeFastly::with_config(&[
            ("current-kid", "ts-old"),
            ("active-kids", "ts-old"),
        ]));

        let error = rotate(&fake, Some("ts-old")).expect_err("should refuse a used kid");

        assert!(error.contains("already exists"), "{error}");
        assert_eq!(fake.config("current-kid").as_deref(), Some("ts-old"));
    }

    /// A secret stored under the new key's id with no public key beside it,
    /// as a half finished rotation can leave one, is never replaced. The
    /// command asks only to create, the create is refused, and nothing is
    /// written to the config store.
    #[test]
    fn rotate_never_replaces_a_secret_stored_under_the_new_kid() {
        let fake = two_keys();
        fake.add_secret("ts-orphan");

        rotate(&fake, Some("ts-orphan")).expect_err("should refuse to replace the secret");

        assert_eq!(
            fake.secrets
                .lock()
                .expect("should lock")
                .get("ts-orphan")
                .map(String::as_str),
            Some("private"),
            "the stored secret must be left as it was"
        );
        let writes = fake.writes();
        assert!(
            writes
                .iter()
                .all(|call| call.args[0] == "secret-store-entry"),
            "nothing may be written to the config store: {writes:?}"
        );
        assert!(
            writes
                .iter()
                .flat_map(|call| &call.args)
                .all(|arg| arg != "--recreate" && arg != "--recreate-allow"),
            "the command must never ask fastly to replace a secret: {writes:?}"
        );
        assert_eq!(fake.config("current-kid").as_deref(), Some("ts-new"));
    }

    /// The wording the stores read a missing entry from is what the real CLI
    /// prints, for the config store and for the secret store.
    #[test]
    fn the_recorded_404_of_the_real_cli_reads_as_not_found() {
        for recorded in [RECORDED_CONFIG_404, RECORDED_SECRET_404] {
            assert!(recorded.contains(API_NOT_FOUND), "{recorded}");
        }
    }

    /// The value is read from the fields `fastly config-store-entry describe
    /// --json` prints in 15.1.0 and 16.1.0, here with made up values.
    #[test]
    fn the_value_is_read_from_the_fields_the_real_cli_prints() {
        let answer = r#"{"created_at":"2026-01-01T00:00:00Z","deleted_at":null,"item_key":"current-kid","item_value":"ts-new","store_id":"examplestoreid","updated_at":"2026-01-01T00:00:00Z"}"#;

        assert_eq!(item_value(answer, "current-kid"), Ok("ts-new".to_owned()));
    }

    #[test]
    fn a_key_is_never_named_as_one_of_the_bookkeeping_entries() {
        // On an empty store nothing else would refuse these. The public key
        // would be stored as `current-kid` and then written over by the
        // pointer of the same name.
        for kid in BOOKKEEPING_ENTRIES {
            let fake = Arc::new(FakeFastly::default());

            let error = rotate(&fake, Some(kid)).expect_err("should refuse the kid");

            assert!(error.contains("cannot name a key"), "{error}");
            assert!(fake.calls().is_empty(), "nothing should be asked of fastly");
        }
    }

    #[test]
    fn a_bookkeeping_entry_is_never_retired_as_a_key() {
        // Deleting either would take the service's key list with it.
        for kid in BOOKKEEPING_ENTRIES {
            let fake = two_keys();

            let error = deactivate(&fake, kid, true).expect_err("should refuse the kid");

            assert!(error.contains("cannot name a key"), "{error}");
            assert!(fake.calls().is_empty(), "nothing should be asked of fastly");
            assert!(fake.config(kid).is_some(), "should keep `{kid}`");
        }
    }

    #[test]
    fn deactivate_refuses_a_kid_that_is_not_a_key_id_before_asking_fastly_anything() {
        let too_long = "a".repeat(129);
        for kid in ["", "ts old", "ts-old,ts-new", "ts/old", &too_long] {
            let fake = two_keys();

            let error = deactivate(&fake, kid, true).expect_err("should refuse the kid");

            assert!(error.contains("must hold only"), "{error}");
            assert!(fake.calls().is_empty(), "nothing should be asked of fastly");
        }
    }

    #[test]
    fn a_key_made_under_an_earlier_rule_can_still_be_retired() {
        // Neither id could be given to a new key. Both begin with a
        // character `fastly` would read as a flag if it stood alone.
        for kid in ["-legacy", "9legacy"] {
            let fake = Arc::new(FakeFastly::with_config(&[
                ("current-kid", "ts-new"),
                ("active-kids", &format!("{kid},ts-new")),
                (kid, "{}"),
            ]));
            fake.add_secret(kid);

            deactivate(&fake, kid, true).expect("should retire the key");

            assert_eq!(fake.config("active-kids").as_deref(), Some("ts-new"));
            assert!(fake.config(kid).is_none(), "should delete the public key");
            assert!(!fake.has_secret(kid), "should delete the private key");
        }
    }

    #[test]
    fn every_value_is_joined_to_its_flag() {
        let fake = Arc::new(FakeFastly::with_config(&[
            ("current-kid", "ts-new"),
            ("active-kids", "-legacy,ts-new"),
            ("-legacy", "{}"),
        ]));
        fake.add_secret("-legacy");

        rotate(&fake, Some("ts-newer")).expect("should rotate");
        deactivate(&fake, "-legacy", true).expect("should retire the key");

        for call in fake.calls() {
            for arg in &call.args[2..] {
                assert!(
                    arg.starts_with("--"),
                    "a value standing alone can be read as a flag: {:?}",
                    call.args
                );
            }
        }
    }

    #[test]
    fn deactivate_takes_a_key_out_of_the_active_set_and_keeps_its_material() {
        let fake = two_keys();

        let report = deactivate(&fake, "ts-old", false).expect("should deactivate");

        assert_eq!(
            report["remaining_active_kids"],
            serde_json::json!(["ts-new"])
        );
        assert_eq!(fake.config("active-kids").as_deref(), Some("ts-new"));
        assert!(
            fake.config("ts-old").is_some(),
            "should keep the public key"
        );
        assert!(fake.has_secret("ts-old"), "should keep the private key");
    }

    #[test]
    fn deactivate_with_delete_removes_the_key_from_both_stores() {
        let fake = two_keys();

        deactivate(&fake, "ts-old", true).expect("should delete");

        assert!(
            fake.config("ts-old").is_none(),
            "should delete the public key"
        );
        assert!(!fake.has_secret("ts-old"), "should delete the private key");
    }

    #[test]
    fn deactivate_refuses_the_current_key() {
        let fake = two_keys();

        let error = deactivate(&fake, "ts-new", false).expect_err("should refuse the current key");

        assert!(error.contains("current signing key"), "{error}");
        assert_eq!(fake.config("active-kids").as_deref(), Some("ts-old,ts-new"));
    }

    #[test]
    fn a_delete_tried_again_finishes_when_part_of_the_key_is_already_gone() {
        // The first attempt deleted the private key and failed before the
        // public one.
        let fake = Arc::new(FakeFastly::with_config(&[
            ("current-kid", "ts-new"),
            ("active-kids", "ts-new"),
            ("ts-old", "{}"),
        ]));

        deactivate(&fake, "ts-old", true).expect("should finish the delete");

        assert!(
            fake.config("ts-old").is_none(),
            "should delete the public key"
        );

        deactivate(&fake, "ts-old", true).expect("should have nothing left to delete");
    }

    #[test]
    fn a_missing_store_is_not_taken_for_a_deleted_key() {
        let fake = two_keys();
        let wrong_secret_store = StoreArgs {
            config_store_id: CONFIG_STORE.to_owned(),
            secret_store_id: "another-store-id".to_owned(),
        };

        let error = deactivate_in(&fake, wrong_secret_store, "ts-old", true)
            .expect_err("should report the store that was not found");

        assert!(error.contains("404"), "{error}");
        assert!(fake.has_secret("ts-old"), "should keep the private key");
        assert!(
            fake.config("ts-old").is_some(),
            "should keep the public key while the private key is still stored"
        );
    }

    #[test]
    fn rotate_writes_nothing_when_the_active_keys_cannot_be_read() {
        // Read as an empty list, the rotation would write a list holding the
        // new key alone and drop `ts-old` while its signatures are in flight.
        let fake = Arc::new(FakeFastly::with_config(&[
            ("current-kid", "ts-old"),
            ("active-kids", "ts-old"),
            ("ts-old", "{}"),
        ]));
        fake.fail_next_read_of("active-kids");

        let error = rotate(&fake, Some("ts-new")).expect_err("should not rotate");

        assert!(error.contains("nothing is written"), "{error}");
        assert!(error.contains("network is unreachable"), "{error}");
        assert!(fake.writes().is_empty(), "{:?}", fake.writes());
        assert_eq!(fake.config("active-kids").as_deref(), Some("ts-old"));
    }

    #[test]
    fn the_current_key_is_not_retired_when_the_current_key_cannot_be_read() {
        // Read as no current key, the check that refuses the current key
        // would pass, and the key the service signs with would leave the
        // active set.
        let fake = two_keys();
        fake.fail_next_read_of("current-kid");

        let error = deactivate(&fake, "ts-new", false).expect_err("should not retire the key");

        assert!(error.contains("nothing is written"), "{error}");
        assert!(fake.writes().is_empty(), "{:?}", fake.writes());
        assert_eq!(fake.config("active-kids").as_deref(), Some("ts-old,ts-new"));
    }
}
