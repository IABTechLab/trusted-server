//! Evaluates a provider's [`ConsentPolicy`] against a request's consent.

use super::descriptor::{ConsentPolicy, UsOptOut};
use crate::consent::types::PrivacyFlag;
use crate::consent::{ConsentContext, effective_tcf, tcf_applies};

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

/// The highest TCF purpose the fingerprint has a bit for.
pub(crate) const FP_PURPOSES: u8 = 10;
/// Fingerprint bit of the vendor consent. Purposes 1 to 10 hold bits 0 to 9.
pub(crate) const FP_VENDOR: u32 = 10;
/// Fingerprint bit of the US sale opt-out.
pub(crate) const FP_US_SALE: u32 = 11;
/// Fingerprint bit of Global Privacy Control.
pub(crate) const FP_GPC: u32 = 12;
/// Fingerprint bit of `DNT: 1`.
const FP_DNT: u32 = 13;
/// Fingerprint bit of the US sharing opt-out.
pub(crate) const FP_US_SHARING: u32 = 14;
/// Fingerprint bit of the US targeted-advertising opt-out.
pub(crate) const FP_US_TARGETED: u32 = 15;
/// Fingerprint bit set when TCF applied and a TC string was decoded, so the
/// purpose and vendor bits record a grant or a denial rather than nothing.
pub(crate) const FP_TCF: u32 = 16;
/// The US opt-out bits: sale, sharing, targeted advertising and GPC.
pub(crate) const FP_US_OPT_OUT_MASK: u64 =
    (1 << FP_US_SALE) | (1 << FP_US_SHARING) | (1 << FP_US_TARGETED) | (1 << FP_GPC);

/// Whether the request carries a decoded US sale opt-out, from a GPP US
/// section, GPP section 6 or the US Privacy string.
fn us_sale_opt_out(consent: &ConsentContext) -> bool {
    let section_6 = consent
        .gpp
        .as_ref()
        .and_then(|gpp| gpp.us_privacy_section.as_ref());
    consent.gpp.as_ref().and_then(|gpp| gpp.us_sale_opt_out) == Some(true)
        || section_6
            .into_iter()
            .chain(consent.us_privacy.as_ref())
            .any(|usp| usp.opt_out_sale == PrivacyFlag::Yes)
}

/// Whether the request carries a decoded US sharing opt-out from GPP.
fn us_sharing_opt_out(consent: &ConsentContext) -> bool {
    consent.gpp.as_ref().and_then(|gpp| gpp.us_sharing_opt_out) == Some(true)
}

/// Whether the request carries a decoded US targeted-advertising opt-out from
/// GPP.
fn us_targeted_advertising_opt_out(consent: &ConsentContext) -> bool {
    consent
        .gpp
        .as_ref()
        .and_then(|gpp| gpp.us_targeted_advertising_opt_out)
        == Some(true)
}

/// Whether Global Privacy Control is set, by `Sec-GPC` or GPP's GPC field.
fn gpc_signal(consent: &ConsentContext) -> bool {
    consent.gpc || consent.gpp.as_ref().and_then(|gpp| gpp.us_gpc) == Some(true)
}

/// Whether GPP marks a known child without consent.
fn known_child_without_consent(consent: &ConsentContext) -> bool {
    consent.gpp.as_ref().and_then(|gpp| gpp.us_known_child) == Some(true)
}

