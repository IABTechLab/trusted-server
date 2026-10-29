//! Generate test-only Viceroy mappings using the production backend naming policy.

use core::error::Error;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use trusted_server_core::auction::compile_auction_plan;
use trusted_server_core::auction::plan::ProviderPlan;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::platform::{AuctionTargetId, BackendNamingPolicy, PlatformBackendSpec};

const POLICY: BackendNamingPolicy = BackendNamingPolicy::Fastly;
type TestError = Box<dyn Error + Send + Sync>;

fn main() -> Result<(), TestError> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: publisher_registration_backends APP_CONFIG LOOPBACK_ORIGIN MAPPINGS_TOML EVIDENCE_JSON".into());
    }
    let (mappings, evidence) = generate(&fs::read_to_string(&args[0])?, &args[1])?;
    fs::write(&args[2], mappings)?;
    fs::write(&args[3], serde_json::to_string_pretty(&evidence)?)?;
    Ok(())
}

// Mirror ProviderPlan::backend_spec_with_transport_timeout using public fields.
// In particular, retain endpoint.port() rather than inventing an explicit port,
// leave the Host override absent, and use the exact provider ID discriminator.
fn provider_spec(
    provider: &ProviderPlan,
    timeout_ms: u32,
) -> Result<PlatformBackendSpec, TestError> {
    let endpoint = Url::parse(provider.endpoint.as_str())?;
    let timeout = Duration::from_millis(u64::from(timeout_ms));
    Ok(PlatformBackendSpec {
        scheme: endpoint.scheme().to_owned(),
        host: endpoint
            .host_str()
            .ok_or("provider host missing")?
            .to_owned(),
        port: endpoint.port(),
        host_header_override: None,
        certificate_check: true,
        first_byte_timeout: timeout,
        between_bytes_timeout: timeout,
        discriminator: Some(provider.id.as_str().to_owned()),
    })
}

fn reachable_timers(budget_ms: u32, configured_ms: u32) -> BTreeSet<u32> {
    // Enumeration deliberately delegates bucketing to the shared policy. This
    // small fixture budget needs no copied knowledge of Fastly's timer ladder.
    let mut timers = BTreeSet::from([configured_ms]);
    timers.extend((1..=budget_ms).filter_map(|remaining_ms| {
        let timeout = POLICY.canonicalize_transport_timeout_ms(remaining_ms, configured_ms);
        (timeout > 0).then_some(timeout)
    }));
    timers
}

