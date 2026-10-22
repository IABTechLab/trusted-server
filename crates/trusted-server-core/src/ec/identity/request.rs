//! Request-scoped identity state, carried on `EcContext` to finalize and to
//! the after-send flush.

use http::HeaderMap;
use url::Url;

use super::catalog::ProviderCatalog;
use super::cookie::{
    CookieRead, OpenedRemovals, RBatch, SCookie, SOpen, open, read_prefixed_cookies,
    read_single_cookie,
};
use super::mutation::IdentityMutation;
use super::revision::IdentityKeys;
use crate::constants::{COOKIE_TS_EIDS_R_PREFIX, COOKIE_TS_EIDS_S};
use crate::ec::kv::PartnerIdUpdate;

/// The identity cookies' raw values, captured when the request is read and
/// opened only by the routes that use identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawIdentityCookies {
    s: Option<CookieRead>,
    r: Vec<(String, String)>,
}

impl RawIdentityCookies {
    /// Captures both cookie kinds from request headers.
    #[must_use]
    pub(crate) fn capture(headers: &HeaderMap) -> Self {
        let s = match read_single_cookie(headers, COOKIE_TS_EIDS_S) {
            CookieRead::Absent => None,
            read => Some(read),
        };
        Self {
            s,
            r: read_prefixed_cookies(headers, COOKIE_TS_EIDS_R_PREFIX),
        }
    }

    /// Whether the request carried either cookie.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_any(&self) -> bool {
        self.s.is_some() || !self.r.is_empty()
    }

    /// Opens both cookie kinds for `ec_id`.
    #[must_use]
    pub(crate) fn open(
        &self,
        keys: &IdentityKeys,
        catalog: &ProviderCatalog,
        ec_id: &str,
    ) -> OpenedIdentity {
        let s = match &self.s {
            None => SOpen::Missing,
            Some(CookieRead::Value(value)) => {
                let mut headers = HeaderMap::new();
                match http::HeaderValue::from_str(&format!("{COOKIE_TS_EIDS_S}={value}")) {
                    Ok(header) => {
                        headers.insert(http::header::COOKIE, header);
                        super::cookie::open_s_cookie(keys, ec_id, catalog, &headers)
                    }
                    Err(_) => SOpen::Failed,
                }
            }
            Some(_) => SOpen::Failed,
        };
        let mut removals = OpenedRemovals::default();
        for (name, value) in &self.r {
            match open(&keys.cookie_r, ec_id, value)
                .and_then(|payload| serde_json::from_slice::<RBatch>(&payload).ok())
            {
                Some(batch) => removals.cookies.push((name.clone(), batch)),
                None => removals.unopenable.push(name.clone()),
            }
        }
        OpenedIdentity { s, removals }
    }

    /// Whether the request carried `__Host-ts-eids-s`.
    #[must_use]
    pub(crate) fn has_s(&self) -> bool {
        self.s.is_some()
    }

    /// The names of every removal cookie the request carried.
    #[must_use]
    pub(crate) fn r_names(&self) -> Vec<String> {
        self.r.iter().map(|(name, _)| name.clone()).collect()
    }
}

/// The identity cookies after opening them for this EC.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenedIdentity {
    pub(crate) s: SOpen,
    pub(crate) removals: OpenedRemovals,
}

/// What this response must do with the identity cookies.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StagedCookies {
    /// A `__Host-ts-eids-s` to set.
    pub(crate) s_cookie: Option<SCookie>,
    /// Expire `__Host-ts-eids-s`.
    pub(crate) expire_s: bool,
    /// This response's removal batch.
    pub(crate) new_r_batch: RBatch,
    /// Removal cookies to delete, by name.
    pub(crate) delete_r: Vec<String>,
}

/// Request-scoped identity state.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RequestIdentity {
    writer: bool,
    user_agent: Option<String>,
    dnt: bool,
    page_url: Option<Url>,
    raw: RawIdentityCookies,
    opened: Option<OpenedIdentity>,
    mutation: IdentityMutation,
    staged: StagedCookies,
    seed: Vec<PartnerIdUpdate>,
    client_eids_from_body: bool,
    marked: Vec<super::ids::IdentityProviderId>,
}

