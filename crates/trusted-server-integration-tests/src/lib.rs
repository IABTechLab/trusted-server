//! Fictional native credentials compiled through the production binding policy.
//!
//! These public local values are not provisioning inputs for deployed resources.

use std::collections::BTreeMap;

use trusted_server_core::secret_bindings::{
    CompiledBinding, NativeBinding, SECRET_STORE_ID, compile_bindings, render_native,
    validate_credential,
};
use trusted_server_core::settings::Settings;

/// One generated native fixture. Values here are deliberately public test data.
pub struct NativeFixture {
    /// Fixed credential-role selector associated with this fixture.
    pub descriptor_id: String,
    /// Generated native identity and declarations for the fictional credential.
    pub native: NativeBinding,
    /// Public fictional credential value, never a production provisioning input.
    pub value: String,
}

/// Compile local physical fixtures using the same compiler and renderer as TS.
///
/// Equal compiled requests share one native fixture. Owning-field rows remain
/// separate in the production compiler; only physical output is coalesced here.
///
/// # Errors
/// Returns a safe message if configuration or native rendering fails, or if
/// different requests or values collide at a final native identity.
///
/// # Examples
/// ```
/// use trusted_server_core::settings::Settings;
/// use trusted_server_integration_tests::native_fixtures;
/// let fixtures = native_fixtures(&Settings::default(), "axum")
///     .expect("should compile fictional app credential fixtures");
/// assert_eq!(fixtures.len(), 2);
/// ```
pub fn native_fixtures(settings: &Settings, adapter: &str) -> Result<Vec<NativeFixture>, String> {
    let plan = compile_bindings(settings).map_err(|_| "invalid fixture binding context")?;
    let fixtures = plan
        .entries()
        .iter()
        .map(|entry| {
            let value = match entry.descriptor_id() {
                "publisher.proxy" => "integration-test-proxy-secret-32-bytes-ok".to_owned(),
                "ec.passphrase" => "integration-test-ec-secret-padded-32".to_owned(),
                "handler.password" => "integration-admin-password-32-bytes-ok".to_owned(),
                "ec.inbound" => match entry.subject().get(1).map(String::as_str) {
                    Some("inttest.example.com") => {
                        "integration-test-token-alpha-32-bytes-ok".to_owned()
                    }
                    Some("inttest2.example.com") => {
                        "integration-test-token-bravo-32-bytes-ok".to_owned()
                    }
                    _ => format!("fictional-inbound-{}", entry.key()),
                },
                "ec.pull" | "tinybird.auction" => "fictional-export-token-32-bytes-ok".to_owned(),
                "clientip.shared" => "fictional-client-ip-shared-secret-32-bytes".to_owned(),
                "datadome.server" => "0123456789ABCDEF0123456789ABCDEF".to_owned(),
                "datadome.bypass" => "fictional-datadome-bypass-32-bytes-ok".to_owned(),
                "s3.access-id" => "AKIAFICTIONALTESTONLY".to_owned(),
                "s3.secret" => "fictional-s3-signing-secret-40-bytes-long-ok".to_owned(),
                "s3.session" => "fictional-s3-session-token-32-bytes-ok".to_owned(),
                _ => return Err("unsupported fictional fixture role".to_owned()),
            };
            validate_credential(entry.scope(), value.as_bytes())
                .map_err(|_| "invalid fictional fixture credential")?;
            let native = render_native(adapter, SECRET_STORE_ID, entry.key())
                .map_err(|_| "invalid fixture native identity")?;
            Ok((
                entry,
                NativeFixture {
                    descriptor_id: entry.descriptor_id().to_owned(),
                    native,
                    value,
                },
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    coalesce_native_fixtures(fixtures)
}

fn coalesce_native_fixtures<'a>(
    fixtures: impl IntoIterator<Item = (&'a CompiledBinding, NativeFixture)>,
) -> Result<Vec<NativeFixture>, String> {
    let mut requests: BTreeMap<String, (&CompiledBinding, usize)> = BTreeMap::new();
    let mut physical: Vec<NativeFixture> = Vec::new();
    for (entry, fixture) in fixtures {
        if let Some((previous, index)) = requests.get(&fixture.native.native_name) {
            // An owning array index is not part of a request's authorization.
            // Compare all executable context and native store/key information,
            // never just the derived key or final native name.
            let same_request = previous.same_authorization(entry);
            let existing = &physical[*index];
            if !same_request
                || existing.descriptor_id != fixture.descriptor_id
                || existing.native != fixture.native
                || existing.value != fixture.value
            {
                return Err("conflicting native fixture identity".to_owned());
            }
            continue;
        }
        requests.insert(fixture.native.native_name.clone(), (entry, physical.len()));
        physical.push(fixture);
    }
    Ok(physical)
}

/// Render Viceroy secret blocks without maintaining physical key strings.
///
/// # Errors
/// Returns a safe message if the production compiler rejects the fixture.
///
/// # Examples
/// ```
/// use trusted_server_core::settings::Settings;
/// use trusted_server_integration_tests::fastly_fixture_blocks;
/// let blocks = fastly_fixture_blocks(&Settings::default())
///     .expect("should render fictional native fixture blocks");
/// assert!(blocks.contains("local_server.secret_stores.trusted_server_secrets"));
/// ```
pub fn fastly_fixture_blocks(settings: &Settings) -> Result<String, String> {
    let mut output = String::new();
    for fixture in native_fixtures(settings, "fastly")? {
        output.push_str(&format!(
            "\n[[local_server.secret_stores.{SECRET_STORE_ID}]]\nkey = {}\ndata = {}\n",
            toml::Value::String(fixture.native.native_name),
            toml::Value::String(fixture.value),
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::config::TrustedServerAppConfig;

    fn equal_handler_settings() -> Settings {
        let config: TrustedServerAppConfig = toml::from_str(include_str!(
            "../fixtures/configs/secret-bindings.all-roles.toml"
        ))
        .expect("should parse the all-role fixture before duplicating its handler");
        let mut settings = config.into_settings();
        settings.handlers.push(settings.handlers[0].clone());
        settings
    }

    #[test]
    fn equal_handler_owners_share_one_physical_fixture_on_every_adapter() {
        let settings = equal_handler_settings();
        let plan = compile_bindings(&settings).expect("should compile both equal handler owners");
        assert_eq!(plan.entries().len(), 13);
        let handlers = plan
            .entries()
            .iter()
            .filter(|entry| entry.descriptor_id() == "handler.password")
            .collect::<Vec<_>>();
        assert_eq!(handlers.len(), 2);
        assert_ne!(handlers[0].field_path(), handlers[1].field_path());
        assert_eq!(handlers[0].key(), handlers[1].key());
        for adapter in ["axum", "fastly", "cloudflare", "spin"] {
            let fixtures = native_fixtures(&settings, adapter)
                .expect("should coalesce equal physical requests without losing compiler owners");
            assert_eq!(
                fixtures.len(),
                12,
                "one fixture per exact request on {adapter}"
            );
            assert_eq!(
                fixtures
                    .iter()
                    .filter(|fixture| fixture.descriptor_id == "handler.password")
                    .count(),
                1
            );
        }
    }

    #[test]
    fn colliding_native_names_require_equal_context_store_key_and_value() {
        let mut settings = equal_handler_settings();
        settings.handlers[1].username = "other-admin".to_owned().into();
        let plan = compile_bindings(&settings).expect("should compile distinct handler subjects");
        let handlers = plan
            .entries()
            .iter()
            .filter(|entry| entry.descriptor_id() == "handler.password")
            .collect::<Vec<_>>();
        let fixtures =
            native_fixtures(&settings, "spin").expect("should render distinct handler fixtures");
        let original = fixtures
            .iter()
            .find(|fixture| fixture.native.key == handlers[0].key())
            .expect("should find the first handler physical fixture");
        let copy = || NativeFixture {
            descriptor_id: original.descriptor_id.clone(),
            native: original.native.clone(),
            value: original.value.clone(),
        };
        assert!(
            coalesce_native_fixtures([(handlers[0], copy()), (handlers[1], copy())]).is_err(),
            "different compiled contexts must not coalesce at a forced equal native name"
        );
        for changed in ["store", "key", "value"] {
            let mut conflicting = copy();
            match changed {
                "store" => conflicting.native.store_id = "another_store".to_owned(),
                "key" => conflicting.native.key = "another_key".to_owned(),
                "value" => conflicting.value = "another-fictional-value".to_owned(),
                _ => unreachable!("should only mutate the listed native request components"),
            }
            assert!(
                coalesce_native_fixtures([(handlers[0], copy()), (handlers[0], conflicting)])
                    .is_err(),
                "a conflicting {changed} must not coalesce at an equal native name"
            );
        }
    }

    #[test]
    fn all_twelve_roles_generate_valid_fictional_native_inputs_on_every_adapter() {
        let config: TrustedServerAppConfig = toml::from_str(include_str!(
            "../fixtures/configs/secret-bindings.all-roles.toml"
        ))
        .expect("should deserialize the all-role feature fixture");
        let settings = config.into_settings();
        for adapter in ["axum", "fastly", "cloudflare", "spin"] {
            let fixtures = native_fixtures(&settings, adapter)
                .expect("should compile valid native fixtures for the selected adapter");
            assert_eq!(fixtures.len(), 12, "all roles must be active for {adapter}");
            let names = fixtures
                .iter()
                .map(|fixture| &fixture.native.native_name)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                names.len(),
                12,
                "distinct contexts must retain distinct native identities"
            );
            if adapter == "spin" {
                assert!(
                    fixtures
                        .iter()
                        .all(|fixture| fixture.native.declarations.len() == 2)
                );
            }
        }
    }
}
