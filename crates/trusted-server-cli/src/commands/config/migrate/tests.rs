use std::fs;

use tempfile::TempDir;

use super::rules::Migration;
use super::{ConfigMigrateArgs, migrate, run_with_writers};
use crate::RunOutcome;
use crate::commands::audit::generate::validate::validate_source_config;

/// Main's own example settings document.
const MAIN_EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/main-trusted-server.example.toml"
));

/// The stack's example settings document, which shows the page entries in
/// the order the modules' page changes run.
const STACK_EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../trusted-server.example.toml"
));

/// The settings every document written for main carries, with hosts that
/// are not the template's placeholders.
const MAIN_BASE: &str = r#"[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
origin_url = "https://origin.publisher.example.com"
proxy_secret = "publisher_proxy_secret"

[ec]
passphrase = "ec_passphrase"
"#;

fn migrated(input: &str) -> Migration {
    let migration = migrate(input).expect("should migrate the document");
    let again = migrate(&migration.text).expect("should migrate the migrated document");
    assert!(
        again.report.changed.is_empty(),
        "migrating a migrated document should change nothing, got {:?}",
        again.report.changed
    );
    assert_eq!(
        again.text, migration.text,
        "migrating a migrated document should leave its text as it was"
    );
    migration
}

fn parsed(text: &str) -> toml::Value {
    toml::from_str(text).unwrap_or_else(|error| panic!("should be TOML: {error}\n{text}"))
}

fn main_document(rest: &str) -> String {
    format!("{MAIN_BASE}\n{rest}")
}

fn reported(lines: &[String], fragment: &str) -> bool {
    lines.iter().any(|line| line.contains(fragment))
}

#[test]
fn publisher_origin_settings_move_to_the_origin_source() {
    let input = r#"[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
# Where the pages come from.
origin_url = "https://origin.publisher.example.com"
origin_host_header_override = "www.publisher.example.com"
proxy_secret = "publisher_proxy_secret"
"#;
    let migration = migrated(input);
    let value = parsed(&migration.text);

    assert_eq!(value["source"]["module"].as_str(), Some("origin"));
    assert_eq!(
        value["source"]["origin"]["url"].as_str(),
        Some("https://origin.publisher.example.com")
    );
    assert_eq!(
        value["source"]["origin"]["host_header"].as_str(),
        Some("www.publisher.example.com")
    );
    let publisher = value["publisher"]
        .as_table()
        .expect("should keep [publisher]");
    assert!(!publisher.contains_key("origin_url"));
    assert!(!publisher.contains_key("origin_host_header_override"));
    assert_eq!(
        publisher["proxy_secret"].as_str(),
        Some("publisher_proxy_secret")
    );
    assert!(
        migration.text.contains(
            "# Where the pages come from.\nurl = \"https://origin.publisher.example.com\""
        ),
        "should keep the comment above the moved setting:\n{}",
        migration.text
    );
    assert!(reported(
        &migration.report.changed,
        "moved `[publisher] origin_url` to `[source.origin] url`"
    ));
}

#[test]
fn a_path_in_the_origin_address_is_reported_to_check() {
    let migration =
        migrated(&MAIN_BASE.replace("origin.publisher.example.com", "storage.example.com/site"));

    assert!(reported(
        &migration.report.check,
        "`[source.origin] url` has a path"
    ));
    let plain = migrated(MAIN_BASE);
    assert!(!reported(&plain.report.check, "has a path"));
}

#[test]
fn a_host_header_override_is_reported_to_check() {
    let migration = migrated(&MAIN_BASE.replacen(
        "proxy_secret =",
        "origin_host_header_override = \"www.publisher.example.com\"\nproxy_secret =",
        1,
    ));

    assert!(
        reported(
            &migration.report.check,
            "`[source.origin] host_header` is set, so the addresses a page gives under that host"
        ),
        "{:?}",
        migration.report.check
    );
    let plain = migrated(MAIN_BASE);
    assert!(!reported(&plain.report.check, "host_header"));
}

