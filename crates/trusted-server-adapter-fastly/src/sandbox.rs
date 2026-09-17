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

/// Per-sandbox state carried across requests by the entry point.
///
/// Holds the logger guard, the measurement counters, and the retained
/// application. Never request identity and never native handles: those stay
/// request-scoped.
#[derive(Default)]
pub(crate) struct Sandbox {
    logger_installed: bool,
    requests: u64,
    builds: u64,
    pending_diagnostics: Vec<String>,
    retained: Option<RetainedApp>,
}

/// A successfully built application, kept for the life of the sandbox.
///
/// Only ever holds a build that produced state. A failed build yields an
/// error router with no state, which serves its own request and is dropped;
/// retaining it would pin the sandbox into permanent error mode.
pub(crate) struct RetainedApp {
    pub(crate) app: App,
    pub(crate) state: Arc<AppState>,
}

impl Sandbox {
    /// Installs the global logger on first use.
    ///
    /// [`crate::logging::init_logger`] panics when a global logger is already
    /// installed, which a reused sandbox would otherwise do on its second
    /// request. The guard is owned here rather than inside the logger so that
    /// ownership of the one-time initialization is visible at the entry point.
    pub(crate) fn ensure_logger(&mut self) {
        if self.logger_installed {
            return;
        }
        crate::logging::init_logger();
        self.logger_installed = true;

        // Startup limit resolution runs in `main`, before any logger exists,
        // so its diagnostics are held here and emitted once there is somewhere
        // for them to go. Dropping them would hide the reason reuse is off.
        for message in self.pending_diagnostics.drain(..) {
            log::info!("{message}");
        }
    }

    /// Records a startup diagnostic for emission once the logger is installed.
    pub(crate) fn defer_diagnostic(&mut self, message: impl Into<String>) {
        self.pending_diagnostics.push(message.into());
    }

    /// Records the start of a request and returns its 1-based ordinal.
    pub(crate) fn begin_request(&mut self) -> u64 {
        self.requests = self.requests.saturating_add(1);
        self.requests
    }

    /// Records that the application was constructed in this sandbox.
    pub(crate) fn record_build(&mut self) {
        self.builds = self.builds.saturating_add(1);
    }

    /// Number of requests this sandbox has begun.
    #[cfg(any(feature = "reusable-sandbox", test))]
    pub(crate) fn requests(&self) -> u64 {
        self.requests
    }

    /// Number of application builds this sandbox has performed.
    pub(crate) fn builds(&self) -> u64 {
        self.builds
    }

    /// The retained application, if one has been built and kept.
    pub(crate) fn retained_app(&self) -> Option<&RetainedApp> {
        self.retained.as_ref()
    }

    /// Retains a successfully built application for later requests.
    pub(crate) fn retain_app(&mut self, app: App, state: Arc<AppState>) {
        self.retained = Some(RetainedApp { app, state });
    }

