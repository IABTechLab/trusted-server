//! Shared fixtures for identity tests.

use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::{PrivacyFlag, TcfConsent, UsPrivacy};

/// Consent signals for building a [`ConsentContext`] in tests.
#[derive(Debug, Clone, Default)]
pub(crate) struct TestSignals {
    /// GDPR applies, with a decoded TC string granting these purposes.
    pub(crate) gdpr: bool,
    /// Granted TCF purposes (1-indexed).
    pub(crate) purposes: Vec<usize>,
    /// Granted GVL vendors.
    pub(crate) vendors: Vec<u16>,
    /// A decoded US Privacy string opting out of sale.
    pub(crate) us_sale_opt_out: bool,
    /// `Sec-GPC: 1`.
    pub(crate) gpc: bool,
    /// A raw TC string with no decoded form.
    pub(crate) raw_tc_without_decode: bool,
}

/// A decoded TC string granting `purposes` and `vendors`.
pub(crate) fn test_tcf(purposes: &[usize], vendors: &[u16]) -> TcfConsent {
    let max = purposes.iter().copied().max().unwrap_or(0).max(11);
    TcfConsent {
        version: 2,
        cmp_id: 1,
        cmp_version: 1,
        consent_screen: 1,
        consent_language: "EN".to_owned(),
        vendor_list_version: 1,
        tcf_policy_version: 4,
        created_ds: 1,
        last_updated_ds: 1,
        purpose_consents: (1..=max)
            .map(|purpose| purposes.contains(&purpose))
            .collect(),
        purpose_legitimate_interests: vec![false; max],
        vendor_consents: vendors.to_vec(),
        vendor_legitimate_interests: Vec::new(),
        special_feature_opt_ins: Vec::new(),
    }
}

/// Builds a consent context from `signals`. Without GDPR the jurisdiction is
/// non-regulated.
pub(crate) fn consent_with(signals: &TestSignals) -> ConsentContext {
    let mut context = ConsentContext {
        gdpr_applies: signals.gdpr,
        gpc: signals.gpc,
        jurisdiction: if signals.gdpr {
            Jurisdiction::Gdpr
        } else {
            Jurisdiction::NonRegulated
        },
        ..ConsentContext::default()
    };
    if signals.gdpr {
        context.raw_tc_string = Some("example-tc-string".to_owned());
        context.tcf = Some(test_tcf(&signals.purposes, &signals.vendors));
    }
    if signals.us_sale_opt_out {
        context.raw_us_privacy = Some("1YYN".to_owned());
        context.us_privacy = Some(UsPrivacy {
            version: 1,
            notice_given: PrivacyFlag::Yes,
            opt_out_sale: PrivacyFlag::Yes,
            lspa_covered: PrivacyFlag::No,
        });
    }
    if signals.raw_tc_without_decode {
        context.raw_tc_string = Some("undecodable".to_owned());
        context.tcf = None;
    }
    context
}

/// A consent context that every policy allows.
pub(crate) fn allowing_consent() -> ConsentContext {
    consent_with(&TestSignals::default())
}

/// A descriptor-only provider for tests.
pub(crate) struct FixedProvider(pub(crate) crate::ec::identity::ProviderDescriptor);

impl crate::ec::identity::IdentityProvider for FixedProvider {
    fn descriptor(&self) -> &crate::ec::identity::ProviderDescriptor {
        &self.0
    }
}

/// A resolving provider over `sources` with the given policy.
pub(crate) fn provider_with_policy(
    id: &str,
    sources: &[&str],
    policy: crate::ec::identity::ConsentPolicy,
    expiring: bool,
) -> std::sync::Arc<dyn crate::ec::identity::IdentityProvider> {
    let mut builder = crate::ec::identity::ProviderDescriptor::builder(
        id.parse().expect("should parse provider id"),
    );
    for source in sources {
        builder = builder.with_source(source.parse().expect("should parse source"));
    }
    builder = builder
        .with_resolves(
            crate::ec::identity::Resolves::new().with_endpoint("https://api.example.com/v1"),
        )
        .with_consent_policy(policy);
    if expiring {
        builder = builder.with_expiring_ids();
    }
    std::sync::Arc::new(FixedProvider(
        builder.build().expect("should build provider"),
    ))
}

