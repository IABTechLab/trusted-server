//! Cooperative timer capability supplied by supporting adapters.

use futures::future::LocalBoxFuture;
use std::time::Duration;

/// Creates local delay futures without requiring a platform executor in core.
///
/// A delay cannot preempt blocking calls or work that never yields. Fastly does
/// not supply this service because its host calls block. Consumers on Fastly
/// must independently bound their calls using the remaining EOF budget.
pub trait PlatformTimer: Send + Sync {
    /// Wait for the duration on the adapter's request executor.
    fn delay(&self, duration: Duration) -> LocalBoxFuture<'static, ()>;
}
