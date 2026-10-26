//! Evaluates a provider's [`ConsentPolicy`] against a request's consent.

use super::descriptor::{ConsentPolicy, UsOptOut};
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::PrivacyFlag;
use crate::consent::{ConsentContext, effective_tcf};

/// Signals the consent context does not carry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PolicySignals {
    /// `DNT: 1` was present.
    pub(crate) dnt: bool,
}

impl PolicySignals {
    /// Reads the signals a request's headers carry.
    #[must_use]
    pub(crate) fn from_headers(headers: &http::HeaderMap) -> Self {
        Self {
            dnt: headers
                .get("dnt")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.trim() == "1"),
        }
    }
}

/// Where TCF applies: `gdpr_applies`, or a jurisdiction of GDPR or unknown.
#[must_use]
pub(crate) fn tcf_applies(consent: &ConsentContext) -> bool {
    consent.gdpr_applies
        || matches!(
            consent.jurisdiction,
            Jurisdiction::Gdpr | Jurisdiction::Unknown
        )
}

/// Whether the request carries a decoded US sale opt-out, from GPP or from
/// the US Privacy string.
pub(crate) fn us_sale_opt_out(consent: &ConsentContext) -> bool {
    consent.gpp.as_ref().and_then(|gpp| gpp.us_sale_opt_out) == Some(true)
        || consent
            .us_privacy
            .as_ref()
            .is_some_and(|usp| usp.opt_out_sale == PrivacyFlag::Yes)
}