/// A resolving provider with generic pull's permissive policy.
pub(crate) fn fixed_provider(
    id: &str,
    sources: &[&str],
) -> std::sync::Arc<dyn crate::ec::identity::IdentityProvider> {
    provider_with_policy(
        id,
        sources,
        crate::ec::identity::ConsentPolicy::eid_persistence_only(),
        false,
    )
}

/// Identity keys derived from the shared test settings.
pub(crate) fn test_keys() -> crate::ec::identity::revision::IdentityKeys {
    crate::ec::identity::revision::IdentityKeys::from_settings(
        &crate::test_support::tests::create_test_settings(),
    )
}

/// A catalog with one provider that owns `source`.
pub(crate) fn catalog_with_owner(
    provider: &str,
    source: &str,
) -> crate::ec::identity::catalog::ProviderCatalog {
    crate::ec::identity::catalog::ProviderCatalog::new(
        vec![fixed_provider(provider, &[source])],
        [(
            source.parse().expect("should parse source"),
            provider.parse().expect("should parse provider id"),
        )]
        .into_iter()
        .collect(),
    )
}

/// Facts for a returning visitor's writer navigation with no auction and
/// every input available.
pub(crate) fn facts_for_writer() -> crate::ec::identity::coordinator::RequestFacts {
    crate::ec::identity::coordinator::RequestFacts {
        is_real_browser: true,
        ec_persistence_allowed: true,
        policy_signals: crate::ec::identity::policy::PolicySignals::default(),
        writer: true,
        new_visitor: false,
        runs_auction: false,
        now: crate::ec::identity::UnixTime::from_secs(1_000),
        available_inputs: [
            crate::ec::identity::Input::ClientIp,
            crate::ec::identity::Input::UserAgent,
            crate::ec::identity::Input::PageUrl,
            crate::ec::identity::Input::Region,
        ]
        .into_iter()
        .collect(),
    }
}

/// Drives one provider the way the coordinator does, for provider tests.
pub(crate) struct ProviderHarness<'p> {
    provider: &'p dyn crate::ec::identity::IdentityProvider,
    client_ip: Option<std::net::IpAddr>,
    user_agent: Option<String>,
    ec_id: Option<String>,
    stored: std::collections::BTreeMap<
        crate::ec::identity::SourceDomain,
        crate::ec::identity::StoredId,
    >,
    state_json: Option<String>,
    now: crate::ec::identity::UnixTime,
    random: crate::ec::identity::input::SeededRandom,
    last_request: Option<crate::ec::identity::ProviderRequest>,
}

impl<'p> ProviderHarness<'p> {
    /// A harness with no inputs, at a fixed test time.
    pub(crate) fn new(provider: &'p dyn crate::ec::identity::IdentityProvider) -> Self {
        Self {
            provider,
            client_ip: None,
            user_agent: None,
            ec_id: None,
            stored: std::collections::BTreeMap::new(),
            state_json: None,
            now: crate::ec::identity::UnixTime::from_secs(1_760_000_000),
            random: crate::ec::identity::input::SeededRandom::new(1),
            last_request: None,
        }
    }

    /// Sets the client IP.
    #[must_use]
    pub(crate) fn with_client_ip(mut self, ip: &str) -> Self {
        self.client_ip = Some(ip.parse().expect("should parse the client IP"));
        self
    }

    /// Sets the user agent.
    #[must_use]
    pub(crate) fn with_user_agent(mut self, user_agent: &str) -> Self {
        self.user_agent = Some(user_agent.to_owned());
        self
    }

    /// Sets the EC ID, which only generic pull may read.
    #[must_use]
    pub(crate) fn with_ec_id(mut self, ec_id: &str) -> Self {
        self.ec_id = Some(ec_id.to_owned());
        self
    }

    /// Stores a record for `source`, which makes its kind `Refresh`.
    #[must_use]
    pub(crate) fn with_stored(
        mut self,
        source: &str,
        record: &crate::ec::kv_types::KvPartnerId,
    ) -> Self {
        self.stored.insert(
            source.parse().expect("should parse source"),
            crate::ec::identity::StoredId::from_record(record),
        );
        self
    }