    /// Resolves the application for this request, building it if needed.
    ///
    /// This is the whole build/reuse/retry decision, in one place so it can be
    /// exercised with an injected builder rather than re-implemented by tests.
    ///
    /// Returns `None` when the caller should use [`Self::retained_app`], and
    /// `Some(app)` when the build failed: that value is an error router with
    /// no state, which serves the current request and is then dropped.
    /// Retaining it would pin the sandbox into permanent error mode, so the
    /// next request calls `build` again.
    ///
    /// `build` is not called at all once an application is retained.
    pub(crate) fn resolve_app<F>(&mut self, build: F) -> Option<App>
    where
        F: FnOnce() -> (App, Option<Arc<AppState>>),
    {
        if self.retained.is_some() {
            return None;
        }

        let (app, state) = build();
        self.record_build();

        match state {
            Some(state) => {
                self.retain_app(app, state);
                None
            }
            None => Some(app),
        }
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
                builds: sandbox.builds(),
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
    fn sandbox_installs_the_logger_once_then_reuses_it() {
        let mut sandbox = Sandbox::default();
        sandbox.defer_diagnostic("startup note");

        // First request: really installs the global logger.
        sandbox.ensure_logger();
        assert!(
            sandbox.logger_installed,
            "the first request should install the logger"
        );
        assert!(
            sandbox.pending_diagnostics.is_empty(),
            "installation should flush the deferred startup diagnostics"
        );

        // Second request in the same sandbox. Without the guard this reaches
        // `fern`'s `apply()` a second time and panics, which is the failure
        // this test exists to catch.
        sandbox.ensure_logger();
        sandbox.ensure_logger();

        assert!(
            sandbox.logger_installed,
            "the guard should stay set across requests"
        );
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
    fn deferred_diagnostics_survive_until_the_logger_exists() {
        let mut sandbox = Sandbox::default();
        sandbox.defer_diagnostic("reuse declined");

        assert_eq!(
            sandbox.pending_diagnostics.len(),
            1,
            "startup runs before the logger, so the message must be held"
        );

        // Simulates `ensure_logger` past the point of installation; calling it
        // for real would install a global logger and break sibling tests.
        sandbox.logger_installed = true;
        let drained: Vec<String> = sandbox.pending_diagnostics.drain(..).collect();

        assert_eq!(
            drained,
            vec!["reuse declined".to_owned()],
            "the held diagnostic should be emitted, not dropped"
        );
    }

    #[test]
    fn a_sandbox_starts_with_no_retained_application() {
        let sandbox = Sandbox::default();

        assert!(
            sandbox.retained_app().is_none(),
            "construction must be lazy so the health probe never pays for it"
        );
        assert_eq!(
            sandbox.builds(),
            0,
            "a sandbox that has served nothing should report no builds"
        );
    }

    #[test]
    fn a_retained_application_is_reused_without_rebuilding() {
        let mut sandbox = Sandbox::default();
        let builds = Cell::new(0_u32);

        // First request builds through the production decision.
        let transient = sandbox.resolve_app(|| {
            builds.set(builds.get() + 1);
            (test_app(), Some(test_state()))
        });
        assert!(transient.is_none(), "a successful build should be retained");
        assert_eq!(builds.get(), 1, "the first request should build");

        // Later requests must not call the builder at all.
        for ordinal in 2..=4 {
            let transient = sandbox.resolve_app(|| {
                builds.set(builds.get() + 1);
                panic!("request {ordinal} must not rebuild a retained application")
            });
            assert!(
                transient.is_none(),
                "request {ordinal} should use the retained application"
            );
        }

        assert_eq!(
            builds.get(),
            1,
            "four requests should cost exactly one build; that is the whole point"
        );
        assert_eq!(sandbox.builds(), 1, "the counter should agree");
    }

    #[test]
    fn a_failed_build_is_retried_and_a_later_success_is_retained() {
        let mut sandbox = Sandbox::default();
        let builds = Cell::new(0_u32);

        // Request 1: the build fails. `build_app_with_state` returns an error
        // router with no state, which is what `None` models here.
        let transient = sandbox.resolve_app(|| {
            builds.set(builds.get() + 1);
            (test_app(), None)
        });
        assert!(
            transient.is_some(),
            "a failed build must be handed back for this request only"
        );
        assert!(
            sandbox.retained_app().is_none(),
            "a failed build must not be retained"
        );

        // Request 2: the build succeeds. This is the recovery step, and it is
        // only reachable because the failure was not retained.
        let transient = sandbox.resolve_app(|| {
            builds.set(builds.get() + 1);
            (test_app(), Some(test_state()))
        });
        assert!(
            transient.is_none(),
            "the retry should succeed and be retained"
        );
        assert!(
            sandbox.retained_app().is_some(),
            "the recovered application should be retained"
        );

        // Request 3: recovery is durable — no further build.
        let transient = sandbox.resolve_app(|| {
            builds.set(builds.get() + 1);
            panic!("a recovered application must not be rebuilt")
        });
        assert!(
            transient.is_none(),
            "request 3 should reuse the recovered app"
        );

        assert_eq!(
            builds.get(),
            2,
            "one failed build plus one successful build, then reuse"
        );
        assert_eq!(
            sandbox.builds(),
            2,
            "both attempts should be counted so a retry loop stays visible"
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

    #[test]
    fn ordinals_increase_and_builds_count_separately() {
        let mut sandbox = Sandbox::default();

        assert_eq!(
            sandbox.begin_request(),
            1,
            "first request should be ordinal 1"
        );
        sandbox.record_build();
        assert_eq!(sandbox.begin_request(), 2, "ordinals should increase");

        assert_eq!(sandbox.requests(), 2, "should have begun two requests");
        assert_eq!(
            sandbox.builds(),
            1,
            "a reused sandbox should report fewer builds than requests"
        );
    }
}
