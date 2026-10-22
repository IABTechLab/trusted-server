//! Evaluates a provider's [`ConsentPolicy`] against a request's consent.

use super::descriptor::{ConsentPolicy, UsOptOut};
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::types::PrivacyFlag;
use crate::consent::{ConsentContext, effective_tcf};

/// Signals a policy reads that the consent context does not carry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PolicySignals {
    /// `DNT: 1` was present.
    pub(crate) dnt: bool,
}

/// Where TCF applies: `gdpr_applies`, or a jurisdiction of GDPR or unknown.
/// The same test [`crate::consent::consent_allows_server_side_auction`] uses.
#[must_use]
pub(crate) fn tcf_applies(consent: &ConsentContext) -> bool {
    consent.gdpr_applies
        || matches!(
            consent.jurisdiction,
            Jurisdiction::Gdpr | Jurisdiction::Unknown
        )
}

/// Whether the request carries a US sale opt-out, from GPP or US Privacy.
fn us_sale_opted_out(consent: &ConsentContext) -> bool {
    consent.gpp.as_ref().and_then(|gpp| gpp.us_sale_opt_out) == Some(true)
        || consent
            .us_privacy
            .as_ref()
            .is_some_and(|usp| usp.opt_out_sale == PrivacyFlag::Yes)
}

/// Whether a raw consent string is present without its decoded form, for
/// example in proxy mode or after a decode failure. A strict policy fails
/// closed on it.
fn has_undecoded_signal(consent: &ConsentContext) -> bool {
    (consent.raw_tc_string.is_some() && consent.tcf.is_none())
        || (consent.raw_gpp_string.is_some() && consent.gpp.is_none())
        || (consent.raw_us_privacy.is_some() && consent.us_privacy.is_none())
}