    fn available_inputs(&self) -> std::collections::BTreeSet<crate::ec::identity::Input> {
        let mut available = std::collections::BTreeSet::new();
        if self.client_ip.is_some() {
            available.insert(crate::ec::identity::Input::ClientIp);
        }
        if self.user_agent.is_some() {
            available.insert(crate::ec::identity::Input::UserAgent);
        }
        available
    }

    fn kinds(
        &self,
    ) -> std::collections::BTreeMap<
        crate::ec::identity::SourceDomain,
        crate::ec::identity::ResolveKind,
    > {
        let descriptor = self.provider.descriptor();
        let Some(resolves) = descriptor.resolves() else {
            return std::collections::BTreeMap::new();
        };
        let available = self.available_inputs();
        descriptor
            .sources()
            .iter()
            .filter_map(|source| {
                let kind = if self.stored.contains_key(source) {
                    crate::ec::identity::ResolveKind::Refresh
                } else {
                    crate::ec::identity::ResolveKind::Initial
                };
                let needs_met = resolves
                    .needs(kind)
                    .iter()
                    .all(|input| available.contains(input));
                (resolves.kinds().contains(&kind) && needs_met).then(|| (source.clone(), kind))
            })
            .collect()
    }

    fn signals(&self) -> crate::ec::identity::input::ResolveSignals<'_> {
        crate::ec::identity::input::ResolveSignals {
            client_ip: self.client_ip,
            user_agent: self.user_agent.as_deref(),
            page_url: None,
            region: None,
            hashed_email: None,
            consent: crate::ec::identity::ProviderConsent::default(),
            publisher_domain: "www.example.com",
            now: self.now,
            random: &self.random,
            ec_id: self.ec_id.clone(),
            stored: self.stored.clone(),
            state_json: self.state_json.clone(),
            dry_run: false,
        }
    }

    /// Calls `request` as the coordinator would. With no source left after
    /// the needs check it returns `Ok(None)` without calling the provider.
    pub(crate) fn request(
        &mut self,
    ) -> Result<
        Option<crate::ec::identity::ProviderRequest>,
        error_stack::Report<crate::ec::identity::IdentityProviderError>,
    > {
        let kinds = self.kinds();
        if kinds.is_empty() {
            return Ok(None);
        }
        let result = {
            let signals = self.signals();
            let input =
                crate::ec::identity::ResolveInput::new(self.provider.descriptor(), kinds, &signals);
            let result = self.provider.request(&input);
            assert!(
                input.recorded_errors().is_empty(),
                "provider read undeclared inputs: {:?}",
                input.recorded_errors()
            );
            result
        };
        self.last_request = result.as_ref().ok().cloned().flatten();
        result
    }

    /// Calls `response` with a JSON body and the kept call context.
    pub(crate) fn respond(
        &mut self,
        status: http::StatusCode,
        body: &serde_json::Value,
    ) -> Result<
        crate::ec::identity::Resolution,
        error_stack::Report<crate::ec::identity::IdentityProviderError>,
    > {
        let bytes = serde_json::to_vec(body).expect("should serialize the response body");
        self.respond_raw(status, &[], &bytes)
    }

    /// Calls `response` with raw headers and body.
    pub(crate) fn respond_raw(
        &mut self,
        status: http::StatusCode,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<
        crate::ec::identity::Resolution,
        error_stack::Report<crate::ec::identity::IdentityProviderError>,
    > {
        let mut header_map = http::HeaderMap::new();
        for (name, value) in headers {
            header_map.insert(
                http::HeaderName::from_bytes(name.as_bytes()).expect("should be a header name"),
                http::HeaderValue::from_str(value).expect("should be a header value"),
            );
        }
        let context = self
            .last_request
            .as_ref()
            .and_then(|request| request.call_context_json().map(str::to_owned));
        let response =
            crate::ec::identity::ProviderResponse::new(status, header_map, body.to_vec(), context);
        let signals = self.signals();
        let input = crate::ec::identity::ResolveInput::new(
            self.provider.descriptor(),
            self.kinds(),
            &signals,
        );
        self.provider.response(&response, &input)
    }
}
