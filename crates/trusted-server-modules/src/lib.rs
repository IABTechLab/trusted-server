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
        // Prebid registers from the auction plan, and what it registers runs
        // ahead of the modules below.
        trusted_server_auction_prebid::builder(),
        trusted_server_testing_testlight::builder(),
        trusted_server_framework_nextjs::builder(),
        trusted_server_audience_permutive::builder(),
        trusted_server_identity_lockr::builder(),
        trusted_server_cmp_didomi::builder(),
        trusted_server_cmp_sourcepoint::builder(),
        trusted_server_cmp_osano::builder(),
        trusted_server_tag_google_tag_manager::builder(),
        trusted_server_bot_protection_datadome::builder(),
        trusted_server_ad_tag_google::builder(),
        trusted_server_ad_tag_google::diagnostics::builder(),
        // Implementations `[demand]` and `[ad-server]` can name, which no
        // section selects.
        trusted_server_auction_protocol_openrtb::builder(),
        trusted_server_auction_prebid_server::builder(),
        trusted_server_auction_aps::builder(),
        trusted_server_ad_server_mock::builder(),
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

/// The section that selects the stock module with the integration id `id`,
/// and the name the module is written under there.
///
/// A tool that knows an integration by its id, as the `ts` audit does from
/// what it finds on a page, writes a configuration with this and names no
/// module itself.
#[must_use]
pub fn selection_of(id: &str) -> Option<(&'static str, &'static str)> {
    let builder = builders().into_iter().find(|builder| builder.id() == id)?;
    let section = builder.section()?;
    Some((
        section,
        trusted_server_core::module_name::short_form(section, builder.module_name()?),
    ))
}

#[cfg(test)]
mod tests {
    use trusted_server_core::integrations::IntegrationBuilder;

    use super::{builders, builders_with, selection_of};

