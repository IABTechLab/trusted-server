use std::collections::BTreeMap;
use std::path::Path;

use error_stack::Report;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use trusted_server_core::auction_config_types::{BidderId, ProviderId};
use trusted_server_core::integrations::prebid_server::PREBID_SERVER_ID;

use super::{Output, PbsError, Result, identifier, read_text};

/// The name `[auction] modules` selects the Prebid module by.
const PREBID_MODULE: &str = "prebid";

/// Deliberately partial: inspecting PBS requirements must not require unrelated TS settings.
#[derive(Default, Deserialize)]
struct Source {
    auction: Option<Auction>,
    #[serde(default)]
    demand: Demand,
}

#[derive(Default, Deserialize)]
struct Auction {
    enabled: Option<bool>,
    #[serde(default)]
    bidders: BTreeMap<BidderId, AuctionBidder>,
    /// The modules `[auction]` selects, `prebid` among them.
    #[serde(default)]
    modules: Vec<String>,
    /// The Prebid module's own table, `[auction.prebid]`.
    prebid: Option<Prebid>,
}

/// The `[demand]` table: `modules` selects what runs, and every other key is
/// one demand source's settings table.
#[derive(Default, Deserialize)]
struct Demand {
    #[serde(default)]
    modules: Vec<ProviderId>,
    #[serde(flatten)]
    sources: BTreeMap<ProviderId, DemandSource>,
}

#[derive(Deserialize)]
struct DemandSource {
    implementation: Option<String>,
    endpoint: Option<String>,
    timeout_ms: Option<u32>,
    test_mode: Option<bool>,
    debug: Option<bool>,
    #[serde(default)]
    bid_param_override_rules: Vec<toml::Value>,
}

impl DemandSource {
    /// A table runs the implementation it names, or the one its name is.
    fn runs_prebid_server(&self, name: &ProviderId) -> bool {
        self.implementation.as_deref().unwrap_or(name.as_str()) == PREBID_SERVER_ID
    }
}

#[derive(Deserialize)]
struct AuctionBidder {
    module: ProviderId,
}

#[derive(Serialize)]
struct ServerBidderCandidate {
    bidder: String,
    source_key: String,
    host_secret_requirement: &'static str,
    partner_authorization: &'static str,
}

#[derive(Serialize)]
struct ServerProviderReport {
    provider: String,
    selected: bool,
    endpoint_configured: bool,
    timeout_ms_explicit: Option<u32>,
    test_mode_explicit: Option<bool>,
    debug_explicit: Option<bool>,
    bid_param_override_rule_count: usize,
    server_bidder_candidates: Vec<ServerBidderCandidate>,
}

#[derive(Default, Clone, Deserialize)]
struct Prebid {
    account_id: Option<String>,
    timeout_ms: Option<u32>,
    debug: Option<bool>,
    #[serde(default, deserialize_with = "bidder_list")]
    client_side_bidders: Vec<String>,
    #[serde(default)]
    bundle: Bundle,
}

#[derive(Default, Clone, Deserialize)]
struct Bundle {
    #[serde(default)]
    modules: BundleModules,
}

/// Explicit selections from core's bundle module schema; never expand generator presets.
#[derive(Default, Clone, Deserialize)]
struct BundleModules {
    #[serde(default)]
    bidder: Vec<String>,
    #[serde(default)]
    user_id: Vec<String>,
    #[serde(default)]
    analytics: Vec<String>,
}