/// Whether `policy` allows a provider under this request's consent.
///
/// The global EC and EID-persistence gates are checked separately by the
/// coordinator and are not repeated here.
#[must_use]
pub(crate) fn policy_allows(
    policy: &ConsentPolicy,
    consent: &ConsentContext,
    signals: PolicySignals,
) -> bool {
    if policy.is_eid_persistence_only() {
        return true;
    }
    if has_undecoded_signal(consent) {
        return false;
    }
    if tcf_applies(consent) {
        let Some(tcf) = effective_tcf(consent) else {
            return false;
        };
        let purposes_granted = policy
            .tcf_purposes()
            .iter()
            .all(|purpose| tcf.has_purpose_consent(usize::from(*purpose)));
        let vendor_granted = policy
            .tcf_vendor()
            .is_none_or(|vendor| tcf.has_vendor_consent(vendor));
        if !purposes_granted || !vendor_granted {
            return false;
        }
    }
    if !policy.allows_us_opt_out(UsOptOut::Sale) && us_sale_opted_out(consent) {
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

/// The canonical consent signals `policy` reads, for the opt-out
/// fingerprint. Built from decoded decisions, never from the raw consent
/// string, which changes whenever a CMP re-saves it.
#[must_use]
pub(crate) fn fingerprint_bits(
    policy: &ConsentPolicy,
    consent: &ConsentContext,
    signals: PolicySignals,
) -> String {
    let tcf = effective_tcf(consent);
    let mut bits: Vec<String> = policy
        .tcf_purposes()
        .iter()
        .map(|purpose| {
            let granted = tcf.is_some_and(|tcf| tcf.has_purpose_consent(usize::from(*purpose)));
            format!("p{purpose}={}", u8::from(granted))
        })
        .collect();
    if let Some(vendor) = policy.tcf_vendor() {
        let granted = tcf.is_some_and(|tcf| tcf.has_vendor_consent(vendor));
        bits.push(format!("v{vendor}={}", u8::from(granted)));
    }
    if !policy.allows_us_opt_out(UsOptOut::Sale) {
        bits.push(format!("sale={}", u8::from(us_sale_opted_out(consent))));
    }
    if !policy.allows_gpc() {
        bits.push(format!("gpc={}", u8::from(consent.gpc)));
    }
    if !policy.allows_dnt() {
        bits.push(format!("dnt={}", u8::from(signals.dnt)));
    }
    bits.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::descriptor::{ConsentPolicy, UsOptOut};
    use crate::ec::identity::test_support::{TestSignals, consent_with};

    const NO_DNT: PolicySignals = PolicySignals { dnt: false };

    #[test]
    fn strict_blocks_each_signal_it_decodes() {
        let strict = ConsentPolicy::strict();
        assert!(
            policy_allows(&strict, &consent_with(&TestSignals::default()), NO_DNT),
            "control: nothing set"
        );
        let gdpr_ok = TestSignals {
            gdpr: true,
            purposes: vec![1, 4],
            ..TestSignals::default()
        };
        assert!(
            policy_allows(&strict, &consent_with(&gdpr_ok), NO_DNT),
            "control: GDPR with purposes 1 and 4"
        );

        for (name, signals, dnt) in [
            (
                "gdpr without purpose 4",
                TestSignals {
                    gdpr: true,
                    purposes: vec![1],
                    ..TestSignals::default()
                },
                false,
            ),
            (
                "us sale opt-out",
                TestSignals {
                    us_sale_opt_out: true,
                    ..TestSignals::default()
                },
                false,
            ),
            (
                "gpc",
                TestSignals {
                    gpc: true,
                    ..TestSignals::default()
                },
                false,
            ),
            ("dnt", TestSignals::default(), true),
            (
                "undecodable tc string",
                TestSignals {
                    raw_tc_without_decode: true,
                    ..TestSignals::default()
                },
                false,
            ),
        ] {
            assert!(
                !policy_allows(&strict, &consent_with(&signals), PolicySignals { dnt }),
                "strict should block: {name}"
            );
        }
    }

    #[test]
    fn relaxations_and_vendor_consent() {
        let gpc = consent_with(&TestSignals {
            gpc: true,
            ..TestSignals::default()
        });
        assert!(
            policy_allows(&ConsentPolicy::strict().allowing_gpc(), &gpc, NO_DNT),
            "gpc relaxed"
        );
        let sale = consent_with(&TestSignals {
            us_sale_opt_out: true,
            ..TestSignals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::Sale),
                &sale,
                NO_DNT
            ),
            "sale relaxed"
        );
        assert!(
            policy_allows(
                &ConsentPolicy::strict().allowing_dnt(),
                &consent_with(&TestSignals::default()),
                PolicySignals { dnt: true }
            ),
            "dnt relaxed"
        );
        let gdpr = consent_with(&TestSignals {
            gdpr: true,
            purposes: vec![1, 4],
            vendors: vec![9999],
            ..TestSignals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::strict().with_tcf_vendor(9999),
                &gdpr,
                NO_DNT
            ),
            "vendor granted"
        );
        assert!(
            !policy_allows(
                &ConsentPolicy::strict().with_tcf_vendor(1234),
                &gdpr,
                NO_DNT
            ),
            "vendor missing"
        );
        assert!(
            !policy_allows(
                &ConsentPolicy::strict().with_tcf_purposes([7]),
                &gdpr,
                NO_DNT
            ),
            "extra purpose missing"
        );
    }

    #[test]
    fn eid_persistence_only_ignores_provider_signals() {
        let consent = consent_with(&TestSignals {
            gpc: true,
            us_sale_opt_out: true,
            ..TestSignals::default()
        });
        assert!(
            policy_allows(
                &ConsentPolicy::eid_persistence_only(),
                &consent,
                PolicySignals { dnt: true }
            ),
            "generic pull's parity policy checks only the global gates"
        );
    }

    #[test]
    fn fingerprint_bits_follow_decisions_not_raw_strings() {
        let policy = ConsentPolicy::strict().with_tcf_vendor(9999);
        let mut granted = consent_with(&TestSignals {
            gdpr: true,
            purposes: vec![1, 4],
            vendors: vec![9999],
            ..TestSignals::default()
        });
        let before = fingerprint_bits(&policy, &granted, NO_DNT);
        assert_eq!(
            before, "p1=1;p4=1;v9999=1;sale=0;gpc=0;dnt=0",
            "should list each decision the policy reads"
        );
        granted.raw_tc_string = Some("re-saved-by-the-cmp".to_owned());
        assert_eq!(
            fingerprint_bits(&policy, &granted, NO_DNT),
            before,
            "a re-saved string with the same decisions keeps the bits"
        );
        let denied = consent_with(&TestSignals {
            gdpr: true,
            purposes: vec![1],
            vendors: vec![9999],
            ..TestSignals::default()
        });
        assert_ne!(
            fingerprint_bits(&policy, &denied, NO_DNT),
            before,
            "dropping purpose 4 changes the bits"
        );
        assert_eq!(
            fingerprint_bits(
                &ConsentPolicy::strict().allowing_gpc().allowing_dnt(),
                &granted,
                NO_DNT
            ),
            "p1=1;p4=1;sale=0",
            "relaxed signals are left out"
        );
    }

    #[test]
    fn tcf_applies_in_unknown_jurisdiction() {
        let mut consent = consent_with(&TestSignals::default());
        consent.jurisdiction = crate::consent::jurisdiction::Jurisdiction::Unknown;
        assert!(tcf_applies(&consent), "unknown jurisdiction needs TCF");
        assert!(
            !policy_allows(&ConsentPolicy::strict(), &consent, NO_DNT),
            "strict blocks without a TC string where TCF applies"
        );
    }
}
