//! Reusable-sandbox lifecycle state and limit resolution.
//!
//! Fastly Compute normally starts a fresh Wasm sandbox per request. SDK 0.12.1
//! exposes [`fastly::http::serve::Serve`], which lets one sandbox serve
//! several. This module owns the opt-in decision, the bounds, and the
//! per-sandbox bookkeeping the entry point carries across requests.
//!
//! [`Sandbox`] owns the state the entry point carries across requests: the
//! logger guard, the measurement counters, and the retained applications.

use std::cell::{Cell, RefCell};
use std::convert::Infallible;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use edgezero_core::app::App;
use trusted_server_core::settings::Settings;
#[cfg(feature = "reusable-sandbox")]
use trusted_server_core::settings_data::DEFAULT_CONFIG_STORE_ID;

use crate::app::AppState;

/// Header carrying the guest-instance identifier.
pub(crate) const HEADER_SANDBOX_INSTANCE: &str = "x-ts-sandbox-instance";

/// Header carrying the request ordinal within the current sandbox, 1-based.
pub(crate) const HEADER_SANDBOX_ORDINAL: &str = "x-ts-sandbox-ordinal";

/// Header carrying the number of application builds this sandbox has performed.
pub(crate) const HEADER_SANDBOX_BUILDS: &str = "x-ts-sandbox-builds";

/// Header carrying the per-request correlation id.
pub(crate) const HEADER_SANDBOX_REQUEST_ID: &str = "x-ts-sandbox-request-id";

/// Header carrying cumulative guest vCPU milliseconds, where supported.
pub(crate) const HEADER_SANDBOX_VCPU_MS: &str = "x-ts-sandbox-vcpu-ms";

/// Header carrying the guest heap snapshot in MiB, where supported.
pub(crate) const HEADER_SANDBOX_HEAP_MIB: &str = "x-ts-sandbox-heap-mib";

/// Value reported when a runtime counter is not supported by the host.
///
/// Distinguished from a zero reading: unsupported is not the same as idle.
pub(crate) const COUNTER_UNSUPPORTED: &str = "unsupported";

/// Path of the counters snapshot endpoint.
///
/// Gated on the feature alone, not `any(feature, test)`: its only caller is
/// the feature-gated short-circuit, so a `test` arm would make it dead code
/// in a feature-off test build.
#[cfg(feature = "reusable-sandbox")]
pub(crate) const SANDBOX_METRICS_PATH: &str = "/_ts/debug/sandbox";

/// Label used when the runtime reports no usable guest-instance identifier.
///
/// Recorded rather than papered over: a measurement run that sees this value
/// has no instance identity and cannot claim observed reuse.
pub(crate) const INSTANCE_ID_UNAVAILABLE: &str = "unavailable";

/// Per-sandbox state, owned by `edgezero_adapter_fastly::lifecycle::Sandbox`.
///
/// The framework owns the callback count and the one-time setup guard, and
/// holds the payload this alias names. The payload owns retention.
pub(crate) type Sandbox = edgezero_adapter_fastly::lifecycle::Sandbox<RetainedApps>;

/// How many applications a sandbox keeps.
///
/// Each holds a publisher's settings, registry and routes, and all of them
/// share the sandbox's one memory bound, so the least recently served is
/// dropped past this many.
pub(crate) const MAX_RETAINED_APPS: usize = 8;

/// The application state a retained sandbox carries for one app-config blob.
///
/// Only ever reachable after a successful build: a failed build returns the
/// error router as the build's error instead, so it is served for the
/// current request and dropped rather than retained.
pub(crate) struct RetainedApp {
    pub(crate) app: App,
    pub(crate) state: Arc<AppState>,
}

/// The applications a sandbox has built, each under the key of the app-config
/// blob it was built from.
///
/// The framework hands its payload out by shared reference, so the list sits
/// behind a [`RefCell`].
#[derive(Default)]
pub(crate) struct RetainedApps {
    /// Most recently served first.
    apps: RefCell<Vec<(String, Rc<RetainedApp>)>>,
    build_attempts: Cell<u64>,
}

impl RetainedApps {
    /// The application built from the blob at `config_key`, building it when
    /// the sandbox holds none.
    ///
    /// Retains only success. A failed build hands its error back unchanged,
    /// which serves this request and is dropped, so a transient config-store
    /// failure cannot pin the sandbox into error mode and the next request
    /// for the key builds again.
    ///
    /// # Errors
    ///
    /// Returns the error `build` returned.
    pub(crate) fn get_or_build<E>(
        &self,
        config_key: &str,
        build: impl FnOnce() -> Result<RetainedApp, E>,
    ) -> Result<Rc<RetainedApp>, E> {
        if let Some(app) = self.served(config_key) {
            return Ok(app);
        }

        self.build_attempts
            .set(self.build_attempts.get().saturating_add(1));
        let app = Rc::new(build()?);

        let mut apps = self.apps.borrow_mut();
        apps.insert(0, (config_key.to_owned(), Rc::clone(&app)));
        apps.truncate(MAX_RETAINED_APPS);
        Ok(app)
    }

