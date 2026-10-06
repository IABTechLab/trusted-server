//! IAB TCF v2 as a permission signal provider.
//!
//! Answers from the decoded TCF record for the purposes this crate maps to
//! each permission, and is the one place that knows what a TCF purpose is.
//! Core decodes the TC string, keeps the record against the Edge Cookie
//! identifier, expires it by age and resolves it against a GPP EU section, and
//! this provider reads what that pipeline produced rather than decoding the
//! cookie a second time. Reading the wire directly would silently skip the
//! cached record on a returning visitor and the expiry rule, and answer
//! differently from every other reader of the same request.
//!
//! This provider lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

mod mapping;

use std::sync::OnceLock;

pub use mapping::purpose_for;

#[cfg(test)]
use trusted_server_core::consent::types::TcfConsent;
use trusted_server_core::consent::{ConsentContext, effective_tcf};
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::permission_signal::{PermissionSignalProvider, SignalInput};
use trusted_server_core::permissions::{
    ConsentSignal, Permission, PermissionSet, SignalPolicy, ValidSignal,
};

/// The stable identifier this provider answers to in `[permission_signal]`
/// `provider`, in logs, and when a peer consults it.
pub const ID: &str = "tcf";

/// TCF v2, when the policy says TCF answers for this deployment.
///
/// The mapping from permission to purpose is this crate's, in
/// [`purpose_for`], so core carries no table of another scheme's numbers. A
/// permission no purpose maps to gets silence, not a refusal.
#[derive(Debug, Default, Clone, Copy)]
pub struct TcfProvider;

impl TcfProvider {
    /// A new provider.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl PermissionSignalProvider for TcfProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        // The policy still says whether a TCF record answers for this
        // deployment at all. What it no longer says is which purpose grants
        // which permission, because that is this scheme's own knowledge.
        if !input.policy.tcf_authoritative() {
            return ConsentSignal::Neutral;
        }
        let Some(purpose) = mapping::purpose_for(permission) else {
            // TCF has nothing to say about this Data Use, so it says nothing.
            return ConsentSignal::Neutral;
        };
        if unreadable(input.consent) {
            // A TC string arrived and could not be read. The visitor expressed
            // a preference this provider cannot see, which is not the same as
            // no preference, so it fails closed on everything it maps rather
            // than leaving the place baseline standing. An expired record is
            // not this case, expiry being its own explicit state.
            return ConsentSignal::Revoke;
        }
        let Some(record) = effective_tcf(input.consent) else {
            // No TCF record on the request. Silence, not refusal, because
            // reading an absent scheme as a refusal would revoke on every
            // request that did not carry it.
            return ConsentSignal::Neutral;
        };
        if record.has_purpose_consent(usize::from(purpose)) {
            ConsentSignal::Grant
        } else {
            // A purpose the visitor did not consent to is a refusal. Reading it
            // as silence would leave the country baseline standing and grant
            // what they declined.
            ConsentSignal::Revoke
        }
    }

    /// Every Data Use a purpose maps to, when the policy lets a record answer,
    /// and nothing when it does not, because a silenced record grants nothing
    /// and a page must not wait for it.
    fn grants(&self, policy: &SignalPolicy) -> PermissionSet {
        if !policy.tcf_authoritative() {
            return PermissionSet::none();
        }
        mapped()
    }

    /// The standalone TC string, when it decoded and has not expired. A record
    /// carried inside a GPP string is the GPP string's, which the GPP
    /// provider vouches for.
    fn valid_signal(
        &self,
        consent: &ConsentContext,
        _evidence: &dyn RequestInfo,
    ) -> Option<ValidSignal> {
        consent.tcf.as_ref()?;
        let raw = consent.raw_tc_string.as_deref()?;
        Some(ValidSignal::new(ID, "tcf", raw))
    }

    /// Only a TCF record refusing storage withdraws, because only TCF records
    /// a visitor declining the very signal storage depended on. A US-style
    /// opt-out suppresses use for the request and never destroys an identifier,
    /// so the other providers leave this at its default.
    ///
    /// Whether the refusal is destructive at all is core's to decide from the
    /// jurisdiction's storage baseline, which is why this answers the narrow
    /// question only. It does not consult `tcf_authoritative`, matching the
    /// rule as it stood before the seam, where a record refusing storage
    /// withdrew whether or not the policy let the record grant anything.
    fn withdraws(&self, permission: Permission, input: &SignalInput<'_>) -> bool {
        if permission != Permission::StoreOnDevice {
            return false;
        }
        effective_tcf(input.consent).is_some_and(|record| !record.has_storage_consent())
    }
}

/// Every Data Use a purpose maps to, computed once rather than on every
/// request, because the mapping is a constant of this crate.
fn mapped() -> PermissionSet {
    static MAPPED: OnceLock<PermissionSet> = OnceLock::new();
    *MAPPED.get_or_init(|| {
        Permission::all()
            .filter(|permission| mapping::purpose_for(*permission).is_some())
            .collect()
    })
}