/// Accept the same encodings as the private core list deserializer without expanding defaults.
/// Unparseable escapes instead reach identifier validation for a sanitized error.
/// Parity tests compare accepted representations with `PrebidIntegrationConfig`.
///
/// # Errors
/// Rejects malformed lists, invalid numeric indexes, and non-string items.
fn bidder_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Array(values) => {
            serde_json::from_value(Value::Array(values)).map_err(serde::de::Error::custom)
        }
        Value::Object(values) => {
            let mut indexed = Vec::with_capacity(values.len());
            for (index, value) in values {
                let index = index.parse::<usize>().map_err(serde::de::Error::custom)?;
                let value: String =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                indexed.push((index, value));
            }
            indexed.sort_by_key(|(index, _)| *index);
            Ok(indexed.into_iter().map(|(_, value)| value).collect())
        }
        Value::String(value) => {
            let text = value.trim();
            let bracketed = text.starts_with('[') && text.ends_with(']');
            if bracketed && let Ok(values) = serde_json::from_str::<Vec<String>>(text) {
                return Ok(values);
            }
            let parts = if bracketed {
                text[1..text.len() - 1]
                    .trim()
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
            } else if text.contains(',') {
                text.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect()
            } else {
                vec![text]
            };
            Ok(parts
                .into_iter()
                .map(|part| {
                    let json = format!("\"{}\"", part.replace('"', "\\\""));
                    // Core propagates this error; keep raw text here so identifier validation
                    // rejects it without a serde snippet that could echo source contents.
                    serde_json::from_str(&json).unwrap_or_else(|_| part.to_owned())
                })
                .collect())
        }
        _ => Err(serde::de::Error::custom(
            "expected bidder list, indexed map, or list string",
        )),
    }
}

