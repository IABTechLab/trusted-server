//! Fictional native credentials compiled through the production binding policy.
//!
//! These public local values are not provisioning inputs for deployed resources.

use trusted_server_core::secret_bindings::{
    NativeBinding, SECRET_STORE_ID, compile_bindings, render_native, validate_credential,
};
use trusted_server_core::settings::Settings;

/// One generated native fixture. Values here are deliberately public test data.
pub struct NativeFixture {
    pub descriptor_id: String,
    pub native: NativeBinding,
    pub value: String,
}

/// Compile local fixture identities using the same compiler and renderer as TS.
///
/// # Errors
/// Returns a safe message if configuration or native rendering fails.
pub fn native_fixtures(settings: &Settings, adapter: &str) -> Result<Vec<NativeFixture>, String> {
    let plan = compile_bindings(settings).map_err(|_| "invalid fixture binding context")?;
    plan.entries()
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
            Ok(NativeFixture {
                descriptor_id: entry.descriptor_id().to_owned(),
                native,
                value,
            })
        })
        .collect()
}

/// Render Viceroy secret blocks without maintaining physical key strings.
///
/// # Errors
/// Returns a safe message if the production compiler rejects the fixture.
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

    #[test]
    fn all_twelve_roles_generate_valid_fictional_native_inputs_on_every_adapter() {
        let config: TrustedServerAppConfig = toml::from_str(include_str!(
            "../fixtures/configs/secret-bindings.all-roles.toml"
        ))
        .expect("valid all-role fixture");
        let settings = config.into_settings();
        for adapter in ["axum", "fastly", "cloudflare", "spin"] {
            let fixtures =
                native_fixtures(&settings, adapter).expect("compiled valid native fixtures");
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
