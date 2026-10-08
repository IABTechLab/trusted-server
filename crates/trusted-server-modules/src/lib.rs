//! The modules a stock build of Trusted Server ships from crates of their own.
//!
//! Every adapter and the `ts` command line tool take this list, so one place
//! says which of those modules a stock build offers and in what order their
//! hooks run. Offering a module does not run it, because the registry builds a
//! module only when a section of the settings selects it.
//!
//! A deployment that ships a module of its own hands that module's builder to
//! its adapter, which runs it after these.

#![cfg_attr(
    test,
    allow(clippy::panic, reason = "tests use panic-on-failure helpers")
)]

use trusted_server_core::integrations::IntegrationBuilder;

/// The builders of the modules a stock build ships from crates of their own,
/// in hook order.
#[must_use]
pub fn builders() -> Vec<IntegrationBuilder> {
    vec![
        trusted_server_testing_testlight::builder(),
        trusted_server_framework_nextjs::builder(),
        trusted_server_audience_permutive::builder(),
        trusted_server_identity_lockr::builder(),
        trusted_server_cmp_didomi::builder(),
        trusted_server_cmp_sourcepoint::builder(),
        trusted_server_cmp_osano::builder(),
        trusted_server_tag_google_tag_manager::builder(),
        trusted_server_ad_tag_google::builder(),
        trusted_server_ad_tag_google::diagnostics::builder(),
    ]
}

/// The stock builders followed by `extra`, the builders a deployment added,
/// which is the order their hooks run in.
#[must_use]
pub fn builders_with(extra: &[IntegrationBuilder]) -> Vec<IntegrationBuilder> {
    let mut all = builders();
    all.extend_from_slice(extra);
    all
}

#[cfg(test)]
mod tests {
    use trusted_server_core::integrations::IntegrationBuilder;

    use super::{builders, builders_with};

    #[test]
    fn stock_modules_are_offered_in_hook_order() {
        let names: Vec<&str> = builders()
            .iter()
            .filter_map(IntegrationBuilder::module_name)
            .collect();

        assert_eq!(
            names,
            [
                "testing.testlight",
                "framework.nextjs",
                "audience.permutive",
                "identity.lockr",
                "cmp.didomi",
                "cmp.sourcepoint",
                "cmp.osano",
                "tag.google-tag-manager",
                "ad-tag.google",
                "ad-tag.google.diagnostics",
            ],
            "should offer the stock modules in the order their hooks run"
        );
    }

    #[test]
    fn a_deployment_s_builders_follow_the_stock_ones() {
        let stock = builders().len();
        let extra = [trusted_server_cmp_osano::builder()];

        let all = builders_with(&extra);

        assert_eq!(
            all.len(),
            stock + 1,
            "should keep every stock builder and add the deployment's"
        );
        assert_eq!(
            all.last().map(IntegrationBuilder::source),
            Some("trusted-server-cmp-osano"),
            "should put the deployment's builder last"
        );
    }

