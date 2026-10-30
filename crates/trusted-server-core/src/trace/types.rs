//! Closed public request and cookie projection types.

use serde::Serialize;

/// Shape of one incoming reserved cookie without its value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CookieHealthState {
    /// No exact occurrence is visible.
    Absent,
    /// One complete value passes its canonical validator.
    PresentValid,
    /// One occurrence is malformed, oversized or unsupported.
    PresentInvalid,
    /// More than one exact occurrence is visible.
    Duplicate,
    /// Aggregate inspection cannot be trusted.
    Unavailable,
}

/// Bounded explanation of cookie shape without values or parser messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CookieHealthDetail {
    /// The complete EC value passes the canonical validator.
    ValidEcFormat,
    /// The complete EID value passes the bounded existing parser.
    ValidEidsFormat,
    /// The tester value is exactly `true`.
    ValidTesterValue,
    /// The diagnostics value is exactly `1`.
    ValidDiagnosticsValue,
    /// The occurrence does not match the expected syntax.
    Malformed,
    /// The visible value exceeds its individual bound.
    Oversized,
    /// The complete literal value is unsupported.
    UnsupportedValue,
    /// More than one exact reserved occurrence is visible.
    MultipleValues,
    /// Visible Cookie fields exceed the aggregate byte bound.
    HeaderTooLarge,
    /// A visible Cookie field contains actual invalid UTF-8 bytes.
    HeaderNotUtf8,
    /// Markers make unproved runtime preservation ambiguous.
    RuntimeHeaderAmbiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CookieSource {
    Request,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(super) enum ValidCookieDetail {
    #[serde(rename = "valid_ec_format")]
    EcFormat,
    #[serde(rename = "valid_eids_format")]
    EidsFormat,
    #[serde(rename = "valid_tester_value")]
    TesterValue,
    #[serde(rename = "valid_diagnostics_value")]
    DiagnosticsValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum InvalidCookieDetail {
    Malformed,
    Oversized,
    UnsupportedValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum UnavailableCookieDetail {
    HeaderTooLarge,
    HeaderNotUtf8,
    RuntimeHeaderAmbiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DuplicateCookieDetail {
    MultipleValues,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum CookieOutcome {
    Absent,
    PresentValid { detail: ValidCookieDetail },
    PresentInvalid { detail: InvalidCookieDetail },
    Duplicate { detail: DuplicateCookieDetail },
    Unavailable { detail: UnavailableCookieDetail },
}

/// Immutable health with only legal state and detail combinations.
///
/// Construction stays inside the scanner, which also binds valid details to
/// their reserved cookie names. Serialization always uses `source: request`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CookieHealth {
    source: CookieSource,
    #[serde(flatten)]
    outcome: CookieOutcome,
}

impl CookieHealth {
    /// Return the observed shape of this cookie.
    ///
    /// # Examples
    ///
    /// ```
    /// use http::HeaderMap;
    /// use trusted_server_core::trace::{CookieHealthState, inspect_cookies};
    /// let cookies = inspect_cookies(&HeaderMap::new(), None);
    /// assert_eq!(cookies.ts_ec().state(), CookieHealthState::Absent);
    /// ```
    #[must_use]
    pub fn state(&self) -> CookieHealthState {
        match self.outcome {
            CookieOutcome::Absent => CookieHealthState::Absent,
            CookieOutcome::PresentValid { .. } => CookieHealthState::PresentValid,
            CookieOutcome::PresentInvalid { .. } => CookieHealthState::PresentInvalid,
            CookieOutcome::Duplicate { .. } => CookieHealthState::Duplicate,
            CookieOutcome::Unavailable { .. } => CookieHealthState::Unavailable,
        }
    }

    /// Return the bounded explanation, absent when no occurrence is visible.
    ///
    /// # Examples
    ///
    /// ```
    /// let cookies = trusted_server_core::trace::inspect_cookies(&http::HeaderMap::new(), None);
    /// assert_eq!(cookies.ts_ec().detail(), None);
    /// ```
    #[must_use]
    pub fn detail(&self) -> Option<CookieHealthDetail> {
        Some(match self.outcome {
            CookieOutcome::Absent => return None,
            CookieOutcome::PresentValid { detail } => match detail {
                ValidCookieDetail::EcFormat => CookieHealthDetail::ValidEcFormat,
                ValidCookieDetail::EidsFormat => CookieHealthDetail::ValidEidsFormat,
                ValidCookieDetail::TesterValue => CookieHealthDetail::ValidTesterValue,
                ValidCookieDetail::DiagnosticsValue => CookieHealthDetail::ValidDiagnosticsValue,
            },
            CookieOutcome::PresentInvalid { detail } => match detail {
                InvalidCookieDetail::Malformed => CookieHealthDetail::Malformed,
                InvalidCookieDetail::Oversized => CookieHealthDetail::Oversized,
                InvalidCookieDetail::UnsupportedValue => CookieHealthDetail::UnsupportedValue,
            },
            CookieOutcome::Duplicate { .. } => CookieHealthDetail::MultipleValues,
            CookieOutcome::Unavailable { detail } => match detail {
                UnavailableCookieDetail::HeaderTooLarge => CookieHealthDetail::HeaderTooLarge,
                UnavailableCookieDetail::HeaderNotUtf8 => CookieHealthDetail::HeaderNotUtf8,
                UnavailableCookieDetail::RuntimeHeaderAmbiguous => {
                    CookieHealthDetail::RuntimeHeaderAmbiguous
                }
            },
        })
    }

    pub(super) const fn absent() -> Self {
        Self::new(CookieOutcome::Absent)
    }

    pub(super) const fn valid(detail: ValidCookieDetail) -> Self {
        Self::new(CookieOutcome::PresentValid { detail })
    }

    pub(super) const fn invalid(detail: InvalidCookieDetail) -> Self {
        Self::new(CookieOutcome::PresentInvalid { detail })
    }

    pub(super) const fn duplicate() -> Self {
        Self::new(CookieOutcome::Duplicate {
            detail: DuplicateCookieDetail::MultipleValues,
        })
    }

    pub(super) const fn unavailable(detail: UnavailableCookieDetail) -> Self {
        Self::new(CookieOutcome::Unavailable { detail })
    }

    const fn new(outcome: CookieOutcome) -> Self {
        Self {
            source: CookieSource::Request,
            outcome,
        }
    }
}

/// Frozen redacted observations of the four reserved cookies.
///
/// This owns no incoming headers, values, fidelity metadata or identity state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TraceCookies {
    ts_ec: CookieHealth,
    ts_eids: CookieHealth,
    ts_tester: CookieHealth,
    diagnostics_session: CookieHealth,
}

impl TraceCookies {
    /// Report whether exactly one valid diagnostics session was inspected.
    ///
    /// A false result means no valid session was observed, not cookie absence.
    ///
    /// # Examples
    ///
    /// ```
    /// let cookies = trusted_server_core::trace::inspect_cookies(&http::HeaderMap::new(), None);
    /// assert!(!cookies.observed_active());
    /// ```
    #[must_use]
    pub const fn observed_active(&self) -> bool {
        matches!(
            self.diagnostics_session.outcome,
            CookieOutcome::PresentValid {
                detail: ValidCookieDetail::DiagnosticsValue
            }
        )
    }

    pub(super) const fn new(health: [CookieHealth; 4]) -> Self {
        Self {
            ts_ec: health[0],
            ts_eids: health[1],
            ts_tester: health[2],
            diagnostics_session: health[3],
        }
    }

    /// Borrow the frozen EC shape.
    ///
    /// # Examples
    ///
    /// ```
    /// let cookies = trusted_server_core::trace::inspect_cookies(&http::HeaderMap::new(), None);
    /// assert_eq!(cookies.ts_ec().detail(), None);
    /// ```
    #[must_use]
    pub const fn ts_ec(&self) -> &CookieHealth {
        &self.ts_ec
    }

    /// Borrow the frozen EID shape.
    ///
    /// # Examples
    ///
    /// ```
    /// let cookies = trusted_server_core::trace::inspect_cookies(&http::HeaderMap::new(), None);
    /// assert_eq!(cookies.ts_eids().detail(), None);
    /// ```
    #[must_use]
    pub const fn ts_eids(&self) -> &CookieHealth {
        &self.ts_eids
    }

    /// Borrow the frozen tester shape.
    ///
    /// # Examples
    ///
    /// ```
    /// let cookies = trusted_server_core::trace::inspect_cookies(&http::HeaderMap::new(), None);
    /// assert_eq!(cookies.ts_tester().detail(), None);
    /// ```
    #[must_use]
    pub const fn ts_tester(&self) -> &CookieHealth {
        &self.ts_tester
    }

    /// Borrow the frozen diagnostics-session shape.
    ///
    /// # Examples
    ///
    /// ```
    /// let cookies = trusted_server_core::trace::inspect_cookies(&http::HeaderMap::new(), None);
    /// assert_eq!(cookies.diagnostics_session().detail(), None);
    /// ```
    #[must_use]
    pub const fn diagnostics_session(&self) -> &CookieHealth {
        &self.diagnostics_session
    }
}

/// Optional allowlisted network facts with bounded strings and a masked IP.
///
/// Unsupported HTTP-version and POP enrichment remains absent in this phase.
#[derive(Clone, Debug, Serialize)]
pub struct TraceNetwork {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) masked_client_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) asn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tls_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tls_cipher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) edge_hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) edge_region: Option<String>,
}

/// Immutable version-one request context without page or identity properties.
#[derive(Clone, Debug, Serialize)]
pub struct TraceRequestContextV1 {
    pub(super) schema_version: u8,
    pub(super) captured_at: String,
    pub(super) network: TraceNetwork,
    pub(super) cookies: TraceCookies,
}

impl TraceRequestContextV1 {
    /// Borrow the frozen cookie observations used by state and capture gates.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let observed_active = context.cookies().observed_active();
    /// ```
    #[must_use]
    pub const fn cookies(&self) -> &TraceCookies {
        &self.cookies
    }

    /// Borrow the exact UTC capture timestamp.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// assert!(context.captured_at().ends_with('Z'));
    /// ```
    #[must_use]
    pub fn captured_at(&self) -> &str {
        &self.captured_at
    }

    /// Borrow the allowlisted network projection.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let network = serde_json::to_value(context.network())?;
    /// ```
    #[must_use]
    pub const fn network(&self) -> &TraceNetwork {
        &self.network
    }
}
