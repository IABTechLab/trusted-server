//! What a provider is, may write, needs and must respect.

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use http::Method;
use url::Url;

use super::ids::{IdentityProviderId, SourceDomain};
use super::resolution::IdentityProviderError;
use crate::cookies::{CONSENT_COOKIE_NAMES, TS_IDENTITY_COOKIE_NAMES};
use crate::ec::kv_types::{MAX_EID_FIELD_BYTES, is_valid_eid_field};
use crate::ec::partner::normalize_partner_source_domain;

/// What a provider is, may write, needs and must respect.
///
/// Built from config with no secrets, so `ts config validate` and the running
/// coordinator read the same value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    id: IdentityProviderId,
    sources: Vec<SourceDomain>,
    resolves: Option<Resolves>,
    endpoint: Option<Url>,
    captures: Option<Captures>,
    consent_policy: ConsentPolicy,
    provenance: BTreeMap<SourceDomain, EidProvenance>,
    expiring: BTreeSet<SourceDomain>,
}

impl ProviderDescriptor {
    /// Starts a descriptor for `id`.
    #[must_use]
    pub fn builder(id: IdentityProviderId) -> ProviderDescriptorBuilder {
        ProviderDescriptorBuilder {
            id,
            sources: Vec::new(),
            resolves: None,
            captures: None,
            consent_policy: ConsentPolicy::strict(),
            provenance: BTreeMap::new(),
            expiring_all: false,
            expiring: BTreeSet::new(),
        }
    }

    /// The provider's ID.
    #[must_use]
    pub fn id(&self) -> &IdentityProviderId {
        &self.id
    }

    /// The sources this provider may write, in declaration order.
    #[must_use]
    pub fn sources(&self) -> &[SourceDomain] {
        &self.sources
    }

    /// The first source added. Every built descriptor has one.
    #[must_use]
    pub fn primary_source(&self) -> &SourceDomain {
        &self.sources[0]
    }

    /// Finds a declared source by a vendor's spelling of it, normalized.
    #[must_use]
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain> {
        self.sources.iter().find(|source| source.matches(name))
    }

    /// Active resolution, when declared.
    #[must_use]
    pub fn resolves(&self) -> Option<&Resolves> {
        self.resolves.as_ref()
    }

    /// The endpoint declared with [`Resolves::with_endpoint`], if any.
    #[must_use]
    pub fn endpoint(&self) -> Option<&Url> {
        self.endpoint.as_ref()
    }

    /// Passive capture, when declared.
    #[must_use]
    pub fn captures(&self) -> Option<&Captures> {
        self.captures.as_ref()
    }

    /// The consent this provider requires.
    #[must_use]
    pub fn consent_policy(&self) -> &ConsentPolicy {
        &self.consent_policy
    }

    /// Default EID-level fields for `source`, if declared.
    #[must_use]
    pub fn provenance(&self, source: &SourceDomain) -> Option<&EidProvenance> {
        self.provenance.get(source)
    }

    /// Whether IDs of `source` are declared as expiring.
    #[must_use]
    pub fn is_expiring(&self, source: &SourceDomain) -> bool {
        self.expiring.contains(source)
    }
}

/// Builds a [`ProviderDescriptor`].
#[derive(Debug, Clone)]
pub struct ProviderDescriptorBuilder {
    id: IdentityProviderId,
    sources: Vec<SourceDomain>,
    resolves: Option<Resolves>,
    captures: Option<Captures>,
    consent_policy: ConsentPolicy,
    provenance: BTreeMap<SourceDomain, EidProvenance>,
    expiring_all: bool,
    expiring: BTreeSet<SourceDomain>,
}

impl ProviderDescriptorBuilder {
    /// Adds a source this provider may write. Repeatable. A repeat is ignored.
    #[must_use]
    pub fn with_source(mut self, source: SourceDomain) -> Self {
        if !self.sources.contains(&source) {
            self.sources.push(source);
        }
        self
    }

    /// Declares active resolution.
    #[must_use]
    pub fn with_resolves(mut self, resolves: Resolves) -> Self {
        self.resolves = Some(resolves);
        self
    }

    /// Declares passive capture.
    #[must_use]
    pub fn with_captures(mut self, captures: Captures) -> Self {
        self.captures = Some(captures);
        self
    }

    /// Replaces the default policy, [`ConsentPolicy::strict`].
    #[must_use]
    pub fn with_consent_policy(mut self, policy: ConsentPolicy) -> Self {
        self.consent_policy = policy;
        self
    }

