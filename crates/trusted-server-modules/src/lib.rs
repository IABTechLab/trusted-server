//! The modules a stock build of Trusted Server ships from crates of their own.
//!
//! Every adapter and the `ts` command line tool take this list, so one place
//! says which of those modules a stock build offers and in what order their
//! hooks run. Offering a module does not run it, because the registry builds a
//! module only when a section of the settings selects it.
//!
//! A deployment that ships a module of its own hands that module's builder to
//! its adapter, which runs it after these.

use trusted_server_core::integrations::IntegrationBuilder;

/// The builders of the modules a stock build ships from crates of their own,
/// in hook order.
#[must_use]
pub fn builders() -> Vec<IntegrationBuilder> {
    vec![trusted_server_cmp_osano::builder()]
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
            ["cmp.osano"],
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
}
