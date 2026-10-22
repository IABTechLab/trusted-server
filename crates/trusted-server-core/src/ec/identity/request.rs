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
    #[must_use]
    pub(crate) fn has_any(&self) -> bool {
        self.s.is_some() || !self.r.is_empty()
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
        if self.opened.is_some() {
            return;
        }
        let s = match &self.raw.s {
            None => SOpen::Missing,
            Some(raw) => {
                let mut headers = HeaderMap::new();
                if let CookieRead::Value(value) = raw
                    && let Ok(header) =
                        http::HeaderValue::from_str(&format!("{COOKIE_TS_EIDS_S}={value}"))
                {
                    headers.insert(http::header::COOKIE, header);
                    super::cookie::open_s_cookie(keys, ec_id, catalog, &headers)
                } else {
                    SOpen::Failed
                }
            }
        };
        let mut removals = OpenedRemovals::default();
        for (name, value) in &self.raw.r {
            match open(&keys.cookie_r, ec_id, value)
                .and_then(|payload| serde_json::from_slice::<RBatch>(&payload).ok())
            {
                Some(batch) => removals.cookies.push((name.clone(), batch)),
                None => removals.unopenable.push(name.clone()),
            }
        }
        self.opened = Some(OpenedIdentity { s, removals });
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

    /// What the response must do with the identity cookies, to change it.
    pub(crate) fn staged_mut(&mut self) -> &mut StagedCookies {
        &mut self.staged
    }
}