    /// Marks the IDs of every declared source as expiring (spec 6.1).
    #[must_use]
    pub fn with_expiring_ids(mut self) -> Self {
        self.expiring_all = true;
        self
    }

    /// Marks the IDs of one declared source as expiring.
    #[must_use]
    pub fn with_expiring_source(mut self, source: &SourceDomain) -> Self {
        self.expiring.insert(source.clone());
        self
    }

    /// Default EID-level fields for a declared source. At output, the
    /// coordinator applies them to a stored entry of that source, written by
    /// this provider, only when the entry sets none of `inserter`, `matcher`
    /// and `mm`. An entry that sets any of them is the vendor's complete
    /// statement, so vendor and config provenance never mix in one entry.
    /// The defaults are not stored, so a config change applies at once.
    #[must_use]
    pub fn with_provenance(mut self, source: &SourceDomain, provenance: EidProvenance) -> Self {
        self.provenance.insert(source.clone(), provenance);
        self
    }

    /// Validates and builds the descriptor.
    ///
    /// # Errors
    ///
    /// Returns `Config` when the descriptor has no source, no capability, a
    /// resolver with no endpoint or host, an endpoint that is not an `https`
    /// URL, a host that is not a bare hostname, provenance or an expiring
    /// mark for a source it does not declare, a provenance `inserter` or
    /// `matcher` that is blank or over 255 bytes, or a provenance `mm` below 0.
    pub fn build(self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let id = self.id;
        let fail = |what: &str| IdentityProviderError::config(format!("provider `{id}`: {what}"));

        if self.sources.is_empty() {
            return Err(fail("declares no source"));
        }
        if self.resolves.is_none() && self.captures.is_none() {
            return Err(fail("declares neither resolves nor captures"));
        }
        for source in self.provenance.keys().chain(self.expiring.iter()) {
            if !self.sources.contains(source) {
                return Err(fail(&format!("names undeclared source `{source}`")));
            }
        }
        for (source, provenance) in &self.provenance {
            let fields = [
                ("inserter", provenance.inserter()),
                ("matcher", provenance.matcher()),
            ];
            for (name, value) in fields {
                if let Some(value) = value
                    && !is_valid_eid_field(value)
                {
                    return Err(fail(&format!(
                        "provenance `{name}` for `{source}` is blank or over {MAX_EID_FIELD_BYTES} bytes"
                    )));
                }
            }
            if provenance.mm().is_some_and(|mm| mm < 0) {
                return Err(fail(&format!("provenance `mm` for `{source}` is below 0")));
            }
        }

        let mut endpoint = None;
        let mut resolves = self.resolves;
        if let Some(resolves) = resolves.as_mut() {
            if let Some(raw) = resolves.raw_endpoint.as_deref() {
                let url = Url::parse(raw).map_err(|_| fail("endpoint is not a URL"))?;
                if url.scheme() != "https" {
                    return Err(fail("endpoint must use https"));
                }
                let host = url
                    .host_str()
                    .ok_or_else(|| fail("endpoint has no host"))?
                    .to_owned();
                if !resolves.hosts.contains(&host) {
                    resolves.hosts.insert(0, host);
                }
                endpoint = Some(url);
            }
            if resolves.hosts.is_empty() {
                return Err(fail("resolves declares no endpoint or host"));
            }
            if let Some(grace) = resolves.refresh_grace {
                if !resolves.kinds.contains(&ResolveKind::Refresh) {
                    return Err(fail("declares a refresh grace but not the refresh kind"));
                }
                if grace.is_zero() {
                    return Err(fail("declares a zero refresh grace"));
                }
            }
            for host in &resolves.hosts {
                if normalize_partner_source_domain(host).ok().as_deref() != Some(host.as_str()) {
                    return Err(fail(&format!(
                        "host `{host}` is not a bare lowercase hostname"
                    )));
                }
            }
            for name in &resolves.cookies {
                if !is_cookie_name(name) {
                    return Err(fail(&format!("cookie `{name}` is not a valid cookie name")));
                }
                if is_reserved_cookie(name) {
                    return Err(fail(&format!(
                        "cookie `{name}` is a Trusted Server identity or consent cookie"
                    )));
                }
            }
            if let Some(gap) = self.consent_policy.resolver_gap() {
                return Err(fail(gap));
            }
        }

        let expiring = if self.expiring_all {
            self.sources.iter().cloned().collect()
        } else {
            self.expiring
        };

        Ok(ProviderDescriptor {
            id,
            sources: self.sources,
            resolves,
            endpoint,
            captures: self.captures,
            consent_policy: self.consent_policy,
            provenance: self.provenance,
            expiring,
        })
    }
}

