//! Invocation-local async bootstrap for Spin's synchronous TS store consumers.
//!
//! Remove this read-only bridge when core's config and secret traits become async.

use std::collections::BTreeMap;

use error_stack::Report;
use trusted_server_core::platform::{
    PlatformConfigStore, PlatformError, PlatformSecretStore, StoreId, StoreName,
};

/// Immutable values, not SDK handles or pending work, owned by one invocation.
#[derive(Default)]
pub(crate) struct SpinStoreSnapshot {
    // Preserve the old adapter's single-default-store mapping: logical config
    // names all address the same KV store, and keys are not variable-encoded.
    config: BTreeMap<String, String>,
    secrets: BTreeMap<(String, String), Vec<u8>>,
}

impl PlatformConfigStore for SpinStoreSnapshot {
    fn get(&self, _: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        self.config.get(key).cloned().ok_or_else(|| {
            Report::new(PlatformError::ConfigStore)
                .attach("key is absent from Spin config snapshot")
        })
    }

    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("Spin snapshots are read-only"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("Spin snapshots are read-only"))
    }
}

impl PlatformSecretStore for SpinStoreSnapshot {
    fn get_bytes(&self, store: &StoreName, key: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        self.secrets
            .get(&(store.as_ref().to_owned(), key.to_owned()))
            .cloned()
            .ok_or_else(|| {
                Report::new(PlatformError::SecretStore)
                    .attach("key is absent from Spin secret snapshot")
            })
    }

    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("Spin snapshots are read-only"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("Spin snapshots are read-only"))
    }
}

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
pub(crate) use loading::initialize;

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
mod loading {
    use std::collections::BTreeSet;
    use std::future::Future;
    use std::sync::Arc;
    use std::time::Duration;

    use edgezero_core::app::App;
    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    use edgezero_core::config_store::ConfigStore as _;
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::request_builder;
    use edgezero_core::params::PathParams;
    use edgezero_core::time::{Deadline, MonotonicClock};
    use futures::future::{Either, select};
    use trusted_server_core::config_payload::VerifiedConfigBlob;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::request_signing::{
        JWKS_CONFIG_STORE_NAME, RequestSigner, SIGNING_SECRET_STORE_NAME, get_active_jwks,
        verify_signature,
    };
    use trusted_server_core::settings::Settings;

    use error_stack::Report;
    use trusted_server_core::platform::{PlatformConfigStore, StoreName};

    use super::SpinStoreSnapshot;
    use crate::app::{TrustedServerApp, build_state_with_settings};

    const MAX_CONFIG_KEYS: usize = 128;
    const MAX_KEY_BYTES: usize = 256;
    const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
    const MAX_SECRET_KEYS: usize = 64;
    const MAX_SECRET_BYTES: usize = 64 * 1024;
    const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
    const BOOTSTRAP_BUDGET: Duration = Duration::from_secs(5);

    fn failure(message: &str) -> Report<TrustedServerError> {
        Report::new(TrustedServerError::Configuration {
            message: message.to_owned(),
        })
    }

    #[async_trait::async_trait(?Send)]
    trait Source {
        async fn config_keys(&self) -> Result<Vec<String>, Report<TrustedServerError>>;
        async fn config(
            &self,
            key: &str,
            clock: &MonotonicClock,
            deadline: Deadline,
        ) -> Result<Option<String>, Report<TrustedServerError>>;
        async fn secret(
            &self,
            store: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<TrustedServerError>>;
    }

    fn check_key(key: &str) -> Result<(), Report<TrustedServerError>> {
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return Err(failure("Spin snapshot key must contain 1..=256 bytes"));
        }
        Ok(())
    }

    fn charge(total: &mut usize, bytes: usize) -> Result<(), Report<TrustedServerError>> {
        *total = total.saturating_add(bytes);
        if *total > MAX_SNAPSHOT_BYTES {
            return Err(failure(
                "Spin snapshot exceeds the 4 MiB aggregate byte limit",
            ));
        }
        Ok(())
    }

    async fn required_config(
        source: &impl Source,
        key: &str,
        clock: &MonotonicClock,
        deadline: Deadline,
    ) -> Result<String, Report<TrustedServerError>> {
        check_key(key)?;
        let value = source.config(key, clock, deadline).await?.ok_or_else(|| {
            failure("required Spin config key is missing or disappeared during bootstrap")
        })?;
        if value.len() as u64 > MAX_CONFIG_BYTES {
            return Err(failure("Spin config value exceeds the 1 MiB limit"));
        }
        Ok(value)
    }

