//! EC-specific permission gating, resolved through the permission model.
//!
//! The Edge Cookie provider advertises the [`Permission`]s its data use
//! requires. [`assemble_permissions`] resolves which permissions are set for a
//! request, from its session signals and the country it maps to, and the
//! context construction gates the provider on that state. The EC permission
//! decision lives here, in the EC subsystem, and nowhere else, so callers
//! route every EC permission check through this module rather than
//! re-deriving one.

use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::permissions::{
    Acquisition, ConsentSignal, OptOutSource, Permission, PermissionMaps, PermissionState,
    SignalPolicy,
};
use crate::platform::GeoInfo;

/// The outcome of the geo lookup for a request, separating "no location
/// resolved" from "the lookup failed".
///
/// The two must not collapse: with no location (the provider is disabled, or
/// had no data for the address) the permission policy's top node applies, but
/// when the lookup errored the request's place is unknown in a way that top
/// node must not paper over, so every permission resolves to the
/// requires-signal floor instead.
#[derive(Debug, Clone, Copy)]
pub enum GeoStatus<'a> {
    /// The provider resolved a location.
    Located(&'a GeoInfo),
    /// The provider resolved no location, so the policy's top node applies.
    NoLocation,
    /// The lookup errored, so the requires-signal floor applies.
    Failed,
}

impl<'a> GeoStatus<'a> {
    /// The resolved location, when one exists.
    #[must_use]
    pub fn info(self) -> Option<&'a GeoInfo> {
        match self {
            GeoStatus::Located(info) => Some(info),
            GeoStatus::NoLocation | GeoStatus::Failed => None,
        }
    }
}

impl<'a> From<Option<&'a GeoInfo>> for GeoStatus<'a> {
    fn from(geo: Option<&'a GeoInfo>) -> Self {
        match geo {
            Some(info) => GeoStatus::Located(info),
            None => GeoStatus::NoLocation,
        }
    }
}

/// The jurisdiction the consent gates apply to a request, from its resolved
/// location or, with none, from the permission policy's top node.
///
/// The consent gates (for example the server-side auction gate) detect a
/// jurisdiction from geolocation. With no location they would resolve
/// `Unknown` and fail closed even where the policy declares what to do, so the
/// same fallback the permission model applies is offered here: the top node's
/// `jurisdiction` stands in for the missing location. A failed lookup stays
/// unknown, so the consent gates fail closed alongside the requires-signal
/// floor.
#[must_use]
pub fn default_jurisdiction(geo: GeoStatus<'_>) -> Jurisdiction {
    match geo {
        GeoStatus::NoLocation => PermissionMaps::standard().default_jurisdiction(),
        GeoStatus::Located(_) | GeoStatus::Failed => Jurisdiction::Unknown,
    }
}

/// Assembles the permission state for a request: the place baseline from the
/// tree in `permissions.yaml`, augmented by the session's signals.
///
/// Permissions exist without a consent model. With no signal present the result
/// is simply the baseline for the request's country and region. When the geo
/// provider resolves no location, or a country/region that has no rule, the
/// policy's top node applies, and the top node's `group` is required so one is
/// always available. A failed lookup ([`GeoStatus::Failed`]) instead resolves
/// every permission to the requires-signal floor, so an outage is handled
/// protectively rather than as the policy's declared default.
#[must_use]
pub fn assemble_permissions(consent: &ConsentContext, geo: GeoStatus<'_>) -> PermissionState {
    let maps = PermissionMaps::standard();
    let signal = permission_signal(consent, maps.signals());
    match geo {
        GeoStatus::Failed => PermissionMaps::floor_with(signal),
        GeoStatus::Located(_) | GeoStatus::NoLocation => {
            let info = geo.info();
            maps.resolve_with(
                info.map(|info| info.country.as_str()),
                info.and_then(|info| info.region.as_deref()),
                signal,
            )
        }
    }
}