    /// The retained application for `config_key`, marked as the most recently
    /// served.
    fn served(&self, config_key: &str) -> Option<Rc<RetainedApp>> {
        let mut apps = self.apps.borrow_mut();
        let position = apps.iter().position(|(key, _)| key == config_key)?;
        let entry = apps.remove(position);
        let app = Rc::clone(&entry.1);
        apps.insert(0, entry);
        Some(app)
    }

    /// Builds attempted, failed ones included.
    pub(crate) fn build_attempts(&self) -> u64 {
        self.build_attempts.get()
    }

    /// How many applications are retained.
    #[cfg(test)]
    pub(crate) fn retained(&self) -> usize {
        self.apps.borrow().len()
    }
}

/// Makes the holder of `sandbox`'s applications, once.
pub(crate) fn hold_applications(sandbox: &mut Sandbox) {
    let Ok(()) = sandbox.initialize(|| Ok::<_, Infallible>(RetainedApps::default()));
}

/// Application builds `sandbox` has attempted, failed ones included.
pub(crate) fn build_attempts(sandbox: &Sandbox) -> u64 {
    sandbox.state().map_or(0, RetainedApps::build_attempts)
}

/// Startup diagnostics held until a logger exists.
///
/// Limit resolution runs in `main`, before any logger is installed, so its
/// messages are collected here and flushed by the first callback that
/// installs logging. This is callback-local state rather than sandbox state:
/// `serve_custom` owns the `Sandbox` and exposes no slot for it.
#[derive(Debug, Default)]
pub(crate) struct StartupDiagnostics(Vec<String>);

impl StartupDiagnostics {
    /// Records a message for emission once a logger exists.
    pub(crate) fn push(&mut self, message: impl Into<String>) {
        self.0.push(message.into());
    }

    /// Emits and clears everything held so far.
    pub(crate) fn flush(&mut self) {
        for message in self.0.drain(..) {
            log::info!("{message}");
        }
    }

    /// Whether anything is still waiting to be emitted.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.0.len()
    }
}

/// Snapshots of the sandbox's counters, taken around one callback.
///
/// `serve_custom` owns the [`Sandbox`] and drops it when serving ends, so the
/// retirement line cannot read it afterwards. These are snapshots of the
/// callback count `EdgeZero` keeps and the build count [`RetainedApps`] keeps,
/// taken while the sandbox is still borrowed. Nothing here increments
/// anything.
///
/// The two are read at different points on purpose. The framework increments
/// its callback count *before* invoking the callback, so `requests` is correct
/// on entry. An application is built *during* the callback, so `attempts` must
/// be read on the way out. The count itself is never lost — it lives in the
/// sandbox until serving ends — but a snapshot taken on entry is stale by the
/// time the final callback finishes, so a build it performed goes unreported.
#[cfg(any(feature = "reusable-sandbox", test))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetirementCounters {
    requests: u64,
    attempts: u64,
}

#[cfg(any(feature = "reusable-sandbox", test))]
impl RetirementCounters {
    /// Runs one callback against `sandbox`, capturing counters around it.
    pub(crate) fn observe<R>(
        &mut self,
        sandbox: &mut Sandbox,
        callback: impl FnOnce(&mut Sandbox) -> R,
    ) -> R {
        self.requests = sandbox.requests();
        let outcome = callback(sandbox);
        self.attempts = build_attempts(sandbox);
        outcome
    }

    /// Callback count observed on entry to the last callback.
    pub(crate) fn requests(&self) -> u64 {
        self.requests
    }

    /// Build attempts observed on exit from the last callback.
    pub(crate) fn attempts(&self) -> u64 {
        self.attempts
    }
}

/// Counter values captured for one response.
///
/// An owned snapshot rather than a borrow of [`Sandbox`], so attaching counters
/// never competes with the mutable borrow the request path holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxCounters {
    pub(crate) ordinal: u64,
    pub(crate) builds: u64,
    pub(crate) request_id: String,
}

impl SandboxCounters {
    /// Captures the current counters, or `None` when metrics are disabled.
    ///
    /// Returning `None` is what keeps the counters off every response in the
    /// default configuration, including the correlation id.
    pub(crate) fn capture(
        sandbox: &Sandbox,
        ordinal: u64,
        request_id: &str,
        settings: Option<&Settings>,
    ) -> Option<Self> {
        settings
            .filter(|settings| metrics_enabled(settings))
            .map(|_| Self {
                ordinal,
                builds: build_attempts(sandbox),
                request_id: request_id.to_owned(),
            })
    }
}

