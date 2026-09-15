//! EC-specific permission gating, resolved through the permission model.
//!
//! The Edge Cookie provider advertises the [`Permission`]s its data use
//! requires. [`assemble_permissions`] resolves which permissions are set for a
//! request, from the country it maps to and what the signal providers say,
//! and the context construction gates the provider on that state. The EC
//! permission decision lives here, in the EC subsystem, and nowhere else, so
//! callers route every EC permission check through this module rather than
//! re-deriving one.
//!
//! No scheme is decoded here. Which signals count, and what each says about
//! a permission, is answered by the [`PermissionSignalProvider`]s the adapter
//! hands in, every one of which is a crate outside core. This module asks
//! them in order and applies the country and region rules to what they
//! settle on.

use std::sync::Arc;

use crate::consent::ConsentContext;
use crate::consent::jurisdiction::Jurisdiction;
use crate::evidence::RequestInfo;
use crate::permission_signal::{self, PermissionSignalProvider};
use crate::permissions::{
    Acquisition, ConsentSignal, Permission, PermissionMaps, PermissionState, SignalPolicy,
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
/// tree in `permissions.yaml`, amended by what the signal providers say.
///
/// Permissions exist without a consent model. With no provider having an
/// opinion the result is simply the baseline for the request's country and
/// region. When the geo provider resolves no location, or a country/region
/// that has no rule, the policy's top node applies, and the top node's `group`
/// is required so one is always available. A failed lookup
/// ([`GeoStatus::Failed`]) instead resolves every permission to the
/// requires-signal floor, so an outage is handled protectively rather than as
/// the policy's declared default.
///
/// `providers` are the signal providers the adapter selected, in the order
/// they run. A scheme missing from the list does not run, so a publisher
/// removes one by leaving it out rather than by configuring it off, and an
/// empty slice runs none of them, which leaves every permission at its
/// country and region baseline. The same providers answer whether storage
/// was explicitly withdrawn, recorded on the state and read through
/// [`PermissionState::storage_withdrawn`], and declare the terms documents the
/// request's data is available under, read through
/// [`PermissionState::tdls`].
#[must_use]
pub fn assemble_permissions(
    consent: &ConsentContext,
    evidence: &dyn RequestInfo,
    geo: GeoStatus<'_>,
    providers: &[Arc<dyn PermissionSignalProvider>],
) -> PermissionState {
    let maps = PermissionMaps::standard();
    let signal = permission_signal(consent, evidence, maps.signals(), providers);
    let state = match geo {
        GeoStatus::Failed => PermissionMaps::floor_with(signal),
        GeoStatus::Located(_) | GeoStatus::NoLocation => {
            let info = geo.info();
            maps.resolve_with(
                info.map(|info| info.country.as_str()),
                info.and_then(|info| info.region.as_deref()),
                signal,
            )
        }
    };
    let withdrawn = permission_signal::withdrawn(
        providers,
        Permission::StoreOnDevice,
        consent,
        evidence,
        maps.signals(),
        storage_acquisition(geo),
    );
    state
        .with_storage_withdrawn(withdrawn)
        .with_tdls(permission_signal::tdls(providers, consent, evidence))
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

/// Maps a request to a [`ConsentSignal`] for each permission, applying the
/// [`SignalPolicy`] the permission model parsed from `permissions.yaml`.
///
/// This is the only place the EC subsystem consults the signal providers. The
/// policy, not this function, decides which schemes are authoritative and what
/// a US-style opt-out revokes, and each provider decides what its own scheme
/// says. This function only hands each provider the request, in order, so no
/// signal-to-permission rule lives in core.
///
/// Each provider amends what the ones before it settled on. The order is the
/// policy, and [`combine`] documents why.
///
/// Whether an amendment changes anything is then decided by the country/region
/// map, which drops a `granted` baseline on a `Revoke` and has nothing to drop
/// where the permission is `requires_signal` or `denied`.
///
/// [`combine`]: crate::permission_signal::combine
fn permission_signal<'a>(
    consent: &'a ConsentContext,
    evidence: &'a dyn RequestInfo,
    signals: &'a SignalPolicy,
    providers: &'a [Arc<dyn PermissionSignalProvider>],
) -> impl Fn(Permission, Acquisition) -> ConsentSignal + 'a {
    move |permission, baseline| {
        // A record that arrived and could not be read fails closed, ahead of
        // every configured provider and regardless of which are configured.
        //
        // This is not a signaling scheme and is deliberately not in the
        // configured list. A publisher chooses which signals to act on, but
        // not what happens when one of those signals arrives unreadable. An
        // unreadable record is a preference someone expressed that cannot be
        // read, which is different from no record at all, so it must not
        // degrade to the no-signal baseline.
        //
        // It overrides rather than taking a place in the order because the
        // ordered rule would otherwise let a readable record from one scheme
        // overwrite the refusal caused by an unreadable one from another.
        if consent.has_malformed_record() {
            return ConsentSignal::Revoke;
        }
        permission_signal::combine(providers, permission, consent, evidence, signals, baseline)
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::*;
    use crate::evidence::OwnedRequestInfo;
    use crate::permission_signal::SignalInput;
    use crate::test_support::tests::create_test_settings;

    /// A provider that grants every permission, standing in for a scheme that
    /// answered a prompt, so the assembly rules can be exercised without any
    /// real scheme in core.
    struct Granting;

    impl PermissionSignalProvider for Granting {
        fn id(&self) -> &'static str {
            "granting"
        }

        fn signal(&self, _permission: Permission, _input: &SignalInput<'_>) -> ConsentSignal {
            ConsentSignal::Grant
        }
    }

    /// A provider that declares a terms document, standing in for a terms
    /// scheme such as Model Terms for Marketing, which is the next provider
    /// and is not one of the four that ship here.
    struct DeclaringTerms;

    impl PermissionSignalProvider for DeclaringTerms {
        fn id(&self) -> &'static str {
            "declaring-terms"
        }

        fn signal(&self, _permission: Permission, _input: &SignalInput<'_>) -> ConsentSignal {
            ConsentSignal::Neutral
        }

        fn tdls(
            &self,
            _consent: &ConsentContext,
            _evidence: &dyn crate::evidence::RequestInfo,
        ) -> Vec<crate::tdl::Tdl> {
            vec![
                crate::tdl::Tdl::new("https://terms.example.com/marketing/2.txt")
                    .expect("should accept the test locator"),
            ]
        }
    }

    fn no_evidence() -> OwnedRequestInfo {
        OwnedRequestInfo::new(String::new(), HeaderMap::new())
    }

    fn granting() -> Vec<Arc<dyn PermissionSignalProvider>> {
        vec![Arc::new(Granting)]
    }

    fn assembled(
        consent: &ConsentContext,
        geo: GeoStatus<'_>,
        providers: &[Arc<dyn PermissionSignalProvider>],
    ) -> PermissionState {
        assemble_permissions(consent, &no_evidence(), geo, providers)
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
    fn a_provider_declaring_terms_reaches_the_assembled_state() {
        let consent = ConsentContext::default();
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![Arc::new(DeclaringTerms)];
        let state =
            assemble_permissions(&consent, &no_evidence(), GeoStatus::NoLocation, &providers);
        let addresses: Vec<&str> = state.tdls().iter().map(crate::tdl::Tdl::as_str).collect();
        assert_eq!(
            addresses,
            vec!["https://terms.example.com/marketing/2.txt"],
            "should carry the terms the provider declared through to whatever reads the state"
        );
    }

    #[test]
    fn a_state_assembled_from_the_shipped_kind_of_provider_declares_no_terms() {
        let consent = ConsentContext::default();
        let state =
            assemble_permissions(&consent, &no_evidence(), GeoStatus::NoLocation, &granting());
        assert!(
            state.tdls().is_empty(),
            "should declare nothing, because a scheme carrying no terms says nothing about them"
        );
    }

    #[test]
    fn hmac_provider_is_blocked_without_a_storage_signal() {
        let settings = create_test_settings();
        // The test settings select the HMAC provider, which requires
        // necessary.operations.storage. The policy's top node resolves storage
        // as requires-signal, so with no provider granting it the permission is
        // not set and the provider's requirement is not met.
        let provider = crate::ec::provider::build_provider(&settings.ec, None, None)
            .expect("should build the configured provider")
            .expect("should select the hmac provider");
        let state = assembled(&ConsentContext::default(), GeoStatus::NoLocation, &[]);
        assert!(
            !state.all_set(provider.required_permissions()),
            "the requires-signal default should not satisfy the HMAC provider without a signal"
        );
    }

    #[test]
    fn no_signal_uses_the_us_opt_out_baseline() {
        // US/CA maps to the us-opt-out group, where every purpose is granted
        // without a signal, so EC identity and bidstream EIDs are both permitted.
        let geo = us_ca_geo();
        let state = assembled(&ConsentContext::default(), GeoStatus::Located(&geo), &[]);
        assert!(
            state.is_set(Permission::StoreOnDevice)
                && state.is_set(Permission::SelectPersonalisedAds),
            "a US opt-out state should grant necessary.operations.storage and advertising_marketing.first_party.targeted"
        );
    }

    #[test]
    fn a_grant_sets_a_requires_signal_permission() {
        // The top node requires a signal for storage, and a provider granting
        // it is what a signal arriving looks like from here.
        let state = assembled(
            &ConsentContext::default(),
            GeoStatus::NoLocation,
            &granting(),
        );
        assert!(
            state.is_set(Permission::StoreOnDevice),
            "a provider granting storage should set it under a requires-signal baseline"
        );
        assert!(
            !state.storage_withdrawn(),
            "and a grant is the opposite of a withdrawal"
        );
    }

    // ------------------------------------------------------------------
    // An unreadable record. Not a scheme a publisher lists, so it applies
    // whatever they configured, and it is not subject to the ordering.
    // ------------------------------------------------------------------

    #[test]
    fn an_unreadable_record_revokes_even_with_no_providers_configured() {
        let consent = ConsentContext {
            raw_tc_string: Some("this is not a TC string".to_owned()),
            ..ConsentContext::default()
        };
        assert!(
            consent.has_malformed_record(),
            "the fixture has to actually be unreadable for this to test anything"
        );
        let geo = us_ca_geo();
        let state = assembled(&consent, GeoStatus::Located(&geo), &[]);
        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "a preference someone expressed that cannot be read must not degrade to the \
             no-signal baseline, and a publisher cannot configure that away"
        );
    }

    #[test]
    fn a_readable_record_does_not_overwrite_an_unreadable_one() {
        // The regression the override exists to prevent: under the ordered
        // rule alone, a provider that grants would be asked after the
        // unreadable GPP string and would overwrite the refusal it caused.
        let consent = ConsentContext {
            raw_gpp_string: Some("this is not a GPP string".to_owned()),
            ..ConsentContext::default()
        };
        assert!(
            consent.has_malformed_record(),
            "the fixture has to actually be unreadable for this to test anything"
        );
        let geo = us_ca_geo();
        let state = assembled(&consent, GeoStatus::Located(&geo), &granting());
        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "one scheme arriving unreadable is not cured by another scheme granting"
        );
    }

    #[test]
    fn a_malformed_gpp_or_us_privacy_record_is_detected() {
        // Each undecodable record form has to be detected, or the override
        // above would not fire for it.
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
        let geo = us_ca_geo();
        assert!(
            !assembled(&usp, GeoStatus::Located(&geo), &granting())
                .is_set(Permission::StoreOnDevice),
            "an unreadable US Privacy string blocks the granted baseline like any other record"
        );
    }

    #[test]
    fn an_unreadable_record_is_not_a_withdrawal() {
        let consent = ConsentContext {
            raw_tc_string: Some("not-a-tc-string".to_owned()),
            ..ConsentContext::default()
        };
        let state = assembled(&consent, GeoStatus::NoLocation, &granting());
        assert!(
            !state.storage_withdrawn(),
            "an unreadable record fails closed by suppression, never destructively"
        );
    }

    #[test]
    fn running_no_providers_leaves_the_place_baseline() {
        let geo = us_ca_geo();
        let state = assembled(&ConsentContext::default(), GeoStatus::Located(&geo), &[]);
        assert!(
            state.is_set(Permission::StoreOnDevice),
            "an empty list is a publisher acting on no signal at all, so only the country \
             and region rules apply"
        );
        assert!(!state.storage_withdrawn(), "and nothing can have withdrawn");
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
            assembled(&ConsentContext::default(), GeoStatus::Located(&geo), &[])
                .is_set(Permission::StoreOnDevice),
            "the located baseline must grant storage, or this test proves nothing"
        );
        let state = assembled(&ConsentContext::default(), GeoStatus::Failed, &[]);
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
        let state = assembled(&ConsentContext::default(), GeoStatus::NoLocation, &[]);
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
}
