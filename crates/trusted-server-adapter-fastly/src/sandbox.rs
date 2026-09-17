//! Reusable-sandbox lifecycle state and limit resolution.
//!
//! Fastly Compute normally starts a fresh Wasm sandbox per request. SDK 0.12.1
//! exposes [`fastly::http::serve::Serve`], which lets one sandbox serve
//! several. This module owns the opt-in decision, the bounds, and the
//! per-sandbox bookkeeping the entry point carries across requests.
//!
//! [`Sandbox`] owns the state the entry point carries across requests: the
//! logger guard, the measurement counters, and the retained application.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::app::App;
use trusted_server_core::settings::Settings;

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
#[cfg(any(feature = "reusable-sandbox", test))]
pub(crate) const SANDBOX_METRICS_PATH: &str = "/_ts/debug/sandbox";

/// Label used when the runtime reports no usable guest-instance identifier.
///
/// Recorded rather than papered over: a measurement run that sees this value
/// has no instance identity and cannot claim observed reuse.
pub(crate) const INSTANCE_ID_UNAVAILABLE: &str = "unavailable";

/// Per-sandbox state, owned by `edgezero_adapter_fastly::lifecycle::Sandbox`.
///
/// The framework owns lazy successful-only retention, the callback count, the
/// initialization-attempt count, and the one-time setup guard. This alias
/// names the application-specific payload it retains.
pub(crate) type Sandbox = edgezero_adapter_fastly::lifecycle::Sandbox<RetainedApp>;

/// The application state a retained sandbox carries.
///
/// Only ever reachable after a successful build: a failed build returns the
/// error router as the `initialize` error instead, so it is served for the
/// current request and dropped rather than retained.
pub(crate) struct RetainedApp {
    pub(crate) app: App,
    pub(crate) state: Arc<AppState>,
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

/// Snapshots of the framework's counters, taken around one callback.
///
/// `serve_custom` owns the [`Sandbox`] and drops it when serving ends, so the
/// retirement line cannot read it afterwards. These are snapshots of
/// `EdgeZero`'s counters taken while the sandbox is still borrowed; nothing here
/// increments anything.
///
/// The two are read at different points on purpose. The framework increments
/// its callback count *before* invoking the callback, so `requests` is correct
/// on entry. Initialization happens *during* the callback, so `attempts` must
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
        self.attempts = sandbox.initialization_attempts();
        outcome
    }

    /// Callback count observed on entry to the last callback.
    pub(crate) fn requests(&self) -> u64 {
        self.requests
    }

    /// Initialization attempts observed on exit from the last callback.
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
                builds: sandbox.initialization_attempts(),
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
    pub(crate) max_lifetime_ms: Option<u64>,
    pub(crate) timeout_ms: Option<u64>,
}

/// Resolves raw configuration into a serving mode.
///
/// Reuse requires all three bounds. A bare request limit is refused because the
/// SDK's omitted lifetime and wait timeout both default to [`Duration::MAX`],
/// which would leave a sandbox waiting without a bound. An application-level
/// request limit of `0` normalizes to `1`, because the SDK reads
/// `with_max_requests(0)` as unlimited — the opposite of what an operator
/// writing `0` intends.
#[cfg(any(feature = "reusable-sandbox", test))]
pub(crate) fn resolve_mode(raw: RawLimits) -> ServeMode {
    let (Some(max_requests), Some(max_lifetime_ms), Some(timeout_ms)) =
        (raw.max_requests, raw.max_lifetime_ms, raw.timeout_ms)
    else {
        return ServeMode::Single;
    };

    let max_requests = max_requests.max(1);
    if max_requests <= 1 || max_lifetime_ms == 0 || timeout_ms == 0 {
        return ServeMode::Single;
    }

    let Ok(max_requests) = usize::try_from(max_requests) else {
        return ServeMode::Single;
    };

    ServeMode::Reuse(SandboxLimits {
        max_requests,
        max_lifetime: Duration::from_millis(max_lifetime_ms),
        timeout: Duration::from_millis(timeout_ms),
    })
}

/// Suffixes of the service-scoped runtime-environment keys holding the bounds.
#[cfg(any(feature = "reusable-sandbox", test))]
const KEY_MAX_REQUESTS: &str = "TS__SANDBOX__MAX_REQUESTS";
#[cfg(any(feature = "reusable-sandbox", test))]
const KEY_MAX_LIFETIME_MS: &str = "TS__SANDBOX__MAX_LIFETIME_MS";
#[cfg(any(feature = "reusable-sandbox", test))]
const KEY_TIMEOUT_MS: &str = "TS__SANDBOX__TIMEOUT_MS";