    async fn load_secret(
        source: &impl Source,
        snapshot: &mut SpinStoreSnapshot,
        total: &mut usize,
        store: &StoreName,
        key: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        check_key(key)?;
        check_key(store.as_ref())?;
        let address = (store.as_ref().to_owned(), key.to_owned());
        if snapshot.secrets.contains_key(&address) {
            return Ok(());
        }
        if snapshot.secrets.len() >= MAX_SECRET_KEYS {
            return Err(failure("Spin snapshot exceeds the 64 secret limit"));
        }
        let value = source.secret(store, key).await?;
        if value.is_empty() || value.len() > MAX_SECRET_BYTES {
            return Err(failure(
                "required Spin secret is empty or exceeds the 64 KiB limit",
            ));
        }
        charge(total, value.len())?;
        charge(total, store.as_ref().len() + key.len())?;
        snapshot.secrets.insert(address, value);
        Ok(())
    }

    async fn load(
        source: &impl Source,
        config_key: &str,
        secret_store: &StoreName,
        clock: &MonotonicClock,
        deadline: Deadline,
    ) -> Result<(Settings, Arc<SpinStoreSnapshot>), Report<TrustedServerError>> {
        let envelope = required_config(source, config_key, clock, deadline).await?;
        let verified = VerifiedConfigBlob::parse(&envelope)?;
        verified.check_structure(64, 100_000)?;
        let required_secrets = verified.required_secret_keys()?;
        if required_secrets.len() > MAX_SECRET_KEYS {
            return Err(failure("Spin config references more than 64 secrets"));
        }
        let mut snapshot = SpinStoreSnapshot::default();
        let mut total = 0;
        charge(&mut total, envelope.len())?;
        snapshot.config.insert(config_key.to_owned(), envelope);
        for key in required_secrets {
            load_secret(source, &mut snapshot, &mut total, secret_store, &key).await?;
        }
        let settings = verified.into_settings(&snapshot, secret_store)?;

        // Snapshot the whole bounded default config store. Verification accepts
        // arbitrary existing public KIDs, not just active-kids. DataDome sources
        // also use this store under the old adapter's single-store mapping.
        let keys = source.config_keys().await?;
        if keys.len() > MAX_CONFIG_KEYS {
            return Err(failure(
                "Spin default config store exceeds the 128 key limit",
            ));
        }
        let keys: BTreeSet<_> = keys.into_iter().collect();
        if !keys.contains(config_key) {
            return Err(failure("Spin config envelope disappeared during bootstrap"));
        }
        for key in keys {
            check_key(&key)?;
            charge(&mut total, key.len())?;
            if key == config_key {
                continue;
            }
            let value = required_config(source, &key, clock, deadline).await?;
            charge(&mut total, value.len())?;
            snapshot.config.insert(key, value);
        }
        if settings
            .request_signing
            .as_ref()
            .is_some_and(|signing| signing.enabled)
        {
            let jwks_store = StoreName::from(JWKS_CONFIG_STORE_NAME);
            let current =
                PlatformConfigStore::get(&snapshot, &jwks_store, "current-kid").map_err(|_| {
                    failure("Spin signing requires current-kid in the default config store")
                })?;
            let active =
                PlatformConfigStore::get(&snapshot, &jwks_store, "active-kids").map_err(|_| {
                    failure("Spin signing requires active-kids in the default config store")
                })?;
            if !active.split(',').any(|kid| kid.trim() == current) {
                return Err(failure("Spin current signing KID must be in active-kids"));
            }
            load_secret(
                source,
                &mut snapshot,
                &mut total,
                &StoreName::from(SIGNING_SECRET_STORE_NAME),
                &current,
            )
            .await?;
        }
        let snapshot = Arc::new(snapshot);
        if settings
            .request_signing
            .as_ref()
            .is_some_and(|signing| signing.enabled)
        {
            validate_signing(&snapshot)?;
        }
        if deadline.is_expired_at(clock.now()) {
            return Err(failure("Spin bootstrap deadline exceeded"));
        }
        Ok((settings, snapshot))
    }

    fn validate_signing(
        snapshot: &Arc<SpinStoreSnapshot>,
    ) -> Result<(), Report<TrustedServerError>> {
        // Exercise the same parsers and key formats that request consumers use.
        let mut request = request_builder()
            .uri("/")
            .body(edgezero_core::body::Body::empty())
            .map_err(|_| failure("failed to construct Spin signing validation context"))?;
        request.extensions_mut().insert(Arc::clone(snapshot));
        let context = RequestContext::new(request, PathParams::default());
        let services = crate::platform::build_runtime_services(&context);
        let jwks = get_active_jwks(&services)?;
        let _: serde_json::Value =
            serde_json::from_str(&jwks).map_err(|_| failure("Spin active JWKS is malformed"))?;
        let signer = RequestSigner::from_services(&services)?;
        let challenge = b"Spin bootstrap signing key consistency";
        let signature = signer.sign(challenge)?;
        if !verify_signature(challenge, &signature, &signer.kid, &services)? {
            return Err(failure(
                "Spin current public/private signing keys do not match",
            ));
        }
        Ok(())
    }