fn generate(app_config: &str, loopback_origin: &str) -> Result<(String, Value), TestError> {
    let origin = Url::parse(loopback_origin)?;
    if origin.scheme() != "http"
        || origin.host_str() != Some("127.0.0.1")
        || origin.port().is_none()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        return Err(
            "fixture origin must be a loopback HTTP URL with an explicit port and no path prefix"
                .into(),
        );
    }
    let app: TrustedServerAppConfig = toml::from_str(app_config)?;
    let app = TrustedServerAppConfig::new(app.into_settings())?;
    let settings = app.settings();
    let plan = compile_auction_plan(settings)?;
    plan.validate_for_target(AuctionTargetId::Fastly)?;
    if !plan.enabled() || plan.providers().len() != 1 || !settings.proxy.certificate_check {
        return Err(
            "fixture must enable exactly one provider and retain certificate checks".into(),
        );
    }
    let provider = &plan.providers()[0];
    let endpoint = Url::parse(provider.endpoint.as_str())?;
    if endpoint.scheme() != "https" || provider.profile.id() != "prebid-server" {
        return Err("fixture provider must use HTTPS and the prebid-server profile".into());
    }
    let fixture_budget = settings
        .creative_opportunities
        .as_ref()
        .and_then(|config| config.auction_timeout_ms)
        .unwrap_or(settings.auction.timeout_ms);
    let budget = settings.auction.timeout_ms.max(fixture_budget);
    let mut names = BTreeSet::new();
    let mut backends = Vec::new();
    let mut mappings = String::from("\n# Test-only static provider transport overrides.\n");
    for timeout_ms in reachable_timers(budget, provider.timeout_ms) {
        let spec = provider_spec(provider, timeout_ms)?;
        let predicted = POLICY.predict(&spec)?;
        if names.insert(predicted.name.clone()) {
            mappings.push_str(&format!(
                "[local_server.backends.{:?}]\nurl = {:?}\n",
                predicted.name,
                origin.as_str()
            ));
            backends.push(json!({
                "name": predicted.name,
                "transport_timeout_ms": timeout_ms,
                "resolved_port": predicted.port,
            }));
        }
    }
    let evidence = json!({
        "provider_id": provider.id.as_str(),
        "endpoint": provider.endpoint.as_str(),
        "provider_path": endpoint.path(),
        "profile": provider.profile.id(),
        "configured_timeout_ms": provider.timeout_ms,
        "auction_budget_ms": budget,
        "fixture_budget_ms": fixture_budget,
        "certificate_check": true,
        "host_header_override": null,
        "discriminator": provider.id.as_str(),
        "loopback_origin": origin.as_str(),
        "backends": backends,
    });
    Ok((mappings, evidence))
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str =
        include_str!("../fixtures/configs/trusted-server.publisher-registration.toml");

    #[test]
    fn covers_full_reduced_and_minimum_positive_budgets() {
        let timers = reachable_timers(1500, 1000);
        assert!(timers.contains(&1000), "should cover the configured timer");
        assert!(timers.contains(&250), "should cover reduced budget");
        assert!(timers.contains(&50), "should cover minimum positive timer");
        assert!(!timers.contains(&0), "should omit expired budgets");
        for remaining in 1..=1500 {
            let timer = POLICY.canonicalize_transport_timeout_ms(remaining, 1000);
            assert!(timer == 0 || timers.contains(&timer));
        }
    }

    #[test]
    fn preserves_provider_discriminator_flags_and_path() {
        let app: TrustedServerAppConfig = toml::from_str(APP).expect("should parse fixture");
        let plan = compile_auction_plan(app.settings()).expect("should compile fixture provider");
        let provider = &plan.providers()[0];
        let spec = provider_spec(provider, 250).expect("should build provider spec");
        assert_eq!(spec.scheme, "https");
        assert_eq!(spec.host, "auction.example.com");
        assert_eq!(
            spec.port, None,
            "should preserve default HTTPS port semantics"
        );
        assert_eq!(spec.discriminator.as_deref(), Some("fixture-pbs"));
        assert!(spec.certificate_check);
        assert!(spec.host_header_override.is_none());
        assert_eq!(spec.first_byte_timeout, Duration::from_millis(250));
        assert_eq!(spec.between_bytes_timeout, Duration::from_millis(250));
        let (mappings, evidence) = generate(APP, "http://127.0.0.1:8888").expect("should generate");
        let parsed: toml::Value = toml::from_str(&mappings).expect("should parse rendered TOML");
        let predicted = POLICY
            .predict(&spec)
            .expect("should predict provider backend");
        assert_eq!(predicted.port, 443);
        assert_eq!(
            parsed["local_server"]["backends"][&predicted.name]["url"].as_str(),
            Some("http://127.0.0.1:8888/")
        );
        assert_eq!(evidence["provider_path"], "/api/fixture-auction");
        assert_eq!(
            Url::parse("http://127.0.0.1:8888")
                .expect("should parse origin")
                .join(
                    evidence["provider_path"]
                        .as_str()
                        .expect("should retain path")
                )
                .expect("should join path")
                .path(),
            "/api/fixture-auction"
        );
        assert_eq!(
            parsed["local_server"]["backends"]
                .as_table()
                .expect("should contain backends")
                .len(),
            evidence["backends"]
                .as_array()
                .expect("should list backends")
                .len()
        );
    }

    #[test]
    fn rejects_path_prefixes_and_https_validation_regressions() {
        assert!(generate(APP, "http://127.0.0.1:8888/api/fixture-auction").is_err());
        assert!(
            generate(
                &APP.replace("https://auction.example.com", "http://auction.example.com"),
                "http://127.0.0.1:8888"
            )
            .is_err()
        );
        assert!(
            generate(
                &APP.replace("certificate_check = true", "certificate_check = false"),
                "http://127.0.0.1:8888"
            )
            .is_err()
        );
    }
}