/// Whether `name` is an RFC 6265 cookie-name token.
fn is_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&byte))
}

/// Trusted Server's own identity cookies and the consent cookies, which no
/// provider reads directly.
fn is_reserved_cookie(name: &str) -> bool {
    TS_IDENTITY_COOKIE_NAMES
        .iter()
        .chain(CONSENT_COOKIE_NAMES)
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// The kind of resolution one source needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display)]
#[non_exhaustive]
pub enum ResolveKind {
    /// No stored record for the source.
    #[display("initial")]
    Initial,
    /// A stored record exists, valid or expired, and is due.
    #[display("refresh")]
    Refresh,
}

/// Active resolution: the provider calls an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolves {
    raw_endpoint: Option<String>,
    hosts: Vec<String>,
    needs: BTreeSet<Input>,
    needs_for: BTreeMap<ResolveKind, BTreeSet<Input>>,
    wants: BTreeSet<Input>,
    kinds: Vec<ResolveKind>,
    private_state: bool,
    refresh_grace: Option<Duration>,
    ec_id: bool,
    cookies: BTreeSet<String>,
}

impl Default for Resolves {
    fn default() -> Self {
        Self::new()
    }
}

impl Resolves {
    /// A resolver with no inputs, handling `Initial` and `Refresh`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            raw_endpoint: None,
            hosts: Vec::new(),
            needs: BTreeSet::new(),
            needs_for: BTreeMap::new(),
            wants: BTreeSet::new(),
            kinds: vec![ResolveKind::Initial, ResolveKind::Refresh],
            private_state: false,
            refresh_grace: None,
            ec_id: false,
            cookies: BTreeSet::new(),
        }
    }

    /// The endpoint the provider calls, as config spells it. `build()` parses
    /// it and requires `https`. Its host becomes an allowed host.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.raw_endpoint = Some(endpoint.to_owned());
        self
    }

    /// Adds another host the provider may call. Repeatable.
    #[must_use]
    pub fn with_host(mut self, host: &str) -> Self {
        if !self.hosts.iter().any(|known| known == host) {
            self.hosts.push(host.to_owned());
        }
        self
    }

    /// Inputs required for every kind.
    #[must_use]
    pub fn with_needs(mut self, inputs: impl IntoIterator<Item = Input>) -> Self {
        self.needs.extend(inputs);
        self
    }

    /// Inputs required for one kind only.
    #[must_use]
    pub fn with_needs_for(
        mut self,
        kind: ResolveKind,
        inputs: impl IntoIterator<Item = Input>,
    ) -> Self {
        self.needs_for.entry(kind).or_default().extend(inputs);
        self
    }

    /// Inputs passed when available, never required.
    #[must_use]
    pub fn with_wants(mut self, inputs: impl IntoIterator<Item = Input>) -> Self {
        self.wants.extend(inputs);
        self
    }

    /// The kinds this provider handles. Default: `Initial` and `Refresh`.
    #[must_use]
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = ResolveKind>) -> Self {
        self.kinds = kinds.into_iter().collect();
        self
    }

    /// This provider keeps private state in KV (`ps.st`, spec 6.1). It can
    /// then read the stored state and return a state change.
    #[must_use]
    pub fn with_private_state(mut self) -> Self {
        self.private_state = true;
        self
    }

    /// Keeps an expired record this provider wrote for a source it owns as
    /// a refresh anchor until `exp` plus `grace` (spec 5.4, step 4). The
    /// record is never served, but the next eligible request still selects
    /// `Refresh` for it rather than `Initial`. For providers whose refresh
    /// call accepts an expired ID and that keep no private state, so cannot
    /// set `refresh_until`. `build()` requires `Refresh` among the kinds and a
    /// grace above zero.
    #[must_use]
    pub fn with_refresh_grace(mut self, grace: Duration) -> Self {
        self.refresh_grace = Some(grace);
        self
    }

    /// A first-party cookie the provider may read, such as the vendor's own
    /// ID cookie on the publisher's domain. Repeatable. It is passed when the
    /// request carries it and is never required. `build()` refuses Trusted
    /// Server's identity cookies and the consent cookies, which providers
    /// read through [`ResolveInput::consent`](super::input::ResolveInput::consent).
    #[must_use]
    pub fn with_cookie(mut self, name: &str) -> Self {
        self.cookies.insert(name.to_owned());
        self
    }

    /// Gives the provider the full EC ID. Only generic pull may use this.
    #[must_use]
    pub(in crate::ec::identity) fn with_ec_id(mut self) -> Self {
        self.ec_id = true;
        self
    }

    /// Hosts the provider may call.
    #[must_use]
    pub(crate) fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// Inputs required for `kind`: the common needs plus that kind's.
    #[must_use]
    pub(crate) fn needs(&self, kind: ResolveKind) -> BTreeSet<Input> {
        let mut needs = self.needs.clone();
        if let Some(extra) = self.needs_for.get(&kind) {
            needs.extend(extra.iter().copied());
        }
        needs
    }

    /// Every input declared for any kind, needed or wanted.
    #[must_use]
    pub(crate) fn declared_inputs(&self) -> BTreeSet<Input> {
        let mut all = self.needs.clone();
        all.extend(self.wants.iter().copied());
        for extra in self.needs_for.values() {
            all.extend(extra.iter().copied());
        }
        all
    }

    /// Inputs passed when available.
    #[must_use]
    pub(crate) fn wants(&self) -> &BTreeSet<Input> {
        &self.wants
    }

    /// The kinds this provider handles.
    #[must_use]
    pub(crate) fn kinds(&self) -> &[ResolveKind] {
        &self.kinds
    }

    /// How long an expired record this provider wrote stays a refresh
    /// anchor, if it declared a grace.
    #[must_use]
    pub(crate) fn refresh_grace(&self) -> Option<Duration> {
        self.refresh_grace
    }

    /// Whether the provider keeps private state.
    #[must_use]
    pub(crate) fn has_private_state(&self) -> bool {
        self.private_state
    }

    /// Whether the provider receives the full EC ID.
    #[must_use]
    pub(crate) fn gets_ec_id(&self) -> bool {
        self.ec_id
    }

    /// The first-party cookies the provider may read.
    #[must_use]
    pub(crate) fn cookies(&self) -> &BTreeSet<String> {
        &self.cookies
    }
}

