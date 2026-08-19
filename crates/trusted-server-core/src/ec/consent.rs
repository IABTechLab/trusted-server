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
use crate::permissions::{
    Acquisition, ConsentSignal, OptOutSource, Permission, PermissionMaps, PermissionState,
    SignalPolicy,
};
use crate::platform::GeoInfo;
use crate::settings::Settings;

/// The outcome of the geo lookup for a request, separating "no location
/// resolved" from "the lookup failed".
///
/// The two must not collapse: with no location (the provider is disabled, or
/// had no data for the address) the deployer's `[geo] default_country`
/// baseline applies, but when the lookup errored the request's location is
/// unknown in a way the deployer default must not paper over, so every
/// permission resolves to the requires-signal floor instead.
#[derive(Debug, Clone, Copy)]
pub enum GeoStatus<'a> {
    /// The provider resolved a location.
    Located(&'a GeoInfo),
    /// The provider resolved no location, so the configured default applies.
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

/// The configured `[geo] default_country` as a [`GeoInfo`], for jurisdiction
/// detection when the geo provider resolves no location.
///
/// The consent gates (for example the server-side auction gate) detect a
/// jurisdiction from geolocation. With no location they would resolve
/// `Unknown` and fail closed even where the deployer has declared a default
/// jurisdiction, so the same fallback the permission model applies is
/// offered here: the default country and region stand in for the missing
/// location. Only the country and region carry meaning; every other field is
/// empty. A failed lookup must not use this (see [`GeoStatus::Failed`]), so
/// the caller decides when to apply it.
#[must_use]
pub fn default_location_geo(settings: &Settings) -> Option<GeoInfo> {
    let (country, region) = default_location(settings);
    let country = country?;
    Some(GeoInfo {
        city: String::new(),
        country: country.to_owned(),
        continent: String::new(),
        latitude: 0.0,
        longitude: 0.0,
        metro_code: 0,
        region: region.map(str::to_owned),
        asn: None,
    })
}

/// Splits the configured `[geo] default_country` into its country and region
/// parts (`US/CA` names a country and region, `US` a bare country).
fn default_location(settings: &Settings) -> (Option<&str>, Option<&str>) {
    match settings.geo.default_country.as_deref() {
        Some(spec) => match spec.split_once('/') {
            Some((country, region)) => (Some(country), Some(region)),
            None => (Some(spec), None),
        },
        None => (None, None),
    }
}

/// Assembles the permission state for a request: the country/region baseline
/// from the default maps in `permissions.yaml`, augmented by the session's
/// signals.
///
/// Permissions exist without a consent model. With no signal present the result
/// is simply the baseline for the request's country and region. When the geo
/// provider resolves no location, or a country/region that has no rule, the
/// deployer's configured `[geo] default_country` applies. A default is required,
/// so it is always available. A failed lookup ([`GeoStatus::Failed`]) instead
/// resolves every permission to the requires-signal floor, so an outage is
/// handled protectively rather than as the deployer's default jurisdiction.
#[must_use]
pub fn assemble_permissions(
    settings: &Settings,
    consent: &ConsentContext,
    geo: GeoStatus<'_>,
) -> PermissionState {
    let maps = PermissionMaps::standard();
    let (default_country, default_region) = match geo {
        GeoStatus::Failed => (None, None),
        GeoStatus::Located(_) | GeoStatus::NoLocation => default_location(settings),
    };
    let info = geo.info();
    maps.resolve_with(
        info.map(|info| info.country.as_str()),
        info.and_then(|info| info.region.as_deref()),
        default_country,
        default_region,
        permission_signal(consent, maps.signals()),
    )
}

/// The acquisition rule for Edge Cookie storage in the request's resolved
/// jurisdiction, used to scope destructive withdrawal.
///
/// Resolves the same rules as [`assemble_permissions`] (the request's
/// country/region, the configured default when unmatched, and the
/// requires-signal floor when the lookup failed or nothing resolves) and
/// returns the rule for [`Permission::StoreOnDevice`].
#[must_use]
pub fn storage_acquisition(settings: &Settings, geo: GeoStatus<'_>) -> Acquisition {
    let maps = PermissionMaps::standard();
    let (default_country, default_region) = match geo {
        GeoStatus::Failed => (None, None),
        GeoStatus::Located(_) | GeoStatus::NoLocation => default_location(settings),
    };
    let info = geo.info();
    maps.rules_or_default(
        info.map(|info| info.country.as_str()),
        info.and_then(|info| info.region.as_deref()),
        default_country,
        default_region,
    )
    .map_or(Acquisition::RequiresSignal, |rules| {
        rules.rule_for(Permission::StoreOnDevice)
    })
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
        // The test settings select the HMAC provider, which requires
        // necessary.operations.storage. The configured default country (FR)
        // resolves storage as requires-signal, so with no signal the
        // permission is not set and the provider's requirement is not met.
        let settings = create_test_settings();
        let provider = crate::ec::provider::build_provider(&settings.ec, None, None)
            .expect("should build the configured provider")
            .expect("should select the hmac provider");
        let state =
            assemble_permissions(&settings, &ConsentContext::default(), GeoStatus::NoLocation);
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
        let settings = create_test_settings();
        let geo = us_ca_geo();
        let state = assemble_permissions(
            &settings,
            &ConsentContext::default(),
            GeoStatus::Located(&geo),
        );
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
        let settings = create_test_settings();
        let consent = ConsentContext {
            gpc: true,
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));
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
        let settings = create_test_settings();
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&[1, 4])),
            gpc: true,
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "GPC should suppress storage and sharing even when the TCF record consents"
        );
    }

    #[test]
    fn us_privacy_opt_out_suppresses_storage_even_with_a_consenting_tcf_record() {
        let settings = create_test_settings();
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
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "a US Privacy opt-out should suppress storage and sharing even when the TCF record consents"
        );
    }

    #[test]
    fn gpp_sale_opt_out_suppresses_storage_even_with_a_consenting_tcf_record() {
        let settings = create_test_settings();
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
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "a GPP sale opt-out should suppress storage and sharing even when the TCF record consents"
        );
    }

    #[test]
    fn gpc_suppresses_storage_even_when_us_privacy_reports_no_opt_out() {
        let settings = create_test_settings();
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
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));
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
        let settings = create_test_settings();
        let consent = ConsentContext {
            raw_tc_string: Some("not-a-tc-string".to_owned()),
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));
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
        let settings = create_test_settings();
        let consent = ConsentContext {
            raw_tc_string: Some("CPc-old-string".to_owned()),
            expired: true,
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));
        assert!(
            state.is_set(Permission::StoreOnDevice),
            "expiry is its own explicit state, deliberately distinct from malformed"
        );
    }

    // ------------------------------------------------------------------
    // Geo status: a failed lookup resolves at the requires-signal floor,
    // while no location falls back to the configured default.
    // ------------------------------------------------------------------

    #[test]
    fn a_failed_geo_lookup_resolves_to_the_requires_signal_floor() {
        let mut settings = create_test_settings();
        settings.geo.default_country = Some("US/CA".to_owned());
        let state = assemble_permissions(&settings, &ConsentContext::default(), GeoStatus::Failed);
        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "a lookup failure must not fall back to the deployer default baseline"
        );
        assert_eq!(
            storage_acquisition(&settings, GeoStatus::Failed),
            Acquisition::RequiresSignal,
            "the storage baseline follows the same floor on failure"
        );
    }

    #[test]
    fn no_location_falls_back_to_the_configured_default() {
        let mut settings = create_test_settings();
        settings.geo.default_country = Some("US/CA".to_owned());
        let state =
            assemble_permissions(&settings, &ConsentContext::default(), GeoStatus::NoLocation);
        assert!(
            state.is_set(Permission::StoreOnDevice),
            "no location should resolve at the configured default baseline"
        );
        assert_eq!(
            storage_acquisition(&settings, GeoStatus::NoLocation),
            Acquisition::Granted,
            "the storage baseline follows the default on no location"
        );
    }

    #[test]
    fn tcf_resolves_every_mapped_purpose_not_just_storage_and_ads() {
        // A TCF record now grants or revokes every one of the eleven mapped
        // purposes, not only Purpose 1 and Purpose 4. Consent to all purposes
        // except Purpose 7 (measure ad performance), in a US opt-out state where
        // the baseline granted them all, so a revoke is observable as a drop.
        let settings = create_test_settings();
        let consented: Vec<usize> = (1..=11).filter(|&p| p != 7).collect();
        let consent = ConsentContext {
            tcf: Some(tcf_with_purposes(&consented)),
            ..ConsentContext::default()
        };
        let geo = us_ca_geo();
        let state = assemble_permissions(&settings, &consent, GeoStatus::Located(&geo));

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
