//! What an adapter tells core about itself as an auction target.
//!
//! The adapter builds the descriptor at startup from its own backend
//! implementation and its own capabilities, and core validates the auction
//! plan against it. The backend names core predicts here are the names the
//! same implementation registers at runtime, so the two cannot drift.

use std::sync::Arc;

use super::PlatformBackend;

/// Adapter capabilities relevant to auction dispatch validation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct AuctionTargetCapabilities {
    concurrent_provider_fanout: bool,
    enforceable_total_request_deadline: bool,
}

impl AuctionTargetCapabilities {
    /// Declare whether several provider requests can be in flight at once and
    /// whether the adapter caps a provider request's complete lifetime.
    #[must_use]
    pub const fn new(
        concurrent_provider_fanout: bool,
        enforceable_total_request_deadline: bool,
    ) -> Self {
        Self {
            concurrent_provider_fanout,
            enforceable_total_request_deadline,
        }
    }

    /// Return whether multiple provider requests can be in flight concurrently.
    #[must_use]
    pub const fn supports_concurrent_provider_fanout(self) -> bool {
        self.concurrent_provider_fanout
    }

    /// Return whether the adapter enforces a total request deadline per provider.
    ///
    /// Transport first-byte or between-byte timers do not satisfy this
    /// capability because they do not cap the complete request lifetime.
    #[must_use]
    pub const fn has_enforceable_total_request_deadline(self) -> bool {
        self.enforceable_total_request_deadline
    }
}

/// The adapter as plan validation sees it: its name, the backend
/// implementation that names and registers backends, and its capabilities.
#[derive(Clone)]
pub struct AuctionTargetDescriptor {
    id: &'static str,
    backend: Arc<dyn PlatformBackend>,
    capabilities: AuctionTargetCapabilities,
}

impl AuctionTargetDescriptor {
    /// Describe an adapter by the name its errors carry, the backend
    /// implementation its runtime services use, and its capabilities.
    #[must_use]
    pub fn new(
        id: &'static str,
        backend: Arc<dyn PlatformBackend>,
        capabilities: AuctionTargetCapabilities,
    ) -> Self {
        Self {
            id,
            backend,
            capabilities,
        }
    }

    /// The adapter's name, as validation errors report it.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// The backend implementation whose names and timers validation predicts.
    #[must_use]
    pub fn backend(&self) -> &dyn PlatformBackend {
        self.backend.as_ref()
    }

    /// This target's auction dispatch capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> AuctionTargetCapabilities {
        self.capabilities
    }
}

impl core::fmt::Debug for AuctionTargetDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AuctionTargetDescriptor")
            .field("id", &self.id)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::test_support::{NamingBackend, StubBackend};

    #[test]
    fn a_descriptor_answers_what_the_adapter_gave_it() {
        let descriptor = AuctionTargetDescriptor::new(
            "fixture",
            Arc::new(StubBackend),
            AuctionTargetCapabilities::new(true, false),
        );
        assert_eq!(descriptor.id(), "fixture");
        assert!(
            descriptor
                .capabilities()
                .supports_concurrent_provider_fanout()
        );
        assert!(
            !descriptor
                .capabilities()
                .has_enforceable_total_request_deadline()
        );
        assert_eq!(
            descriptor.backend().auction_backend_budget(),
            None,
            "a backend that declares no budget has none"
        );
        assert_eq!(
            descriptor.backend().transport_timeout_bucket_count(2000),
            1,
            "a backend that declares no buckets reaches one name per timeout"
        );
        assert_eq!(
            format!("{descriptor:?}"),
            "AuctionTargetDescriptor { id: \"fixture\", capabilities: \
             AuctionTargetCapabilities { concurrent_provider_fanout: true, \
             enforceable_total_request_deadline: false }, .. }"
        );
    }

    #[test]
    fn a_descriptor_carries_the_backend_budget_the_adapter_declares() {
        let backend = NamingBackend::new()
            .with_auction_backend_budget(7)
            .with_transport_timeout_buckets(3);
        let descriptor = AuctionTargetDescriptor::new(
            "fixture",
            Arc::new(backend),
            AuctionTargetCapabilities::new(false, true),
        );
        assert_eq!(descriptor.backend().auction_backend_budget(), Some(7));
        assert_eq!(descriptor.backend().transport_timeout_bucket_count(500), 3);
        assert!(
            !descriptor
                .capabilities()
                .supports_concurrent_provider_fanout()
        );
        assert!(
            descriptor
                .capabilities()
                .has_enforceable_total_request_deadline()
        );
    }
}
