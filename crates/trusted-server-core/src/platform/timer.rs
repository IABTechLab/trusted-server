//! Cooperative timer capability supplied by supporting adapters.

use futures::future::LocalBoxFuture;
use std::time::Duration;

/// Creates local delay futures without requiring a platform executor in core.
///
/// A delay cannot preempt blocking calls or work that never yields.
pub trait PlatformTimer: Send + Sync {
    /// Wait for the duration on the adapter's request executor.
    fn delay(&self, duration: Duration) -> LocalBoxFuture<'static, ()>;
}