/// Whether the counters endpoint and response counters are enabled.
pub(crate) fn metrics_enabled(settings: &Settings) -> bool {
    settings.debug.sandbox_metrics_enabled
}

/// Resolved sandbox limits, already normalized for the SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SandboxLimits {
    pub(crate) max_requests: usize,
    pub(crate) max_memory_mib: u32,
    pub(crate) max_lifetime: Duration,
    pub(crate) timeout: Duration,
}

/// How this sandbox will serve requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServeMode {
    /// Handle exactly one request, as a non-reusable sandbox does today.
    Single,
    /// Enter the SDK serving loop under the given bounds.
    #[cfg_attr(
        not(any(feature = "reusable-sandbox", test)),
        expect(dead_code, reason = "constructed only on the reuse path")
    )]
    Reuse(SandboxLimits),
}

/// Raw limit values as read from the runtime environment.
#[cfg(any(feature = "reusable-sandbox", test))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawLimits {
    pub(crate) max_requests: Option<u64>,
    pub(crate) max_memory_mib: Option<u64>,
    pub(crate) max_lifetime_ms: Option<u64>,
    pub(crate) timeout_ms: Option<u64>,
}

/// Resolves raw configuration into a serving mode.
///
/// Reuse requires all four bounds. A bare request limit is refused because the
/// SDK's omitted lifetime and wait timeout both default to [`Duration::MAX`],
/// which would leave a sandbox waiting without a bound. An application-level
/// request limit of `0` normalizes to `1`, because the SDK reads
/// `with_max_requests(0)` as unlimited — the opposite of what an operator
/// writing `0` intends.
#[cfg(any(feature = "reusable-sandbox", test))]
pub(crate) fn resolve_mode(raw: RawLimits) -> ServeMode {
    let (Some(max_requests), Some(max_lifetime_ms), Some(timeout_ms), Some(max_memory_mib)) = (
        raw.max_requests,
        raw.max_lifetime_ms,
        raw.timeout_ms,
        raw.max_memory_mib,
    ) else {
        return ServeMode::Single;
    };

    let max_requests = max_requests.max(1);
    if max_requests <= 1 || max_lifetime_ms == 0 || timeout_ms == 0 {
        return ServeMode::Single;
    }

    let Ok(max_requests) = usize::try_from(max_requests) else {
        return ServeMode::Single;
    };

    let Ok(max_memory_mib) = u32::try_from(max_memory_mib) else {
        return ServeMode::Single;
    };
    // The SDK uses u32::MAX for unsupported heap snapshots. Keep the bound
    // below that sentinel so an unavailable reading always retires the guest.
    if max_memory_mib == 0 || max_memory_mib == u32::MAX {
        return ServeMode::Single;
    }

    ServeMode::Reuse(SandboxLimits {
        max_requests,
        max_memory_mib,
        max_lifetime: Duration::from_millis(max_lifetime_ms),
        timeout: Duration::from_millis(timeout_ms),
    })
}

/// Keys holding the sandbox bounds in the application's own config store.
#[cfg(any(feature = "reusable-sandbox", test))]
const KEY_MAX_REQUESTS: &str = "TS__SANDBOX__MAX_REQUESTS";
#[cfg(any(feature = "reusable-sandbox", test))]
const KEY_MAX_LIFETIME_MS: &str = "TS__SANDBOX__MAX_LIFETIME_MS";
#[cfg(any(feature = "reusable-sandbox", test))]
const KEY_TIMEOUT_MS: &str = "TS__SANDBOX__TIMEOUT_MS";
#[cfg(any(feature = "reusable-sandbox", test))]
const KEY_MAX_MEMORY_MIB: &str = "TS__SANDBOX__MAX_MEMORY_MIB";

