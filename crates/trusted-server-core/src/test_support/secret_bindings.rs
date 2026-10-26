//! Native-store fixtures for exercising the real credential compiler in tests.

use std::collections::BTreeMap;

use error_stack::Report;

use crate::error::TrustedServerError;
use crate::platform::{PlatformError, PlatformSecretStore, StoreId, StoreName};
use crate::secret_bindings::{SECRET_STORE_ID, compile_bindings, resolve_bindings};
use crate::settings::{AssetOriginAuth, Settings};

struct FixtureStore(BTreeMap<String, Vec<u8>>);

impl PlatformSecretStore for FixtureStore {
    fn get_bytes(&self, _: &StoreName, key: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        self.0
            .get(key)
            .cloned()
            .ok_or_else(|| Report::new(PlatformError::SecretStore))
    }
    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        unreachable!("should never write fixture native credentials")
    }
    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        unreachable!("should never delete fixture native credentials")
    }
}

fn index(path: &str) -> Option<usize> {
    path.split_once('[')?.1.split_once(']')?.0.parse().ok()
}

/// Resolve existing fictional shadow values through actual generated native keys.
///
/// Available only to test/test-utils builds. This does not construct a private
/// capability directly or supply a fallback value for an invalid fixture.
///
/// # Errors
/// Returns a configuration error if any active fixture role lacks a valid value.
pub fn install_test_bindings(settings: &mut Settings) -> Result<(), Report<TrustedServerError>> {
    let plan = compile_bindings(settings)?;
    let mut values = BTreeMap::new();
    for entry in plan.entries() {
        let value = match entry.descriptor_id() {
            "publisher.proxy" => Some(settings.publisher.proxy_secret.expose().as_str()),
            "ec.passphrase" => Some(settings.ec.passphrase.expose().as_str()),
            "handler.password" => index(entry.field_path())
                .and_then(|i| settings.handlers.get(i))
                .map(|handler| handler.password.expose().as_str()),
            "ec.inbound" => index(entry.field_path())
                .and_then(|i| settings.ec.partners.get(i))
                .and_then(|partner| partner.api_token.as_ref())
                .map(|value| value.expose().as_str()),
            "ec.pull" => index(entry.field_path())
                .and_then(|i| settings.ec.partners.get(i))
                .and_then(|partner| partner.ts_pull_token.as_ref())
                .map(|value| value.expose().as_str()),
            "clientip.shared" => settings
                .trusted_client_ip
                .as_ref()
                .map(|config| config.shared_secret.expose().as_str()),
            "tinybird.auction" => settings
                .tinybird
                .auction_token_secret
                .as_ref()
                .map(|value| value.expose().as_str()),
            "datadome.server" => settings
                .integrations
                .get("datadome")
                .and_then(|config| config.get("server_side_key_secret_name"))
                .and_then(serde_json::Value::as_str),
            "datadome.bypass" => settings
                .integrations
                .get("datadome")
                .and_then(|config| config.pointer("/protection_test_bypass/credential_secret_name"))
                .and_then(serde_json::Value::as_str),
            "s3.access-id" | "s3.secret" | "s3.session" => index(entry.field_path())
                .and_then(|i| settings.proxy.asset_routes.get(i))
                .and_then(|route| route.auth.as_ref())
                .and_then(|auth| match auth {
                    AssetOriginAuth::S3SigV4(auth) => match entry.descriptor_id() {
                        "s3.access-id" => Some(auth.access_key_id.expose().as_str()),
                        "s3.secret" => Some(auth.secret_access_key.expose().as_str()),
                        _ => auth
                            .session_token
                            .as_ref()
                            .map(|value| value.expose().as_str()),
                    },
                }),
            _ => None,
        };
        if let Some(value) = value {
            values.insert(entry.key().to_owned(), value.as_bytes().to_vec());
        }
    }
    settings.resolved_bindings = resolve_bindings(
        settings,
        &FixtureStore(values),
        &StoreName::from(SECRET_STORE_ID),
    )?;
    Ok(())
}
