//! Platform abstraction layer for `trusted-server-core`.
//!
//! This module defines platform-neutral service contracts and request-scoped
//! runtime state. Concrete implementations live in adapter crates such as
//! `trusted-server-adapter-fastly`.
//!
//! ## Traits
//!
//! - [`PlatformConfigStore`] — key-value config store access
//! - [`PlatformSecretStore`] — encrypted secret store access
//! - [`PlatformKvStore`] — (re-exported from `edgezero_core`)
//! - [`PlatformBackend`] — dynamic backend registration
//! - [`PlatformHttpClient`] — outbound HTTP client
//! - [`PlatformGeo`] — geographic information lookup
//!
//! ## Platform-Agnostic Components
//!
//! The following components were evaluated for platform-specific behavior
//! (verified 2026-03-31; see `docs/superpowers/plans/2026-03-31-pr8-content-rewriting-verification.md`)
//! and found to have a platform-agnostic rewriting pipeline. No
//! platform trait is required; future adapters (Cloudflare Workers, Axum, Spin) need not provide
//! any content-rewriting implementation:
//!
//! - **Content rewriting** — `html_processor`, `streaming_processor`,
//!   `streaming_replacer`, and `rsc_flight` modules use only standard Rust
//!   (`std::io::Read`/`Write`, `lol_html`, `flate2`, `brotli`). The pipeline
//!   is accessed via [`StreamingPipeline::process`](crate::streaming_processor::StreamingPipeline::process) which
//!   accepts any reader, including `fastly::Body` (which implements
//!   `std::io::Read`).
//!
//!   No `PlatformContentRewriter` trait exists or is needed.
//!

mod error;
mod http;
mod image_optimizer;
mod kv;
#[cfg(test)]
pub(crate) mod test_support;
mod traits;
mod types;

pub use edgezero_core::key_value_store::{KvError, KvHandle, KvStore as PlatformKvStore};
pub use error::PlatformError;
pub use http::{
    PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    PlatformSelectResult,
};
pub use image_optimizer::{
    PlatformImageOptimizerCrop, PlatformImageOptimizerCropMode, PlatformImageOptimizerOptions,
    PlatformImageOptimizerParams, PlatformImageOptimizerRegion,
};
pub use kv::UnavailableKvStore;
pub use traits::{PlatformBackend, PlatformConfigStore, PlatformGeo, PlatformSecretStore};
pub use types::{
    ClientInfo, GeoInfo, PlatformBackendSpec, RuntimeServices, RuntimeServicesBuilder, StoreId,
    StoreName,
};

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use bytes::Bytes;
    use edgezero_core::key_value_store::KvPage;

    use super::test_support::{noop_services, noop_services_with_client_ip};
    use super::*;

    struct MarkerKvStore(&'static str);

    #[async_trait(?Send)]
    impl PlatformKvStore for MarkerKvStore {
        async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
            if key == "marker" {
                Ok(Some(Bytes::from(self.0.to_owned())))
            } else {
                Ok(None)
            }
        }

        async fn put_bytes(&self, _key: &str, _value: Bytes) -> Result<(), KvError> {
            Ok(())
        }

        async fn put_bytes_with_ttl(
            &self,
            _key: &str,
            _value: Bytes,
            _ttl: Duration,
        ) -> Result<(), KvError> {
            Ok(())
        }

        async fn delete(&self, _key: &str) -> Result<(), KvError> {
            Ok(())
        }

        async fn list_keys_page(
            &self,
            _prefix: &str,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<KvPage, KvError> {
            Ok(KvPage::default())
        }
    }

    fn _assert_config_store_object_safe(_: &dyn PlatformConfigStore) {}
    fn _assert_secret_store_object_safe(_: &dyn PlatformSecretStore) {}
    fn _assert_kv_store_object_safe(_: &dyn PlatformKvStore) {}
    fn _assert_backend_object_safe(_: &dyn PlatformBackend) {}
    fn _assert_http_client_object_safe(_: &dyn PlatformHttpClient) {}
    fn _assert_geo_object_safe(_: &dyn PlatformGeo) {}
    // Arc<dyn Trait> requires the trait impl to be Send + Sync. The assertion
    // below documents that RuntimeServices itself satisfies those bounds — the
    // compiler verifies this at the point where RuntimeServices is constructed.
    fn _assert_runtime_services_send_sync()
    where
        RuntimeServices: Send + Sync,
    {
    }

    #[test]
    fn runtime_services_can_be_constructed_and_cloned() {
        let services = noop_services();
        let cloned = services.clone();

        assert!(
            cloned.client_info().client_ip.is_none(),
            "should preserve client_ip through clone"
        );
        assert!(
            cloned.client_info().tls_protocol.is_none(),
            "should preserve tls_protocol through clone"
        );
    }

    #[test]
    fn runtime_services_geo_lookup_returns_none_for_no_ip() {
        let services = noop_services();
        let result = services
            .geo()
            .lookup(services.client_info().client_ip)
            .expect("should not fail for noop geo with no ip");
        assert!(result.is_none(), "should return None when no IP is present");
    }

    #[test]
    fn runtime_services_with_kv_store_replaces_only_the_new_clone() {
        let services = noop_services_with_client_ip(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
        let replaced = services
            .clone()
            .with_kv_store(Arc::new(MarkerKvStore("replaced")));

        let original_value = futures::executor::block_on(services.kv_store().get_bytes("marker"))
            .expect("should query the original noop store");
        let replaced_value = futures::executor::block_on(replaced.kv_store().get_bytes("marker"))
            .expect("should query the replaced marker store");

        assert_eq!(
            original_value, None,
            "should keep the original RuntimeServices KV store unchanged"
        );
        assert_eq!(
            replaced_value,
            Some(Bytes::from_static(b"replaced")),
            "should expose the replacement KV store through kv_store()"
        );
        assert_eq!(
            replaced.client_info().client_ip,
            services.client_info().client_ip,
            "should preserve client_info through with_kv_store"
        );
    }

    #[test]
    fn platform_pending_request_downcasts_and_preserves_backend_name() {
        let pending = PlatformPendingRequest::new(7_u8).with_backend_name("origin");
        let pending = pending
            .downcast::<String>()
            .expect_err("should reject downcast to the wrong pending type");
        assert_eq!(
            pending.backend_name(),
            Some("origin"),
            "should preserve backend metadata when downcast fails"
        );

        let pending = PlatformPendingRequest::new(7_u8).with_backend_name("origin");
        let value = pending
            .downcast::<u8>()
            .expect("should recover the stored pending request type");
        assert_eq!(value, 7, "should preserve the stored pending request");
    }

    #[test]
    fn geo_info_coordinates_string_formats_correctly() {
        let geo = GeoInfo {
            city: "New York".to_owned(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 40.7128,
            longitude: -74.0060,
            metro_code: 501,
            region: Some("NY".to_owned()),
            asn: None,
        };

        assert_eq!(
            geo.coordinates_string(),
            "40.7128,-74.006",
            "should format coordinates as lat,lon"
        );
    }

    #[test]
    fn geo_info_has_metro_code_returns_true_for_nonzero() {
        let geo = GeoInfo {
            city: String::new(),
            country: String::new(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 807,
            region: None,
            asn: None,
        };
        assert!(
            geo.has_metro_code(),
            "should return true for non-zero metro code"
        );
    }

    #[test]
    fn geo_info_has_metro_code_returns_false_for_zero() {
        let geo = GeoInfo {
            city: String::new(),
            country: String::new(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        };
        assert!(
            !geo.has_metro_code(),
            "should return false for zero metro code"
        );
    }
}