/// Inspect selected local fields, preserving the source and redacting account/parameter values.
///
/// # Errors
/// Rejects invalid TOML or malformed identifiers with sanitized messages.
pub(super) fn inspect(path: &Path) -> Result<Output> {
    let text = read_text(path, 2 * 1024 * 1024)?;
    let source: Source = toml::from_str(&text).map_err(|_| {
        Report::new(PbsError::Input(
            "cannot parse Trusted Server TOML; source details withheld",
        ))
    })?;
    let auction_present = source.auction.is_some();
    let auction = source.auction.unwrap_or_default();
    let prebid_present = auction.prebid.is_some();
    let prebid_selected = auction
        .modules
        .iter()
        .any(|name| name == PREBID_MODULE || name == "auction.prebid");
    let prebid = auction.prebid.clone().unwrap_or_default();
    for name in prebid
        .client_side_bidders
        .iter()
        .chain(&prebid.bundle.modules.bidder)
        .chain(&prebid.bundle.modules.user_id)
        .chain(&prebid.bundle.modules.analytics)
    {
        if !identifier(name) {
            return Err(Report::new(PbsError::Input(
                "invalid bidder or bundle-module identifier",
            )));
        }
    }
    let server_providers: Vec<_> = source
        .demand
        .sources
        .iter()
        .filter(|(name, demand_source)| demand_source.runs_prebid_server(name))
        .map(|(name, demand_source)| {
            let requirements: Vec<_> = auction
                .bidders
                .iter()
                .filter(|(_, bidder)| &bidder.module == name)
                .map(|(bidder_id, _)| ServerBidderCandidate {
                    bidder: bidder_id.as_str().to_owned(),
                    source_key: format!("auction.bidders.{}.module", bidder_id.as_str()),
                    host_secret_requirement: "unresolved",
                    partner_authorization: "unresolved",
                })
                .collect();
            ServerProviderReport {
                provider: name.as_str().to_owned(),
                selected: source.demand.modules.contains(name),
                endpoint_configured: demand_source.endpoint.is_some(),
                timeout_ms_explicit: demand_source.timeout_ms,
                test_mode_explicit: demand_source.test_mode,
                debug_explicit: demand_source.debug,
                bid_param_override_rule_count: demand_source.bid_param_override_rules.len(),
                server_bidder_candidates: requirements,
            }
        })
        .collect();
    let warnings = [
        "Local file only: confirm environment, remote configuration, and request-time overrides.",
        "Omitted fields/defaults are not expanded; empty candidate lists are not proof of no demand.",
        "A disabled auction, a demand source [demand] modules does not select, and browser bundle adapters do not authorize PBS activation.",
        "Host secret requirements need adapter metadata verified against the selected PBS release.",
    ];
    let mut details = vec![
        format!(
            "Auction section present: {auction_present}; enabled explicitly: {:?}",
            auction.enabled
        ),
        format!(
            "Prebid browser section present: {prebid_present}; selected in [auction] modules: {prebid_selected}"
        ),
        format!("Prebid Server demand sources: {}", server_providers.len()),
    ];
    details.extend(server_providers.iter().map(|provider| {
        let bidders = provider
            .server_bidder_candidates
            .iter()
            .map(|candidate| candidate.bidder.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Demand source {}: selected: {}; bidders: {}; endpoint configured: {}; timeout explicit: {:?}; test mode explicit: {:?}; debug explicit: {:?}; bid-parameter rules: {}; values withheld",
            provider.provider,
            provider.selected,
            bidders,
            provider.endpoint_configured,
            provider.timeout_ms_explicit,
            provider.test_mode_explicit,
            provider.debug_explicit,
            provider.bid_param_override_rule_count
        )
    }));
    details.extend([
        format!(
            "Client-side bidders: {}",
            prebid.client_side_bidders.join(", ")
        ),
        format!(
            "Browser bundle adapters: {}",
            prebid.bundle.modules.bidder.join(", ")
        ),
        format!(
            "Browser identity modules: {}",
            prebid.bundle.modules.user_id.join(", ")
        ),
        format!(
            "Browser analytics modules: {}",
            prebid.bundle.modules.analytics.join(", ")
        ),
    ]);
    details.extend(warnings.iter().map(|warning| (*warning).to_owned()));
    Ok(Output {
        failure: None,
        summary: "Local PBS requirements discovery; no files changed or AWS calls made".to_owned(),
        details,
        data: json!({
            "source": path,
            "source_sections": {
                "server": ["demand", "auction.bidders"],
                "browser": "integration.prebid"
            },
            "auction_section_present": auction_present,
            "auction_enabled_explicit": auction.enabled,
            "server_providers": server_providers,
            "section_present": prebid_present,
            "selected": prebid_selected,
            "account_id_configured": prebid.account_id.is_some(),
            "timeout_ms_explicit": prebid.timeout_ms,
            "debug_explicit": prebid.debug,
            "client_side_bidders": prebid.client_side_bidders,
            "bundle_adapters": prebid.bundle.modules.bidder,
            "identity_modules": prebid.bundle.modules.user_id,
            "analytics_modules": prebid.bundle.modules.analytics,
            "warnings": warnings
        }),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn discovery_classifies_bidders_redacts_values_and_preserves_source() {
        let dir = tempfile::tempdir().expect("should create temp directory");
        let path = dir.path().join("trusted-server.toml");
        let source = r#"
[auction]
enabled = true

[demand]
modules = ["pbs_main"]

[demand.pbs_main]
implementation = "prebid_server"
endpoint = "https://user:NEVER_PRINT_ME@pbs.example.com/path?token=NEVER_PRINT_ME"
timeout_ms = 900
routing = "explicit"
debug = false
test_mode = true
bid_param_override_rules = [{ when = { bidder = "serverbidder" }, set = { placementId = "NEVER_PRINT_ME" } }]

[demand.pbs_secondary]
implementation = "prebid_server"
endpoint = "https://NEVER_PRINT_ME@secondary.example.com/openrtb2/auction"
routing = "explicit"

[demand.house]
implementation = "openrtb"
endpoint = "https://house.example.com/openrtb2/auction"

[auction.bidders.serverbidder]
module = "pbs_main"

[auction.bidders.otherbidder]
module = "pbs_secondary"

[auction.prebid]
account_id = "NEVER_PRINT_ME"
client_side_bidders = ["browserbidder"]

[auction.prebid.bundle.modules]
bidder = ["exampleBidAdapter"]
user_id = ["sharedIdSystem"]
analytics = ["exampleAnalyticsAdapter"]
"#;
        fs::write(&path, source).expect("should write fixture");
        let report = inspect(&path).expect("should inspect config");
        assert_eq!(report.data["auction_section_present"], true);
        assert_eq!(report.data["auction_enabled_explicit"], true);
        assert_eq!(report.data["selected"], false);
        assert_eq!(
            report.data["server_providers"].as_array().map(Vec::len),
            Some(2),
            "should report the Prebid Server tables and leave the OpenRTB one out"
        );
        assert_eq!(report.data["server_providers"][0]["provider"], "pbs_main");
        assert_eq!(report.data["server_providers"][0]["selected"], true);
        assert_eq!(
            report.data["server_providers"][0]["server_bidder_candidates"][0]["bidder"],
            "serverbidder"
        );
        assert_eq!(
            report.data["server_providers"][1]["provider"],
            "pbs_secondary"
        );
        assert_eq!(
            report.data["server_providers"][1]["selected"], false,
            "a table [demand] modules does not name is reported as not selected"
        );
        assert_eq!(
            report.data["server_providers"][1]["server_bidder_candidates"][0]["bidder"],
            "otherbidder"
        );
        assert_eq!(
            report.data["server_providers"][0]["server_bidder_candidates"][0]["source_key"],
            "auction.bidders.serverbidder.module"
        );
        assert_eq!(
            report.data["server_providers"][0]["endpoint_configured"],
            true
        );
        assert_eq!(
            report.data["server_providers"][0]["timeout_ms_explicit"],
            900
        );
        assert_eq!(
            report.data["server_providers"][0]["test_mode_explicit"],
            true
        );
        assert_eq!(report.data["server_providers"][0]["debug_explicit"], false);
        assert_eq!(
            report.data["server_providers"][0]["bid_param_override_rule_count"],
            1
        );
        assert_eq!(report.data["client_side_bidders"][0], "browserbidder");
        assert_eq!(report.data["bundle_adapters"], json!(["exampleBidAdapter"]));
        assert_eq!(report.data["identity_modules"], json!(["sharedIdSystem"]));
        assert_eq!(
            report.data["analytics_modules"],
            json!(["exampleAnalyticsAdapter"])
        );
        assert_eq!(
            report.data["server_providers"][0]["server_bidder_candidates"][0]["host_secret_requirement"],
            "unresolved"
        );
        let mut human = Vec::new();
        report
            .write(false, &mut human)
            .expect("should render human report");
        let human = String::from_utf8(human).expect("should emit UTF-8");
        assert!(human.contains("pbs_main"));
        assert!(human.contains("serverbidder"));
        assert!(human.contains("Browser bundle adapters: exampleBidAdapter"));
        assert!(human.contains("Browser identity modules: sharedIdSystem"));
        assert!(human.contains("Browser analytics modules: exampleAnalyticsAdapter"));
        assert!(!human.contains("NEVER_PRINT_ME"));
        let mut json = Vec::new();
        report
            .write(true, &mut json)
            .expect("should render JSON report");
        assert!(!String::from_utf8_lossy(&json).contains("NEVER_PRINT_ME"));
        assert_eq!(
            fs::read_to_string(path).expect("should read fixture"),
            source
        );
    }

    #[test]
    fn invalid_toml_never_exposes_parser_snippets() {
        let dir = tempfile::tempdir().expect("should create temp directory");
        let path = dir.path().join("invalid.toml");
        fs::write(&path, "secret = NEVER_PRINT_ME").expect("should write fixture");
        let error = inspect(&path).err().expect("should reject invalid TOML");
        assert!(!format!("{error:?}").contains("NEVER_PRINT_ME"));
    }

    #[test]
    fn invalid_list_identifier_reports_identifier_error() {
        let file = tempfile::NamedTempFile::new().expect("should create config");
        fs::write(
            file.path(),
            r#"
[auction.prebid]
client_side_bidders = 'examplebidder\'
"#,
        )
        .expect("should write config");

        let error = inspect(file.path())
            .err()
            .expect("should reject invalid identifier");

        assert!(
            error
                .to_string()
                .contains("invalid bidder or bundle-module identifier"),
            "should report identifier validation: {error}"
        );
    }

    #[test]
    fn accepts_the_runtime_browser_bidder_list_encodings() {
        for input in [
            "['examplebidder', 'otherbidder']",
            "'examplebidder,otherbidder'",
            "'[examplebidder, otherbidder]'",
            "'[\"examplebidder\", \"otherbidder\"]'",
            "'example\\u0062idder,otherbidder'",
            "{ '10' = 'otherbidder', '2' = 'examplebidder' }",
        ] {
            let text = format!("[auction.prebid]\nclient_side_bidders={input}\n");
            let file = tempfile::NamedTempFile::new().expect("should create config");
            fs::write(file.path(), &text).expect("should write config");
            let runtime: trusted_server_core::integrations::prebid::PrebidIntegrationConfig =
                toml::from_str(&format!("client_side_bidders={input}"))
                    .expect("runtime should accept encoding");
            let output = inspect(file.path()).expect("inspect should accept runtime encoding");
            assert_eq!(
                output.data["client_side_bidders"],
                json!(runtime.client_side_bidders)
            );
        }
    }

    #[test]
    fn bundle_selections_match_core_without_expanding_defaults() {
        for input in [
            "",
            "[bundle]",
            "[bundle.modules]",
            "[bundle.modules]\nbidder = []\nuser_id = []\nanalytics = []",
            "[bundle.modules]\nbidder = ['exampleBidAdapter', 'otherBidAdapter']\nuser_id = ['sharedIdSystem']\nanalytics = ['exampleAnalyticsAdapter']",
            "[bundle.modules]\nuser_id = ['sharedIdSystem']",
            "[bundle.modules]\nanalytics = ['exampleAnalyticsAdapter']",
        ] {
            let runtime: trusted_server_core::integrations::prebid::PrebidIntegrationConfig =
                toml::from_str(input).expect("should parse current core bundle schema");
            let text = format!(
                "[auction.prebid]\n{}",
                input.replace("[bundle", "[auction.prebid.bundle")
            );
            let file = tempfile::NamedTempFile::new().expect("should create config");
            fs::write(file.path(), &text).expect("should write config");
            let output = inspect(file.path()).expect("should inspect current core bundle schema");
            assert_eq!(
                output.data["bundle_adapters"],
                json!(runtime.bundle.modules.bidder)
            );
            assert_eq!(
                output.data["identity_modules"],
                json!(runtime.bundle.modules.user_id.unwrap_or_default())
            );
            assert_eq!(
                output.data["analytics_modules"],
                json!(runtime.bundle.modules.analytics.unwrap_or_default())
            );
        }
    }

    #[test]
    fn retired_bundle_fields_are_not_reported_as_current_selections() {
        let input =
            "[bundle]\nadapters = ['exampleBidAdapter']\nuser_id_modules = ['sharedIdSystem']";
        assert!(
            toml::from_str::<trusted_server_core::integrations::prebid::PrebidIntegrationConfig>(
                input
            )
            .is_err(),
            "core should reject the retired bundle schema"
        );
        let file = tempfile::NamedTempFile::new().expect("should create config");
        fs::write(
            file.path(),
            input.replace("[bundle]", "[auction.prebid.bundle]"),
        )
        .expect("should write config");
        // Inspection is deliberately partial, not full runtime validation.
        let output =
            inspect(file.path()).expect("should ignore fields outside the inspected schema");
        for field in ["bundle_adapters", "identity_modules", "analytics_modules"] {
            assert_eq!(output.data[field], json!([]), "should not infer {field}");
        }
    }

    #[test]
    fn invalid_bundle_selections_never_echo_source_values() {
        for field in ["bidder", "user_id", "analytics"] {
            for value in ["['NEVER_PRINT_ME invalid']", "'NEVER_PRINT_ME'"] {
                let file = tempfile::NamedTempFile::new().expect("should create config");
                fs::write(
                    file.path(),
                    format!("[auction.prebid.bundle.modules]\n{field} = {value}\n"),
                )
                .expect("should write config");
                let error = inspect(file.path())
                    .err()
                    .expect("should reject invalid module selection");
                assert!(!format!("{error:?}").contains("NEVER_PRINT_ME"));
            }
        }
    }

    #[test]
    fn missing_section_stays_unresolved_instead_of_enabling_defaults() {
        let dir = tempfile::tempdir().expect("should create temp directory");
        let path = dir.path().join("empty.toml");
        fs::write(&path, "").expect("should write fixture");
        let output = inspect(&path).expect("should inspect empty file");
        assert_eq!(output.data["section_present"], false);
        assert_eq!(output.data["selected"], false);
    }
}
