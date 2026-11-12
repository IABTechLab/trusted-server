//! The `pull-sync` provider: the existing per-partner pull sync, run as an
//! identity provider on page-load navigations.

use std::borrow::Cow;
use std::num::NonZeroUsize;

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::ec::eid_types::SourceDomain;
use trusted_server_core::ec::provider::{
    EidLevelFields, EidShape, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities,
    ProviderCommon, ProviderConfigError, ProviderEid, ProviderError, ProviderExpiry, ProviderKind,
    ProviderRequest, ProviderResponse, ProviderUid, SourceCapability, SourceInput,
};
use trusted_server_core::ec::registry::PULL_SYNC_KIND;
use url::Url;

/// `pull-sync` kind.
pub struct PullSync;

/// `pull-sync` has no provider-specific settings; everything comes from the
/// partner's existing pull-sync fields.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullSyncSettings {}

/// A built `pull-sync` provider for one partner.
pub struct PullSyncProvider {
    endpoint: Url,
    source: SourceDomain,
    authorization: Option<http::HeaderValue>,
    capabilities: ProviderCapabilities,
}

#[derive(Deserialize)]
struct PullSyncBody<'a> {
    #[serde(borrow)]
    uid: Option<Cow<'a, str>>,
}

impl ProviderKind for PullSync {
    const KIND: &'static str = PULL_SYNC_KIND;
    type Settings = PullSyncSettings;
    type Provider = PullSyncProvider;

    fn build(
        common: &ProviderCommon<'_>,
        _settings: PullSyncSettings,
    ) -> Result<PullSyncProvider, Report<ProviderConfigError>> {
        let [source] = common.sources else {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach("pull-sync claims exactly one source"));
        };
        let authorization = common
            .secrets
            .token
            .as_ref()
            .map(|token| {
                let mut value = http::HeaderValue::try_from(format!("Bearer {}", token.expose()))
                    .map_err(|_| Report::new(ProviderConfigError::Invalid))?;
                value.set_sensitive(true);
                Ok::<_, Report<ProviderConfigError>>(value)
            })
            .transpose()?;
        let endpoint = common.endpoint.cloned().ok_or_else(|| {
            Report::new(ProviderConfigError::Invalid)
                .attach("a pull-sync provider needs an endpoint")
        })?;
        let one = NonZeroUsize::MIN;
        Ok(PullSyncProvider {
            endpoint,
            source: source.clone(),
            authorization,
            capabilities: ProviderCapabilities {
                sources: Box::new([SourceCapability {
                    source: source.clone(),
                    input: SourceInput::Anonymous,
                }]),
                shape: EidShape {
                    max_eids: one,
                    max_uids_per_eid: one,
                    eid_level_fields: EidLevelFields::Ignored,
                },
            },
        })
    }
}

impl IdentityProvider for PullSyncProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        let mut url = self.endpoint.clone();
        url.query_pairs_mut().append_pair("ec_id", lookup.ec_id);
        let mut headers = http::HeaderMap::new();
        if let Some(value) = &self.authorization {
            headers.insert(http::header::AUTHORIZATION, value.clone());
        }
        Ok(ProviderRequest {
            method: http::Method::GET,
            url,
            headers,
            body: bytes::Bytes::new(),
            context: None,
        })
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        let body: PullSyncBody<'body> = serde_json::from_slice(response.body)
            .map_err(|_| Report::new(ProviderError::InvalidResponse))?;
        let Some(uid) = body.uid.filter(|uid| !uid.trim().is_empty()) else {
            return Ok(LookupOutcome::NoMatch);
        };
        Ok(LookupOutcome::Found(vec![ProviderEid {
            source: Cow::Owned(self.source.as_str().to_owned()),
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
            uids: vec![ProviderUid {
                id: uid,
                atype: None,
                ext: None,
                expiry: ProviderExpiry::Unspecified,
                refresh: None,
            }],
        }]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::consent::ConsentContext;
    use trusted_server_core::ec::eid_types::UnixSeconds;
    use trusted_server_core::ec::provider::{ClientSignals, OwnedEntries, RequestCookies};
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::ProviderSecrets;

    fn provider() -> PullSyncProvider {
        let endpoint = Url::parse("https://sync.example.com/pull?x=1").expect("should parse");
        let sources = [SourceDomain::try_from("partner.example.com").expect("should parse")];
        let secrets = ProviderSecrets {
            token: Some(Redacted::new("example-token".to_owned())),
        };
        PullSync::build(
            &ProviderCommon {
                id: "pull-sync:partner.example.com",
                endpoint: Some(&endpoint),
                sources: &sources,
                secrets: &secrets,
            },
            PullSyncSettings {},
        )
        .expect("should build")
    }

    #[test]
    fn request_carries_ec_id_and_sensitive_bearer_token() {
        let provider = provider();
        let consent = ConsentContext::default();
        let due = [SourceDomain::try_from("partner.example.com").expect("should parse")];

        let request = provider
            .build_request(&LookupInput {
                ec_id: "abc.DEF123",
                consent: &consent,
                client: ClientSignals {
                    ip: None,
                    user_agent: None,
                },
                publisher_domain: "www.example.com",
                due_sources: &due,
                current: OwnedEntries::empty(),
                cookies: RequestCookies::default(),
                hashed_email: None,
                now: UnixSeconds::new(1_700_000_000),
            })
            .expect("should build request");

        assert_eq!(
            request.url.as_str(),
            "https://sync.example.com/pull?x=1&ec_id=abc.DEF123",
            "should append ec_id"
        );
        let auth = request
            .headers
            .get(http::header::AUTHORIZATION)
            .expect("should send token");
        assert_eq!(auth, "Bearer example-token", "should use bearer auth");
        assert!(auth.is_sensitive(), "should mark the token sensitive");
    }

    #[test]
    fn parses_uid_and_treats_null_as_no_match() {
        let provider = provider();
        let headers = http::HeaderMap::new();
        let respond = |body: &'static [u8]| ProviderResponse {
            status: http::StatusCode::OK,
            headers: &headers,
            body,
            context: None,
        };

        let found = provider
            .parse_response(respond(b"{\"uid\":\"u-1\"}"))
            .expect("should parse");
        let none = provider
            .parse_response(respond(b"{\"uid\":null}"))
            .expect("should parse");
        let empty = provider
            .parse_response(respond(b"{\"uid\":\"  \"}"))
            .expect("should parse");
        let invalid = provider.parse_response(respond(b"not json"));

        let LookupOutcome::Found(eids) = found else {
            panic!("should find a UID")
        };
        assert_eq!(
            eids[0].source, "partner.example.com",
            "should use the partner source"
        );
        assert_eq!(eids[0].uids[0].id, "u-1", "should borrow the UID");
        assert!(
            matches!(none, LookupOutcome::NoMatch),
            "should treat null as no match"
        );
        assert!(
            matches!(empty, LookupOutcome::NoMatch),
            "should treat an empty UID as no match"
        );
        assert!(invalid.is_err(), "should reject invalid JSON");
    }
}
