//! What the module builders the Axum adapter links give a deployment, proven
//! from outside the adapter: each module's own deploy rules and secret
//! settings, the diagnostics module's request preparer, the middleware the
//! template and the guide document, the ad server's decision between planned
//! bids, and the maintainers every crate outside core names.

#![allow(clippy::panic, reason = "tests use panic-on-failure helpers")]

use trusted_server_adapter_axum::app::shipped_module_builders;
use trusted_server_core::integrations::{
    IntegrationRegistry, SuppliedMiddleware, supplied_middleware,
};
use trusted_server_core::middleware::{HTML_MEDIA_TYPE, MiddlewarePhase};

/// The middleware the linked modules and core supply, with the integration
/// that supplies each, in the order the registry registers them.
fn listed_middleware() -> Vec<(&'static str, SuppliedMiddleware)> {
    supplied_middleware(&shipped_module_builders())
}

/// The `ts` tool registers the builders it links, so validating a
/// deployment's settings before a push runs each module's own rules.
#[test]
fn deploy_validation_runs_a_linked_module_s_own_rules() {
    use serde_json::json;
    use trusted_server_core::config::{TrustedServerAppConfig, register_deploy_integrations};
    use trusted_server_core::test_support::tests::create_test_settings;

    register_deploy_integrations(shipped_module_builders());

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
        .expect_err("should refuse a table the module's own rules reject");

    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("[cmp.osano]"),
        "should name the module's table: {rendered}"
    );
}

