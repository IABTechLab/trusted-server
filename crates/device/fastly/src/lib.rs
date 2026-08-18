//! The Fastly device provider and host-signal capture.
//!
//! [`FastlyDeviceProvider`] strengthens the built-in User-Agent classification
//! with the host's TLS (JA4) and HTTP/2 fingerprints, for deployments on Fastly
//! Compute. It is selected by `[device] provider = "fastly"` and wired in by the
//! Fastly adapter, which injects the request info and the captured host signals.
//!
//! [`FastlyHostSignals`] captures those fingerprints from a live Fastly request
//! (`get_tls_ja4()`, `get_client_h2_fingerprint()`) into owned values, so it can
//! be shared as an injected [`HostSignals`] service that outlives the borrow of
//! the request. Capturing through the SDK is why this crate depends on the
//! `fastly` crate and builds only for the `wasm32-wasip1` target; off-host the
//! accessors return `None`, so classification degrades to User-Agent only. The
//! platform-neutral [`HostSignals`], [`RequestInfo`], and [`DeviceProvider`]
//! traits and the built-in default live in `trusted-server-core`, where the
//! `DeviceSignals` classification logic stays unit-tested.

use std::sync::Arc;

use fastly::Request as FastlyRequest;
use trusted_server_core::ec::device::{DeviceProvider, DeviceSignals};
use trusted_server_core::evidence::{HostSignals, RequestInfo};

/// Host-computed client fingerprints captured from a live Fastly request.
///
/// Reads the TLS JA4 and HTTP/2 fingerprints once through the Fastly SDK and
/// owns them, so the value can be injected as a [`HostSignals`] service that
/// outlives the borrow of the request it was captured from. Off-host the SDK
/// accessors return `None`, so the signals are simply absent.
#[derive(Debug, Clone, Default)]
pub struct FastlyHostSignals {
    ja4: Option<String>,
    h2: Option<String>,
}

impl FastlyHostSignals {
    /// Builds host signals from already-captured fingerprint values.
    ///
    /// Use this when the adapter has read the fingerprints once (for example
    /// into the client metadata, or from the trusted internal headers the entry
    /// point injects) and wants to share them without another SDK call.
    #[must_use]
    pub fn new(ja4: Option<String>, h2: Option<String>) -> Self {
        Self { ja4, h2 }
    }

    /// Captures the TLS JA4 and HTTP/2 fingerprints from a live Fastly request.
    #[must_use]
    pub fn from_request(req: &FastlyRequest) -> Self {
        Self {
            ja4: req.get_tls_ja4().map(str::to_string),
            h2: req.get_client_h2_fingerprint().map(str::to_string),
        }
    }
}

impl HostSignals for FastlyHostSignals {
    fn ja4(&self) -> Option<&str> {
        self.ja4.as_deref()
    }

    fn h2(&self) -> Option<&str> {
        self.h2.as_deref()
    }
}

/// The Fastly device provider, opt-in via `[device] provider = "fastly"`.
///
/// Classifies a request with the fingerprint-strengthened
/// [`DeviceSignals::derive`], reading the User-Agent from its injected
/// [`RequestInfo`] and the TLS/HTTP-2 fingerprints from its injected
/// [`HostSignals`], so the browser/bot gate is backed by the live request.
pub struct FastlyDeviceProvider {
    host_signals: Arc<dyn HostSignals>,
}

impl FastlyDeviceProvider {
    /// Creates the provider with its injected host signals.
    #[must_use]
    pub fn new(host_signals: Arc<dyn HostSignals>) -> Self {
        Self { host_signals }
    }
}

impl DeviceProvider for FastlyDeviceProvider {
    fn id(&self) -> &'static str {
        "fastly"
    }

    fn detect(&self, request_info: &dyn RequestInfo) -> DeviceSignals {
        DeviceSignals::derive(
            request_info.user_agent(),
            self.host_signals.ja4(),
            self.host_signals.h2(),
        )
    }
}
