use std::collections::BTreeMap;
use std::path::Path;

use error_stack::Report;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use trusted_server_core::auction_config_types::{BidderId, ProviderId};

use super::{Output, PbsError, Result, identifier, read_text};

/// Deliberately partial: inspecting PBS requirements must not require unrelated TS settings.
#[derive(Default, Deserialize)]
struct Source {
    auction: Option<Auction>,
    #[serde(default)]
    integrations: Integrations,
}

#[derive(Default, Deserialize)]
struct Auction {
    enabled: Option<bool>,
    #[serde(default)]
    providers: BTreeMap<ProviderId, AuctionProvider>,
    #[serde(default)]
    bidders: BTreeMap<BidderId, AuctionBidder>,
}

#[derive(Deserialize)]
struct AuctionProvider {
    profile: Option<String>,
    endpoint: Option<String>,
    timeout_ms: Option<u32>,
    profile_config: Option<toml::Value>,
}

impl AuctionProvider {
    fn profile_bool(&self, key: &str) -> Option<bool> {
        self.profile_config.as_ref()?.get(key)?.as_bool()
    }

    fn override_rule_count(&self) -> usize {
        self.profile_config
            .as_ref()
            .and_then(|config| config.get("bid_param_override_rules"))
            .and_then(toml::Value::as_array)
            .map_or(0, Vec::len)
    }
}

#[derive(Deserialize)]
struct AuctionBidder {
    provider: ProviderId,
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
    endpoint_configured: bool,
    timeout_ms_explicit: Option<u32>,
    test_mode_explicit: Option<bool>,
    debug_explicit: Option<bool>,
    bid_param_override_rule_count: usize,
    server_bidder_candidates: Vec<ServerBidderCandidate>,
}

#[derive(Default, Deserialize)]
struct Integrations {
    prebid: Option<Prebid>,
}

#[derive(Default, Deserialize)]
struct Prebid {
    enabled: Option<bool>,
    account_id: Option<String>,
    timeout_ms: Option<u32>,
    debug: Option<bool>,
    #[serde(default, deserialize_with = "bidder_list")]
    client_side_bidders: Vec<String>,
    #[serde(default)]
    bundle: Bundle,
}

#[derive(Default, Deserialize)]
struct Bundle {
    #[serde(default)]
    adapters: Vec<String>,
    #[serde(default)]
    user_id_modules: Vec<String>,
}