/// The `ts` tool registers the builders it links, so the secret settings a
/// module declares are among the leaves a push treats as naming a key.
#[test]
fn a_linked_module_s_secret_settings_are_listed_once_the_list_is_registered() {
    use edgezero_core::app_config::AppConfigMeta as _;
    use trusted_server_core::config::{TrustedServerAppConfig, register_deploy_integrations};

    register_deploy_integrations(shipped_module_builders());

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

/// Every documented table should be push-ready, so uncommenting its
/// section's selection and the table with the shown values must parse and
/// pass field validation. Tables that ship a deliberately invalid
/// placeholder that is not a secret (the Google Tag Manager `container_id`)
/// are left out.
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

/// Every linked page module refuses a setting it does not know, so a
/// misspelt key in its table fails deploy validation naming the table and
/// the key, where it would otherwise be ignored.
#[test]
fn every_linked_module_rejects_a_setting_it_does_not_know() {
    use serde_json::json;
    use trusted_server_core::config::validate_settings_for_deploy_with;
    use trusted_server_core::module_name::short_form;
    use trusted_server_core::test_support::tests::create_test_settings;

    let linked = shipped_module_builders();
    for builder in linked
        .iter()
        .filter(|builder| builder.supplies_integration())
    {
        let name = builder
            .module_name()
            .expect("every linked page module names itself");
        let section = builder
            .section()
            .expect("every linked page module has a section");
        let mut settings = create_test_settings();
        settings
            .insert_module_config(section, name, &json!({ "no_such_setting": true }))
            .expect("should insert the planted table");

        let error = match validate_settings_for_deploy_with(&settings, &linked) {
            Ok(()) => panic!("`{name}` should refuse a setting it does not know"),
            Err(error) => format!("{error:?}"),
        };
        let written = short_form(section, name);
        assert!(
            error.contains(&format!("[{section}.{written}]")) && error.contains("no_such_setting"),
            "`{name}` should name its table and the unknown setting: {error}"
        );
    }
}

/// The adapter runs the registry's request preparers and names no module, so
/// the linked list is what attaches the diagnostics module's preparer. It
/// strips the module's reserved query and cookie in a deployment that does
/// not select the module, as it does in one that does.
#[test]
fn the_linked_list_attaches_the_diagnostics_module_s_request_preparer() {
    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::http::{Method, header, request_builder};
    use trusted_server_core::test_support::tests::create_test_settings;

    let settings = create_test_settings();
    let registry = IntegrationRegistry::with_registrations(&settings, &shipped_module_builders())
        .expect("should build registry");
    let mut request = request_builder()
        .method(Method::GET)
        .uri("https://publisher.example.com/article?ts_console=1&keep=yes")
        .header(header::COOKIE, "__Host-ts-console=1; keep-me=yes")
        .body(EdgeBody::empty())
        .expect("should build request");

    registry
        .prepare_request(&settings, &mut request)
        .expect("should run the linked preparers");

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

/// The template's entries name every middleware the linked modules and core
/// supply, in the order the registry registers them, which is the order a
/// deployment that runs them all names them in.
#[test]
fn the_template_s_entries_name_the_listed_middleware() {
    use trusted_server_core::settings::Settings;
    use trusted_server_core::test_support::template::{
        template_with_resolved_required_secrets, uncomment_block,
    };

    let template = uncomment_block(
        &uncomment_block(&template_with_resolved_required_secrets(), "[[fetch]]"),
        "[[serve]]",
    );
    let settings =
        Settings::from_toml(&template).expect("should load the template with its entries");
    let listed = listed_middleware();

    for phase in MiddlewarePhase::ALL {
        let entries = settings.phase_entries(phase).entries();
        let mut media_types: Vec<&str> = Vec::new();
        for (_, row) in listed.iter().filter(|(_, row)| row.phase == phase) {
            if !media_types.contains(&row.media_type) {
                media_types.push(row.media_type);
            }
        }
        for media_type in media_types {
            let named_in_list: Vec<&str> = listed
                .iter()
                .filter(|(_, row)| row.phase == phase && row.media_type == media_type)
                .map(|(_, row)| row.name)
                .collect();
            let covering: Vec<_> = entries
                .iter()
                .filter(|entry| entry.media_type == media_type)
                .collect();
            if media_type == HTML_MEDIA_TYPE || covering.iter().any(|entry| entry.path.is_none()) {
                // One entry covers every page, for HTML and for a media
                // type a page's own responses come in.
                assert_eq!(
                    covering.len(),
                    1,
                    "should document one [[{phase}]] entry for {media_type}"
                );
                assert!(
                    covering[0].path.is_none(),
                    "should document the [[{phase}]] entry that covers every page"
                );
            } else {
                // A file's middleware is placed on the file's own path, one
                // entry for each file.
                assert!(
                    !covering.is_empty(),
                    "should document a [[{phase}]] entry on a path for each {media_type} file"
                );
            }
            let named: Vec<&str> = covering
                .iter()
                .flat_map(|entry| entry.middleware.iter().map(String::as_str))
                .collect();
            assert_eq!(
                named, named_in_list,
                "should document every {phase} middleware the linked modules supply for \
                 {media_type}, in the listed order"
            );
        }
    }
}

/// The guide's table of middleware lists what the linked modules and core
/// supply, each with its phase, in the order the registry registers them.
#[test]
fn the_guide_s_table_lists_the_same_middleware() {
    const GUIDE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/guide/configuration.md"
    ));

    // A row of the table is a name and then the list it is written in.
    let documented: Vec<(String, String)> = GUIDE
        .lines()
        .filter_map(|line| {
            let mut cells = line.split('|').map(str::trim).skip(1);
            let name = cells.next()?.strip_prefix('`')?.strip_suffix('`')?;
            let list = cells.next()?.strip_prefix("`[[")?.strip_suffix("]]`")?;
            Some((name.to_owned(), list.to_owned()))
        })
        .collect();
    let listed: Vec<(String, String)> = listed_middleware()
        .iter()
        .map(|(_, row)| (row.name.to_owned(), row.phase.to_string()))
        .collect();

    assert_eq!(
        documented, listed,
        "should document the middleware the linked modules supply, each with its phase, in \
         the listed order"
    );
}

/// The ad server decides between the bids APS returned, and the winner
/// keeps the seat the exchange returned, APS's own bidder name and its
/// renderer.
#[tokio::test]
async fn the_ad_server_keeps_an_aps_winner_s_identities_and_renderer() {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    use trusted_server_ad_server_iabtl::{AdServerMockProvider, AdServerMockSettings};
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
    let plan = trusted_server_core::auction::compile_auction_plan_with(
        &settings,
        &shipped_module_builders(),
    )
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
    let inbound = edgezero_core::http::Request::new(edgezero_core::body::Body::empty());
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
/// are read from the source tree.
#[test]
fn every_crate_outside_core_declares_its_maintainers() {
    const STATUSES: [&str; 3] = ["vendor owned", "seeking vendor owner", "project owned"];

    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("should have the crates folder above this crate");
    let mut manifests = Vec::new();
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
