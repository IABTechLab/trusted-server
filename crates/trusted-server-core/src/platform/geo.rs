use std::net::IpAddr;
use std::sync::Arc;

use error_stack::Report;

use super::{ClientInfo, GeoInfo, GeoModule, PlatformError, PlatformGeo, RuntimeServices};
use crate::module_context::{FromModuleContext, ModuleCall};
use crate::permissions::PermissionSet;

/// A geo module that resolves nothing.
///
/// Installed when the `[geo] module` selector is unset, and when it is set to
/// `"none"`, which spells the same choice explicitly. A client IP is then never
/// sent to any host geo service, so a default deployment is not tied to any host
/// geo capability. Every geo consumer already treats [`GeoInfo`] as optional, so
/// a `None` result degrades gracefully: the permission baseline falls back to
/// the top of the `permissions.yaml` rules tree, the auction omits geo, and so on.
///
/// Adapter crates should use this type rather than defining their own stub so
/// the behavior is the same on every platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledGeo;

#[async_trait::async_trait(?Send)]
impl PlatformGeo for DisabledGeo {
    async fn lookup(
        &self,
        _client_ip: Option<IpAddr>,
        _services: &RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

/// A [`PlatformGeo`] called as a [`GeoModule`]: the client address comes from
/// the call's client metadata and the services from the call, and the
/// platform lookup answers. A call carrying no services answers nothing.
pub struct PlatformGeoModule {
    name: &'static str,
    platform: Arc<dyn PlatformGeo>,
}

impl PlatformGeoModule {
    /// Calls `platform` as the module named `name`.
    #[must_use]
    pub fn new(name: &'static str, platform: Arc<dyn PlatformGeo>) -> Self {
        Self { name, platform }
    }
}

impl std::fmt::Debug for PlatformGeoModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformGeoModule")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait(?Send)]
impl GeoModule for PlatformGeoModule {
    fn id(&self) -> &'static str {
        self.name
    }

    async fn locate(&self, call: ModuleCall<'_>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        let Ok(services) = <&RuntimeServices>::from_module_context(&call) else {
            return Ok(None);
        };
        let client_ip = <&ClientInfo>::from_module_context(&call)
            .ok()
            .and_then(|client| client.client_ip);
        self.platform.lookup(client_ip, services).await
    }

    fn required_permissions(&self) -> PermissionSet {
        self.platform.required_permissions()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    use http::Method;

    use super::*;
    use crate::module_context::{ModuleContext, ModuleRequest};
    use crate::permissions::PermissionSet;
    use crate::platform::test_support::noop_services;

    /// A platform lookup that records the address it was asked about.
    #[derive(Default)]
    struct Recording {
        asked: Mutex<Vec<Option<IpAddr>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformGeo for Recording {
        async fn lookup(
            &self,
            client_ip: Option<IpAddr>,
            _services: &RuntimeServices,
        ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            self.asked.lock().expect("lock").push(client_ip);
            Ok(None)
        }
    }

    #[tokio::test]
    async fn the_platform_lookup_is_given_the_address_the_call_carries() {
        let recording = Arc::new(Recording::default());
        let module =
            PlatformGeoModule::new("platform", Arc::clone(&recording) as Arc<dyn PlatformGeo>);
        let address = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        let client = ClientInfo {
            client_ip: Some(address),
            ..ClientInfo::default()
        };
        let services = noop_services().with_client_info(client.clone());
        let method = Method::GET;
        let context = ModuleContext::new(ModuleRequest::new(
            &method,
            "publisher.example",
            "https",
            "/",
        ))
        .with_services(&services)
        .with_client(&client);

        let located = module
            .locate(context.call(module.id(), PermissionSet::none()))
            .await
            .expect("the platform answers");

        assert!(located.is_none(), "the recording platform answers nothing");
        assert_eq!(*recording.asked.lock().expect("lock"), vec![Some(address)]);
        assert_eq!(module.id(), "platform");
    }

    #[tokio::test]
    async fn a_call_without_services_asks_the_platform_nothing() {
        let recording = Arc::new(Recording::default());
        let module =
            PlatformGeoModule::new("platform", Arc::clone(&recording) as Arc<dyn PlatformGeo>);
        let method = Method::GET;
        let context = ModuleContext::new(ModuleRequest::new(
            &method,
            "publisher.example",
            "https",
            "/",
        ));

        let located = module
            .locate(context.call(module.id(), PermissionSet::none()))
            .await
            .expect("no services is not an error");

        assert!(located.is_none());
        assert!(recording.asked.lock().expect("lock").is_empty());
    }
}