    // A timer around the entire bootstrap also bounds a pending variable read,
    // KV open, key stream or key-stream terminal result. Dropping guest work
    // does not assert cancellation inside the host provider.
    async fn within_deadline<F, MakeTimer, Timer, T>(
        work: F,
        clock: &MonotonicClock,
        deadline: Deadline,
        mut make_timer: MakeTimer,
    ) -> Result<T, Report<TrustedServerError>>
    where
        F: Future<Output = Result<T, Report<TrustedServerError>>>,
        MakeTimer: FnMut(Duration) -> Timer,
        Timer: Future<Output = ()>,
    {
        futures::pin_mut!(work);
        loop {
            let remaining = deadline
                .remaining_at(clock.now())
                .ok_or_else(|| failure("Spin bootstrap deadline exceeded"))?;
            let timer = make_timer(remaining);
            futures::pin_mut!(timer);
            match select(timer, work.as_mut()).await {
                Either::Left(((), _)) => continue,
                Either::Right((result, _)) => {
                    if deadline.is_expired_at(clock.now()) {
                        return Err(failure("Spin bootstrap deadline exceeded"));
                    }
                    return result;
                }
            }
        }
    }

    fn assemble(
        settings: Settings,
        snapshot: Arc<SpinStoreSnapshot>,
    ) -> Result<App, Report<TrustedServerError>> {
        let state = build_state_with_settings(settings)?;
        let mut app = App::build::<TrustedServerApp>(edgezero_adapter_spin::SPIN_PLATFORM)
            .map_err(|_| failure("Spin application configuration failed"))?;
        app.insert_state(state);
        app.insert_state(snapshot);
        Ok(app)
    }

    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    struct SpinSource;

    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    #[async_trait::async_trait(?Send)]
    impl Source for SpinSource {
        async fn config_keys(&self) -> Result<Vec<String>, Report<TrustedServerError>> {
            let store = spin_sdk::key_value::Store::open("default")
                .await
                .map_err(|_| failure("failed to open Spin default config store"))?;
            let mut stream = store.get_keys().await;
            let mut keys = Vec::new();
            while let Some(key) = stream.next().await {
                check_key(&key)?;
                if keys.len() == MAX_CONFIG_KEYS {
                    return Err(failure(
                        "Spin default config store exceeds the 128 key limit",
                    ));
                }
                keys.push(key);
            }
            stream
                .result()
                .await
                .map_err(|_| failure("Spin config key listing failed"))?;
            Ok(keys)
        }

        async fn config(
            &self,
            key: &str,
            clock: &MonotonicClock,
            deadline: Deadline,
        ) -> Result<Option<String>, Report<TrustedServerError>> {
            edgezero_adapter_spin::config_store::SpinConfigStore::for_label("default".to_owned())
                .get_bounded(key, clock, deadline, MAX_CONFIG_BYTES, MAX_CONFIG_BYTES)
                .await
                .map(|read| read.value.map(|value| value.as_str().to_owned()))
                .map_err(|_| failure(&format!("Spin bounded config read failed for key `{key}`")))
        }

        async fn secret(
            &self,
            store: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<TrustedServerError>> {
            let variable = crate::platform::spin_secret_variable_name(store, key)
                .map_err(|_| failure("Spin secret store/key cannot be encoded as a variable"))?;
            spin_sdk::variables::get(&variable)
                .await
                .map(String::into_bytes)
                .map_err(|_| {
                    failure(&format!(
                        "required Spin secret variable `{variable}` is unavailable"
                    ))
                })
        }
    }

    /// Await all providers before constructing the dispatchable app.
    ///
    /// Failures here are explicit pre-admission exceptions. No routed response,
    /// readiness state or RC body/egress completion is fabricated for them.
    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    pub(crate) async fn initialize() -> Result<App, Report<TrustedServerError>> {
        let clock = MonotonicClock::default();
        let deadline = clock.deadline_after(BOOTSTRAP_BUDGET);
        let config_key = trusted_server_core::settings_data::default_config_key();
        let secret_store = trusted_server_core::settings_data::default_secret_store_name();
        let (settings, snapshot) = within_deadline(
            load(&SpinSource, &config_key, &secret_store, &clock, deadline),
            &clock,
            deadline,
            spin_sdk::time::sleep,
        )
        .await?;
        let app = assemble(settings, snapshot)?;
        if deadline.is_expired_at(clock.now()) {
            return Err(failure(
                "Spin bootstrap deadline exceeded during app assembly",
            ));
        }
        Ok(app)
    }

    #[cfg(test)]
    mod tests;
}
