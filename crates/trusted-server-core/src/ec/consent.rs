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
use crate::permissions::{ConsentSignal, Permission, PermissionMaps, PermissionState};
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
        permission_signal(consent),
    )
}

/// Maps a consent context to a [`ConsentSignal`] for each permission.
///
/// This is the only place the EC subsystem reads consent signals, and it is
/// jurisdiction-free. A TCF record is authoritative wherever it is present (a CMP
/// under GDPR emits it): it grants or refuses each purpose it carries directly,
/// and a US-style opt-out does not override it. With no TCF record, a US-style
/// opt-out (GPC, GPP sale opt-out, or US Privacy opt-out) revokes the permission,
/// and anything else is neutral so the country/region baseline stands.
///
/// Whether a `Revoke` changes anything is decided by the map: it drops a
/// `granted` baseline and has nothing to drop where the permission is
/// `requires_signal`. Only the two permissions that Edge Cookie identity and
/// bidstream EIDs depend on are resolved against a signal:
/// [`Permission::StoreOnDevice`] (TCF Purpose 1) and
/// [`Permission::SelectPersonalisedAds`] (TCF Purpose 4); every other permission
/// is neutral so its baseline stands.
fn permission_signal(consent: &ConsentContext) -> impl Fn(Permission) -> ConsentSignal + '_ {
    move |permission| {
        if let Some(tcf) = crate::consent::effective_tcf(consent) {
            let consented = match permission {
                Permission::StoreOnDevice => tcf.has_storage_consent(),
                Permission::SelectPersonalisedAds => tcf.has_personalized_ads_consent(),
                _ => return ConsentSignal::Neutral,
            };
            return if consented {
                ConsentSignal::Grant
            } else {
                ConsentSignal::Revoke
            };
        }
        if crate::consent::has_storage_optout_signal(consent) {
            ConsentSignal::Revoke
        } else {
            ConsentSignal::Neutral
        }
    }
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
    use crate::test_support::tests::create_test_settings;

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
        // store-on-device. With no signal and no configured default country,
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
            "a US opt-out state should grant store-on-device and select-personalised-ads"
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
            "GPC should revoke the granted store-on-device and select-personalised-ads baseline"
        );
    }
}