impl RequestIdentity {
    /// Captures the signals phase 1 needs, because after-send code no longer
    /// has the request.
    #[must_use]
    pub(crate) fn capture(headers: &HeaderMap) -> Self {
        let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
        Self {
            user_agent: header("user-agent").map(str::to_owned),
            dnt: header("dnt").is_some_and(|value| value.trim() == "1"),
            raw: RawIdentityCookies::capture(headers),
            ..Self::default()
        }
    }

    /// Replaces the captured cookies, for tests that build a context by hand.
    #[cfg(test)]
    pub(crate) fn set_raw_cookies_for_test(&mut self, headers: &HeaderMap) {
        self.raw = RawIdentityCookies::capture(headers);
        self.opened = None;
    }

    /// Whether this request is the page load's writer.
    #[must_use]
    pub(crate) fn writer(&self) -> bool {
        self.writer
    }

    pub(crate) fn set_writer(&mut self, writer: bool) {
        self.writer = writer;
    }

    /// The request's `User-Agent`.
    #[must_use]
    pub(crate) fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }

    /// Whether `DNT: 1` was sent.
    #[must_use]
    pub(crate) fn dnt(&self) -> bool {
        self.dnt
    }

    /// The page URL, without query or fragment.
    #[must_use]
    pub(crate) fn page_url(&self) -> Option<&Url> {
        self.page_url.as_ref()
    }

    /// Records the page URL, dropping its query and fragment.
    pub(crate) fn set_page_url(&mut self, mut url: Url) {
        url.set_query(None);
        url.set_fragment(None);
        self.page_url = Some(url);
    }

    /// The raw identity cookies.
    #[must_use]
    pub(crate) fn raw_cookies(&self) -> &RawIdentityCookies {
        &self.raw
    }

    /// Opens the identity cookies for `ec_id`, once. Later calls do nothing.
    pub(crate) fn open_cookies(
        &mut self,
        keys: &IdentityKeys,
        catalog: &ProviderCatalog,
        ec_id: &str,
    ) {
        if self.opened.is_none() {
            self.opened = Some(self.raw.open(keys, catalog, ec_id));
        }
    }

    /// The opened cookies, or the raw ones opened now without keeping them.
    #[must_use]
    pub(crate) fn opened_or_open(
        &self,
        keys: &IdentityKeys,
        catalog: &ProviderCatalog,
        ec_id: &str,
    ) -> OpenedIdentity {
        self.opened
            .clone()
            .unwrap_or_else(|| self.raw.open(keys, catalog, ec_id))
    }

    /// The opened cookies, once [`Self::open_cookies`] ran.
    #[must_use]
    pub(crate) fn opened(&self) -> Option<&OpenedIdentity> {
        self.opened.as_ref()
    }

    /// The writer's single mutation.
    #[must_use]
    pub(crate) fn mutation(&self) -> &IdentityMutation {
        &self.mutation
    }

    /// The writer's single mutation, to add changes.
    pub(crate) fn mutation_mut(&mut self) -> &mut IdentityMutation {
        &mut self.mutation
    }

    /// What the response must do with the identity cookies.
    #[must_use]
    pub(crate) fn staged(&self) -> &StagedCookies {
        &self.staged
    }

    /// Whether `/auction`'s client set came from its JSON body rather than
    /// the `ts-eids` cookie. Only a body set is saved as a KV gap.
    #[must_use]
    pub(crate) fn client_eids_from_body(&self) -> bool {
        self.client_eids_from_body
    }

    pub(crate) fn set_client_eids_from_body(&mut self, from_body: bool) {
        self.client_eids_from_body = from_body;
    }

    /// After-send providers this request marked pending in phase 3.
    #[must_use]
    pub(crate) fn marked(&self) -> &[super::ids::IdentityProviderId] {
        &self.marked
    }

    pub(crate) fn mark(&mut self, provider: super::ids::IdentityProviderId) {
        if !self.marked.contains(&provider) {
            self.marked.push(provider);
        }
    }

    /// Browser ingest a new visitor's create carries.
    #[must_use]
    pub(crate) fn seed(&self) -> &[PartnerIdUpdate] {
        &self.seed
    }

    pub(crate) fn set_seed(&mut self, seed: Vec<PartnerIdUpdate>) {
        self.seed = seed;
    }

    /// What the response must do with the identity cookies, to change it.
    pub(crate) fn staged_mut(&mut self) -> &mut StagedCookies {
        &mut self.staged
    }
}