    /// The `ts` tool registers the stock builders, so validating a deployment's
    /// settings before a push runs each stock module's own rules.
    #[test]
    fn deploy_validation_runs_a_stock_module_s_own_rules() {
        use serde_json::json;
        use trusted_server_core::config::{TrustedServerAppConfig, register_deploy_integrations};
        use trusted_server_core::test_support::tests::create_test_settings;

        register_deploy_integrations(builders());

        TrustedServerAppConfig::new(create_test_settings())
            .expect("should accept the settings before a module's table is wrong");

        let mut settings = create_test_settings();
        settings
            .insert_module_config(
                "cmp",
                trusted_server_cmp_osano::MODULE,
                &json!({ "typo": true }),
            )
            .expect("should insert the module's table");

        let error = TrustedServerAppConfig::new(settings)
            .expect_err("should refuse a table the stock module's own rules reject");

        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("[cmp.osano]"),
            "should name the module's table: {rendered}"
        );
    }

    /// Every documented table should be push-ready, so uncommenting its
    /// section's selection and the table with the shown values must parse and
    /// pass field validation. Tables that ship a deliberately invalid
    /// placeholder that is not a secret (the Google Tag Manager
    /// `container_id`) are left out.
    #[test]
    fn documented_module_tables_validate_when_uncommented_and_selected() {
        use trusted_server_audience_permutive as permutive;
        use trusted_server_cmp_sourcepoint as sourcepoint;
        use trusted_server_core::settings::Settings;
        use trusted_server_core::test_support::template::{
            template_with_resolved_required_secrets, uncomment_block,
        };
        use trusted_server_identity_lockr as lockr;

        let base = template_with_resolved_required_secrets();

        for (section, selection, header, name) in [
            (
                "[audience]",
                "module = \"permutive\"",
                "[audience.permutive]",
                permutive::MODULE,
            ),
            (
                "[identity]",
                "module = \"lockr\"",
                "[identity.lockr]",
                lockr::MODULE,
            ),
            (
                "[cmp]",
                "module = \"sourcepoint\"",
                "[cmp.sourcepoint]",
                sourcepoint::MODULE,
            ),
        ] {
            let toml = format!(
                "{}\n{section}\n{selection}\n",
                uncomment_block(&base, header)
            );
            let settings = Settings::from_toml(&toml)
                .unwrap_or_else(|err| panic!("uncommented {header} should parse: {err:?}"));

            let valid = match name {
                permutive::MODULE => settings
                    .module_config::<permutive::PermutiveConfig>(name)
                    .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                    .is_some(),
                lockr::MODULE => settings
                    .module_config::<lockr::LockrConfig>(name)
                    .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                    .is_some(),
                _ => settings
                    .module_config::<sourcepoint::SourcepointConfig>(name)
                    .unwrap_or_else(|err| panic!("{header} should validate: {err:?}"))
                    .is_some(),
            };
            assert!(valid, "{header} should resolve to a valid config");
        }
    }

    /// Every stock page module refuses a setting it does not know, so a
    /// misspelt key in its table fails deploy validation naming the table and
    /// the key, where it would otherwise be ignored.
    #[test]
    fn every_stock_module_rejects_a_setting_it_does_not_know() {
        use serde_json::json;
        use trusted_server_core::config::validate_settings_for_deploy_with;
        use trusted_server_core::module_name::short_form;
        use trusted_server_core::test_support::tests::create_test_settings;

        let stock = builders();
        for builder in stock
            .iter()
            .filter(|builder| builder.supplies_integration())
        {
            let name = builder
                .module_name()
                .expect("every stock page module names itself");
            let section = builder
                .section()
                .expect("every stock page module has a section");
            let mut settings = create_test_settings();
            settings
                .insert_module_config(section, name, &json!({ "no_such_setting": true }))
                .expect("should insert the planted table");

            let error = match validate_settings_for_deploy_with(&settings, &stock) {
                Ok(()) => panic!("`{name}` should refuse a setting it does not know"),
                Err(error) => format!("{error:?}"),
            };
            let written = short_form(section, name);
            assert!(
                error.contains(&format!("[{section}.{written}]"))
                    && error.contains("no_such_setting"),
                "`{name}` should name its table and the unknown setting: {error}"
            );
        }
    }

    /// The adapters run the registry's request preparers and name no module,
    /// so this list is what attaches the diagnostics module's preparer. It
    /// strips the module's reserved query and cookie in a deployment that
    /// does not select the module, as it does in one that does.
    #[test]
    fn the_stock_list_attaches_the_diagnostics_module_s_request_preparer() {
        use edgezero_core::body::Body as EdgeBody;
        use http::{Method, Request, header};
        use trusted_server_core::integrations::IntegrationRegistry;
        use trusted_server_core::test_support::tests::create_test_settings;

        let settings = create_test_settings();
        let registry = IntegrationRegistry::with_registrations(&settings, &builders())
            .expect("should build registry");
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/article?ts_console=1&keep=yes")
            .header(header::COOKIE, "__Host-ts-console=1; keep-me=yes")
            .body(EdgeBody::empty())
            .expect("should build request");

        registry
            .prepare_request(&settings, &mut request)
            .expect("should run the stock preparers");

        assert_eq!(
            request.uri().query(),
            Some("keep=yes"),
            "should strip the reserved diagnostics query and keep the rest"
        );
        assert_eq!(
            request
                .headers()
                .get(header::COOKIE)
                .map(|value| value.to_str().expect("cookie should be text")),
            Some("keep-me=yes"),
            "should strip the reserved diagnostics cookie and keep the rest"
        );
    }
}