/// Revoke-only capture from responses TS proxies for this provider: it
/// can remove IDs, opt out or clear an opt-out, and never stores a positive
/// ID (spec 4.7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Captures {
    paths: Vec<(Method, String)>,
    prefixes: Vec<String>,
}

impl Captures {
    /// No capture rules yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offers proxied responses to exactly this method and path. Repeatable.
    #[must_use]
    pub fn with_path(mut self, method: Method, path: &str) -> Self {
        self.paths.push((method, path.to_owned()));
        self
    }

    /// Offers proxied responses whose path starts with `prefix`, for any
    /// method. Repeatable.
    #[must_use]
    pub fn with_path_prefix(mut self, prefix: &str) -> Self {
        self.prefixes.push(prefix.to_owned());
        self
    }

    /// Whether a proxied request matches a rule.
    #[must_use]
    pub(crate) fn matches(&self, method: &Method, path: &str) -> bool {
        self.paths
            .iter()
            .any(|(rule_method, rule_path)| rule_method == method && rule_path == path)
            || self
                .prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
    }

    /// Exact path rules.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn paths(&self) -> &[(Method, String)] {
        &self.paths
    }

    /// Prefix rules.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn prefixes(&self) -> &[String] {
        &self.prefixes
    }
}

/// Default EID-level fields (`OpenRTB` 2.6 §3.2.27) that bidders read, such
/// as who inserted an ID and how it was matched. They apply only to an entry
/// that sets none of the three. A provider whose vendor returns these per
/// response sets them on each `ResolvedEid` instead (spec 4.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EidProvenance {
    inserter: Option<String>,
    matcher: Option<String>,
    mm: Option<i32>,
}

impl EidProvenance {
    /// No fields set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `inserter`.
    #[must_use]
    pub fn with_inserter(mut self, inserter: &str) -> Self {
        self.inserter = Some(inserter.to_owned());
        self
    }

    /// Sets `matcher`.
    #[must_use]
    pub fn with_matcher(mut self, matcher: &str) -> Self {
        self.matcher = Some(matcher.to_owned());
        self
    }

    /// Sets `mm`.
    #[must_use]
    pub fn with_mm(mut self, mm: i32) -> Self {
        self.mm = Some(mm);
        self
    }

    /// `inserter`, if set.
    #[must_use]
    pub fn inserter(&self) -> Option<&str> {
        self.inserter.as_deref()
    }