/// Whether `policy` allows a provider under this request's consent. The
/// global EC and EID-persistence gates are checked separately (eligibility
/// steps 1 and 2) and are not repeated here.
#[must_use]
pub(crate) fn policy_allows(
    policy: &ConsentPolicy,
    consent: &ConsentContext,
    signals: PolicySignals,
) -> bool {
    if policy.is_eid_persistence_only() {
        return true;
    }
    // Fail closed: a raw string with no decoded form blocks.
    if (consent.raw_tc_string.is_some() && consent.tcf.is_none())
        || (consent.raw_gpp_string.is_some() && consent.gpp.is_none())
        || (consent.raw_us_privacy.is_some() && consent.us_privacy.is_none())
    {
        return false;
    }
    if tcf_applies(consent) {
        // A TC string or a GPP TCF section. A header-only GPP string has
        // neither, so TCF consent is still missing.
        let Some(tcf) = effective_tcf(consent) else {
            return false;
        };
        if !policy
            .tcf_purposes()
            .iter()
            .all(|purpose| tcf.has_purpose_consent(usize::from(*purpose)))
        {
            return false;
        }
        if let Some(vendor) = policy.tcf_vendor()
            && !tcf.has_vendor_consent(vendor)
        {
            return false;
        }
    }
    if !policy.allows_us_opt_out(UsOptOut::Sale) && us_sale_opt_out(consent) {
        return false;
    }
    if !policy.allows_gpc() && consent.gpc {
        return false;
    }
    if !policy.allows_dnt() && signals.dnt {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::ConsentContext;
    use crate::consent::gpp::decode_gpp_string;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::TcfConsent;
    use crate::consent::us_privacy::decode_us_privacy;
    use crate::ec::identity::descriptor::{ConsentPolicy, UsOptOut};

    #[derive(Default)]
    struct Signals {
        gdpr: bool,
        purposes: Vec<usize>,
        vendors: Vec<u16>,
        us_sale_opt_out: bool,
        gpc: bool,
        raw_tc_without_decode: bool,
        gpp_header_only: bool,
        tcf_in_gpp: bool,
    }

    fn tcf_consent(purposes: &[usize], vendors: &[u16]) -> TcfConsent {
        let mut purpose_consents = vec![false; 24];
        for purpose in purposes {
            purpose_consents[purpose - 1] = true;
        }
        TcfConsent {
            version: 2,
            cmp_id: 1,
            cmp_version: 1,
            consent_screen: 1,
            consent_language: "EN".to_owned(),
            vendor_list_version: 1,
            tcf_policy_version: 4,
            created_ds: 0,
            last_updated_ds: 0,
            purpose_consents,
            purpose_legitimate_interests: vec![false; 24],
            vendor_consents: vendors.to_vec(),
            vendor_legitimate_interests: Vec::new(),
            special_feature_opt_ins: vec![false; 12],
        }
    }

    // Takes the builder by value so call sites read as a struct literal.
    #[allow(clippy::needless_pass_by_value)]
    fn consent_with(signals: Signals) -> ConsentContext {
        let mut consent = ConsentContext {
            gdpr_applies: signals.gdpr,
            gpc: signals.gpc,
            jurisdiction: if signals.gdpr {
                Jurisdiction::Gdpr
            } else {
                Jurisdiction::UsState("CA".to_owned())
            },
            ..ConsentContext::default()
        };
        if signals.gdpr && !signals.tcf_in_gpp && !signals.purposes.is_empty() {
            consent.tcf = Some(tcf_consent(&signals.purposes, &signals.vendors));
        }
        if signals.us_sale_opt_out {
            consent.raw_us_privacy = Some("1YYN".to_owned());
            consent.us_privacy =
                Some(decode_us_privacy("1YYN").expect("should decode a US Privacy string"));
        }
        if signals.raw_tc_without_decode {
            consent.raw_tc_string = Some("undecodable".to_owned());
        }
        if signals.gpp_header_only || signals.tcf_in_gpp {
            consent.raw_gpp_string = Some("DBAA".to_owned());
            let mut gpp =
                decode_gpp_string("DBAA").expect("should decode a header-only GPP string");
            if signals.tcf_in_gpp {
                gpp.eu_tcf = Some(tcf_consent(&signals.purposes, &signals.vendors));
            }
            consent.gpp = Some(gpp);
            consent.gpp_section_ids = Some(Vec::new());
        }
        consent
    }

    #[test]
    fn strict_blocks_each_signal_it_decodes() {
        let strict = ConsentPolicy::strict();
        let allowed = consent_with(Signals::default());
        assert!(
            policy_allows(&strict, &allowed, PolicySignals { dnt: false }),
            "should allow a request with no signal set"
        );

        for (name, signals, dnt) in [
            (
                "gdpr without purpose 4",
                Signals {
                    gdpr: true,
                    purposes: vec![1],
                    ..Signals::default()
                },
                false,
            ),
            (
                "us sale opt-out",
                Signals {
                    us_sale_opt_out: true,
                    ..Signals::default()
                },
                false,
            ),
            (
                "gpc",
                Signals {
                    gpc: true,
                    ..Signals::default()
                },
                false,
            ),
            ("dnt", Signals::default(), true),
            (
                "undecodable tc string",
                Signals {
                    raw_tc_without_decode: true,
                    ..Signals::default()
                },
                false,
            ),
        ] {
            assert!(
                !policy_allows(&strict, &consent_with(signals), PolicySignals { dnt }),
                "should block under strict: {name}"
            );
        }
    }

    #[test]
    fn relaxations_and_vendor_consent() {
        let gpc = consent_with(Signals {
            gpc: true,
            ..Signals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::strict().allowing_gpc(),
                &gpc,
                PolicySignals { dnt: false }
            ),
            "should allow GPC when the policy relaxes it"
        );
        let sale = consent_with(Signals {
            us_sale_opt_out: true,
            ..Signals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::Sale),
                &sale,
                PolicySignals { dnt: false }
            ),
            "should allow a sale opt-out when the policy relaxes it"
        );
        let gdpr = consent_with(Signals {
            gdpr: true,
            purposes: vec![1, 4],
            vendors: vec![9999],
            ..Signals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::strict().with_tcf_vendor(9999),
                &gdpr,
                PolicySignals { dnt: false }
            ),
            "should allow a granted vendor"
        );
        assert!(
            !policy_allows(
                &ConsentPolicy::strict().with_tcf_vendor(1234),
                &gdpr,
                PolicySignals { dnt: false }
            ),
            "should block a vendor without consent"
        );
    }

    #[test]
    fn eid_persistence_only_ignores_provider_signals() {
        let consent = consent_with(Signals {
            gpc: true,
            us_sale_opt_out: true,
            ..Signals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::eid_persistence_only(),
                &consent,
                PolicySignals { dnt: true }
            ),
            "should check only the global gates for generic pull's parity policy, which the coordinator applies separately"
        );
    }

    #[test]
    fn a_header_only_gpp_string_does_not_block_strict() {
        let header_only = consent_with(Signals {
            gpp_header_only: true,
            ..Signals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::strict(),
                &header_only,
                PolicySignals { dnt: false }
            ),
            "should not block on a decoded GPP string with no applicable section"
        );

        let under_gdpr = consent_with(Signals {
            gdpr: true,
            gpp_header_only: true,
            ..Signals::default()
        });
        assert!(
            !policy_allows(
                &ConsentPolicy::strict(),
                &under_gdpr,
                PolicySignals { dnt: false }
            ),
            "should still need TCF consent where TCF applies"
        );

        let gpp_tcf = consent_with(Signals {
            gdpr: true,
            purposes: vec![1, 4],
            tcf_in_gpp: true,
            ..Signals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::strict(),
                &gpp_tcf,
                PolicySignals { dnt: false }
            ),
            "should accept TCF consent from a GPP TCF section"
        );
    }
}