/// Whether a TC string arrived that could not be decoded, expiry aside.
fn unreadable(consent: &ConsentContext) -> bool {
    consent.raw_tc_string.is_some() && consent.tcf.is_none() && !consent.expired
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::consent::ConsentContext;
    use trusted_server_core::consent::types::GppConsent;
    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::permission_signal::SignalInput;
    use trusted_server_core::permissions::{Acquisition, PermissionMaps, SignalPolicy};

    /// The shipped policy, under which a TCF record answers.
    fn shipped_policy() -> &'static SignalPolicy {
        PermissionMaps::standard().signals()
    }

    /// Builds a minimal decoded TCF record consenting to the given 1-indexed
    /// purposes, with everything else refused.
    fn tcf_with_purposes(consented: &[usize]) -> TcfConsent {
        let mut purpose_consents = vec![false; 24];
        for &purpose in consented {
            purpose_consents[purpose - 1] = true;
        }
        TcfConsent {
            version: 2,
            cmp_id: 0,
            cmp_version: 0,
            consent_screen: 0,
            consent_language: "EN".to_owned(),
            vendor_list_version: 0,
            tcf_policy_version: 2,
            created_ds: 0,
            last_updated_ds: 0,
            purpose_consents,
            purpose_legitimate_interests: vec![false; 24],
            vendor_consents: Vec::new(),
            vendor_legitimate_interests: Vec::new(),
            special_feature_opt_ins: vec![false; 12],
        }
    }

    fn with_record(consented: &[usize]) -> ConsentContext {
        ConsentContext {
            tcf: Some(tcf_with_purposes(consented)),
            ..ConsentContext::default()
        }
    }

    fn answer(
        consent: &ConsentContext,
        policy: &SignalPolicy,
        permission: Permission,
    ) -> ConsentSignal {
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(consent, &evidence, policy, Acquisition::RequiresSignal);
        TcfProvider::new().signal(permission, &input)
    }

    fn withdraws(consent: &ConsentContext, policy: &SignalPolicy, permission: Permission) -> bool {
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(consent, &evidence, policy, Acquisition::RequiresSignal);
        TcfProvider::new().withdraws(permission, &input)
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(
            TcfProvider::new().id(),
            ID,
            "the provider answers to the identifier configuration names"
        );
    }

    #[test]
    fn grants_a_permission_whose_purpose_the_record_consents_to() {
        assert_eq!(
            answer(
                &with_record(&[1]),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Grant,
            "Purpose 1 consent grants device storage"
        );
    }

    #[test]
    fn revokes_a_permission_whose_purpose_the_record_refuses() {
        assert_eq!(
            answer(
                &with_record(&[1]),
                shipped_policy(),
                Permission::SelectPersonalisedAds
            ),
            ConsentSignal::Revoke,
            "a purpose the visitor did not consent to is a refusal, not silence"
        );
    }

    #[test]
    fn is_silent_for_a_data_use_no_purpose_grants() {
        let sale = Permission::from_identifier("disclosure.sale")
            .expect("should be a known Data Use with no TCF purpose");
        assert_eq!(
            answer(&with_record(&[1]), shipped_policy(), sale),
            ConsentSignal::Neutral,
            "TCF has nothing to say about a Data Use none of its purposes grant"
        );
    }

    #[test]
    fn is_silent_when_no_record_arrived() {
        assert_eq!(
            answer(
                &ConsentContext::default(),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "an absent record is silence, never a refusal"
        );
    }

    #[test]
    fn an_unreadable_tc_string_is_this_providers_refusal_and_an_expired_one_is_not() {
        // A string arrived that could not be decoded. That is a preference
        // this provider cannot see, so it refuses what it maps, silently,
        // and says nothing about a Data Use no purpose covers.
        let unreadable = ConsentContext {
            raw_tc_string: Some("not a TC string".to_owned()),
            ..ConsentContext::default()
        };
        assert_eq!(
            answer(&unreadable, shipped_policy(), Permission::StoreOnDevice),
            ConsentSignal::Revoke,
            "an unreadable record refuses the Data Uses this scheme covers"
        );
        let email = Permission::all()
            .find(|permission| permission.as_str() == "advertising_marketing.communications.email")
            .expect("the taxonomy should carry the email channel");
        assert_eq!(
            answer(&unreadable, shipped_policy(), email),
            ConsentSignal::Neutral,
            "and says nothing about a Data Use no purpose maps to"
        );
        assert_eq!(
            answer(
                &unreadable,
                &SignalPolicy::default(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a silenced scheme says nothing, readable or not"
        );
        let expired = ConsentContext {
            raw_tc_string: Some("CPold".to_owned()),
            expired: true,
            ..ConsentContext::default()
        };
        assert_eq!(
            answer(&expired, shipped_policy(), Permission::StoreOnDevice),
            ConsentSignal::Neutral,
            "expiry is its own explicit state and not an unreadable record"
        );
    }

    #[test]
    fn vouches_for_the_standalone_tc_string_only_when_it_decoded() {
        let evidence = OwnedRequestInfo::default();
        let decoded = ConsentContext {
            raw_tc_string: Some("CPreadable".to_owned()),
            ..with_record(&[1])
        };
        assert_eq!(
            TcfProvider::new().valid_signal(&decoded, &evidence),
            Some(ValidSignal::new(ID, "tcf", "CPreadable")),
            "a decoded record vouches for the string as received"
        );
        let unreadable = ConsentContext {
            raw_tc_string: Some("not a TC string".to_owned()),
            ..ConsentContext::default()
        };
        assert_eq!(
            TcfProvider::new().valid_signal(&unreadable, &evidence),
            None,
            "an unreadable string is not vouched for"
        );
        let expired = ConsentContext {
            raw_tc_string: Some("CPold".to_owned()),
            expired: true,
            ..ConsentContext::default()
        };
        assert_eq!(
            TcfProvider::new().valid_signal(&expired, &evidence),
            None,
            "an expired record is not one this provider uses, so it is not vouched for"
        );
        assert_eq!(
            TcfProvider::new().valid_signal(&ConsentContext::default(), &evidence),
            None,
            "and no record is no signal"
        );
    }

    #[test]
    fn declares_exactly_the_data_uses_a_purpose_maps_to() {
        // Under the shipped policy a record answers, so every mapped Data Use
        // is declared and an unmapped one is not. Under a policy that silences
        // the record nothing is declared, because a record that cannot answer
        // is not one a page should wait for.
        let declared = TcfProvider::new().grants(shipped_policy());
        assert!(
            declared.contains(Permission::StoreOnDevice),
            "purpose 1 maps to storage, so storage is grantable"
        );
        let email = Permission::all()
            .find(|permission| permission.as_str() == "advertising_marketing.communications.email")
            .expect("the taxonomy should carry the email channel");
        assert!(
            !declared.contains(email),
            "no purpose maps to a marketing channel, so it is not grantable"
        );
        assert_eq!(
            declared.iter().count(),
            Permission::all()
                .filter(|permission| purpose_for(*permission).is_some())
                .count(),
            "should declare each mapped Data Use once and nothing else"
        );
        assert!(
            TcfProvider::new()
                .grants(&SignalPolicy::default())
                .is_empty(),
            "a silenced record grants nothing"
        );
    }

    #[test]
    fn a_non_authoritative_policy_silences_the_record_but_not_the_withdrawal() {
        // The default policy declares no TCF block, so the record does not
        // answer for the deployment. Withdrawal is the narrower, destructive
        // question and keeps the rule it had before the seam, which did not
        // consult the flag.
        let silenced = SignalPolicy::default();
        assert!(
            !silenced.tcf_authoritative(),
            "the fixture must not be authoritative"
        );
        assert_eq!(
            answer(
                &with_record(&[4]),
                &silenced,
                Permission::SelectPersonalisedAds
            ),
            ConsentSignal::Neutral,
            "a record the policy does not let answer stays silent"
        );
        assert!(
            withdraws(&with_record(&[4]), &silenced, Permission::StoreOnDevice),
            "but a record refusing storage still withdraws, as it did before the seam"
        );
    }

    #[test]
    fn withdraws_only_for_storage_and_only_when_refused() {
        assert!(
            withdraws(
                &with_record(&[4]),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            "refusing Purpose 1 withdraws storage"
        );
        assert!(
            !withdraws(
                &with_record(&[1]),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            "consenting to Purpose 1 is not a withdrawal"
        );
        assert!(
            !withdraws(
                &with_record(&[]),
                shipped_policy(),
                Permission::SelectPersonalisedAds
            ),
            "no other permission is ever withdrawn, refused or not"
        );
        assert!(
            !withdraws(
                &ConsentContext::default(),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            "and no record is never a withdrawal"
        );
    }

    #[test]
    fn reads_the_eu_section_of_a_gpp_string_when_there_is_no_standalone_record() {
        // Core resolves a GPP string's EU TCF section as the effective record
        // when no TC string arrived, and this provider reads what core resolved
        // rather than the wire, so it sees that section too.
        let consent = ConsentContext {
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![2],
                eu_tcf: Some(tcf_with_purposes(&[4])),
                us_sale_opt_out: None,
            }),
            ..ConsentContext::default()
        };
        assert_eq!(
            answer(
                &consent,
                shipped_policy(),
                Permission::SelectPersonalisedAds
            ),
            ConsentSignal::Grant,
            "the EU section's consent to Purpose 4 grants targeted advertising"
        );
        assert!(
            withdraws(&consent, shipped_policy(), Permission::StoreOnDevice),
            "and its refusal of Purpose 1 withdraws storage"
        );
    }
}