/// Builds the service-scoped runtime-environment key for a bound.
///
/// `EdgeZero`'s own `service_scoped_runtime_env_key` is private, so the shape is
/// reproduced here. It must stay identical to the one `edgezero provision`
/// writes.
///
/// Follow-up: a public `EdgeZero` key-construction or lookup helper would let
/// this duplication go. Until such an API exists this implementation stays, so
/// the key shape has exactly one definition on our side. The `TS__SANDBOX__*`
/// suffixes and the limit-validation policy in [`resolve_mode`] are
/// application-owned either way and would not move.
#[cfg(any(feature = "reusable-sandbox", test))]
fn scoped_key(service_id: &str, suffix: &str) -> String {
    format!("EDGEZERO__SERVICES__{service_id}__{suffix}")
}

/// Collects raw limits from a fallible key lookup.
///
/// Separated from the config-store call so the failure modes are testable
/// without a store: a lookup error, an absent key, and an unparseable value
/// must all degrade to "absent" rather than propagate.
///
/// Diagnostics are returned rather than logged. This runs before the logger
/// exists, so anything logged here would be discarded.
#[cfg(any(feature = "reusable-sandbox", test))]
fn collect_raw_limits<E, F>(service_id: &str, mut lookup: F) -> (RawLimits, Vec<String>)
where
    E: core::fmt::Display,
    F: FnMut(&str) -> Result<Option<String>, E>,
{
    let mut diagnostics = Vec::new();

    let mut read = |suffix: &str| -> Option<u64> {
        let key = scoped_key(service_id, suffix);
        match lookup(&key) {
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
        max_lifetime_ms: read(KEY_MAX_LIFETIME_MS),
        timeout_ms: read(KEY_TIMEOUT_MS),
    };

    (limits, diagnostics)
}

/// Reads the sandbox bounds from the runtime-environment config store.
///
/// These keys deliberately bypass `edgezero_adapter_fastly::runtime_env_config`.
/// That helper resolves a closed allowlist — adapter host and port, logging
/// settings, and per-store `__NAME`/`__KEY` selectors — and silently drops
/// everything else, so a sandbox key routed through it would always read as
/// absent and reuse would never engage. Still true at the pinned revision.
///
/// Uses [`fastly::ConfigStore::try_get`], never `get`: `get` panics on a
/// lookup error, and this runs in `main` before the health probe, so a panic
/// here would take down liveness rather than merely disabling reuse.
///
/// Any failure resolves to [`RawLimits::default`], and hence to
/// [`ServeMode::Single`]: an absent or unopenable store, an empty service id,
/// a failed lookup, or a value that does not parse as a `u64`.
#[cfg(feature = "reusable-sandbox")]
pub(crate) fn read_raw_limits() -> (RawLimits, Vec<String>) {
    use edgezero_adapter_fastly::RUNTIME_ENV_STORE_NAME;

    let Ok(store) = fastly::ConfigStore::try_open(RUNTIME_ENV_STORE_NAME) else {
        return (
            RawLimits::default(),
            vec![format!(
                "sandbox reuse disabled: config store `{RUNTIME_ENV_STORE_NAME}` unavailable"
            )],
        );
    };

    // Viceroy reports a service id of twenty-two zeros, which is a valid
    // scope for key construction. Only an empty id is unusable.
    let service_id = fastly::compute_runtime::service_id();
    if service_id.is_empty() {
        return (
            RawLimits::default(),
            vec!["sandbox reuse disabled: no service id available for key scoping".to_owned()],
        );
    }

    collect_raw_limits(service_id, |key| store.try_get(key))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn scoped_key_matches_the_edgezero_shape() {
        // Viceroy reports twenty-two zeros locally; it is a valid scope.
        let service_id = "0000000000000000000000";

        assert_eq!(
            scoped_key(service_id, KEY_MAX_REQUESTS),
            "EDGEZERO__SERVICES__0000000000000000000000__TS__SANDBOX__MAX_REQUESTS",
            "should reproduce the service-scoped key edgezero writes"
        );
        assert_eq!(
            scoped_key(service_id, KEY_MAX_LIFETIME_MS),
            "EDGEZERO__SERVICES__0000000000000000000000__TS__SANDBOX__MAX_LIFETIME_MS",
            "should scope the lifetime bound the same way"
        );
        assert_eq!(
            scoped_key(service_id, KEY_TIMEOUT_MS),
            "EDGEZERO__SERVICES__0000000000000000000000__TS__SANDBOX__TIMEOUT_MS",
            "should scope the wait timeout the same way"
        );
    }

    fn full(max_requests: u64) -> RawLimits {
        RawLimits {
            max_requests: Some(max_requests),
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
                max_lifetime: Duration::from_millis(30_000),
                timeout: Duration::from_millis(500),
            }),
            "all three bounds present should enter the serving loop"
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
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.example"
            cookie_domain = ".test-publisher.example"
            origin_url = "https://origin.test-publisher.example"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should parse sandbox test settings")
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
        let (limits, diagnostics) =
            collect_raw_limits::<LookupFailed, _>("svc", |_key| Err(LookupFailed));

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
            3,
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
        let (limits, diagnostics) = collect_raw_limits::<LookupFailed, _>("svc", |key| {
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
    fn absent_keys_report_nothing_and_stay_single_request() {
        let (limits, diagnostics) = collect_raw_limits::<LookupFailed, _>("svc", |_key| Ok(None));

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
        let (limits, diagnostics) = collect_raw_limits::<LookupFailed, _>("svc", |key| {
            Ok(Some(
                if key.ends_with(KEY_MAX_REQUESTS) {
                    "10"
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

    #[test]
    fn a_sandbox_starts_with_no_retained_application() {
        let sandbox = Sandbox::default();

        assert!(
            sandbox.state().is_none(),
            "construction must be lazy so the health probe never pays for it"
        );
        assert_eq!(
            sandbox.initialization_attempts(),
            0,
            "a sandbox that has served nothing should report no build attempts"
        );
    }

    #[test]
    fn a_retained_application_is_reused_without_rebuilding() {
        let mut sandbox = Sandbox::default();
        let builds = Cell::new(0_u32);

        sandbox
            .initialize(|| {
                builds.set(builds.get() + 1);
                Ok::<_, &str>(test_retained())
            })
            .expect("the first build should succeed");
        assert_eq!(builds.get(), 1, "the first request should build");

        for attempt in 2..=4_u32 {
            sandbox
                .initialize(|| -> Result<RetainedApp, &str> {
                    builds.set(builds.get() + 1);
                    panic!("request {attempt} must not rebuild a retained application")
                })
                .expect("a retained application should be reused");
        }

        assert_eq!(
            builds.get(),
            1,
            "four requests should cost exactly one build; that is the whole point"
        );
        assert_eq!(
            sandbox.initialization_attempts(),
            1,
            "the framework counter should agree"
        );
    }

    #[test]
    fn a_failed_build_is_retried_and_a_later_success_is_retained() {
        let mut sandbox = Sandbox::default();
        let builds = Cell::new(0_u32);

        // Request 1: the build fails. The error payload stands in for the
        // error router `build_app_with_state` returns when state is `None`.
        let failed = sandbox.initialize(|| {
            builds.set(builds.get() + 1);
            Err::<RetainedApp, &str>("error router")
        });
        assert_eq!(
            failed.err(),
            Some("error router"),
            "a failed build must be handed back for this request only"
        );
        assert!(
            sandbox.state().is_none(),
            "a failed build must not be retained"
        );

        // Request 2: the retry succeeds. Only reachable because the failure
        // was not retained.
        sandbox
            .initialize(|| {
                builds.set(builds.get() + 1);
                Ok::<_, &str>(test_retained())
            })
            .expect("the retry should succeed");
        assert!(
            sandbox.state().is_some(),
            "the recovered application should be retained"
        );

        // Request 3: recovery is durable — no further build.
        sandbox
            .initialize(|| -> Result<RetainedApp, &str> {
                builds.set(builds.get() + 1);
                panic!("a recovered application must not be rebuilt")
            })
            .expect("request 3 should reuse the recovered app");

        assert_eq!(
            builds.get(),
            2,
            "one failed build plus one successful build, then reuse"
        );
        assert_eq!(
            sandbox.initialization_attempts(),
            2,
            "both attempts should be counted so a retry loop stays visible"
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
            "a callback that never initializes should report no attempts"
        );

        // Callback 2 succeeds. The build happens DURING this callback, so it
        // only shows up if the count is read on the way out.
        counters.observe(&mut sandbox, |sandbox| {
            sandbox
                .initialize(|| Ok::<_, &str>(test_retained()))
                .expect("the build should succeed");
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

        // The application state is not retained, but the attempt itself stays
        // in `initialization_attempts()` until the sandbox is dropped. What a
        // stale snapshot loses is the report, not the count.
        counters.observe(&mut sandbox, |sandbox| {
            let failed = sandbox.initialize(|| Err::<RetainedApp, &str>("error router"));
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
        assert!(
            sandbox.state().is_none(),
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

    // Request ordinals and build attempts are counted by
    // `edgezero_adapter_fastly::lifecycle::Sandbox` itself, incremented in its
    // private per-callback hook. They are exercised through `serve_custom` /
    // `run_custom` at runtime and covered by the framework's own tests, so
    // there is nothing left here to unit test.
}