    /// `matcher`, if set.
    #[must_use]
    pub fn matcher(&self) -> Option<&str> {
        self.matcher.as_deref()
    }

    /// `mm`, if set.
    #[must_use]
    pub fn mm(&self) -> Option<i32> {
        self.mm
    }
}

/// A US opt-out a policy can relax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display)]
#[non_exhaustive]
pub enum UsOptOut {
    /// Sale of personal information.
    #[display("sale")]
    Sale,
    /// Sharing for cross-context behavioural advertising.
    #[display("sharing")]
    Sharing,
    /// Targeted advertising.
    #[display("targeted_advertising")]
    TargetedAdvertising,
}

/// The consent a provider requires. Evaluated by the coordinator (Task 12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentPolicy {
    tcf_purposes: BTreeSet<u8>,
    tcf_vendor: Option<u16>,
    allow_dnt: bool,
    allow_gpc: bool,
    allowed_us_opt_outs: BTreeSet<UsOptOut>,
    eid_persistence_only: bool,
    purposes_declared: bool,
    without_vendor: bool,
}

impl ConsentPolicy {
    /// The default. Blocked when any consent signal present cannot be
    /// decoded. Where TCF applies, requires Purpose 1 and whatever the
    /// provider declares with [`with_tcf_purposes`](Self::with_tcf_purposes)
    /// and [`with_tcf_vendor`](Self::with_tcf_vendor). Blocked by every US
    /// opt-out TS decodes, by `Sec-GPC: 1` and by `DNT: 1`.
    ///
    /// A resolver's policy must declare its purposes and its vendor, or say
    /// it has none with [`without_tcf_vendor`](Self::without_tcf_vendor);
    /// [`ProviderDescriptorBuilder::build`] rejects it otherwise.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            tcf_purposes: [1].into_iter().collect(),
            tcf_vendor: None,
            allow_dnt: false,
            allow_gpc: false,
            allowed_us_opt_outs: BTreeSet::new(),
            eid_persistence_only: false,
            purposes_declared: false,
            without_vendor: false,
        }
    }

    /// Declares the TCF purposes the provider processes under consent, as
    /// its GVL entry lists them. Each is required where TCF applies, beside
    /// Purpose 1, which is always required. An empty list declares Purpose 1
    /// alone.
    #[must_use]
    pub fn with_tcf_purposes(mut self, purposes: impl IntoIterator<Item = u8>) -> Self {
        self.tcf_purposes.extend(purposes);
        self.purposes_declared = true;
        self
    }

    /// Declares that the provider has no IAB Global Vendor List ID, so no
    /// vendor consent is checked: purpose consent alone lets it run where
    /// TCF applies. Needs a documented legal basis (TCF Policies, Chapter
    /// III, Policy 14(3)).
    #[must_use]
    pub fn without_tcf_vendor(mut self) -> Self {
        self.without_vendor = true;
        self
    }

    /// Requires vendor consent for this IAB Global Vendor List ID.
    #[must_use]
    pub fn with_tcf_vendor(mut self, vendor_id: u16) -> Self {
        self.tcf_vendor = Some(vendor_id);
        self
    }

    /// Does not block on `DNT: 1`. Needs a documented legal basis.
    #[must_use]
    pub fn allowing_dnt(mut self) -> Self {
        self.allow_dnt = true;
        self
    }

    /// Does not block on GPC. Needs a documented legal basis.
    #[must_use]
    pub fn allowing_gpc(mut self) -> Self {
        self.allow_gpc = true;
        self
    }

    /// Does not block on one US opt-out. Needs a documented legal basis.
    #[must_use]
    pub fn allowing_us_opt_out(mut self, opt_out: UsOptOut) -> Self {
        self.allowed_us_opt_outs.insert(opt_out);
        self
    }

    /// Blocks exactly what pull sync blocks today: only the global EC and
    /// EID-persistence gates. For generic pull's parity (spec 10.4).
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    #[must_use]
    pub(in crate::ec::identity) fn eid_persistence_only() -> Self {
        Self {
            eid_persistence_only: true,
            purposes_declared: true,
            without_vendor: true,
            ..Self::strict()
        }
    }

    /// Required TCF purposes.
    #[must_use]
    pub fn tcf_purposes(&self) -> &BTreeSet<u8> {
        &self.tcf_purposes
    }

    /// Required GVL vendor, if any.
    #[must_use]
    pub fn tcf_vendor(&self) -> Option<u16> {
        self.tcf_vendor
    }

    /// Whether `DNT: 1` is allowed.
    #[must_use]
    pub fn allows_dnt(&self) -> bool {
        self.allow_dnt
    }

    /// Whether GPC is allowed.
    #[must_use]
    pub fn allows_gpc(&self) -> bool {
        self.allow_gpc
    }

    /// Whether a US opt-out is allowed.
    #[must_use]
    pub fn allows_us_opt_out(&self, opt_out: UsOptOut) -> bool {
        self.allowed_us_opt_outs.contains(&opt_out)
    }

    /// Whether this is generic pull's parity policy.
    #[must_use]
    pub(crate) fn is_eid_persistence_only(&self) -> bool {
        self.eid_persistence_only
    }

    /// Why a resolver may not use this policy, if it may not.
    fn resolver_gap(&self) -> Option<&'static str> {
        if !self.purposes_declared {
            Some("consent policy declares no TCF purposes; call with_tcf_purposes")
        } else if self.tcf_vendor.is_none() && !self.without_vendor {
            Some(
                "consent policy names no TCF vendor; call with_tcf_vendor, or without_tcf_vendor with a documented legal basis",
            )
        } else {
            None
        }
    }
}

