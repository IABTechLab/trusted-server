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
