//! The key-value store the Spin adapter declares, from the label `spin.toml`
//! grants to the services a request is given.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::app::Hooks as _;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::http::request_builder;
use edgezero_core::key_value_store::{KvError, KvHandle, KvPage, KvStore};
use edgezero_core::params::PathParams;
use edgezero_core::store_registry::KvRegistry;
use trusted_server_adapter_spin::app::TrustedServerApp;
use trusted_server_adapter_spin::platform::build_runtime_services;
use trusted_server_core::permissions::PermissionPolicy;
use trusted_server_core::settings::Settings;

/// A key-value store kept in memory, standing in for the Spin store.
#[derive(Default)]
struct MemoryStore {
    entries: Mutex<HashMap<String, Bytes>>,
}

#[async_trait::async_trait(?Send)]
impl KvStore for MemoryStore {
    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.entries
            .lock()
            .expect("should lock the store")
            .remove(key);
        Ok(())
    }

    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        Ok(self
            .entries
            .lock()
            .expect("should lock the store")
            .get(key)
            .cloned())
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Ok(KvPage::default())
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.entries
            .lock()
            .expect("should lock the store")
            .insert(key.to_owned(), value);
        Ok(())
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        _ttl: Duration,
    ) -> Result<(), KvError> {
        self.put_bytes(key, value).await
    }
}

/// `EdgeZero` opens the key-value store the adapter declares for every
/// request and treats it as required, so the label it opens has to be one
/// `spin.toml` grants the component, or every request fails.
#[test]
fn the_declared_key_value_store_is_a_label_spin_grants() {
    let declared = TrustedServerApp::stores()
        .kv
        .expect("the adapter should declare its key-value store");
    let manifest = include_str!("../spin.toml");

    // `EdgeZero` takes the label from an `EDGEZERO__STORES__KV__<ID>__NAME`
    // variable in the component's environment and falls back to the id.
    assert!(
        !manifest.contains("EDGEZERO__STORES__KV__"),
        "spin.toml should not rename the declared store"
    );
    let granted = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("key_value_stores = "))
        .expect("spin.toml should grant the component its key-value stores");
    assert!(
        granted.contains(&format!("\"{}\"", declared.default)),
        "spin.toml should grant `{}`, which the declared store opens, and grants {granted}",
        declared.default
    );
}

/// The store `run_app` opens for the declaration is the key-value store a
/// request's services are given, so what one request writes there the next
/// one reads. The registry is built from the declaration the way `EdgeZero`
/// builds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_declared_store_is_the_key_value_store_a_request_is_given() {
    let declared = TrustedServerApp::stores()
        .kv
        .expect("the adapter should declare its key-value store");
    let store = Arc::new(MemoryStore::default());
    let handle = KvHandle::new(Arc::clone(&store) as Arc<dyn KvStore>);
    let by_id = declared
        .ids
        .iter()
        .map(|id| ((*id).to_owned(), handle.clone()))
        .collect();
    let registry = KvRegistry::from_parts(by_id, declared.default.to_owned())
        .expect("the declared default should be a declared id");
    let mut request = request_builder()
        .method("GET")
        .uri("https://test-publisher.example.com/")
        .body(Body::empty())
        .expect("should build request");
    request.extensions_mut().insert(registry);
    let ctx = RequestContext::new(request, PathParams::default());

    let services = build_runtime_services(
        &ctx,
        &Settings::default(),
        &Arc::default(),
        &PermissionPolicy::sample(),
    );
    services
        .kv_store()
        .put_bytes("a-key", Bytes::from_static(b"a value"))
        .await
        .expect("should write through the request's key-value store");

    let written = store
        .entries
        .lock()
        .expect("should lock the store")
        .get("a-key")
        .cloned();
    assert_eq!(
        written.as_deref(),
        Some(&b"a value"[..]),
        "the write should reach the declared store"
    );
}
