//! Pixel sync endpoint (`GET /sync`).

use error_stack::{Report, ResultExt};
use fastly::erl::{CounterDuration, RateCounter};
use fastly::http::StatusCode;
use fastly::{Request, Response};
use url::Url;

use crate::consent::{allows_ec_creation, gpp, tcf, ConsentContext};
use crate::error::TrustedServerError;
use crate::settings::Settings;

use super::generation::{ec_hash, is_valid_ec_id};
use super::kv::KvIdentityGraph;
use super::partner::{PartnerRecord, PartnerStore};
use super::EcContext;

const RATE_COUNTER_NAME: &str = "counter_store";

/// Handles `GET /sync` pixel sync requests.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when request validation fails (`400`) or
/// required stores are unavailable (`503`).
pub fn handle_sync(
    _settings: &Settings,
    kv: &KvIdentityGraph,
    partner_store: &PartnerStore,
    req: &Request,
    ec_context: &mut EcContext,
) -> Result<Response, Report<TrustedServerError>> {
    let query = SyncQuery::parse(req)?;

    let partner = partner_store.get(&query.partner)?.ok_or_else(|| {
        Report::new(TrustedServerError::BadRequest {
            message: format!("unknown partner '{}'", query.partner),
        })
    })?;

    let return_url = validate_return_url(&query.return_url, &partner)?;

    let Some(cookie_ec_id) = ec_context
        .existing_cookie_ec_id()
        .filter(|v| is_valid_ec_id(v))
        .map(str::to_owned)
    else {
        return Ok(redirect_with_status(&return_url, "0", Some("no_ec")));
    };

    if ec_context.consent().is_empty() {
        if let Some(consent_query) = query.consent.as_deref() {
            if let Some(fallback) =
                decode_query_fallback_consent(ec_context.consent(), consent_query)
            {
                *ec_context.consent_mut() = fallback;
            }
        }
    }

    if !allows_ec_creation(ec_context.consent()) {
        return Ok(redirect_with_status(&return_url, "0", Some("no_consent")));
    }

    let hash = ec_hash(&cookie_ec_id);
    let limiter = FastlyRateLimiter::new(RATE_COUNTER_NAME);
    if limiter.exceeded(&format!("{}:{hash}", partner.id), partner.sync_rate_limit)? {
        return Ok(Response::from_status(StatusCode::TOO_MANY_REQUESTS)
            .with_body_text_plain("rate_limit_exceeded"));
    }

    if let Err(err) = kv.upsert_partner_id(hash, &partner.id, &query.uid) {
        log::warn!(
            "Pixel sync write failed for partner '{}' and hash '{}': {err:?}",
            partner.id,
            hash,
        );
        return Ok(redirect_with_status(&return_url, "0", Some("write_failed")));
    }

    Ok(redirect_with_status(&return_url, "1", None))
}

#[derive(Debug)]
struct SyncQuery {
    partner: String,
    uid: String,
    return_url: String,
    consent: Option<String>,
}

impl SyncQuery {
    fn parse(req: &Request) -> Result<Self, Report<TrustedServerError>> {
        let mut partner = None;
        let mut uid = None;
        let mut return_url = None;
        let mut consent = None;

        let raw_query = req.get_query_str().unwrap_or("");
        for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
            match key.as_ref() {
                "partner" => partner = Some(value.into_owned()),
                "uid" => uid = Some(value.into_owned()),
                "return" => return_url = Some(value.into_owned()),
                "consent" => consent = Some(value.into_owned()),
                _ => {}
            }
        }

        Ok(Self {
            partner: required_query_param(partner, "partner")?,
            uid: required_query_param(uid, "uid")?,
            return_url: required_query_param(return_url, "return")?,
            consent,
        })
    }
}

fn required_query_param(
    value: Option<String>,
    key: &str,
) -> Result<String, Report<TrustedServerError>> {
    let Some(value) = value else {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: format!("missing required query parameter '{key}'"),
        }));
    };

    if value.trim().is_empty() {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: format!("query parameter '{key}' must not be empty"),
        }));
    }

    Ok(value)
}