#[test]
fn an_asset_route_origin_moves_to_its_source_with_the_signer_renamed() {
    let input = main_document(
        r#"[proxy]

[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://assets.publisher.example.com"
path_pattern = "^/.image/(.*)$"
target_path = "/image/$1"

[proxy.asset_routes.auth]
type = "s3_sig_v4"
region = "us-east-1"
access_key_id = "access_key_id"
secret_access_key = "secret_access_key"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);
    let route = &value["proxy"]["asset_routes"][0];

    assert_eq!(route["prefix"].as_str(), Some("/.image/"));
    assert!(route.get("origin_url").is_none());
    assert!(route.get("auth").is_none());
    assert_eq!(route["source"]["module"].as_str(), Some("origin"));
    assert_eq!(
        route["source"]["origin"]["url"].as_str(),
        Some("https://assets.publisher.example.com")
    );
    assert_eq!(
        route["source"]["origin"]["auth"]["type"].as_str(),
        Some("s3_sigv4")
    );
    assert_eq!(
        route["source"]["origin"]["auth"]["region"].as_str(),
        Some("us-east-1")
    );
    assert!(reported(
        &migration.report.changed,
        "old spelling of the S3 signer"
    ));
}

#[test]
fn a_publisher_origin_auth_table_moves_with_the_origin() {
    let input = r#"[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
origin_url = "https://storage.publisher.example.com/bucket"
proxy_secret = "publisher_proxy_secret"
origin_auth = { type = "s3_sig_v4", region = "us-east-1" }
"#;
    let migration = migrated(input);
    let value = parsed(&migration.text);

    assert_eq!(
        value["source"]["origin"]["auth"]["type"].as_str(),
        Some("s3_sigv4")
    );
    assert!(value["publisher"].get("origin_auth").is_none());
}

#[test]
fn admin_handlers_are_removed_and_any_other_is_left_to_do() {
    let input = main_document(
        r#"[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "handler_password"

[[handlers]]
path = "^/secure"
username = "user"
password = "user_password"

[[handlers]]
path = "^/_ts/admin|^/members"
username = "user"
password = "user_password"

[[handlers]]
path = "^/admin/keys(?:/|$)"
username = "admin"
password = "handler_password"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);
    let handlers = value["handlers"]
        .as_array()
        .expect("should keep two handlers");

    assert_eq!(handlers.len(), 2);
    assert_eq!(handlers[0]["path"].as_str(), Some("^/secure"));
    assert_eq!(handlers[1]["path"].as_str(), Some("^/_ts/admin|^/members"));
    assert!(reported(
        &migration.report.changed,
        "removed `[[handlers]]` entry 1"
    ));
    assert!(reported(
        &migration.report.changed,
        "removed `[[handlers]]` entry 4"
    ));
    assert!(reported(&migration.report.to_do, "`[[handlers]]` entry 2"));
    assert!(reported(&migration.report.to_do, "`[[handlers]]` entry 3"));
}

/// A document written for main with one handler guarding `pattern`.
fn guarded(pattern: &str) -> String {
    main_document(&format!(
        "[[handlers]]\npath = '{pattern}'\nusername = \"admin\"\npassword = \"handler_password\"\n"
    ))
}

#[test]
fn a_handler_is_removed_only_where_every_path_it_matches_is_closed() {
    for pattern in [
        "^/_ts/admin",
        "^/_ts/admin/",
        "^/admin/keys/",
        "^/admin/keys$",
        "^/admin/keys(?:/|$)",
        "^/admin/keys(/|$)",
    ] {
        let migration = migrated(&guarded(pattern));
        assert!(
            parsed(&migration.text).get("handlers").is_none(),
            "should remove the handler on `{pattern}`:\n{}",
            migration.text
        );
        assert!(
            migration.report.to_do.is_empty(),
            "`{pattern}`: {:?}",
            migration.report.to_do
        );
    }
    // Main matched a pattern anywhere in the path, so `^/admin/keys` also
    // guarded `/admin/keystore`, which the stack forwards to the origin.
    for pattern in [
        "^/admin/keys",
        "^/admin/keys.*",
        "^/admin/keysx",
        "^/admin",
        "/_ts/admin",
        "^/admin/keys(?:/|$)|^/members",
    ] {
        let migration = migrated(&guarded(pattern));
        assert_eq!(
            parsed(&migration.text)["handlers"][0]["path"].as_str(),
            Some(pattern),
            "should keep the handler on `{pattern}`"
        );
        assert!(
            reported(
                &migration.report.to_do,
                "`[[handlers]]` entry 1 can guard paths beyond the `/_ts/admin` and \
                 `/admin/keys` paths this release closes"
            ),
            "`{pattern}`: {:?}",
            migration.report.to_do
        );
    }
}

#[test]
fn request_signing_store_ids_are_removed() {
    let input = main_document(
        r#"[request_signing]
enabled = true
config_store_id = "config-store-id"
secret_store_id = "secret-store-id"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert_eq!(value["request_signing"]["enabled"].as_bool(), Some(true));
    assert!(value["request_signing"].get("config_store_id").is_none());
    assert!(value["request_signing"].get("secret_store_id").is_none());
    assert!(reported(&migration.report.check, "ts keys rotate"));
}

#[test]
fn the_ec_passphrase_moves_to_the_hmac_table() {
    let input = r#"[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
origin_url = "https://origin.publisher.example.com"
proxy_secret = "publisher_proxy_secret"

[ec]
# The secret the Edge Cookie is made with.
passphrase = "ec_passphrase"
ec_store = "ec_identity_store"
"#;
    let migration = migrated(input);
    let value = parsed(&migration.text);

    assert_eq!(value["ec"]["module"].as_str(), Some("hmac"));
    assert_eq!(
        value["ec"]["hmac"]["passphrase"].as_str(),
        Some("ec_passphrase")
    );
    assert!(value["ec"].get("passphrase").is_none());
    assert_eq!(value["ec"]["ec_store"].as_str(), Some("ec_identity_store"));
    assert!(
        migration
            .text
            .contains("[ec.hmac]\n# The secret the Edge Cookie is made with.\npassphrase"),
        "should keep the comment above the passphrase:\n{}",
        migration.text
    );
}

#[test]
fn ec_provider_tables_and_module_names_take_their_new_names() {
    let input = r#"[ec]
module = "host-signals"

[ec.providers.host-signals]
passphrase = "ec_host_signals_passphrase"
"#;
    let migration = migrated(input);
    let value = parsed(&migration.text);

    assert_eq!(value["ec"]["module"].as_str(), Some("host_signals"));
    assert_eq!(
        value["ec"]["host_signals"]["passphrase"].as_str(),
        Some("ec_host_signals_passphrase")
    );
    assert!(value["ec"].get("providers").is_none());
}

#[test]
fn permission_signal_sources_become_modules_with_the_new_names() {
    let input = r#"[permission_signal]
sources = ["gpc", "gpp_sale_opt_out", "us_privacy", "tcf"]
"#;
    let migration = migrated(input);
    let value = parsed(&migration.text);

    assert!(value.get("permission_signal").is_none());
    let modules: Vec<&str> = value["permission-signal"]["modules"]
        .as_array()
        .expect("should write modules")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(modules, ["gpc", "gpp", "us-privacy", "tcf"]);
}

#[test]
fn the_platform_device_module_is_spelled_platform() {
    let migration = migrated("[device]\nmodule = \"fastly\"\n");
    let value = parsed(&migration.text);

    assert_eq!(value["device"]["module"].as_str(), Some("platform"));
}

#[test]
fn auction_providers_move_to_demand_with_their_bidder_routes() {
    let input = main_document(
        r#"[auction]
enabled = false

# The server-side demand.
[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[auction.providers.pbs-main.profile_config]
debug = false
consent_forwarding = "both"

[auction.providers.pbs-main.notifications]
suppress_all = false

[auction.providers.aps-main]
protocol = "openrtb-2.6"
profile = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"

[auction.providers.aps-main.profile_config]
account_id = "example-aps-account-id"

[auction.providers.plain]
protocol = "openrtb-2.6"
endpoint = "https://ssp.example.com/bid"

[auction.bidders.example-bidder]
provider = "pbs-main"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);
    let demand = &value["demand"];

    let modules: Vec<&str> = demand["modules"]
        .as_array()
        .expect("should select the demand sources")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(modules, ["pbs_main", "aps_main", "plain"]);
    assert_eq!(
        demand["pbs_main"]["implementation"].as_str(),
        Some("auction.prebid-server")
    );
    assert_eq!(demand["pbs_main"]["debug"].as_bool(), Some(false));
    assert_eq!(
        demand["pbs_main"]["consent_forwarding"].as_str(),
        Some("both")
    );
    assert_eq!(
        demand["pbs_main"]["notifications"]["suppress_all"].as_bool(),
        Some(false)
    );
    assert!(demand["pbs_main"].get("protocol").is_none());
    assert!(demand["pbs_main"].get("profile_config").is_none());
    assert_eq!(
        demand["aps_main"]["implementation"].as_str(),
        Some("auction.aps")
    );
    assert_eq!(
        demand["aps_main"]["account_id"].as_str(),
        Some("example-aps-account-id")
    );
    assert_eq!(
        demand["plain"]["implementation"].as_str(),
        Some("auction-protocol.openrtb")
    );
    assert!(value["auction"].get("providers").is_none());
    let route = &value["auction"]["bidders"]["example-bidder"];
    assert_eq!(route["module"].as_str(), Some("pbs_main"));
    assert!(route.get("provider").is_none());
    assert!(
        migration
            .text
            .contains("# The server-side demand.\n[demand]"),
        "should keep the comment above the first provider with [demand]:\n{}",
        migration.text
    );
}

#[test]
fn a_provider_profile_no_implementation_matches_is_left_to_do() {
    let input = main_document(
        r#"[auction.providers.odd]
protocol = "openrtb-2.6"
profile = "something-else"
endpoint = "https://ssp.example.com/bid"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert!(value["auction"]["providers"].get("odd").is_some());
    assert!(reported(
        &migration.report.to_do,
        "`[auction.providers.odd] profile`"
    ));
}

#[test]
fn a_profile_setting_named_as_one_of_the_providers_own_is_left_to_do() {
    let input = main_document(
        r#"[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"

[auction.providers.pbs-main.profile_config]
endpoint = "https://other.example.com/openrtb2/auction"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert!(value["auction"]["providers"].get("pbs-main").is_some());
    assert!(value.get("demand").is_none());
    assert!(reported(
        &migration.report.to_do,
        "`[auction.providers.pbs-main.profile_config]` holds a setting named as one of the \
         provider's own"
    ));
}

#[test]
fn both_forms_of_a_setting_are_left_to_do_as_written() {
    let input = r#"[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
origin_url = "https://origin.publisher.example.com"
proxy_secret = "publisher_proxy_secret"

[source]
module = "origin"

[source.origin]
url = "https://origin.publisher.example.com"

[ec]
module = "hmac"
passphrase = "ec_passphrase"

[ec.hmac]
passphrase = "ec_passphrase"

[ec.providers.hmac]
passphrase = "ec_passphrase"

[permission_signal]
sources = ["gpc"]

[permission-signal]
modules = ["gpc"]

[auction]
mediator = "adserver_mock"

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"

[demand]
modules = []

[integrations.adserver_mock]
enabled = true
endpoint = "https://adserver.example.com/mediate"

[adserver]
module = "iabtl"

[ad-server]
module = "iabtl"

[tinybird]
enabled = true
api_host = "api.us-east.tinybird.example"

[analytics]
module = "tinybird"

[proxy]

[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://assets.publisher.example.com"

[proxy.asset_routes.source]
module = "origin"
"#;
    let migration = migrated(input);
    let to_do = &migration.report.to_do;

    for fragment in [
        "`[publisher]` holds origin settings and `[source]` is written too",
        "`[ec] passphrase` sits beside an Edge Cookie module of the new form",
        "`[ec.providers.hmac]` and `[ec.hmac]` are both written",
        "`[permission_signal]` and `[permission-signal]` are both written",
        "`[auction.providers]` and `[demand]` are both written",
        "`[auction] mediator` and `[ad-server]` are both written",
        "`[adserver]` and `[ad-server]` are both written",
        "`[tinybird]` and `[analytics]` are both written",
        "`[[proxy.asset_routes]]` entry 1 holds origin settings and a `source` table too",
    ] {
        assert!(
            reported(to_do, fragment),
            "should leave `{fragment}`: {to_do:?}"
        );
    }
    assert!(
        !reported(to_do, "names an ad server whose settings"),
        "should report the mediator once, beside the ad server it names: {to_do:?}"
    );
    assert!(
        migration.report.changed.is_empty(),
        "should change nothing it left: {:?}",
        migration.report.changed
    );
    assert_eq!(migration.text, input);
}

#[test]
fn the_integration_table_is_left_to_do() {
    let migration = migrated("[integration]\nprovider = [\"didomi\"]\n");

    assert!(reported(
        &migration.report.to_do,
        "`[integration]` is not read"
    ));
    assert_eq!(migration.text, "[integration]\nprovider = [\"didomi\"]\n");
}

#[test]
fn an_enabled_integration_moves_to_the_section_of_its_type() {
    let input = main_document(
        r#"# Google's ad tag.
[integrations.gpt]
enabled = true
# Attribution.
gam_attribution_enabled = false
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert!(value.get("integrations").is_none());
    let modules = value["ad-tag"]["modules"]
        .as_array()
        .expect("should select");
    assert_eq!(modules[0].as_str(), Some("google"));
    assert_eq!(
        value["ad-tag"]["google"]["gam_attribution_enabled"].as_bool(),
        Some(false)
    );
    assert!(value["ad-tag"]["google"].get("enabled").is_none());
    assert!(
        migration
            .text
            .contains("# Google's ad tag.\n[ad-tag]\nmodules = [\"google\"]"),
        "should write the section where the table was, under its comment:\n{}",
        migration.text
    );
    assert!(
        migration
            .text
            .contains("# Attribution.\ngam_attribution_enabled = false")
    );
}

#[test]
fn an_integration_that_was_not_enabled_is_commented_out_under_its_new_name() {
    let input = main_document(
        r#"# The consent prompt.
[integrations.didomi]
enabled = false
geo_query_parameters = false
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert!(value.get("integrations").is_none());
    assert!(value.get("cmp").is_none());
    assert!(
        migration
            .text
            .contains("# The consent prompt.\n# [cmp.didomi]\n# geo_query_parameters = false\n"),
        "should comment the table out under its new name:\n{}",
        migration.text
    );
    assert!(reported(
        &migration.report.changed,
        "commented out `[integrations.didomi]` as `[cmp.didomi]`"
    ));
}

#[test]
fn an_integration_main_ran_by_default_runs_without_an_enabled_line() {
    let input = main_document(
        r#"[integrations.lockr]
app_id = "lockr-app"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert_eq!(value["identity"]["module"].as_str(), Some("lockr"));
    assert_eq!(
        value["identity"]["lockr"]["app_id"].as_str(),
        Some("lockr-app")
    );
}

#[test]
fn a_module_with_no_settings_is_selected_with_no_table() {
    let input = main_document(
        r#"[integrations.osano]
enabled = true

[integrations.gpt_diagnostics]
enabled = true
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert_eq!(value["cmp"]["module"].as_str(), Some("osano"));
    assert!(value["cmp"].get("osano").is_none());
    let modules = value["ad-tag"]["modules"]
        .as_array()
        .expect("should select");
    assert_eq!(modules[0].as_str(), Some("google.diagnostics"));
    assert!(value["ad-tag"].get("google").is_none());
}

#[test]
fn integrations_of_one_type_are_selected_together_in_the_builders_order() {
    let input = main_document(
        r#"[integrations.gpt_diagnostics]
enabled = true

[integrations.sourcepoint]
enabled = true
rewrite_sdk = true

[integrations.gpt]
enabled = true

[integrations.didomi]
enabled = true
geo_query_parameters = false
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    let names = |section: &str| -> Vec<String> {
        value[section]["modules"]
            .as_array()
            .expect("should select several")
            .iter()
            .filter_map(toml::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(names("ad-tag"), ["google", "google.diagnostics"]);
    assert_eq!(names("cmp"), ["didomi", "sourcepoint"]);
    assert_eq!(
        value["cmp"]["sourcepoint"]["rewrite_sdk"].as_bool(),
        Some(true)
    );
}

#[test]
fn prebid_and_testlight_are_selected_in_the_auction_section() {
    let input = main_document(
        r#"[auction]
enabled = true

[integrations.testlight]
enabled = true
endpoint = "https://testlight.example.com/openrtb2/auction"

[integrations.prebid]
timeout_ms = 1000

[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);
    let auction = &value["auction"];

    let modules: Vec<&str> = auction["modules"]
        .as_array()
        .expect("should select")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(modules, ["prebid", "testing.testlight"]);
    assert_eq!(
        auction["testing"]["testlight"]["endpoint"].as_str(),
        Some("https://testlight.example.com/openrtb2/auction")
    );
    assert_eq!(auction["prebid"]["timeout_ms"].as_integer(), Some(1000));
    assert_eq!(
        auction["prebid"]["bundle"]["modules"]["bidder"][0].as_str(),
        Some("rubiconBidAdapter")
    );
    assert_eq!(auction["enabled"].as_bool(), Some(true));
}

#[test]
fn the_script_proxy_is_selected_in_the_proxy_section_with_its_assets() {
    let input = main_document(
        r#"[integrations.js_asset_proxy]
enabled = true

[[integrations.js_asset_proxy.assets]]
path = "/assets/vendor.js"
origin_url = "https://cdn.example.com/vendor.js"
proxy = "enabled"

[proxy]
certificate_check = true
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert_eq!(
        value["proxy"]["modules"][0].as_str(),
        Some("js_asset_proxy")
    );
    assert_eq!(value["proxy"]["certificate_check"].as_bool(), Some(true));
    assert_eq!(
        value["proxy"]["js_asset_proxy"]["assets"][0]["path"].as_str(),
        Some("/assets/vendor.js")
    );
}

#[test]
fn an_integration_no_module_replaces_is_left_to_do() {
    let input = main_document("[integrations.unknown]\nenabled = true\n");
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert!(value["integrations"].get("unknown").is_some());
    assert!(reported(
        &migration.report.to_do,
        "`[integrations.unknown]` names no module"
    ));
}

#[test]
fn an_enabled_line_that_is_not_a_boolean_is_left_to_do() {
    let input = main_document("[integrations.lockr]\nenabled = \"yes\"\napp_id = \"lockr\"\n");
    let migration = migrated(&input);

    assert!(reported(
        &migration.report.to_do,
        "`[integrations.lockr] enabled` is not true or false"
    ));
    assert!(
        parsed(&migration.text)["integrations"]
            .get("lockr")
            .is_some()
    );
}

#[test]
fn the_aps_rendering_mode_moves_into_each_aps_demand_source() {
    let input = main_document(
        r#"[auction.providers.aps-main]
protocol = "openrtb-2.6"
profile = "aps"
endpoint = "https://aps.example.com/e/pb/bid"

[auction.providers.aps-main.profile_config]
account_id = "example-aps-account-id"

[integrations.aps]
enabled = true
rendering_mode = "publisher_native"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert_eq!(
        value["demand"]["aps_main"]["rendering_mode"].as_str(),
        Some("publisher_native")
    );
    assert!(value.get("integrations").is_none());
}

#[test]
fn an_aps_rendering_mode_with_no_aps_source_is_left_to_do() {
    let input = main_document(
        "[integrations.aps]\nenabled = true\nrendering_mode = \"publisher_native\"\n",
    );
    let migration = migrated(&input);

    assert!(reported(
        &migration.report.to_do,
        "`[integrations.aps] rendering_mode` belongs in the table"
    ));
    assert!(parsed(&migration.text)["integrations"].get("aps").is_some());

    let disabled = migrated(&main_document(
        "[integrations.aps]\nenabled = false\nrendering_mode = \"publisher_native\"\n",
    ));
    assert!(parsed(&disabled.text).get("integrations").is_none());
    assert!(reported(
        &disabled.report.check,
        "main did not apply `[integrations.aps] rendering_mode`"
    ));
}

#[test]
fn the_mediator_and_the_mock_ad_server_become_the_ad_server() {
    let input = main_document(
        r#"[auction]
enabled = true
mediator = "adserver_mock"

[integrations.adserver_mock]
enabled = true
endpoint = "https://adserver.example.com/mediate"

[integrations.adserver_mock.context_query_params]
example_segments = "segments"
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert_eq!(value["ad-server"]["module"].as_str(), Some("iabtl"));
    assert_eq!(
        value["ad-server"]["iabtl"]["endpoint"].as_str(),
        Some("https://adserver.example.com/mediate")
    );
    assert_eq!(
        value["ad-server"]["iabtl"]["context_query_params"]["example_segments"].as_str(),
        Some("segments")
    );
    assert!(value["auction"].get("mediator").is_none());
    assert!(value.get("integrations").is_none());
}

#[test]
fn a_mediator_with_no_mock_ad_server_is_left_to_do_and_an_unnamed_one_is_commented_out() {
    let lone = migrated(&main_document("[auction]\nmediator = \"adserver_mock\"\n"));
    assert!(reported(
        &lone.report.to_do,
        "`[auction] mediator` names an ad server"
    ));
    assert!(parsed(&lone.text)["auction"].get("mediator").is_some());

    let unnamed = migrated(&main_document(
        "[integrations.adserver_mock]\nenabled = true\nendpoint = \"https://adserver.example.com/mediate\"\n",
    ));
    assert!(parsed(&unnamed.text).get("ad-server").is_none());
    assert!(unnamed.text.contains("# [ad-server.iabtl]\n"));
}

#[test]
fn the_ad_server_and_the_ads_txt_module_take_their_new_names() {
    let input = r#"[adserver]
module = "mock"

[adserver.mock]
endpoint = "https://adserver.example.com/decide"

[ads-txt]
modules = ["sellers"]

[ads-txt.sellers]
lines = ["ssp.example, 12345, DIRECT"]

[[fetch]]
media_type = "text/plain"
path = "/ads.txt"
middleware = ["ads-txt.sellers"]
"#;
    let migration = migrated(input);
    let value = parsed(&migration.text);

    assert!(value.get("adserver").is_none());
    assert_eq!(value["ad-server"]["module"].as_str(), Some("iabtl"));
    assert!(value["ad-server"].get("iabtl").is_some());
    assert_eq!(value["ads-txt"]["modules"][0].as_str(), Some("iabtl"));
    assert!(value["ads-txt"].get("iabtl").is_some());
    assert_eq!(
        value["fetch"][0]["middleware"][0].as_str(),
        Some("ads-txt.iabtl")
    );
    assert_eq!(
        value["fetch"].as_array().map(Vec::len),
        Some(1),
        "should add no second entry for the file"
    );
}

#[test]
fn a_selected_ads_txt_module_and_robots_txt_get_their_file_entries() {
    let input = r#"[ads-txt]
modules = ["iabtl"]

[ads-txt.iabtl]
lines = ["ssp.example, 12345, DIRECT"]

[robots-txt]
modules = ["refuse_all"]
"#;
    let migration = migrated(input);
    let value = parsed(&migration.text);
    let entries = value["fetch"].as_array().expect("should add the entries");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["media_type"].as_str(), Some("text/plain"));
    assert_eq!(entries[0]["path"].as_str(), Some("/ads.txt"));
    assert_eq!(entries[0]["middleware"][0].as_str(), Some("ads-txt.iabtl"));
    assert_eq!(entries[1]["path"].as_str(), Some("/robots.txt"));
    assert_eq!(entries[1]["middleware"][0].as_str(), Some("robots_txt"));
}

#[test]
fn tinybird_moves_to_analytics_and_is_commented_out_when_not_enabled() {
    let enabled = migrated(&main_document(
        r#"[tinybird]
enabled = true
api_host = "api.us-east.tinybird.example"
auction_token_secret = "tinybird_auction_append_token"
"#,
    ));
    let value = parsed(&enabled.text);
    assert_eq!(value["analytics"]["module"].as_str(), Some("tinybird"));
    assert_eq!(
        value["analytics"]["tinybird"]["api_host"].as_str(),
        Some("api.us-east.tinybird.example")
    );
    assert!(value["analytics"]["tinybird"].get("enabled").is_none());
    assert!(value.get("tinybird").is_none());
    assert!(reported(
        &enabled.report.check,
        "analytics.ad_reporting.measure_ad_performance"
    ));

    let disabled = migrated(&main_document(
        "[tinybird]\nenabled = false\napi_host = \"api.us-east.tinybird.example\"\n",
    ));
    let value = parsed(&disabled.text);
    assert!(value.get("analytics").is_none());
    assert!(value.get("tinybird").is_none());
    assert!(
        disabled
            .text
            .contains("# [analytics.tinybird]\n# api_host = \"api.us-east.tinybird.example\"\n")
    );
}

#[test]
fn the_mediator_response_option_is_renamed() {
    let input = main_document(
        r#"[debug.auction_html_comment_options]
include_mediator_response = false
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);
    let options = &value["debug"]["auction_html_comment_options"];

    assert_eq!(options["include_adserver_response"].as_bool(), Some(false));
    assert!(options.get("include_mediator_response").is_none());
}

#[test]
fn the_consent_place_lists_are_left_to_do() {
    let input = main_document(
        r#"[consent]
mode = "interpreter"

[consent.gdpr]
applies_in = ["FR"]

[consent.us_states]
privacy_states = ["CA"]
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);

    assert!(value["consent"].get("gdpr").is_some());
    assert!(reported(
        &migration.report.to_do,
        "`[consent.gdpr]` is not read"
    ));
    assert!(reported(
        &migration.report.to_do,
        "`[consent.us_states]` is not read"
    ));
}

#[test]
fn a_template_key_cookie_under_the_public_prefix_is_left_to_do() {
    let input = main_document(
        r#"[creative_opportunities]
gam_network_id = "123456789"
template_cache_key_cookies = ["ab_bucket", "ts-tester"]
"#,
    );
    let migration = migrated(&input);

    assert!(reported(
        &migration.report.to_do,
        "`[creative_opportunities] template_cache_key_cookies` names a cookie under the `ts` prefix"
    ));
    let clean = migrated(&main_document(
        "[creative_opportunities]\ngam_network_id = \"123456789\"\ntemplate_cache_key_cookies = [\"ab_bucket\"]\n",
    ));
    assert!(clean.report.to_do.is_empty());
}

#[test]
fn a_document_for_main_keeps_the_location_lookup_and_the_geo_headers() {
    let migration = migrated(MAIN_BASE);
    let value = parsed(&migration.text);

    assert_eq!(value["geo"]["module"].as_str(), Some("platform"));
    assert_eq!(value["headers"]["modules"][0].as_str(), Some("geo"));
    let serve = value["serve"].as_array().expect("should place the headers");
    assert_eq!(serve.len(), 1);
    assert_eq!(serve[0]["media_type"].as_str(), Some("text/html"));
    assert_eq!(serve[0]["middleware"][0].as_str(), Some("headers.geo"));
    assert!(value.get("fetch").is_none());
    assert!(reported(&migration.report.check, "`x-ts-version`"));
    assert!(reported(&migration.report.check, "Flight response"));
    assert!(reported(
        &migration.report.check,
        "`x-geo-*` headers now go only"
    ));
    assert!(
        migration.report.to_do.is_empty(),
        "{:?}",
        migration.report.to_do
    );
}

#[test]
fn a_document_already_in_the_new_shape_gets_no_location_lookup() {
    let input = r#"[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
proxy_secret = "publisher_proxy_secret"

[source]
module = "origin"

[source.origin]
url = "https://origin.publisher.example.com"
"#;
    let migration = migrated(input);

    assert!(
        migration.report.changed.is_empty(),
        "{:?}",
        migration.report.changed
    );
    assert!(
        migration.report.check.is_empty(),
        "{:?}",
        migration.report.check
    );
    assert_eq!(migration.text, input);
}

/// The middleware names the stack's example lists in the commented entry for
/// `phase` and `media_type`.
fn example_entry(phase: &str, media_type: &str) -> Vec<String> {
    let start = format!("# [[{phase}]]\n# media_type = \"{media_type}\"\n# middleware = [\n");
    let at = STACK_EXAMPLE
        .find(&start)
        .unwrap_or_else(|| panic!("the example should list a {phase} entry for {media_type}"));
    STACK_EXAMPLE[at + start.len()..]
        .lines()
        .take_while(|line| *line != "# ]")
        .map(|line| {
            line.trim_start_matches('#')
                .trim()
                .trim_end_matches(',')
                .trim_matches('"')
                .to_owned()
        })
        .collect()
}

#[test]
fn page_entries_name_the_middleware_in_the_order_the_example_shows() {
    let input = main_document(
        r#"[integrations.prebid]
enabled = true

[integrations.js_asset_proxy]
enabled = true

[integrations.testlight]
enabled = true
endpoint = "https://testlight.example.com/openrtb2/auction"

[integrations.nextjs]
enabled = true

[integrations.permutive]
organization_id = "organization"
workspace_id = "workspace"

[integrations.lockr]
app_id = "lockr"

[integrations.didomi]

[integrations.sourcepoint]
enabled = true

[integrations.google_tag_manager]
enabled = true
container_id = "GTM-ABCDEF"

[integrations.datadome]
enabled = true

[integrations.gpt]

[integrations.gpt_diagnostics]
enabled = true
"#,
    );
    let migration = migrated(&input);
    let value = parsed(&migration.text);
    let names = |entry: &toml::Value| -> Vec<String> {
        entry["middleware"]
            .as_array()
            .expect("should name middleware")
            .iter()
            .filter_map(toml::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let entry = |phase: &str, media_type: &str| -> Vec<String> {
        value[phase]
            .as_array()
            .expect("should add entries")
            .iter()
            .find(|entry| entry["media_type"].as_str() == Some(media_type))
            .map(names)
            .unwrap_or_default()
    };

    let fetch = entry("fetch", "text/html");
    let expected: Vec<String> = example_entry("fetch", "text/html")
        .into_iter()
        .filter(|name| name != "cmp.inmobi")
        .collect();
    assert_eq!(fetch, expected);

    let serve = entry("serve", "text/html");
    let expected: Vec<String> = example_entry("serve", "text/html")
        .into_iter()
        .filter(|name| {
            ![
                "tag.google-tag",
                "51degrees.client_hints",
                "service_version",
            ]
            .contains(&name.as_str())
        })
        .collect();
    assert_eq!(serve, expected);

    assert_eq!(
        entry("fetch", "text/x-component"),
        ["framework.nextjs.flight"],
        "the Next.js module should place its Flight middleware"
    );
    assert!(!reported(&migration.report.check, "Flight response"));
    assert!(
        !reported(&migration.report.to_do, "does not link"),
        "every main integration should name a module the tool links: {:?}",
        migration.report.to_do
    );
}

#[test]
fn a_document_with_page_entries_gets_none_added() {
    let input = r#"[cmp]
module = "didomi"

[[fetch]]
media_type = "text/html"
middleware = []
"#;
    let migration = migrated(input);

    assert!(
        migration.report.changed.is_empty(),
        "{:?}",
        migration.report.changed
    );
}

/// Main's example with its placeholder hosts replaced, as an operator's
/// document would have them.
fn main_example_with_hosts() -> String {
    MAIN_EXAMPLE
        .replace("\"example.com\"", "\"publisher.example.com\"")
        .replace("\".example.com\"", "\".publisher.example.com\"")
        .replace(
            "https://origin.example.com",
            "https://origin.publisher.example.com",
        )
}

#[test]
fn mains_example_migrates_into_a_document_that_validates() {
    let input = main_example_with_hosts();
    let migration = migrated(&input);

    validate_source_config(&migration.text).unwrap_or_else(|error| {
        panic!(
            "the migrated example should validate: {error}\n{}",
            migration.text
        )
    });
    assert!(
        migration.report.to_do.is_empty(),
        "{:?}",
        migration.report.to_do
    );

    let value = parsed(&migration.text);
    assert!(value.get("integrations").is_none());
    assert!(value.get("handlers").is_none());
    assert_eq!(value["source"]["module"].as_str(), Some("origin"));
    assert_eq!(value["ec"]["module"].as_str(), Some("hmac"));
    assert_eq!(value["demand"]["modules"][0].as_str(), Some("pbs_main"));
    assert_eq!(
        value["auction"]["bidders"]["example-bidder"]["module"].as_str(),
        Some("pbs_main")
    );
    for block in [
        "# [proxy.js_asset_proxy]\n",
        "# [[proxy.js_asset_proxy.assets]]\n",
        "# [auction.prebid]\n# timeout_ms = 1000\n",
        "# [cmp.didomi]\n# geo_query_parameters = false\n",
        "# [bot-protection.datadome]\n",
        "# [ad-tag.google]\n",
        "# [tag.google-tag-manager]\n",
    ] {
        assert!(
            migration.text.contains(block),
            "should comment out `{block}`:\n{}",
            migration.text
        );
    }
    assert!(
        migration
            .text
            .starts_with("# trusted-server.example.toml from IABTechLab/trusted-server main"),
        "should keep the comments that open the file"
    );
}

#[test]
fn mains_example_validates_only_once_migrated() {
    assert!(
        validate_source_config(&main_example_with_hosts()).is_err(),
        "main's example should not load as written"
    );
}

#[test]
fn comments_and_table_order_survive() {
    let input = r#"# The opening comment.

# About the publisher.
[publisher]
# The domain.
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
# The origin.
origin_url = "https://origin.publisher.example.com"
proxy_secret = "publisher_proxy_secret"
# A trailing comment of the publisher.

# About the Edge Cookie.
[ec]
# The passphrase.
passphrase = "ec_passphrase"
ec_store = "ec_identity_store" # the store

# About the auction.
[auction]
enabled = false

# About the templates.
[creative_opportunities]
gam_network_id = "123456789"

# The closing comment.
"#;
    let migration = migrated(input);

    for line in input.lines().filter(|line| line.contains('#')) {
        assert!(
            migration.text.contains(line),
            "should keep `{line}`:\n{}",
            migration.text
        );
    }
    let order: Vec<usize> = [
        "[publisher]",
        "[source]",
        "[ec]",
        "[auction]",
        "[creative_opportunities]",
    ]
    .iter()
    .map(|header| {
        migration
            .text
            .find(&format!("\n{header}\n"))
            .unwrap_or_else(|| panic!("should write {header}:\n{}", migration.text))
    })
    .collect();
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "should keep the tables in order:\n{}",
        migration.text
    );
    assert!(
        migration.text.contains(
            "proxy_secret = \"publisher_proxy_secret\"\n# A trailing comment of the publisher.\n"
        ),
        "should keep the trailing comment with the publisher:\n{}",
        migration.text
    );
    assert!(migration.text.ends_with("# The closing comment.\n"));
}

#[test]
fn windows_line_endings_are_kept() {
    let input = MAIN_BASE.replace('\n', "\r\n");
    let migration = migrated(&input);

    assert!(!migration.report.changed.is_empty());
    assert!(
        !migration
            .text
            .bytes()
            .zip(migration.text.bytes().skip(1))
            .any(|(before, after)| before != b'\r' && after == b'\n'),
        "every line should end with a carriage return and a line feed:\n{:?}",
        migration.text
    );
    assert!(!migration.text.starts_with('\n'));
}

/// A document every rule rewrites or reports, whose every value is a marker
/// no line may print.
const SECRET_DOCUMENT: &str = r#"[publisher]
domain = "SECRET-domain"
cookie_domain = "SECRET-cookie-domain"
origin_url = "https://SECRET-origin.example.com/SECRET-path"
origin_host_header_override = "SECRET-host"
proxy_secret = "SECRET-proxy"

[ec]
passphrase = "SECRET-passphrase"

[[ec.partners]]
name = "SECRET-partner"
source_domain = "SECRET-source"
api_token = "SECRET-token"

[[handlers]]
path = "^/_ts/admin"
username = "SECRET-admin"
password = "SECRET-password"

[[handlers]]
path = "^/SECRET-secure"
username = "SECRET-user"
password = "SECRET-user-password"

[request_signing]
config_store_id = "SECRET-config-store"
secret_store_id = "SECRET-secret-store"

[auction]
mediator = "SECRET-mediator"

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "SECRET-profile"
endpoint = "https://SECRET-pbs.example.com"

[auction.providers.pbs-other]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://SECRET-pbs.example.com"

[auction.providers.pbs-other.profile_config]
debug_query_params = "SECRET-debug"

[auction.bidders.bidder]
provider = "pbs-other"

[integrations.gpt]
script_url = "https://SECRET-gpt.example.com/gpt.js"

[integrations.didomi]
enabled = false
sdk_origin = "https://SECRET-didomi.example.com"

[integrations.aps]
enabled = true
rendering_mode = "SECRET-rendering"

[integrations.SECRET-integration]
enabled = true

[tinybird]
enabled = true
api_host = "SECRET-tinybird"
auction_token_secret = "SECRET-tinybird-token"

[consent.gdpr]
applies_in = ["SECRET-country"]

[creative_opportunities]
gam_network_id = "SECRET-network"
template_cache_key_cookies = ["ts-SECRET-cookie"]
"#;

#[test]
fn no_line_carries_a_value_from_the_document() {
    let migration = migrate(SECRET_DOCUMENT).expect("should migrate the document");
    let lines: Vec<&String> = migration
        .report
        .changed
        .iter()
        .chain(&migration.report.to_do)
        .chain(&migration.report.check)
        .collect();
    assert!(!lines.is_empty());
    for line in lines {
        let shown = line.replace("SECRET-integration", "");
        assert!(
            !shown.contains("SECRET"),
            "a line printed a value from the document: {line}"
        );
    }

    let temp = TempDir::new().expect("should make a folder");
    let file = temp.path().join("trusted-server.toml");
    fs::write(&file, SECRET_DOCUMENT).expect("should write the document");
    let mut out = Vec::new();
    let mut report = Vec::new();
    run_with_writers(
        &ConfigMigrateArgs {
            file: file.clone(),
            write: false,
            check: false,
        },
        &mut out,
        &mut report,
    )
    .expect("should run");
    let report = String::from_utf8(report).expect("should report text");
    assert!(
        !report.replace("SECRET-integration", "").contains("SECRET"),
        "the report printed a value from the document:\n{report}"
    );

    fs::write(&file, "token = \"SECRET-token\"\n[broken\n").expect("should write the document");
    let error = run_with_writers(
        &ConfigMigrateArgs {
            file,
            write: false,
            check: false,
        },
        &mut Vec::new(),
        &mut Vec::new(),
    )
    .expect_err("should refuse a document that is not TOML");
    assert!(error.contains("line 2"), "should name the line: {error}");
    assert!(!error.contains("SECRET"), "should quote nothing: {error}");
}

fn args(file: std::path::PathBuf, write: bool, check: bool) -> ConfigMigrateArgs {
    ConfigMigrateArgs { file, write, check }
}

#[test]
fn check_changes_nothing_and_says_whether_the_document_needs_migrating() {
    let temp = TempDir::new().expect("should make a folder");
    let file = temp.path().join("trusted-server.toml");
    fs::write(&file, MAIN_BASE).expect("should write the document");

    let mut out = Vec::new();
    let mut report = Vec::new();
    let outcome = run_with_writers(&args(file.clone(), false, true), &mut out, &mut report)
        .expect("should check");
    assert_eq!(outcome, RunOutcome::AssertionFailed);
    assert!(out.is_empty(), "should print no document");
    assert_eq!(
        fs::read_to_string(&file).expect("should read"),
        MAIN_BASE,
        "should leave the file as it was"
    );
    let report = String::from_utf8(report).expect("should report text");
    assert!(report.contains("changed: "), "{report}");
    assert!(
        report.ends_with("the document needs migrating\n"),
        "{report}"
    );

    let migrated = migrate(MAIN_BASE).expect("should migrate").text;
    fs::write(&file, &migrated).expect("should write the migrated document");
    let mut report = Vec::new();
    let outcome = run_with_writers(&args(file, false, true), &mut Vec::new(), &mut report)
        .expect("should check");
    assert_eq!(outcome, RunOutcome::Success);
    let report = String::from_utf8(report).expect("should report text");
    assert!(report.contains("nothing to migrate"), "{report}");
    assert!(
        report.ends_with("the document needs no migrating\n"),
        "{report}"
    );
}

#[test]
fn write_rewrites_the_file_and_the_default_prints_the_document() {
    let temp = TempDir::new().expect("should make a folder");
    let file = temp.path().join("trusted-server.toml");
    fs::write(&file, MAIN_BASE).expect("should write the document");
    let expected = migrate(MAIN_BASE).expect("should migrate").text;

    let mut out = Vec::new();
    let mut report = Vec::new();
    let outcome = run_with_writers(&args(file.clone(), false, false), &mut out, &mut report)
        .expect("should migrate");
    assert_eq!(String::from_utf8(out).expect("should print text"), expected);
    assert_eq!(fs::read_to_string(&file).expect("should read"), MAIN_BASE);
    assert_eq!(outcome, RunOutcome::Success);
    let report = String::from_utf8(report).expect("should report text");
    assert!(
        report.ends_with("the migrated document passes validation\n"),
        "{report}"
    );

    let mut out = Vec::new();
    run_with_writers(&args(file.clone(), true, false), &mut out, &mut Vec::new())
        .expect("should migrate");
    assert!(out.is_empty(), "should print no document with --write");
    assert_eq!(fs::read_to_string(&file).expect("should read"), expected);
}

#[test]
fn an_item_left_to_do_fails_the_run() {
    let temp = TempDir::new().expect("should make a folder");
    let file = temp.path().join("trusted-server.toml");
    fs::write(
        &file,
        main_document("[[handlers]]\npath = \"^/secure\"\nusername = \"u\"\npassword = \"p\"\n"),
    )
    .expect("should write the document");

    let mut report = Vec::new();
    let outcome = run_with_writers(&args(file, false, false), &mut Vec::new(), &mut report)
        .expect("should migrate");
    assert_eq!(outcome, RunOutcome::AssertionFailed);
    let report = String::from_utf8(report).expect("should report text");
    assert!(report.contains("to do: `[[handlers]]` entry 1"), "{report}");
    assert!(
        report.contains("the migrated document does not pass validation"),
        "{report}"
    );
}