/// The acquisition rule for Edge Cookie storage in the request's resolved
/// jurisdiction, used to scope destructive withdrawal.
///
/// Resolves the same rules as [`assemble_permissions`] (the request's
/// country/region, the policy's top node when unmatched, and the
/// requires-signal floor when the lookup failed) and returns the rule for
/// [`Permission::StoreOnDevice`].
#[must_use]
pub fn storage_acquisition(geo: GeoStatus<'_>) -> Acquisition {
    match geo {
        GeoStatus::Failed => Acquisition::RequiresSignal,
        GeoStatus::Located(_) | GeoStatus::NoLocation => {
            let info = geo.info();
            PermissionMaps::standard()
                .rules_or_default(
                    info.map(|info| info.country.as_str()),
                    info.and_then(|info| info.region.as_deref()),
                )
                .map_or(Acquisition::RequiresSignal, |rules| {
                    rules.rule_for(Permission::StoreOnDevice)
                })
        }
    }
}

/// Maps a consent context to a [`ConsentSignal`] for each permission, applying
/// the [`SignalPolicy`] the permission model parsed from `permissions.yaml`.
///
/// This is the only place the EC subsystem reads consent signals. The policy,
/// not this function, decides which sources are authoritative, which TCF purpose
/// maps to which Data Use, and what a US-style opt-out revokes. This function
/// only decodes the request and applies that policy, so no signal-to-permission
/// policy lives in the code.
///
/// It considers every source the policy names: a TCF record (a standalone TC
/// string or the EU TCF section of a GPP string), and the US-style opt-out
/// signals (GPC, a GPP sale opt-out, or a US Privacy opt-out). Precedence is
/// most-restrictive-first and is fixed in code, not policy:
///
/// 1. A US-style opt-out revokes the Data Uses the policy lists, even when a
///    TCF record consents. An opt-out is an explicit user signal, so no other
///    signal may override it.
/// 2. A consent record that is present but cannot be decoded revokes
///    everything, so an unreadable expression of preference fails closed
///    instead of degrading to the no-signal baseline.
/// 3. When the policy marks TCF authoritative, a present TCF record then
///    decides the mapped Data Uses: granted where the record consents to the
///    mapped purpose, revoked where it does not, and neutral where no purpose
///    is mapped. The `authoritative` flag governs only whether TCF grants and
///    revokes apply, never whether an opt-out may be overridden.
///
/// Whether a `Revoke` changes anything is decided by the country/region map,
/// which drops a `granted` baseline and has nothing to drop where the
/// permission is `requires_signal` or `denied`.
fn permission_signal<'a>(
    consent: &'a ConsentContext,
    signals: &'a SignalPolicy,
) -> impl Fn(Permission) -> ConsentSignal + 'a {
    move |permission| {
        if opt_out_present(consent, signals.opt_out_sources())
            && signals.opt_out_revokes(permission)
        {
            return ConsentSignal::Revoke;
        }
        if consent.has_malformed_record() {
            return ConsentSignal::Revoke;
        }
        if signals.tcf_authoritative()
            && let Some(tcf) = crate::consent::effective_tcf(consent)
        {
            return match signals.tcf_purpose(permission) {
                Some(purpose) => {
                    if tcf.has_purpose_consent(usize::from(purpose)) {
                        ConsentSignal::Grant
                    } else {
                        ConsentSignal::Revoke
                    }
                }
                None => ConsentSignal::Neutral,
            };
        }
        ConsentSignal::Neutral
    }
}

/// Whether the request carries any of the `sources` a US-style opt-out is
/// declared to use. Decoding only, so the policy (not this function) decides
/// which sources count and what the opt-out revokes.
fn opt_out_present(consent: &ConsentContext, sources: &[OptOutSource]) -> bool {
    sources.iter().any(|source| match source {
        OptOutSource::Gpc => consent.gpc,
        OptOutSource::GppSaleOptOut => {
            consent.gpp.as_ref().and_then(|gpp| gpp.us_sale_opt_out) == Some(true)
        }
        OptOutSource::UsPrivacyOptOut => consent
            .us_privacy
            .as_ref()
            .is_some_and(|usp| usp.opt_out_sale == crate::consent::PrivacyFlag::Yes),
    })
}