/// Mirror the private core list deserializer without expanding runtime defaults.
/// Parity tests compare these accepted representations with `PrebidIntegrationConfig`.
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
                    match serde_json::from_str(&json) {
                        Ok(value) => value,
                        Err(_) => part.to_owned(),
                    }
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
    let prebid_present = source.integrations.prebid.is_some();
    let prebid = source.integrations.prebid.unwrap_or_default();
    for name in prebid
        .client_side_bidders
        .iter()
        .chain(&prebid.bundle.adapters)
        .chain(&prebid.bundle.user_id_modules)
    {
        if !identifier(name) {
            return Err(Report::new(PbsError::Input(
                "invalid bidder or identity-module identifier",
            )));
        }
    }
    let server_providers: Vec<_> = auction
        .providers
        .iter()
        .filter(|(_, provider)| provider.profile.as_deref() == Some("prebid-server"))
        .map(|(provider_id, provider)| {
            let requirements: Vec<_> = auction
                .bidders
                .iter()
                .filter(|(_, bidder)| &bidder.provider == provider_id)
                .map(|(bidder_id, _)| ServerBidderCandidate {
                    bidder: bidder_id.as_str().to_owned(),
                    source_key: format!("auction.bidders.{}.provider", bidder_id.as_str()),
                    host_secret_requirement: "unresolved",
                    partner_authorization: "unresolved",
                })
                .collect();
            ServerProviderReport {
                provider: provider_id.as_str().to_owned(),
                endpoint_configured: provider.endpoint.is_some(),
                timeout_ms_explicit: provider.timeout_ms,
                test_mode_explicit: provider.profile_bool("test_mode"),
                debug_explicit: provider.profile_bool("debug"),
                bid_param_override_rule_count: provider.override_rule_count(),
                server_bidder_candidates: requirements,
            }
        })
        .collect();
    let warnings = [
        "Local file only: confirm environment, remote configuration, and request-time overrides.",
        "Omitted fields/defaults are not expanded; empty candidate lists are not proof of no demand.",
        "Disabled auctions, providers, and browser bundle adapters do not authorize PBS activation.",
        "Host secret requirements need adapter metadata verified against the selected PBS release.",
    ];
    let mut details = vec![
        format!(
            "Auction section present: {auction_present}; enabled explicitly: {:?}",
            auction.enabled
        ),
        format!(
            "Prebid browser section present: {prebid_present}; enabled explicitly: {:?}",
            prebid.enabled
        ),
        format!("Prebid Server providers: {}", server_providers.len()),
    ];
    details.extend(server_providers.iter().map(|provider| {
        let bidders = provider
            .server_bidder_candidates
            .iter()
            .map(|candidate| candidate.bidder.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Server provider {}: bidders: {}; endpoint configured: {}; timeout explicit: {:?}; test mode explicit: {:?}; debug explicit: {:?}; bid-parameter rules: {}; values withheld",
            provider.provider,
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
            prebid.bundle.adapters.join(", ")
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
                "server": ["auction.providers", "auction.bidders"],
                "browser": "integrations.prebid"
            },
            "auction_section_present": auction_present,
            "auction_enabled_explicit": auction.enabled,
            "server_providers": server_providers,
            "section_present": prebid_present,
            "enabled_explicit": prebid.enabled,
            "account_id_configured": prebid.account_id.is_some(),
            "timeout_ms_explicit": prebid.timeout_ms,
            "debug_explicit": prebid.debug,
            "client_side_bidders": prebid.client_side_bidders,
            "bundle_adapters": prebid.bundle.adapters,
            "identity_modules": prebid.bundle.user_id_modules,
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

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://user:NEVER_PRINT_ME@pbs.example.com/path?token=NEVER_PRINT_ME"
timeout_ms = 900
routing = "explicit"

[auction.providers.pbs-main.profile_config]
debug = false
test_mode = true
bid_param_override_rules = [{ when = { bidder = "serverbidder" }, set = { placementId = "NEVER_PRINT_ME" } }]

[auction.bidders.serverbidder]
provider = "pbs-main"

[auction.providers.pbs-secondary]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://NEVER_PRINT_ME@secondary.example.com/openrtb2/auction"
routing = "explicit"

[auction.bidders.otherbidder]
provider = "pbs-secondary"

[integrations.prebid]
enabled = false
account_id = "NEVER_PRINT_ME"
client_side_bidders = ["browserbidder"]

[integrations.prebid.bundle]
adapters = ["bundlebidder"]
user_id_modules = ["sharedIdSystem"]
"#;
        fs::write(&path, source).expect("should write fixture");
        let report = inspect(&path).expect("should inspect config");
        assert_eq!(report.data["auction_section_present"], true);
        assert_eq!(report.data["auction_enabled_explicit"], true);
        assert_eq!(report.data["enabled_explicit"], false);
        assert_eq!(
            report.data["server_providers"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(report.data["server_providers"][0]["provider"], "pbs-main");
        assert_eq!(
            report.data["server_providers"][0]["server_bidder_candidates"][0]["bidder"],
            "serverbidder"
        );
        assert_eq!(
            report.data["server_providers"][1]["provider"],
            "pbs-secondary"
        );
        assert_eq!(
            report.data["server_providers"][1]["server_bidder_candidates"][0]["bidder"],
            "otherbidder"
        );
        assert_eq!(
            report.data["server_providers"][0]["server_bidder_candidates"][0]["source_key"],
            "auction.bidders.serverbidder.provider"
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
        assert_eq!(report.data["bundle_adapters"][0], "bundlebidder");
        assert_eq!(
            report.data["server_providers"][0]["server_bidder_candidates"][0]["host_secret_requirement"],
            "unresolved"
        );
        let mut human = Vec::new();
        report
            .write(false, &mut human)
            .expect("should render human report");
        let human = String::from_utf8(human).expect("should emit UTF-8");
        assert!(human.contains("pbs-main"));
        assert!(human.contains("serverbidder"));
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
[integrations.prebid]
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
                .contains("invalid bidder or identity-module identifier"),
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
            let text = format!("[integrations.prebid]\nclient_side_bidders={input}\n");
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
    fn missing_section_stays_unresolved_instead_of_enabling_defaults() {
        let dir = tempfile::tempdir().expect("should create temp directory");
        let path = dir.path().join("empty.toml");
        fs::write(&path, "").expect("should write fixture");
        let output = inspect(&path).expect("should inspect empty file");
        assert_eq!(output.data["section_present"], false);
        assert!(output.data["enabled_explicit"].is_null());
    }
}