/// An input a provider can ask for.
///
/// Consent strings, the provider's stored IDs, the publisher domain, the
/// time and the declared endpoint are always available. Private state is
/// available to providers declared `with_private_state()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display)]
#[non_exhaustive]
pub enum Input {
    /// The platform's client address.
    #[display("client_ip")]
    ClientIp,
    /// The request's `User-Agent`.
    #[display("user_agent")]
    UserAgent,
    /// The top-level page URL without query string or fragment.
    #[display("page_url")]
    PageUrl,
    /// Country and subdivision only.
    #[display("region")]
    Region,
    /// A SHA-256 of a normalized email address, from the `ts-hem` cookie the
    /// JS SDK writes. Never stored.
    #[display("hashed_email")]
    HashedEmail,
}

/// Something a provider read without declaring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display)]
#[non_exhaustive]
pub enum Access {
    /// An undeclared [`Input`].
    #[display("{_0}")]
    Input(Input),
    /// The endpoint, when the descriptor declares no `resolves`.
    #[display("endpoint")]
    Endpoint,
    /// Private state, without `with_private_state()`.
    #[display("private_state")]
    PrivateState,
    /// The full EC ID, without `with_ec_id()`. Only generic pull declares it.
    #[display("ec_id")]
    EcId,
    /// A first-party cookie, without `with_cookie()` for its name.
    #[display("cookie")]
    Cookie,
}

#[cfg(test)]
mod tests {
    use http::Method;

    use super::*;
    use crate::ec::kv_types::MAX_EID_FIELD_BYTES;

    fn source(name: &str) -> SourceDomain {
        name.parse().expect("should parse source")
    }

    fn id(name: &str) -> IdentityProviderId {
        name.parse().expect("should parse id")
    }

