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

mod error;
mod http;
mod kv;
mod traits;
mod types;

pub use edgezero_core::key_value_store::{KvError, KvHandle, KvStore as PlatformKvStore};
pub use error::PlatformError;
pub use http::{
    PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    PlatformSelectResult,
};
pub use kv::UnavailableKvStore;
pub use traits::{PlatformBackend, PlatformConfigStore, PlatformGeo, PlatformSecretStore};
pub use types::{
    ClientInfo, GeoInfo, PlatformBackendSpec, RuntimeServices, RuntimeServicesBuilder, StoreId,
    StoreName,
};

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Arc;

    use error_stack::Report;

    use super::*;

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

    struct NoopConfigStore;
    impl PlatformConfigStore for NoopConfigStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopSecretStore;
    impl PlatformSecretStore for NoopSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopBackend;
    impl PlatformBackend for NoopBackend {
        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopHttpClient;
    // ?Send matches PlatformHttpClient — Body wraps LocalBoxStream which is !Send
    // by design; see http.rs for the full rationale.
    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for NoopHttpClient {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopGeo;
    impl PlatformGeo for NoopGeo {
        fn lookup(
            &self,
            _client_ip: Option<IpAddr>,
        ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            Ok(None)
        }
    }

    fn noop_services() -> RuntimeServices {
        // edgezero_core::key_value_store::NoopKvStore is available via the
        // test-utils feature enabled in dev-dependencies.
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(NoopBackend))
            .http_client(Arc::new(NoopHttpClient))
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            })
            .build()
    }

    #[test]
    fn runtime_services_can_be_constructed_and_cloned() {
        let services = noop_services();
        let cloned = services.clone();

        assert!(
            cloned.client_info.client_ip.is_none(),
            "should preserve client_ip through clone"
        );
        assert!(
            cloned.client_info.tls_protocol.is_none(),
            "should preserve tls_protocol through clone"
        );
    }

    #[test]
    fn runtime_services_geo_lookup_returns_none_for_no_ip() {
        let services = noop_services();
        let result = services
            .geo
            .lookup(services.client_info.client_ip)
            .expect("should not fail for noop geo with no ip");
        assert!(result.is_none(), "should return None when no IP is present");
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
            city: "New York".to_string(),
            country: "US".to_string(),
            continent: "NorthAmerica".to_string(),
            latitude: 40.7128,
            longitude: -74.0060,
            metro_code: 501,
            region: Some("NY".to_string()),
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
        };
        assert!(
            !geo.has_metro_code(),
            "should return false for zero metro code"
        );
    }
}
