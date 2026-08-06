//! EC-specific permission gating, resolved through the permission model.
//!
//! The Edge Cookie provider advertises the [`Permission`]s its data use
//! requires. [`ec_permission_granted`] resolves which permissions are set for a
//! request, from its session signals and the country it maps to, and reports
//! whether every required permission is set. The EC permission decision lives
//! here, in the EC subsystem, and nowhere else, so callers route every EC
//! permission check through this module rather than re-deriving one.

use std::sync::Arc;

use error_stack::Report;

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::evidence::HostSignals;
use crate::permissions::{
    ConsentSignal, OptOutSource, Permission, PermissionMaps, PermissionState, SignalPolicy,
};
use crate::platform::GeoInfo;
use crate::settings::Settings;

use super::provider::build_provider;

/// Whether the configured Edge Cookie provider's required permissions are set
/// for this request.
///
/// The permission state is assembled by [`assemble_permissions`] (the
/// country/region baseline augmented by the session's signals), and this gate
/// only asks whether every permission the provider requires is set. The gate
/// never inspects consent itself: that lives in the signal mapping, so the
/// decision depends solely on the resolved permissions. A request with no Edge
/// Cookie provider configured has nothing to gate, so this returns `true`. The
/// generation path still skips when no provider is built, so no Edge Cookie is
/// written in that case.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when the selected provider requires a service
/// the host does not supply (for example the host-signal provider on a host with
/// no fingerprints), so a misconfigured deployment fails loudly rather than
/// silently treating the permission as ungranted.
pub fn ec_permission_granted(
    settings: &Settings,
    consent: &ConsentContext,
    geo: Option<&GeoInfo>,
    host_signals: Option<Arc<dyn HostSignals>>,
) -> Result<bool, Report<TrustedServerError>> {
    // The provider declares the permissions its data use requires. Build it to
    // read that declaration; with no provider configured there is nothing to
    // gate, so the check passes. Reading `required_permissions()` needs no request
    // data, so no request info is threaded here.
    let Some(provider) = build_provider(&settings.ec, host_signals)? else {
        return Ok(true);
    };
    Ok(assemble_permissions(settings, consent, geo).all_set(provider.required_permissions()))
}

/// Assembles the permission state for a request: the country/region baseline
/// from the default maps in `permissions.yaml`, augmented by the session's
/// signals.
///
/// Permissions exist without a consent model. With no signal present the result
/// is simply the baseline for the request's country and region. When the geo
/// provider returns no country, or a country/region that has no rule, the
/// deployer's configured `[geo] default_country` applies. A default is required,
/// so it is always available.
#[must_use]
pub fn assemble_permissions(
    settings: &Settings,
    consent: &ConsentContext,
    geo: Option<&GeoInfo>,
) -> PermissionState {
    let maps = PermissionMaps::standard();
    let (default_country, default_region) = match settings.geo.default_country.as_deref() {
        Some(spec) => match spec.split_once('/') {
            Some((country, region)) => (Some(country), Some(region)),
            None => (Some(spec), None),
        },
        None => (None, None),
    };
    maps.resolve_with(
        geo.map(|info| info.country.as_str()),
        geo.and_then(|info| info.region.as_deref()),
        default_country,
        default_region,
        permission_signal(consent, maps.signals()),
    )
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
/// signals (GPC, a GPP sale opt-out, or a US Privacy opt-out). When the policy
/// marks TCF authoritative, a present TCF record wins and a US-style opt-out does
/// not override it. Each permission is granted when the TCF record consents to
/// the purpose the policy maps to it, and revoked when it does not. A Data Use
/// the policy maps to no purpose stays neutral, so its baseline stands.
///
/// With no authoritative TCF record, a US-style opt-out revokes the Data Uses the
/// policy lists and anything else is neutral. Whether a `Revoke` changes anything
/// is decided by the country/region map, which drops a `granted` baseline and has
/// nothing to drop where the permission is `requires_signal` or `denied`.
fn permission_signal<'a>(
    consent: &'a ConsentContext,
    signals: &'a SignalPolicy,
) -> impl Fn(Permission) -> ConsentSignal + 'a {
    move |permission| {
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
        if opt_out_present(consent, signals.opt_out_sources())
            && signals.opt_out_revokes(permission)
        {
            ConsentSignal::Revoke
        } else {
            ConsentSignal::Neutral
        }
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
/// and writes the authoritative identity-graph tombstone) from a pre-consent or
/// fail-closed state where the permission is simply not set (which strips EC
/// response headers but must not destroy an already-issued identifier, or a
/// returning user would be permanently withdrawn before they ever get to
/// consent). A TCF record is authoritative where present: it is a withdrawal
/// when it refuses storage (Purpose 1). With no TCF record, a US-style storage
/// opt-out (GPC, GPP sale opt-out, or US Privacy) is a withdrawal. No signal at
/// all is not a withdrawal.
#[must_use]
pub fn ec_storage_withdrawn(consent: &ConsentContext) -> bool {
    if let Some(tcf) = crate::consent::effective_tcf(consent) {
        return !tcf.has_storage_consent();
    }
    crate::consent::has_storage_optout_signal(consent)
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
    fn no_edge_cookie_provider_is_vacuously_granted() {
        // With no provider selected there are no required permissions to
        // satisfy, so the check passes. The generation path still skips when no
        // provider is built, so nothing is written to the device.
        let mut settings = create_test_settings();
        settings.ec.provider = None;
        assert!(
            ec_permission_granted(&settings, &ConsentContext::default(), None, None)
                .expect("the gate should evaluate without error"),
            "no provider means nothing to gate, so the check passes"
        );
    }

    #[test]
    fn hmac_provider_is_blocked_without_storage_consent() {
        // The test settings select the HMAC provider, which requires
        // necessary.operations.storage. With no signal and no configured default country,
        // that permission sits at the requires-signal floor, so it is not set and
        // no Edge Cookie is written.
        let settings = create_test_settings();
        assert!(
            !ec_permission_granted(&settings, &ConsentContext::default(), None, None)
                .expect("the gate should evaluate without error"),
            "the floor should not run the HMAC provider without the permission set"
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
        let state = assemble_permissions(&settings, &ConsentContext::default(), Some(&us_ca_geo()));
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
        let state = assemble_permissions(&settings, &consent, Some(&us_ca_geo()));
        assert!(
            !state.is_set(Permission::StoreOnDevice)
                && !state.is_set(Permission::SelectPersonalisedAds),
            "GPC should revoke the granted necessary.operations.storage and advertising_marketing.first_party.targeted baseline"
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
        let state = assemble_permissions(&settings, &consent, Some(&us_ca_geo()));

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