    #[test]
    fn builds_the_documented_example() {
        let descriptor = ProviderDescriptor::builder(id("exampleid"))
            .with_source(source("exampleid.example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://api.exampleid.example.com/v1/fetch")
                    .with_needs([Input::ClientIp, Input::UserAgent])
                    .with_wants([Input::PageUrl]),
            )
            .with_consent_policy(
                ConsentPolicy::strict()
                    .with_tcf_purposes([4])
                    .with_tcf_vendor(9999),
            )
            .build()
            .expect("should build");

        assert_eq!(descriptor.id().as_str(), "exampleid", "should keep the id");
        assert_eq!(
            descriptor.primary_source(),
            &source("exampleid.example.com"),
            "should keep the source"
        );
        assert_eq!(
            descriptor.endpoint().map(Url::as_str),
            Some("https://api.exampleid.example.com/v1/fetch"),
            "should parse the endpoint"
        );
        let resolves = descriptor.resolves().expect("should resolve");
        assert_eq!(
            resolves.hosts(),
            &["api.exampleid.example.com".to_owned()],
            "should allow the endpoint host"
        );
        assert_eq!(
            resolves.kinds(),
            &[ResolveKind::Initial, ResolveKind::Refresh],
            "should default to Initial and Refresh"
        );
        assert!(descriptor.captures().is_none(), "should not capture");
        assert!(
            !descriptor.is_expiring(&source("exampleid.example.com")),
            "should not expire by default"
        );
        assert_eq!(
            descriptor.consent_policy().tcf_vendor(),
            Some(9999),
            "should keep the vendor"
        );
        assert_eq!(
            descriptor
                .consent_policy()
                .tcf_purposes()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 4],
            "should require Purpose 1 and the declared Purpose 4"
        );
    }

    #[test]
    fn strict_requires_only_purpose_one_until_purposes_are_declared() {
        let policy = ConsentPolicy::strict();

        assert_eq!(
            policy.tcf_purposes().iter().copied().collect::<Vec<_>>(),
            vec![1],
            "should require Purpose 1 alone"
        );
        assert_eq!(
            ConsentPolicy::strict()
                .with_tcf_purposes([3, 5])
                .tcf_purposes()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 3, 5],
            "should add the declared purposes to Purpose 1"
        );
    }

    #[test]
    fn a_resolver_must_declare_its_purposes_and_vendor() {
        let resolver = |policy: ConsentPolicy| {
            ProviderDescriptor::builder(id("p"))
                .with_source(source("example.com"))
                .with_resolves(Resolves::new().with_endpoint("https://a.example.com"))
                .with_consent_policy(policy)
                .build()
        };

        let undeclared = resolver(ConsentPolicy::strict().with_tcf_vendor(5))
            .expect_err("should reject a resolver that declares no purposes");
        let no_vendor = resolver(ConsentPolicy::strict().with_tcf_purposes([4]))
            .expect_err("should reject a resolver that names no vendor");
        let relaxed = resolver(
            ConsentPolicy::strict()
                .with_tcf_purposes([])
                .without_tcf_vendor(),
        )
        .expect("should build a resolver that opts out of vendor consent");
        let capture = ProviderDescriptor::builder(id("p"))
            .with_source(source("example.com"))
            .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
            .build();

        assert!(
            format!("{undeclared:?}").contains("with_tcf_purposes"),
            "should name the missing purposes"
        );
        assert!(
            format!("{no_vendor:?}").contains("without_tcf_vendor"),
            "should name the vendor relaxation"
        );
        assert_eq!(
            relaxed.consent_policy().tcf_vendor(),
            None,
            "should check no vendor"
        );
        assert!(
            capture.is_ok(),
            "should not require a policy of a capture-only provider"
        );
    }

    #[test]
    fn keeps_declared_cookies() {
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(source("example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://a.example.com")
                    .with_cookie("_vendor_fpid")
                    .with_cookie("_vendor_fpid"),
            )
            .with_consent_policy(crate::ec::identity::test_support::resolver_policy())
            .build()
            .expect("should build");

        let cookies: Vec<&str> = descriptor
            .resolves()
            .expect("should resolve")
            .cookies()
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(cookies, ["_vendor_fpid"], "should keep each name once");
    }

    #[test]
    fn build_rejects_invalid_descriptors() {
        let cases: Vec<(&str, ProviderDescriptorBuilder)> = vec![
            (
                "no source",
                ProviderDescriptor::builder(id("p"))
                    .with_resolves(Resolves::new().with_endpoint("https://a.example.com")),
            ),
            (
                "no capability",
                ProviderDescriptor::builder(id("p")).with_source(source("example.com")),
            ),
            (
                "no endpoint or host",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(Resolves::new()),
            ),
            (
                "http endpoint",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(Resolves::new().with_endpoint("http://a.example.com")),
            ),
            (
                "bad host",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(Resolves::new().with_host("a.example.com/path")),
            ),
            (
                "provenance for undeclared source",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(
                        &source("other.example.com"),
                        EidProvenance::new().with_mm(2),
                    ),
            ),
            (
                "refresh grace without refresh",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_kinds([ResolveKind::Initial])
                            .with_refresh_grace(core::time::Duration::from_secs(60)),
                    ),
            ),
            (
                "zero refresh grace",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_refresh_grace(core::time::Duration::ZERO),
                    ),
            ),
            (
                "expiring undeclared source",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_expiring_source(&source("other.example.com")),
            ),
            (
                "blank provenance inserter",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(
                        &source("example.com"),
                        EidProvenance::new().with_inserter("  "),
                    ),
            ),
            (
                "blank provenance matcher",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(
                        &source("example.com"),
                        EidProvenance::new().with_matcher(""),
                    ),
            ),
            (
                "oversized provenance matcher",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(
                        &source("example.com"),
                        EidProvenance::new().with_matcher(&"m".repeat(MAX_EID_FIELD_BYTES + 1)),
                    ),
            ),
            (
                "negative provenance mm",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
                    .with_provenance(&source("example.com"), EidProvenance::new().with_mm(-1)),
            ),
            (
                "cookie name with a separator",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie("vendor;id"),
                    ),
            ),
            (
                "empty cookie name",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie(""),
                    ),
            ),
            (
                "Trusted Server identity cookie",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie("TS-HEM"),
                    ),
            ),
            (
                "consent cookie",
                ProviderDescriptor::builder(id("p"))
                    .with_source(source("example.com"))
                    .with_resolves(
                        Resolves::new()
                            .with_endpoint("https://a.example.com")
                            .with_cookie("euconsent-v2"),
                    ),
            ),
        ];
        for (name, builder) in cases {
            let error = builder
                .build()
                .expect_err(&format!("should reject a descriptor with {name}"));
            assert!(
                matches!(
                    error.current_context(),
                    IdentityProviderError::Config { .. }
                ),
                "should be a Config error: {name}"
            );
        }
    }

    #[test]
    fn expiring_and_provenance_are_per_source() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(a.clone())
            .with_source(b.clone())
            .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
            .with_expiring_source(&b)
            .with_provenance(
                &a,
                EidProvenance::new()
                    .with_inserter("example-inserter")
                    .with_mm(2),
            )
            .build()
            .expect("should build");

        assert!(!descriptor.is_expiring(&a), "should not mark a as expiring");
        assert!(descriptor.is_expiring(&b), "should mark b as expiring");
        let provenance = descriptor
            .provenance(&a)
            .expect("should have provenance for a");
        assert_eq!(
            provenance.inserter(),
            Some("example-inserter"),
            "should keep the inserter"
        );
        assert_eq!(provenance.matcher(), None, "should leave the matcher unset");
        assert_eq!(provenance.mm(), Some(2), "should keep mm");
        assert!(
            descriptor.provenance(&b).is_none(),
            "should have no provenance for b"
        );
        assert_eq!(
            descriptor.source_named("A.EXAMPLE.COM"),
            Some(&a),
            "should find a by any spelling"
        );
    }

    #[test]
    fn expiring_ids_marks_every_source() {
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(source("a.example.com"))
            .with_source(source("b.example.com"))
            .with_expiring_ids()
            .with_captures(Captures::new().with_path_prefix("/integrations/p/"))
            .build()
            .expect("should build");
        assert!(
            descriptor
                .sources()
                .iter()
                .all(|s| descriptor.is_expiring(s)),
            "should mark every source"
        );
    }

    #[test]
    fn captures_match_exact_paths_and_prefixes() {
        let captures = Captures::new()
            .with_path(Method::POST, "/integrations/p/api/token")
            .with_path_prefix("/integrations/p/stream/");
        assert!(
            captures.matches(&Method::POST, "/integrations/p/api/token"),
            "should match the exact path"
        );
        assert!(
            !captures.matches(&Method::GET, "/integrations/p/api/token"),
            "should require the method"
        );
        assert!(
            !captures.matches(&Method::POST, "/integrations/p/api/token/x"),
            "should not treat an exact path as a prefix"
        );
        assert!(
            captures.matches(&Method::GET, "/integrations/p/stream/1"),
            "should match a prefix for any method"
        );
        assert!(
            !captures.matches(&Method::GET, "/integrations/p/settings"),
            "should not match others"
        );
    }

    #[test]
    fn needs_differ_by_kind() {
        let resolves = Resolves::new()
            .with_endpoint("https://a.example.com")
            .with_needs([Input::ClientIp])
            .with_needs_for(ResolveKind::Initial, [Input::HashedEmail]);
        assert_eq!(
            resolves.needs(ResolveKind::Initial),
            [Input::ClientIp, Input::HashedEmail].into_iter().collect(),
            "should need both inputs for Initial"
        );
        assert_eq!(
            resolves.needs(ResolveKind::Refresh),
            [Input::ClientIp].into_iter().collect(),
            "should need only the IP for Refresh"
        );
    }

    #[test]
    fn valid_provenance_builds() {
        let descriptor = ProviderDescriptor::builder(id("p"))
            .with_source(source("example.com"))
            .with_captures(Captures::new().with_path(Method::POST, "/integrations/p/api"))
            .with_provenance(
                &source("example.com"),
                EidProvenance::new()
                    .with_inserter("example-inserter")
                    .with_matcher("example-matcher")
                    .with_mm(0),
            )
            .build()
            .expect("should accept non-blank provenance and mm 0");
        assert!(
            descriptor.provenance(&source("example.com")).is_some(),
            "should keep the provenance"
        );
    }
}