/// Collects raw limits from a fallible key lookup.
///
/// Separated from the config-store call so the failure modes are testable
/// without a store: a lookup error, an absent key, and an unparseable value
/// must all degrade to "absent" rather than propagate.
///
/// Diagnostics are returned rather than logged. This runs before the logger
/// exists, so anything logged here would be discarded.
#[cfg(any(feature = "reusable-sandbox", test))]
fn collect_raw_limits<E, F>(mut lookup: F) -> (RawLimits, Vec<String>)
where
    E: core::fmt::Display,
    F: FnMut(&str) -> Result<Option<String>, E>,
{
    let mut diagnostics = Vec::new();

    let mut read = |key: &str| -> Option<u64> {
        match lookup(key) {
            Ok(Some(raw)) => match raw.trim().parse::<u64>() {
                Ok(value) => Some(value),
                Err(e) => {
                    diagnostics.push(format!(
                        "sandbox limit `{key}` is not a non-negative integer ({e}); ignoring"
                    ));
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                diagnostics.push(format!(
                    "sandbox limit `{key}` lookup failed ({e}); ignoring"
                ));
                None
            }
        }
    };

    let limits = RawLimits {
        max_requests: read(KEY_MAX_REQUESTS),
        max_memory_mib: read(KEY_MAX_MEMORY_MIB),
        max_lifetime_ms: read(KEY_MAX_LIFETIME_MS),
        timeout_ms: read(KEY_TIMEOUT_MS),
    };

    (limits, diagnostics)
}

/// Reads the sandbox bounds from the application's own config store.
///
/// The bounds live beside the app config under the logical store ID that a
/// managed deployment links, so no deprecated selector store is involved.
/// `EdgeZero` removed `edgezero_runtime_env` when store selection moved to
/// deploy-time resource links, and nothing in this repository provisions it.
///
/// These keys are read directly rather than through an `EdgeZero` environment
/// helper. Those resolve a closed allowlist, adapter host and port, logging
/// settings, and per-store `__NAME`/`__KEY` selectors, and silently drop
/// everything else, so a sandbox key routed through one would always read as
/// absent and reuse would never engage.
///
/// Uses [`fastly::ConfigStore::try_get`], never `get`: `get` panics on a
/// lookup error, and this runs in `main` before the health probe, so a panic
/// here would take down liveness rather than merely disabling reuse.
///
/// Any failure resolves to [`RawLimits::default`], and hence to
/// [`ServeMode::Single`]: an unopenable store, a failed lookup, or a value
/// that does not parse as a `u64`.
#[cfg(feature = "reusable-sandbox")]
pub(crate) fn read_raw_limits() -> (RawLimits, Vec<String>) {
    let Ok(store) = fastly::ConfigStore::try_open(DEFAULT_CONFIG_STORE_ID) else {
        return (
            RawLimits::default(),
            vec![format!(
                "sandbox reuse disabled: config store `{DEFAULT_CONFIG_STORE_ID}` unavailable"
            )],
        );
    };

    collect_raw_limits(|key| store.try_get(key))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    fn full(max_requests: u64) -> RawLimits {
        RawLimits {
            max_requests: Some(max_requests),
            max_memory_mib: Some(128),
            max_lifetime_ms: Some(30_000),
            timeout_ms: Some(500),
        }
    }

    #[test]
    fn absent_configuration_stays_single_request() {
        assert_eq!(
            resolve_mode(RawLimits::default()),
            ServeMode::Single,
            "an unconfigured sandbox should behave as it does today"
        );
    }

    #[test]
    fn zero_request_limit_normalizes_to_single_not_unlimited() {
        assert_eq!(
            resolve_mode(full(0)),
            ServeMode::Single,
            "0 should mean one request, never the SDK's unlimited"
        );
    }

    #[test]
    fn one_request_limit_stays_single_request() {
        assert_eq!(
            resolve_mode(full(1)),
            ServeMode::Single,
            "a limit of 1 should not enter the serving loop"
        );
    }

    #[test]
    fn partial_configuration_refuses_to_reuse() {
        let raw = RawLimits {
            max_requests: Some(10),
            max_memory_mib: Some(128),
            max_lifetime_ms: None,
            timeout_ms: Some(500),
        };

        assert_eq!(
            resolve_mode(raw),
            ServeMode::Single,
            "a missing bound should not fall back to the SDK's unbounded default"
        );
    }

    #[test]
    fn zero_lifetime_or_timeout_refuses_to_reuse() {
        assert_eq!(
            resolve_mode(RawLimits {
                max_lifetime_ms: Some(0),
                ..full(10)
            }),
            ServeMode::Single,
            "a zero lifetime should not mean unlimited"
        );
        assert_eq!(
            resolve_mode(RawLimits {
                timeout_ms: Some(0),
                ..full(10)
            }),
            ServeMode::Single,
            "a zero wait timeout should not mean unlimited"
        );
    }

    #[test]
    fn complete_configuration_enables_reuse() {
        assert_eq!(
            resolve_mode(full(10)),
            ServeMode::Reuse(SandboxLimits {
                max_requests: 10,
                max_memory_mib: 128,
                max_lifetime: Duration::from_millis(30_000),
                timeout: Duration::from_millis(500),
            }),
            "all four bounds present should enter the serving loop"
        );
    }

    #[test]
    fn setup_runs_once_on_success_and_retries_after_failure() {
        let mut sandbox = Sandbox::default();
        let attempts = Cell::new(0_u32);

        // A failed install must leave setup eligible for retry.
        let failed = sandbox.setup_once(|| {
            attempts.set(attempts.get() + 1);
            Err::<(), &str>("install failed")
        });
        assert_eq!(
            failed.err(),
            Some("install failed"),
            "the error should surface"
        );

        // The retry runs, because setup was never marked complete.
        sandbox
            .setup_once(|| {
                attempts.set(attempts.get() + 1);
                Ok::<(), &str>(())
            })
            .expect("the retry should succeed");
        assert_eq!(attempts.get(), 2, "a failed install should be retried");

        // Once successful, it never runs again. Reinstalling the global logger
        // panics inside `fern`, which is the failure this guards.
        sandbox
            .setup_once(|| -> Result<(), &str> {
                attempts.set(attempts.get() + 1);
                panic!("setup must not run again after success")
            })
            .expect("a completed setup should be skipped");
        assert_eq!(attempts.get(), 2, "setup should stay complete");
    }

    /// Minimal settings sufficient to build real application state.
    fn test_settings() -> Settings {
        Settings::from_toml(
            r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.example"

            [publisher]
            domain = "test-publisher.example"
            cookie_domain = ".test-publisher.example"
            proxy_secret = "unit-test-proxy-secret"

            [geo]
            assume_single_jurisdiction = true

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should parse sandbox test settings")
        // A loaded domain carries its policy.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
    }

    /// A real `AppState`, so retention is exercised against the type the
    /// entry point actually keeps rather than a stand-in.
    fn test_state() -> Arc<AppState> {
        crate::app::build_state_from_settings(test_settings())
            .expect("should build sandbox test state")
    }

    /// A real `RetainedApp`, so retention is exercised against the type the
    /// entry point actually keeps rather than a stand-in.
    fn test_retained() -> RetainedApp {
        RetainedApp {
            app: test_app(),
            state: test_state(),
        }
    }

    /// An empty but real `App`.
    fn test_app() -> App {
        App::with_name(
            edgezero_core::router::RouterService::builder().build(),
            "sandbox-test",
        )
    }

    /// Stand-in for `fastly::config_store::LookupError`, which cannot be
    /// constructed outside the SDK.
    #[derive(Debug, derive_more::Display)]
    #[display("simulated lookup failure")]
    struct LookupFailed;

    #[test]
    fn a_failed_lookup_degrades_to_single_request_instead_of_panicking() {
        // `ConfigStore::get` panics on a lookup error and runs before the
        // health probe, so the fallible path must absorb the error.
        let (limits, diagnostics) = collect_raw_limits::<LookupFailed, _>(|_key| Err(LookupFailed));

        assert_eq!(
            limits,
            RawLimits::default(),
            "a failed lookup should read as absent"
        );
        assert_eq!(
            resolve_mode(limits),
            ServeMode::Single,
            "a failed lookup should leave the sandbox single-request"
        );
        assert_eq!(
            diagnostics.len(),
            4,
            "each failed key should report why reuse was declined"
        );
        assert!(
            diagnostics[0].contains("lookup failed"),
            "the diagnostic should name the failure, got: {}",
            diagnostics[0]
        );
    }

    #[test]
    fn an_unparseable_value_degrades_to_single_request() {
        let (limits, diagnostics) = collect_raw_limits::<LookupFailed, _>(|key| {
            Ok(Some(if key.ends_with(KEY_MAX_REQUESTS) {
                "ten".to_owned()
            } else {
                "500".to_owned()
            }))
        });

        assert_eq!(
            limits.max_requests, None,
            "an unparseable limit should read as absent"
        );
        assert_eq!(
            resolve_mode(limits),
            ServeMode::Single,
            "an unparseable request limit should not enable reuse"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.contains("not a non-negative integer")),
            "should explain the rejected value, got: {diagnostics:?}"
        );
    }

    #[test]
    fn positive_memory_limits_are_preserved_without_truncation() {
        for memory in [1, 128, u32::MAX - 1] {
            let mode = resolve_mode(RawLimits {
                max_memory_mib: Some(u64::from(memory)),
                ..full(10)
            });
            assert!(
                matches!(mode, ServeMode::Reuse(limits) if limits.max_memory_mib == memory),
                "should pass a valid memory bound unchanged to the SDK"
            );
        }
    }

    #[test]
    fn missing_or_invalid_memory_limit_refuses_reuse() {
        for memory in [
            None,
            Some("0"),
            Some("-1"),
            Some("many"),
            Some("4294967295"),
            Some("4294967296"),
        ] {
            let (limits, _) = collect_raw_limits::<LookupFailed, _>(|key| {
                Ok(if key.ends_with("TS__SANDBOX__MAX_MEMORY_MIB") {
                    memory.map(str::to_owned)
                } else {
                    Some("10".to_owned())
                })
            });

            assert_eq!(
                resolve_mode(limits),
                ServeMode::Single,
                "should refuse reuse with missing or invalid memory limit {memory:?}"
            );
        }
    }

    #[test]
    fn limits_are_looked_up_unscoped_in_the_app_config_store() {
        // The bounds live in the application's own config store under the
        // logical id a deployment links, so every lookup must use the bare
        // key. Asserting on the constants alone would only fail if someone
        // edited them and this test together, and the sibling tests match on
        // `ends_with`, so a service-scoped or otherwise prefixed lookup would
        // slip past them. Capture the exact keys requested instead.
        let requested = RefCell::new(Vec::new());

        let (_limits, _diagnostics) = collect_raw_limits::<LookupFailed, _>(|key| {
            requested.borrow_mut().push(key.to_owned());
            Ok(None)
        });

        assert_eq!(
            requested.borrow().as_slice(),
            [
                KEY_MAX_REQUESTS,
                KEY_MAX_MEMORY_MIB,
                KEY_MAX_LIFETIME_MS,
                KEY_TIMEOUT_MS,
            ],
            "should look up each sandbox bound by its unscoped app-config key"
        );
    }

    #[test]
    fn absent_keys_report_nothing_and_stay_single_request() {
        let (limits, diagnostics) = collect_raw_limits::<LookupFailed, _>(|_key| Ok(None));

        assert_eq!(
            limits,
            RawLimits::default(),
            "absent keys should read absent"
        );
        assert!(
            diagnostics.is_empty(),
            "an unconfigured sandbox is the normal case, not a diagnostic"
        );
    }

    #[test]
    fn a_complete_store_enables_reuse_end_to_end() {
        let (limits, diagnostics) = collect_raw_limits::<LookupFailed, _>(|key| {
            Ok(Some(
                if key.ends_with(KEY_MAX_REQUESTS) {
                    "10"
                } else if key.ends_with(KEY_MAX_MEMORY_MIB) {
                    "128"
                } else if key.ends_with(KEY_MAX_LIFETIME_MS) {
                    "30000"
                } else {
                    "500"
                }
                .to_owned(),
            ))
        });

        assert!(diagnostics.is_empty(), "a valid store should be quiet");
        assert_eq!(
            resolve_mode(limits),
            ServeMode::Reuse(SandboxLimits {
                max_requests: 10,
                max_memory_mib: 128,
                max_lifetime: Duration::from_millis(30_000),
                timeout: Duration::from_millis(500),
            }),
            "a fully configured store should enter the serving loop"
        );
    }

    #[test]
    fn deferred_diagnostics_are_held_until_flushed() {
        let mut startup = StartupDiagnostics::default();
        startup.push("reuse declined");

        assert_eq!(
            startup.pending(),
            1,
            "startup runs before the logger, so the message must be held"
        );

        startup.flush();

        assert_eq!(
            startup.pending(),
            0,
            "flushing should emit and clear everything held"
        );
    }

    /// A sandbox holding its applications, as the entry point makes it.
    fn holding_sandbox() -> Sandbox {
        let mut sandbox = Sandbox::default();
        hold_applications(&mut sandbox);
        sandbox
    }

    /// Hands back the application for `key`, which must already be retained.
    fn retained_for(apps: &RetainedApps, key: &str) -> Rc<RetainedApp> {
        apps.get_or_build(key, || -> Result<RetainedApp, &str> {
            panic!("`{key}` is retained and must not be rebuilt")
        })
        .expect("a retained application should be reused")
    }

    /// Builds and retains an application for `key`.
    fn build_for(apps: &RetainedApps, key: &str) -> Rc<RetainedApp> {
        apps.get_or_build(key, || Ok::<_, &str>(test_retained()))
            .expect("the build should succeed")
    }

    #[test]
    fn a_sandbox_starts_with_no_retained_application() {
        let sandbox = Sandbox::default();

        assert!(
            sandbox.state().is_none(),
            "construction must be lazy so the health probe never pays for it"
        );
        assert_eq!(
            build_attempts(&sandbox),
            0,
            "a sandbox that has served nothing should report no build attempts"
        );
    }

    #[test]
    fn holding_applications_builds_none() {
        let mut sandbox = holding_sandbox();

        assert_eq!(
            sandbox
                .state()
                .expect("should hold its applications")
                .retained(),
            0,
            "should hold no application before one is asked for"
        );
        assert_eq!(
            build_attempts(&sandbox),
            0,
            "making the holder should not count as a build"
        );

        // Asking again must keep what is held, or every request would start
        // from an empty holder and nothing would ever be reused.
        build_for(
            sandbox.state().expect("should hold its applications"),
            "trusted_server_config",
        );
        hold_applications(&mut sandbox);
        assert_eq!(
            sandbox
                .state()
                .expect("should hold its applications")
                .retained(),
            1,
            "holding again should keep the applications already built"
        );
    }

    #[test]
    fn a_retained_application_is_reused_without_rebuilding() {
        let apps = RetainedApps::default();
        let builds = Cell::new(0_u32);

        let first = apps
            .get_or_build("trusted_server_config", || {
                builds.set(builds.get() + 1);
                Ok::<_, &str>(test_retained())
            })
            .expect("the first build should succeed");
        assert_eq!(builds.get(), 1, "the first request should build");

        for attempt in 2..=4_u32 {
            let again = apps
                .get_or_build("trusted_server_config", || -> Result<RetainedApp, &str> {
                    builds.set(builds.get() + 1);
                    panic!("request {attempt} must not rebuild a retained application")
                })
                .expect("a retained application should be reused");
            assert!(
                Rc::ptr_eq(&first, &again),
                "request {attempt} should be handed the application built first"
            );
        }

        assert_eq!(
            builds.get(),
            1,
            "four requests should cost exactly one build; that is the whole point"
        );
        assert_eq!(apps.build_attempts(), 1, "the build counter should agree");
    }

    #[test]
    fn a_failed_build_is_retried_and_a_later_success_is_retained() {
        let apps = RetainedApps::default();
        let builds = Cell::new(0_u32);

        // Request 1: the build fails. The error payload stands in for the
        // error router `build_app_with_state` returns when state is `None`.
        let failed = apps.get_or_build("trusted_server_config", || {
            builds.set(builds.get() + 1);
            Err::<RetainedApp, &str>("error router")
        });
        assert_eq!(
            failed.err(),
            Some("error router"),
            "a failed build must be handed back for this request only"
        );
        assert_eq!(apps.retained(), 0, "a failed build must not be retained");

        // Request 2: the retry succeeds. Only reachable because the failure
        // was not retained.
        apps.get_or_build("trusted_server_config", || {
            builds.set(builds.get() + 1);
            Ok::<_, &str>(test_retained())
        })
        .expect("the retry should succeed");
        assert_eq!(
            apps.retained(),
            1,
            "the recovered application should be retained"
        );

        // Request 3: recovery is durable, so no further build.
        retained_for(&apps, "trusted_server_config");

        assert_eq!(
            builds.get(),
            2,
            "one failed build plus one successful build, then reuse"
        );
        assert_eq!(
            apps.build_attempts(),
            2,
            "both attempts should be counted so a retry loop stays visible"
        );
    }

    #[test]
    fn an_application_answers_only_for_the_key_it_was_built_from() {
        let apps = RetainedApps::default();

        let first = build_for(&apps, "a.example");
        let second = build_for(&apps, "b.example");

        assert!(
            !Rc::ptr_eq(&first, &second),
            "a second publisher must be built an application of its own"
        );
        assert_eq!(apps.build_attempts(), 2, "two blobs should cost two builds");

        for (key, built) in [
            ("b.example", &second),
            ("a.example", &first),
            ("a.example", &first),
            ("b.example", &second),
        ] {
            assert!(
                Rc::ptr_eq(&retained_for(&apps, key), built),
                "`{key}` must be answered by the application built from its own blob"
            );
        }
        assert_eq!(
            apps.build_attempts(),
            2,
            "serving both in any order should build nothing more"
        );
    }

    #[test]
    fn a_failed_build_for_one_key_leaves_another_s_application_retained() {
        let apps = RetainedApps::default();
        let first = build_for(&apps, "a.example");

        let failed = apps.get_or_build("b.example", || Err::<RetainedApp, &str>("error router"));
        assert_eq!(
            failed.err(),
            Some("error router"),
            "the second publisher's failed build should be handed back"
        );

        assert!(
            Rc::ptr_eq(&retained_for(&apps, "a.example"), &first),
            "the first publisher's application should still be retained"
        );
        assert_eq!(
            apps.retained(),
            1,
            "the failed build should not have been retained under either key"
        );
    }

    #[test]
    fn the_least_recently_served_application_is_dropped_past_the_bound() {
        let apps = RetainedApps::default();
        let key = |index: usize| format!("{index}.example");

        for index in 0..MAX_RETAINED_APPS {
            build_for(&apps, &key(index));
        }
        assert_eq!(
            apps.retained(),
            MAX_RETAINED_APPS,
            "should keep every application up to the bound"
        );

        // Serving the oldest makes the second oldest the one to drop.
        retained_for(&apps, &key(0));
        build_for(&apps, "one-more.example");

        assert_eq!(
            apps.retained(),
            MAX_RETAINED_APPS,
            "should never keep more than the bound"
        );
        retained_for(&apps, &key(0));
        retained_for(&apps, "one-more.example");
        for index in 2..MAX_RETAINED_APPS {
            retained_for(&apps, &key(index));
        }

        let rebuilt = Cell::new(false);
        apps.get_or_build(&key(1), || {
            rebuilt.set(true);
            Ok::<_, &str>(test_retained())
        })
        .expect("the dropped application should build again");
        assert!(
            rebuilt.get(),
            "the least recently served application should have been dropped"
        );
    }

    #[test]
    fn captured_counters_report_the_builds_the_sandbox_attempted() {
        let sandbox = holding_sandbox();
        let mut settings = Settings::default();
        settings.debug.sandbox_metrics_enabled = true;

        let apps = sandbox.state().expect("should hold its applications");
        build_for(apps, "a.example");
        build_for(apps, "b.example");
        retained_for(apps, "a.example");

        assert_eq!(
            SandboxCounters::capture(&sandbox, 3, "req-3", Some(&settings)),
            Some(SandboxCounters {
                ordinal: 3,
                builds: 2,
                request_id: "req-3".to_owned(),
            }),
            "should report a build for each blob and none for a reuse"
        );
    }

    /// Drives the real reporting path: the same `observe` that `serve_loop`
    /// wraps every callback in. A build performed by the FINAL callback must
    /// appear in the retirement snapshot; reading the attempt count on entry
    /// instead of on exit silently loses it.
    #[test]
    fn retirement_counters_include_a_build_from_the_final_callback() {
        let mut sandbox = Sandbox::default();
        let mut counters = RetirementCounters::default();

        // Callback 1 builds nothing, so nothing is attempted yet.
        counters.observe(&mut sandbox, |_sandbox| {});
        assert_eq!(
            counters.attempts(),
            0,
            "a callback that never builds should report no attempts"
        );

        // Callback 2 succeeds. The build happens DURING this callback, so it
        // only shows up if the count is read on the way out.
        counters.observe(&mut sandbox, |sandbox| {
            hold_applications(sandbox);
            build_for(
                sandbox.state().expect("should hold its applications"),
                "trusted_server_config",
            );
        });
        assert_eq!(
            counters.attempts(),
            1,
            "a successful build on the final callback must be reported"
        );
    }

    #[test]
    fn retirement_counters_include_a_failed_build_from_the_final_callback() {
        let mut sandbox = Sandbox::default();
        let mut counters = RetirementCounters::default();

        // The application is not retained, but the attempt itself stays in
        // the holder's count until the sandbox is dropped. What a stale
        // snapshot loses is the report, not the count.
        counters.observe(&mut sandbox, |sandbox| {
            hold_applications(sandbox);
            let failed = sandbox
                .state()
                .expect("should hold its applications")
                .get_or_build("trusted_server_config", || {
                    Err::<RetainedApp, &str>("error router")
                });
            assert_eq!(
                failed.err(),
                Some("error router"),
                "the failed build should be handed back"
            );
        });

        assert_eq!(
            counters.attempts(),
            1,
            "a failed build on the final callback must still be reported"
        );
        assert_eq!(
            sandbox
                .state()
                .expect("should hold its applications")
                .retained(),
            0,
            "a failed build must not be retained"
        );
    }

    #[test]
    fn retirement_counters_report_the_ordinal_observed_on_entry() {
        let mut sandbox = Sandbox::default();
        let mut counters = RetirementCounters::default();

        // This covers snapshot copying only: that `observe` reports whatever
        // the framework counter says rather than deriving a value of its own.
        // That EdgeZero increments before invoking the callback is the
        // framework's behaviour, covered by its tests and by runtime
        // observation, not by this test — `requests()` advances only through
        // `serve_custom` / `run_custom`, so a direct `observe` sees zero.
        counters.observe(&mut sandbox, |sandbox| {
            assert_eq!(
                sandbox.requests(),
                0,
                "no callback has been dispatched through the framework here"
            );
        });

        assert_eq!(
            counters.requests(),
            0,
            "the snapshot should mirror the framework counter, not a local one"
        );
    }

    #[test]
    fn counters_are_captured_only_when_metrics_are_enabled() {
        let sandbox = Sandbox::default();
        let mut settings = Settings::default();

        assert_eq!(
            SandboxCounters::capture(&sandbox, 1, "req-1", None),
            None,
            "no settings should mean no counters"
        );

        settings.debug.sandbox_metrics_enabled = false;
        assert_eq!(
            SandboxCounters::capture(&sandbox, 1, "req-1", Some(&settings)),
            None,
            "counters must stay off in the default configuration"
        );

        settings.debug.sandbox_metrics_enabled = true;
        assert_eq!(
            SandboxCounters::capture(&sandbox, 7, "req-7", Some(&settings)),
            Some(SandboxCounters {
                ordinal: 7,
                builds: 0,
                request_id: "req-7".to_owned(),
            }),
            "enabling the flag should capture the current counters"
        );
    }

    // Request ordinals are counted by
    // `edgezero_adapter_fastly::lifecycle::Sandbox` itself, incremented in its
    // private per-callback hook. They are exercised through `serve_custom` /
    // `run_custom` at runtime and covered by the framework's own tests, so
    // there is nothing left here to unit test.
}