fn validate_return_url(
    return_url: &str,
    partner: &PartnerRecord,
) -> Result<Url, Report<TrustedServerError>> {
    let parsed = Url::parse(return_url).change_context(TrustedServerError::BadRequest {
        message: "return URL must be a valid absolute URL".to_owned(),
    })?;

    let host = parsed
        .host_str()
        .ok_or_else(|| {
            Report::new(TrustedServerError::BadRequest {
                message: "return URL must include a hostname".to_owned(),
            })
        })?
        .trim_end_matches('.')
        .to_ascii_lowercase();

    let allowed = partner
        .allowed_return_domains
        .iter()
        .map(|domain| domain.trim().trim_end_matches('.').to_ascii_lowercase())
        .any(|domain| domain == host);

    if !allowed {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: format!(
                "return URL host '{host}' is not allowed for partner '{}'",
                partner.id
            ),
        }));
    }

    Ok(parsed)
}

fn redirect_with_status(return_url: &Url, synced: &str, reason: Option<&str>) -> Response {
    let mut url = return_url.clone();
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("ts_synced", synced);
        if let Some(reason) = reason {
            query.append_pair("ts_reason", reason);
        }
    }

    Response::from_status(StatusCode::FOUND).with_header("location", url.as_str())
}

fn decode_query_fallback_consent(
    base: &ConsentContext,
    raw_consent: &str,
) -> Option<ConsentContext> {
    if raw_consent.trim().is_empty() {
        return None;
    }

    let mut consent = ConsentContext {
        jurisdiction: base.jurisdiction.clone(),
        gpc: base.gpc,
        ..ConsentContext::default()
    };

    if raw_consent.contains('~') || raw_consent.starts_with("DB") {
        match gpp::decode_gpp_string(raw_consent) {
            Ok(decoded) => {
                consent.raw_gpp_string = Some(raw_consent.to_owned());
                consent.gpp_section_ids = Some(decoded.section_ids.clone());
                consent.tcf = decoded.eu_tcf.clone();
                consent.gpp = Some(decoded);
                consent.gdpr_applies = consent
                    .gpp_section_ids
                    .as_ref()
                    .is_some_and(|sids| sids.contains(&2));
                return Some(consent);
            }
            Err(err) => {
                log::warn!("Failed to decode GPP consent query fallback: {err:?}");
                return None;
            }
        }
    }

    match tcf::decode_tc_string(raw_consent) {
        Ok(decoded) => {
            consent.raw_tc_string = Some(raw_consent.to_owned());
            consent.tcf = Some(decoded);
            consent.gdpr_applies = true;
            Some(consent)
        }
        Err(err) => {
            log::warn!("Failed to decode TCF consent query fallback: {err:?}");
            None
        }
    }
}

trait RateLimiter {
    fn exceeded(&self, key: &str, hourly_limit: u32) -> Result<bool, Report<TrustedServerError>>;
}

struct FastlyRateLimiter {
    counter: RateCounter,
}

impl FastlyRateLimiter {
    fn new(counter_name: &str) -> Self {
        Self {
            counter: RateCounter::open(counter_name),
        }
    }
}

