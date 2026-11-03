use std::time::Duration;

/// A timer that can wake a pending future after a delay.
///
/// Optional on [`crate::platform::RuntimeServices`]: its presence is the
/// capability. An adapter provides one only when its runtime can fire a timer
/// while other work of the request is pending. A runtime whose host calls
/// block the guest could not fire one in the middle of a call, so its adapter
/// provides none, and work there is bounded by the transport timeouts alone.
///
/// Uses `?Send` futures, matching [`crate::platform::PlatformHttpClient`] and
/// [`crate::platform::PlatformKvStore`], because the WebAssembly timers are
/// `!Send`.
#[async_trait::async_trait(?Send)]
pub trait PlatformTimer: Send + Sync {
    /// Completes once `duration` has elapsed.
    ///
    /// Dropping the returned future before it completes cancels the timer.
    async fn sleep(&self, duration: Duration);
}