/// Reports whether the request carries an explicit signal withdrawing Edge
/// Cookie storage, rather than merely lacking the permission.
///
/// This separates an affirmative withdrawal (which expires the browser cookie
/// and writes the authoritative identity-graph tombstone) from suppression,
/// where the permission is simply not set for this request (which strips EC
/// response headers but must not destroy an already-issued identifier, or a
/// returning user would be permanently withdrawn before they ever get to
/// consent).
///
/// Only a TCF record refusing storage (Purpose 1) withdraws, and only where
/// the jurisdiction's storage baseline is not `granted`: under a
/// `requires_signal` baseline the refusal is the visitor declining the very
/// signal storage depends on, while under a `granted` baseline storage never
/// depended on the record, so the refusal suppresses use without destroying
/// the identifier. US-style opt-outs (GPC, a GPP sale opt-out, or a US
/// Privacy opt-out) suppress the permissions the policy revokes but are
/// never destructive, and no signal at all is not a withdrawal.
#[must_use]
pub fn ec_storage_withdrawn(consent: &ConsentContext, storage_baseline: Acquisition) -> bool {
    if let Some(tcf) = crate::consent::effective_tcf(consent) {
        return !tcf.has_storage_consent() && !matches!(storage_baseline, Acquisition::Granted);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::TcfConsent;
    use crate::test_support::tests::create_test_settings;

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

    #[test]
    fn hmac_provider_is_blocked_without_a_storage_signal() {
        let settings = create_test_settings();
        // The test settings select the HMAC provider, which requires
        // necessary.operations.storage. The policy's top node resolves storage
        // as requires-signal, so with no signal the permission is not set and
        // the provider's requirement is not met.
        let provider = crate::ec::provider::build_provider(&settings.ec, None, None)
            .expect("should build the configured provider")
            .expect("should select the hmac provider");
        let state = assemble_permissions(&ConsentContext::default(), GeoStatus::NoLocation);
        assert!(
            !state.all_set(provider.required_permissions()),
            "the requires-signal default should not satisfy the HMAC provider without a signal"
        );
    }

    fn us_ca_geo() -> GeoInfo {
        GeoInfo {
            city: String::new(),
            country: "US".to_owned(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: Some("CA".to_owned()),
            asn: None,
        }
    }

    #[test]
    fn no_signal_uses_the_us_opt_out_baseline() {
        // US/CA maps to the us-opt-out group, where every purpose is granted
        // without a signal, so EC identity and bidstream EIDs are both permitted.
        let geo = us_ca_geo();
        let state = assemble_permissions(&ConsentContext::default(), GeoStatus::Located(&geo));
        assert!(
            state.is_set(Permission::StoreOnDevice)
                && state.is_set(Permission::SelectPersonalisedAds),
            "a US opt-out state should grant necessary.operations.storage and advertising_marketing.first_party.targeted"
        );
    }

    #[test]
    fn gpc_revokes_the_granted_baseline_in_a_us_opt_out_state() {
        // A US-style opt-out drops a granted baseline with no jurisdiction match:
        // the map granted these purposes, and GPC revokes them.
        let consent = ConsentContext {
            gpc: true,
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "GPC should revoke the granted necessary.operations.storage and advertising_marketing.first_party.targeted baseline"
        );
    }

    // ------------------------------------------------------------------
    // Opt-out precedence pinning tests. These reinstate the behavior the
    // consent module enforced before the permission model: an explicit
    // opt-out signal suppresses storage and sharing even when a TCF record
    // consents. The permission model must never let a CMP-written record
    // override the visitor's own opt-out.
    // ------------------------------------------------------------------

    #[test]
    fn gpc_suppresses_storage_even_with_a_consenting_tcf_record() {
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&[1, 4])),
            gpc: true,
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "GPC should suppress storage and sharing even when the TCF record consents"
        );
    }

    #[test]
    fn us_privacy_opt_out_suppresses_storage_even_with_a_consenting_tcf_record() {
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&[1, 4])),
            us_privacy: Some(crate::consent::types::UsPrivacy {
                version: 1,
                notice_given: crate::consent::PrivacyFlag::Yes,
                opt_out_sale: crate::consent::PrivacyFlag::Yes,
                lspa_covered: crate::consent::PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "a US Privacy opt-out should suppress storage and sharing even when the TCF record consents"
        );
    }

    #[test]
    fn gpp_sale_opt_out_suppresses_storage_even_with_a_consenting_tcf_record() {
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&[1, 4])),
            gpp: Some(crate::consent::types::GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(true),
            }),
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "a GPP sale opt-out should suppress storage and sharing even when the TCF record consents"
        );
    }

    #[test]
    fn gpc_suppresses_storage_even_when_us_privacy_reports_no_opt_out() {
        let consent = ConsentContext {
            gpc: true,
            us_privacy: Some(crate::consent::types::UsPrivacy {
                version: 1,
                notice_given: crate::consent::PrivacyFlag::Yes,
                opt_out_sale: crate::consent::PrivacyFlag::No,
                lspa_covered: crate::consent::PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "any one opt-out source should suppress, whatever the others say"
        );
    }

    // ------------------------------------------------------------------
    // Withdrawal scoping: only a TCF storage refusal withdraws, and only
    // where the baseline did not grant storage outright. Opt-outs suppress
    // use but never destroy an already-issued identifier.
    // ------------------------------------------------------------------

    #[test]
    fn tcf_storage_refusal_withdraws_under_a_requires_signal_baseline() {
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&[4])),
            ..ConsentContext::default()
        };
        assert!(
            ec_storage_withdrawn(&consent, Acquisition::RequiresSignal),
            "refusing the signal storage depends on should withdraw"
        );
    }

    #[test]
    fn tcf_storage_refusal_does_not_withdraw_under_a_granted_baseline() {
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&[4])),
            ..ConsentContext::default()
        };
        assert!(
            !ec_storage_withdrawn(&consent, Acquisition::Granted),
            "storage never depended on the record here, so refusal suppresses without destroying"
        );
    }

    #[test]
    fn tcf_storage_consent_is_not_a_withdrawal() {
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&[1])),
            ..ConsentContext::default()
        };
        assert!(
            !ec_storage_withdrawn(&consent, Acquisition::RequiresSignal),
            "a consenting record is not a withdrawal"
        );
    }

    #[test]
    fn gpc_alone_never_withdraws() {
        let consent = ConsentContext {
            gpc: true,
            ..ConsentContext::default()
        };
        assert!(
            !ec_storage_withdrawn(&consent, Acquisition::Granted)
                && !ec_storage_withdrawn(&consent, Acquisition::RequiresSignal),
            "GPC suppresses use for the request but never destroys the identifier"
        );
    }

    #[test]
    fn us_style_opt_outs_never_withdraw() {
        let consent = ConsentContext {
            us_privacy: Some(crate::consent::types::UsPrivacy {
                version: 1,
                notice_given: crate::consent::PrivacyFlag::Yes,
                opt_out_sale: crate::consent::PrivacyFlag::Yes,
                lspa_covered: crate::consent::PrivacyFlag::NotApplicable,
            }),
            gpp: Some(crate::consent::types::GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(true),
            }),
            ..ConsentContext::default()
        };
        assert!(
            !ec_storage_withdrawn(&consent, Acquisition::RequiresSignal),
            "sale opt-outs suppress use but never destroy the identifier"
        );
    }

    #[test]
    fn no_signal_is_not_a_withdrawal() {
        assert!(
            !ec_storage_withdrawn(&ConsentContext::default(), Acquisition::RequiresSignal),
            "absence of a signal must never destroy an identifier"
        );
    }

    #[test]
    fn a_malformed_record_is_not_a_withdrawal() {
        let consent = ConsentContext {
            raw_tc_string: Some("not-a-tc-string".to_owned()),
            ..ConsentContext::default()
        };
        assert!(
            !ec_storage_withdrawn(&consent, Acquisition::RequiresSignal),
            "an unreadable record fails closed (suppression), not destructively"
        );
    }

    // ------------------------------------------------------------------
    // Malformed-but-present records block baseline grants (fail closed)
    // instead of degrading to the no-signal baseline.
    // ------------------------------------------------------------------

    #[test]
    fn a_malformed_tcf_record_blocks_baseline_grants() {
        let consent = ConsentContext {
            raw_tc_string: Some("not-a-tc-string".to_owned()),
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "an unreadable record should block the granted baseline, not vanish"
        );
    }

    #[test]
    fn a_malformed_gpp_or_us_privacy_record_is_detected() {
        let gpp = ConsentContext {
            raw_gpp_string: Some("not-a-gpp-string".to_owned()),
            ..ConsentContext::default()
        };
        let usp = ConsentContext {
            raw_us_privacy: Some("bogus".to_owned()),
            ..ConsentContext::default()
        };
        assert!(
            gpp.has_malformed_record() && usp.has_malformed_record(),
            "each undecodable record form should be detected"
        );
    }

    #[test]
    fn an_expired_tcf_record_is_not_treated_as_malformed() {
        let consent = ConsentContext {
            raw_tc_string: Some("CPc-old-string".to_owned()),
            expired: true,
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));
        assert!(
            state.is_set(Permission::StoreOnDevice),
            "expiry is its own explicit state, deliberately distinct from malformed"
        );
    }

    // ------------------------------------------------------------------
    // Geo status: a failed lookup resolves at the requires-signal floor and
    // never consults the tree, while no location resolves at the policy's top
    // node.
    // ------------------------------------------------------------------

    #[test]
    fn a_failed_geo_lookup_resolves_to_the_requires_signal_floor() {
        // The same request, located in a US opt-out state, grants storage
        // without a signal. A failed lookup must not reach that rule, or any
        // other, so nothing is set without a signal.
        let geo = us_ca_geo();
        assert!(
            assemble_permissions(&ConsentContext::default(), GeoStatus::Located(&geo))
                .is_set(Permission::StoreOnDevice),
            "the located baseline must grant storage, or this test proves nothing"
        );
        let state = assemble_permissions(&ConsentContext::default(), GeoStatus::Failed);
        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "a lookup failure must not fall back to any node of the policy tree"
        );
        assert_eq!(
            storage_acquisition(GeoStatus::Failed),
            Acquisition::RequiresSignal,
            "the storage baseline follows the same floor on failure"
        );
    }

    #[test]
    fn no_location_falls_back_to_the_policy_top_node() {
        // The shipped policy's top node is the gdpr-eu group, which requires a
        // signal for storage, so an unplaced visitor gets no identifier until
        // one arrives.
        let state = assemble_permissions(&ConsentContext::default(), GeoStatus::NoLocation);
        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "the top node requires a signal for storage"
        );
        assert_eq!(
            storage_acquisition(GeoStatus::NoLocation),
            Acquisition::RequiresSignal,
            "the storage baseline follows the top node on no location"
        );
    }

    #[test]
    fn no_location_takes_the_jurisdiction_from_the_policy_top_node() {
        assert_eq!(
            default_jurisdiction(GeoStatus::NoLocation),
            Jurisdiction::Gdpr,
            "no location should resolve the top node's declared jurisdiction"
        );
        assert_eq!(
            default_jurisdiction(GeoStatus::Failed),
            Jurisdiction::Unknown,
            "a failed lookup must not adopt the policy's declared jurisdiction"
        );
    }

    #[test]
    fn tcf_resolves_every_mapped_purpose_not_just_storage_and_ads() {
        // A TCF record now grants or revokes every one of the eleven mapped
        // purposes, not only Purpose 1 and Purpose 4. Consent to all purposes
        // except Purpose 7 (measure ad performance), in a US opt-out state where
        // the baseline granted them all, so a revoke is observable as a drop.
        let consented: Vec<usize> = (1..=11).filter(|&p| p != 7).collect();
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&consented)),
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&consent, GeoStatus::Located(&geo));

        // Purpose 2 is now resolved (it was neutral before), so consent sets it.
        assert!(
            state.is_set(Permission::SelectBasicAds),
            "Purpose 2 consent should set advertising_marketing.first_party.contextual"
        );
        // Purpose 7 was refused, so the granted baseline is revoked.
        assert!(
            !state.is_set(Permission::MeasureAdPerformance),
            "Purpose 7 refusal should revoke analytics.ad_reporting.measure_ad_performance"
        );
        // The originally wired purposes still behave.
        assert!(
            state.is_set(Permission::StoreOnDevice)
                && state.is_set(Permission::SelectPersonalisedAds),
            "Purposes 1 and 4 remain resolved from the TCF record"
        );
    }
}