impl RateLimiter for FastlyRateLimiter {
    fn exceeded(&self, key: &str, hourly_limit: u32) -> Result<bool, Report<TrustedServerError>> {
        // Fastly's public rate-counter API currently exposes windows up to 60s.
        // Approximate the story's 1h limit by converting to a per-minute budget.
        //
        // Follow-up: move to exact 1-hour enforcement once platform counters
        // expose longer windows or we add a dedicated KV-backed hour bucket.
        let per_minute_limit = hourly_limit.saturating_add(59) / 60;
        let per_minute_limit = per_minute_limit.max(1);

        let current = self
            .counter
            .lookup_count(key, CounterDuration::SixtySecs)
            .map_err(|e| {
                Report::new(TrustedServerError::KvStore {
                    store_name: RATE_COUNTER_NAME.to_owned(),
                    message: format!("Failed to read sync rate counter: {e}"),
                })
            })?;

        if current >= per_minute_limit {
            return Ok(true);
        }

        self.counter.increment(key, 1).map_err(|e| {
            Report::new(TrustedServerError::KvStore {
                store_name: RATE_COUNTER_NAME.to_owned(),
                message: format!("Failed to increment sync rate counter: {e}"),
            })
        })?;

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_partner() -> PartnerRecord {
        PartnerRecord {
            id: "ssp_x".to_owned(),
            name: "SSP X".to_owned(),
            allowed_return_domains: vec!["sync.example.com".to_owned()],
            api_key_hash: "deadbeef".to_owned(),
            bidstream_enabled: false,
            source_domain: "ssp.example.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 100,
            batch_rate_limit: 60,
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: 86400,
            pull_sync_rate_limit: 10,
            ts_pull_token: None,
        }
    }

    #[test]
    fn redirect_appends_query_when_url_has_none() {
        let url = Url::parse("https://sync.example.com/return").expect("should parse URL");
        let response = redirect_with_status(&url, "1", None);
        let location = response
            .get_header("location")
            .expect("should set location header")
            .to_str()
            .expect("should convert location to UTF-8");

        assert_eq!(
            location, "https://sync.example.com/return?ts_synced=1",
            "should append query with ? when missing"
        );
    }

    #[test]
    fn redirect_appends_query_when_url_already_has_query() {
        let url = Url::parse("https://sync.example.com/return?foo=bar").expect("should parse URL");
        let response = redirect_with_status(&url, "0", Some("no_ec"));
        let location = response
            .get_header("location")
            .expect("should set location header")
            .to_str()
            .expect("should convert location to UTF-8");

        assert_eq!(
            location, "https://sync.example.com/return?foo=bar&ts_synced=0&ts_reason=no_ec",
            "should append sync status after existing query"
        );
    }

    #[test]
    fn fallback_decodes_tcf() {
        let base = ConsentContext::default();
        let decoded =
            decode_query_fallback_consent(&base, "CPXxGfAPXxGfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA")
                .expect("should decode TCF fallback");

        assert!(
            decoded.raw_tc_string.is_some(),
            "should store raw TC string"
        );
    }

    #[test]
    fn query_parse_rejects_missing_required_param() {
        let req = Request::new("GET", "https://edge.example.com/sync?partner=ssp&uid=u1");
        let err = SyncQuery::parse(&req).expect_err("should fail when return param is missing");
        assert!(
            err.to_string()
                .contains("missing required query parameter 'return'"),
            "should mention missing required return parameter"
        );
    }

    #[test]
    fn query_parse_rejects_empty_required_param() {
        let req = Request::new(
            "GET",
            "https://edge.example.com/sync?partner=ssp&uid=u1&return=   ",
        );
        let err = SyncQuery::parse(&req).expect_err("should fail when return param is empty");
        assert!(
            err.to_string()
                .contains("query parameter 'return' must not be empty"),
            "should reject empty required return parameter"
        );
    }

    #[test]
    fn return_url_validation_rejects_subdomain_spoofing() {
        let partner = sample_partner();
        let err = validate_return_url("https://a.sync.example.com/callback", &partner)
            .expect_err("should reject return host not exactly allowlisted");

        assert!(
            err.to_string().contains("is not allowed"),
            "should reject non-exact allowlist host"
        );
    }

    #[test]
    fn return_url_validation_rejects_relative_url() {
        let partner = sample_partner();
        let err = validate_return_url("/callback", &partner)
            .expect_err("should reject non-absolute return URL");
        assert!(
            err.to_string().contains("valid absolute URL"),
            "should require absolute return URLs"
        );
    }

    #[test]
    fn fallback_decodes_gpp() {
        let base = ConsentContext::default();
        let decoded = decode_query_fallback_consent(&base, "DBABTA~1YNN")
            .expect("should decode valid GPP fallback");

        assert!(
            decoded.raw_gpp_string.is_some(),
            "should store raw GPP string"
        );
    }

    #[test]
    fn fallback_returns_none_for_invalid_consent_string() {
        let base = ConsentContext::default();
        let decoded = decode_query_fallback_consent(&base, "not-a-valid-consent");
        assert!(
            decoded.is_none(),
            "should ignore undecodable consent fallback"
        );
    }
}