/// The consent an opt-out's `fp` records, as a lowercase hex bitmask (spec 8).
///
/// Each signal has a fixed bit, whatever the policy reads:
///
/// - bits 0 to 9: TCF purposes 1 to 10, set when consent grants the purpose
/// - bit 10: the policy's vendor, set when consent grants it
/// - bit 11: the US sale opt-out, from `us_privacy`, a GPP US section or GPP
///   section 6
/// - bit 12: GPC
/// - bit 13: DNT
/// - bit 14: the US sharing opt-out
/// - bit 15: the US targeted-advertising opt-out
///
/// The purpose bits are 0 when TCF does not apply or no TCF consent is
/// present, so a request with no TCF consent and one that grants nothing
/// share the same purpose bits.
///
/// A signal the policy does not read or relaxes stays 0. Later parts append
/// bits after these and keep these where they are.
#[must_use]
// Temporary: the request wiring, the first non-test caller, arrives in a later task.
#[allow(dead_code)]
pub(crate) fn fingerprint_bits(
    policy: &ConsentPolicy,
    consent: &ConsentContext,
    signals: PolicySignals,
) -> String {
    let mut bits: u64 = 0;
    if !policy.is_eid_persistence_only() {
        if tcf_applies(consent)
            && let Some(tcf) = effective_tcf(consent)
        {
            bits |= 1 << FP_TCF;
            for purpose in policy.tcf_purposes() {
                if (1..=FP_PURPOSES).contains(purpose)
                    && tcf.has_purpose_consent(usize::from(*purpose))
                {
                    bits |= 1 << (purpose - 1);
                }
            }
            if let Some(vendor) = policy.tcf_vendor()
                && tcf.has_vendor_consent(vendor)
            {
                bits |= 1 << FP_VENDOR;
            }
        }
        if !policy.allows_us_opt_out(UsOptOut::Sale) && us_sale_opt_out(consent) {
            bits |= 1 << FP_US_SALE;
        }
        if !policy.allows_us_opt_out(UsOptOut::Sharing) && us_sharing_opt_out(consent) {
            bits |= 1 << FP_US_SHARING;
        }
        if !policy.allows_us_opt_out(UsOptOut::TargetedAdvertising)
            && us_targeted_advertising_opt_out(consent)
        {
            bits |= 1 << FP_US_TARGETED;
        }
        if !policy.allows_gpc() && gpc_signal(consent) {
            bits |= 1 << FP_GPC;
        }
        if !policy.allows_dnt() && signals.dnt {
            bits |= 1 << FP_DNT;
        }
    }
    format!("{bits:x}")
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
    // A known child, or a US section whose opt-outs could not be read, blocks
    // every provider whatever the policy relaxes.
    if known_child_without_consent(consent)
        || consent
            .gpp
            .as_ref()
            .is_some_and(|gpp| gpp.us_section_decode_failed)
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
    if !policy.allows_us_opt_out(UsOptOut::Sharing) && us_sharing_opt_out(consent) {
        return false;
    }
    if !policy.allows_us_opt_out(UsOptOut::TargetedAdvertising)
        && us_targeted_advertising_opt_out(consent)
    {
        return false;
    }
    if !policy.allows_gpc() && gpc_signal(consent) {
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
    use crate::consent::types::{GppConsent, TcfConsent};
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
    fn header_only_gpp_does_not_block_strict_where_tcf_does_not_apply() {
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

        let mut undecodable = consent_with(Signals::default());
        undecodable.raw_gpp_string = Some("not-a-gpp-string".to_owned());
        assert!(
            !policy_allows(
                &ConsentPolicy::strict(),
                &undecodable,
                PolicySignals { dnt: false }
            ),
            "should fail closed on a GPP string that has no decoded form"
        );
    }

    #[test]
    fn corrupt_us_section_blocks_strict_and_ignores_relaxations() {
        let corrupt = decode_gpp_string("DBABL~not-a-valid-usnat-section")
            .expect("should decode the GPP header with a raw section");
        assert!(
            corrupt.us_section_decode_failed,
            "should flag the undecodable US section"
        );
        let mut consent = consent_with(Signals::default());
        consent.raw_gpp_string = Some("corrupt".to_owned());
        consent.gpp = Some(corrupt);
        let relaxed = ConsentPolicy::strict()
            .allowing_gpc()
            .allowing_dnt()
            .allowing_us_opt_out(UsOptOut::Sale)
            .allowing_us_opt_out(UsOptOut::Sharing)
            .allowing_us_opt_out(UsOptOut::TargetedAdvertising);
        assert!(
            !allows(&ConsentPolicy::strict(), &consent),
            "should block strict on an undecodable US section"
        );
        assert!(
            !allows(&relaxed, &consent),
            "should block whatever the policy relaxes"
        );
        assert!(
            allows(&ConsentPolicy::eid_persistence_only(), &consent),
            "should leave generic pull's parity policy at the global gates"
        );
        let control = consent_with_gpp(|_| {});
        assert!(
            allows(&ConsentPolicy::strict(), &control),
            "should allow a decodable GPP string under the same policy"
        );
    }

    fn consent_with_gpp(edit: impl FnOnce(&mut GppConsent)) -> ConsentContext {
        let mut consent = consent_with(Signals {
            gpp_header_only: true,
            ..Signals::default()
        });
        edit(consent.gpp.as_mut().expect("should carry a decoded GPP"));
        consent
    }

    fn allows(policy: &ConsentPolicy, consent: &ConsentContext) -> bool {
        policy_allows(policy, consent, PolicySignals { dnt: false })
    }

    #[test]
    fn strict_blocks_sharing_targeted_gpc_field_and_known_child() {
        let strict = ConsentPolicy::strict();
        let control = consent_with_gpp(|gpp| {
            gpp.us_sharing_opt_out = Some(false);
            gpp.us_targeted_advertising_opt_out = Some(false);
            gpp.us_gpc = Some(false);
            gpp.us_known_child = Some(false);
        });
        assert!(
            allows(&strict, &control),
            "should allow a GPP string that sets none of the new signals"
        );

        let sharing = consent_with_gpp(|gpp| gpp.us_sharing_opt_out = Some(true));
        assert!(
            !allows(&strict, &sharing),
            "should block on the sharing opt-out"
        );
        assert!(
            allows(
                &ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::Sharing),
                &sharing
            ),
            "should allow the sharing opt-out when the policy relaxes it"
        );
        assert!(
            !allows(
                &ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::TargetedAdvertising),
                &sharing
            ),
            "should keep blocking sharing when only another opt-out is relaxed"
        );

        let targeted = consent_with_gpp(|gpp| gpp.us_targeted_advertising_opt_out = Some(true));
        assert!(
            !allows(&strict, &targeted),
            "should block on the targeted-advertising opt-out"
        );
        assert!(
            allows(
                &ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::TargetedAdvertising),
                &targeted
            ),
            "should allow the targeted-advertising opt-out when the policy relaxes it"
        );

        let gpc = consent_with_gpp(|gpp| gpp.us_gpc = Some(true));
        assert!(!allows(&strict, &gpc), "should block on GPP's GPC field");
        assert!(
            allows(&ConsentPolicy::strict().allowing_gpc(), &gpc),
            "should allow GPP's GPC field when the policy relaxes GPC"
        );

        let child = consent_with_gpp(|gpp| gpp.us_known_child = Some(true));
        let everything_relaxed = ConsentPolicy::strict()
            .allowing_gpc()
            .allowing_dnt()
            .allowing_us_opt_out(UsOptOut::Sale)
            .allowing_us_opt_out(UsOptOut::Sharing)
            .allowing_us_opt_out(UsOptOut::TargetedAdvertising);
        assert!(!allows(&strict, &child), "should block a known child");
        assert!(
            !allows(&everything_relaxed, &child),
            "should block a known child whatever the policy relaxes"
        );
        assert!(
            allows(&everything_relaxed, &control),
            "should allow the control under the same relaxations"
        );
        assert!(
            allows(&ConsentPolicy::eid_persistence_only(), &child),
            "should leave generic pull's parity policy at the global gates only"
        );
    }

    #[test]
    fn targeted_advertising_opt_out_without_sale_opt_out_blocks_strict() {
        let consent = consent_with_gpp(|gpp| {
            gpp.us_sale_opt_out = Some(false);
            gpp.us_targeted_advertising_opt_out = Some(true);
        });
        assert!(
            !allows(&ConsentPolicy::strict(), &consent),
            "should block on targeted advertising alone"
        );
        let sale_only = consent_with_gpp(|gpp| gpp.us_sale_opt_out = Some(false));
        assert!(
            allows(&ConsentPolicy::strict(), &sale_only),
            "should allow when nothing opts out"
        );
    }

    #[test]
    fn section_6_sale_opt_out_inside_gpp_blocks_strict() {
        let opted_out = consent_with_gpp(|gpp| {
            gpp.us_privacy_section = Some(decode_us_privacy("1YYN").expect("should decode"));
        });
        assert!(
            !allows(&ConsentPolicy::strict(), &opted_out),
            "should block on a sale opt-out in GPP section 6"
        );
        assert!(
            allows(
                &ConsentPolicy::strict().allowing_us_opt_out(UsOptOut::Sale),
                &opted_out
            ),
            "should allow it when the policy relaxes the sale opt-out"
        );
        let not_opted_out = consent_with_gpp(|gpp| {
            gpp.us_privacy_section = Some(decode_us_privacy("1YNN").expect("should decode"));
        });
        assert!(
            allows(&ConsentPolicy::strict(), &not_opted_out),
            "should allow a section 6 string that does not opt out"
        );
    }

    #[test]
    fn strict_fails_closed_on_raw_gpp_and_raw_us_privacy_without_a_decoded_form() {
        let mut raw_gpp = consent_with(Signals::default());
        raw_gpp.raw_gpp_string = Some("DBABTA~1YNN".to_owned());
        assert!(
            !allows(&ConsentPolicy::strict(), &raw_gpp),
            "should block a raw GPP string with no decoded form"
        );
        raw_gpp.gpp = Some(decode_gpp_string("DBABTA~1YNN").expect("should decode"));
        assert!(
            allows(&ConsentPolicy::strict(), &raw_gpp),
            "should allow the same string once it decodes"
        );

        let mut raw_usp = consent_with(Signals::default());
        raw_usp.raw_us_privacy = Some("1YNN".to_owned());
        assert!(
            !allows(&ConsentPolicy::strict(), &raw_usp),
            "should block a raw US Privacy string with no decoded form"
        );
        raw_usp.us_privacy = Some(decode_us_privacy("1YNN").expect("should decode"));
        assert!(
            allows(&ConsentPolicy::strict(), &raw_usp),
            "should allow the same string once it decodes"
        );
    }

    #[test]
    fn custom_tcf_purposes_and_gpp_vendor_consent_are_enforced() {
        let gdpr = consent_with(Signals {
            gdpr: true,
            purposes: vec![1, 4],
            vendors: vec![9999],
            ..Signals::default()
        });
        assert!(
            allows(&ConsentPolicy::strict(), &gdpr),
            "should allow purposes 1 and 4 under strict"
        );
        assert!(
            !allows(&ConsentPolicy::strict().with_tcf_purposes([7]), &gdpr),
            "should block when a custom required purpose is not granted"
        );
        let with_seven = consent_with(Signals {
            gdpr: true,
            purposes: vec![1, 4, 7],
            ..Signals::default()
        });
        assert!(
            allows(&ConsentPolicy::strict().with_tcf_purposes([7]), &with_seven),
            "should allow when the custom required purpose is granted"
        );

        let gpp_tcf = consent_with(Signals {
            gdpr: true,
            purposes: vec![1, 4],
            vendors: vec![9999],
            tcf_in_gpp: true,
            ..Signals::default()
        });
        assert!(
            allows(&ConsentPolicy::strict().with_tcf_vendor(9999), &gpp_tcf),
            "should allow a vendor granted in the GPP TCF section"
        );
        assert!(
            !allows(&ConsentPolicy::strict().with_tcf_vendor(1234), &gpp_tcf),
            "should block a vendor the GPP TCF section does not grant"
        );
    }

    fn fingerprint(policy: &ConsentPolicy, consent: &ConsentContext) -> String {
        fingerprint_bits(policy, consent, PolicySignals::default())
    }

    #[test]
    fn fingerprint_is_stable_across_cmp_resaves_and_changes_with_a_purpose() {
        let first = {
            let mut consent = consent_with(Signals {
                gdpr: true,
                purposes: vec![1, 4],
                ..Signals::default()
            });
            consent.raw_tc_string = Some("tc-string-one".to_owned());
            consent
        };
        let resaved = {
            let mut consent = first.clone();
            consent.raw_tc_string = Some("tc-string-two".to_owned());
            let tcf = consent.tcf.as_mut().expect("should carry TCF");
            tcf.last_updated_ds = 1_700_000_000;
            tcf.created_ds = 1_600_000_000;
            tcf.cmp_version = 9;
            consent
        };
        let without_purpose_four = consent_with(Signals {
            gdpr: true,
            purposes: vec![1],
            ..Signals::default()
        });
        let strict = ConsentPolicy::strict();

        assert_eq!(
            fingerprint(&strict, &first),
            fingerprint(&strict, &resaved),
            "should give the same bits after a CMP resave with the same decisions"
        );
        assert_eq!(
            fingerprint(&strict, &first),
            "10009",
            "should set bits 0 and 3 for purposes 1 and 4, and bit 16 for the decoded TC string"
        );
        assert_ne!(
            fingerprint(&strict, &first),
            fingerprint(&strict, &without_purpose_four),
            "should change when purpose 4 is dropped"
        );
        assert_eq!(
            fingerprint(&strict, &without_purpose_four),
            "10001",
            "should keep only bit 0 and the TCF bit when purpose 4 is dropped"
        );
    }

    #[test]
    fn fingerprint_bits_name_each_opt_out_separately() {
        let strict = ConsentPolicy::strict();
        let bits = |consent: &ConsentContext| {
            u64::from_str_radix(&fingerprint(&strict, consent), 16).expect("should parse as hex")
        };
        let neither = consent_with_gpp(|_| {});
        let sharing = consent_with_gpp(|gpp| gpp.us_sharing_opt_out = Some(true));
        let targeted = consent_with_gpp(|gpp| gpp.us_targeted_advertising_opt_out = Some(true));

        assert_eq!(bits(&neither), 0, "should be zero with no opt-out");
        assert_eq!(
            bits(&sharing),
            1 << FP_US_SHARING,
            "should set only the sharing bit"
        );
        assert_eq!(
            bits(&targeted),
            1 << FP_US_TARGETED,
            "should set only the targeted-advertising bit"
        );
        assert_ne!(
            bits(&sharing),
            bits(&targeted),
            "should tell the two opt-outs apart"
        );

        let relaxed = ConsentPolicy::strict()
            .allowing_us_opt_out(UsOptOut::Sharing)
            .allowing_us_opt_out(UsOptOut::TargetedAdvertising);
        assert_eq!(
            fingerprint(&relaxed, &sharing),
            "0",
            "should leave a relaxed sharing opt-out at zero"
        );
        assert_eq!(
            fingerprint(&relaxed, &targeted),
            "0",
            "should leave a relaxed targeted-advertising opt-out at zero"
        );
    }

    #[test]
    fn fingerprint_names_gpp_gpc_sale_and_the_mask() {
        let strict = ConsentPolicy::strict();
        let gpp_gpc = consent_with_gpp(|gpp| gpp.us_gpc = Some(true));
        assert_eq!(
            fingerprint(&strict, &gpp_gpc),
            "1000",
            "should set the existing GPC bit for GPP's GPC field"
        );
        let section_6 = consent_with_gpp(|gpp| {
            gpp.us_privacy_section = Some(decode_us_privacy("1YYN").expect("should decode"));
        });
        assert_eq!(
            fingerprint(&strict, &section_6),
            "800",
            "should set the sale bit for a section 6 opt-out"
        );
        assert_eq!(
            (FP_US_SALE, FP_GPC, FP_US_SHARING, FP_US_TARGETED),
            (11, 12, 14, 15),
            "should keep each signal at its named position"
        );
        assert_eq!(
            FP_US_OPT_OUT_MASK, 0xd800,
            "should cover sale, sharing, targeted advertising and GPC"
        );
    }

    #[test]
    fn fingerprint_keeps_part_1_bit_positions() {
        let policy = ConsentPolicy::strict()
            .with_tcf_purposes([2, 10])
            .with_tcf_vendor(5);
        let mut consent = consent_with(Signals {
            gdpr: true,
            purposes: (1..=10).collect(),
            vendors: vec![5],
            gpc: true,
            us_sale_opt_out: true,
            ..Signals::default()
        });
        consent.gpc = true;
        assert_eq!(
            fingerprint_bits(&policy, &consent, PolicySignals { dnt: true }),
            "13e0b",
            "should keep purposes 1, 2, 4 and 10 at bits 0, 1, 3 and 9, vendor 10, sale 11, GPC 12, DNT 13 and TCF 16"
        );
    }
}