    #[test]
    fn stock_modules_are_offered_in_hook_order() {
        // A module is named by what its section selects, and an
        // implementation by what `[demand]` or `[ad-server]` names.
        let names: Vec<&str> = builders()
            .iter()
            .map(|builder| {
                builder
                    .module_name()
                    .or_else(|| builder.demand().map(|demand| demand.id))
                    .or_else(|| builder.adserver().map(|adserver| adserver.id))
                    .unwrap_or_else(|| builder.id())
            })
            .collect();

        assert_eq!(
            names,
            [
                "auction.prebid",
                "testing.testlight",
                "framework.nextjs",
                "audience.permutive",
                "identity.lockr",
                "cmp.didomi",
                "cmp.sourcepoint",
                "cmp.osano",
                "tag.google-tag-manager",
                "bot-protection.datadome",
                "ad-tag.google",
                "ad-tag.google.diagnostics",
                "auction-protocol.openrtb",
                "auction.prebid-server",
                "auction.aps",
                "ad-server.mock",
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

    #[test]
    fn a_stock_module_s_selection_is_found_by_its_integration_id() {
        for (id, section, written) in [
            ("prebid", "auction", "prebid"),
            ("datadome", "bot-protection", "datadome"),
            ("didomi", "cmp", "didomi"),
            ("sourcepoint", "cmp", "sourcepoint"),
            ("osano", "cmp", "osano"),
            ("lockr", "identity", "lockr"),
            ("permutive", "audience", "permutive"),
            ("nextjs", "framework", "nextjs"),
            ("gpt", "ad-tag", "google"),
            ("google_tag_manager", "tag", "google-tag-manager"),
            // Testlight says `[auction]` selects it, where its full name is
            // written.
            ("testlight", "auction", "testing.testlight"),
        ] {
            assert_eq!(
                selection_of(id),
                Some((section, written)),
                "should find where `{id}` is selected"
            );
        }
        assert_eq!(
            selection_of("no-such-integration"),
            None,
            "should find nothing for an id no stock module has"
        );
    }

    /// The `ts` tool registers the stock builders, so the secret settings a
    /// stock module declares are among the leaves a push treats as naming a
    /// key.
    #[test]
    fn a_stock_module_s_secret_settings_are_listed_once_the_list_is_registered() {
        use edgezero_core::app_config::AppConfigMeta as _;
        use trusted_server_core::config::{TrustedServerAppConfig, register_deploy_integrations};

        register_deploy_integrations(builders());

        let paths = TrustedServerAppConfig::secret_fields()
            .iter()
            .map(edgezero_core::app_config::SecretField::dotted_path)
            .collect::<Vec<_>>();

        for expected in [
            "bot-protection.datadome.server_side_key_secret_name",
            "bot-protection.datadome.protection_test_bypass.credential_secret_name",
        ] {
            assert!(
                paths.iter().any(|path| path == expected),
                "should list `{expected}` among {paths:?}"
            );
        }
    }

    /// The mock ad server decides between the bids APS returned, and the
    /// winner keeps the seat the exchange returned, APS's own bidder name and
    /// its renderer.
    #[tokio::test]
    async fn the_mock_ad_server_keeps_an_aps_winner_s_identities_and_renderer() {
        use std::collections::{BTreeMap, HashMap};
        use std::sync::Arc;

        use trusted_server_ad_server_mock::{AdServerMockProvider, AdServerMockSettings};
        use trusted_server_core::auction::orchestrator::AuctionOrchestrator;
        use trusted_server_core::auction::types::{
            AdFormat, AdSlot, AuctionContext, AuctionRequest, MediaType, PublisherInfo, UserInfo,
        };
        use trusted_server_core::platform::test_support::{
            StubHttpClient, build_services_with_http_client,
        };
        use trusted_server_core::provider_table::ProviderList;
        use trusted_server_core::test_support::tests::create_test_settings;

        let http = Arc::new(StubHttpClient::new());
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "upstream-seat", "bid": [{
                    "id": "aps-bid", "impid": "fictional-slot", "price": 2.0,
                    "w": 300, "h": 250,
                    "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}
                }]}]
            }))
            .expect("should serialize APS response"),
        );
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "aps_instance", "bid": [{
                    "id": "adserver-aps", "impid": "fictional-slot", "price": 2.0,
                    "adm": "ignored", "w": 300, "h": 250, "crid": "aps-creative"
                }]}]
            }))
            .expect("should serialize adserver response"),
        );
        let services = build_services_with_http_client(Arc::clone(&http) as Arc<_>);

        let mut settings = create_test_settings();
        settings.auction.enabled = true;
        settings.auction.timeout_ms = 777;
        let serde_json::Value::Object(aps) = serde_json::json!({
            "implementation": "auction.aps",
            "endpoint": "https://aps.example/e/pb/bid",
            "account_id": "example-account",
            "timeout_ms": 1000,
            "routing": "all_eligible",
        }) else {
            panic!("should build the APS table");
        };
        settings.demand = ProviderList::new(
            vec!["aps_instance".to_string()],
            BTreeMap::from([("aps_instance".to_string(), aps)]),
        );
        let plan = trusted_server_core::auction::compile_auction_plan_with(&settings, &builders())
            .expect("should compile the planned APS auction");
        let adserver = AdServerMockProvider::new(
            "adserver_mock",
            AdServerMockSettings {
                endpoint: "https://adserver.example/mediate".to_string(),
                timeout_ms: 500,
                ..AdServerMockSettings::default()
            },
        );
        let orchestrator = AuctionOrchestrator::from_plan(Arc::new(plan), Some(Arc::new(adserver)));
        let request = AuctionRequest {
            id: "fictional-auction".to_string(),
            slots: vec![AdSlot {
                id: "fictional-slot".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: Some(1.0),
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "publisher.example".to_string(),
                page_url: Some("https://publisher.example/article".to_string()),
            },
            user: UserInfo {
                id: None,
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        };
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should decide planned APS bid");

        let provider_bid = &result.provider_responses[0].bids[0];
        assert_eq!(result.provider_responses[0].provider, "aps_instance");
        assert_eq!(provider_bid.returned_seat.as_deref(), Some("upstream-seat"));
        assert_eq!(provider_bid.bidder, "aps");
        let winner = &result.winning_bids["fictional-slot"];
        assert_eq!(winner.returned_seat.as_deref(), Some("upstream-seat"));
        assert_eq!(winner.bidder, "aps");
        assert!(winner.renderer.is_some());
        assert!(winner.creative.is_none());
        assert_eq!(
            result
                .adserver_response
                .as_ref()
                .map(|response| response.provider.as_str()),
            Some("adserver_mock")
        );
    }

    /// Every crate outside core says who maintains it, in the
    /// `[package.metadata.maintainers]` table of its manifest. The manifests
    /// are read from the source tree, which a Wasm test run cannot reach.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn every_crate_outside_core_declares_its_maintainers() {
        const STATUSES: [&str; 3] = ["vendor owned", "seeking vendor owner", "project owned"];

        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("should have the crates folder above this crate");
        let mut manifests = vec![crates.join("trusted-server-modules").join("Cargo.toml")];
        for entry in std::fs::read_dir(crates).expect("should list the crates folder") {
            let type_folder = entry.expect("should read a folder entry").path();
            // A crate directly under `crates/` is core, an adapter or a tool.
            // A module crate is one folder further down, under its type.
            if !type_folder.is_dir() || type_folder.join("Cargo.toml").is_file() {
                continue;
            }
            for entry in std::fs::read_dir(&type_folder).expect("should list a type folder") {
                let manifest = entry
                    .expect("should read a folder entry")
                    .path()
                    .join("Cargo.toml");
                if manifest.is_file() {
                    manifests.push(manifest);
                }
            }
        }
        assert!(
            manifests.len() > 20,
            "should find the module crates, found {}",
            manifests.len()
        );

        for manifest in manifests {
            let text = std::fs::read_to_string(&manifest).expect("should read a manifest");
            let Some((_, after)) = text.split_once("[package.metadata.maintainers]") else {
                panic!("{} should declare its maintainers", manifest.display());
            };
            let table = after.split("\n[").next().unwrap_or_default();
            let value = |key: &str| {
                table.lines().find_map(|line| {
                    line.strip_prefix(key)?
                        .trim_start()
                        .strip_prefix('=')?
                        .trim()
                        .strip_prefix('"')?
                        .strip_suffix('"')
                })
            };
            assert!(
                value("owner").is_some_and(|owner| !owner.is_empty()),
                "{} should name an owner",
                manifest.display()
            );
            let status = value("status").unwrap_or_default();
            assert!(
                STATUSES.contains(&status),
                "{} should state one of {STATUSES:?}, and states `{status}`",
                manifest.display()
            );
        }
    }
}
