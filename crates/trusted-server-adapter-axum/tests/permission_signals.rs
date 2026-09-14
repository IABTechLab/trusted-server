//! The four shipped signal providers assembled together, as a deployment
//! runs them.
//!
//! Each provider crate tests its own scheme in isolation, in its own unit
//! tests. What is tested here is what only shows when multiple providers
//! run in order through core's assembly: an answer to a prompt applying
//! over an opt-out, one opt-out standing when another is removed, a scheme
//! left off the list not running at all, and withdrawal being TCF's alone
//! and scoped to the place. This sits in the Axum adapter's tests because
//! it is the first crate that links all four, and core deliberately links
//! none.
//!
//! The consent records here are built by hand, so nothing in core's consent
//! pipeline runs. In a deployment that pipeline also synthesizes a US Privacy
//! opt-out from a Global Privacy Control header in a US state when the consent
//! settings say to, and the `us-privacy` provider then acts on it, which is
//! why removing `gpc` from the list alone does not make that header inert.

use std::sync::Arc;

use trusted_server_core::consent::types::{GppConsent, TcfConsent, UsPrivacy};
use trusted_server_core::consent::{ConsentContext, PrivacyFlag};
use trusted_server_core::ec::consent::{GeoStatus, assemble_permissions};
use trusted_server_core::evidence::OwnedRequestInfo;
use trusted_server_core::permission_signal::{
    PermissionSignalProvider, build_permission_signal_providers,
};
use trusted_server_core::permissions::{Permission, PermissionState};
use trusted_server_core::platform::GeoInfo;
use trusted_server_core::settings::Settings;
use trusted_server_permission_signal_gpc::GpcProvider;
use trusted_server_permission_signal_gpp::GppSaleOptOutProvider;
use trusted_server_permission_signal_tcf::TcfProvider;
use trusted_server_permission_signal_us_privacy::UsPrivacyProvider;

/// The four providers an adapter offers, in the default order.
fn all_four() -> Vec<Arc<dyn PermissionSignalProvider>> {
    vec![
        Arc::new(GpcProvider::new()),
        Arc::new(GppSaleOptOutProvider::new()),
        Arc::new(UsPrivacyProvider::new()),
        Arc::new(TcfProvider::new()),
    ]
}

/// The providers a deployment gets from naming these identifiers in
/// `[permission_signal] sources`, through the same entry point an adapter's
/// composition root uses.
fn configured(names: &[&str]) -> Arc<[Arc<dyn PermissionSignalProvider>]> {
    let mut settings = Settings::default();
    settings.permission_signal.sources =
        Some(names.iter().map(|name| (*name).to_owned()).collect());
    build_permission_signal_providers(&settings, &all_four())
        .expect("should select providers this build offers")
}

/// Every provider except the one named, in the default order, as a
/// publisher removes one from configuration.
fn all_but(excluded: &str) -> Arc<[Arc<dyn PermissionSignalProvider>]> {
    let names: Vec<&str> = all_four()
        .iter()
        .map(|provider| provider.id())
        .filter(|id| *id != excluded)
        .collect();
    configured(&names)
}

fn no_evidence() -> OwnedRequestInfo {
    OwnedRequestInfo::default()
}

fn assembled(
    consent: &ConsentContext,
    geo: GeoStatus<'_>,
    providers: &[Arc<dyn PermissionSignalProvider>],
) -> PermissionState {
    assemble_permissions(consent, &no_evidence(), geo, providers)
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

fn us_privacy_opted_out() -> UsPrivacy {
    UsPrivacy {
        version: 1,
        notice_given: PrivacyFlag::Yes,
        opt_out_sale: PrivacyFlag::Yes,
        lspa_covered: PrivacyFlag::NotApplicable,
    }
}

fn gpp_sale_opted_out() -> GppConsent {
    GppConsent {
        version: 1,
        section_ids: vec![7],
        eu_tcf: None,
        us_sale_opt_out: Some(true),
    }
}

/// A US opt-out state, where the baseline grants storage without a signal, so
/// a revoke is observable as a drop and a refusal is never a withdrawal.
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

// ----------------------------------------------------------------------
// Which providers run.
// ----------------------------------------------------------------------

#[test]
fn gpc_revokes_the_granted_baseline_in_a_us_opt_out_state() {
    // A US-style opt-out drops a granted baseline, because the map granted
    // these purposes and Global Privacy Control revokes them.
    let consent = ConsentContext {
        gpc: true,
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        !state.is_set(Permission::StoreOnDevice)
            && !state.is_set(Permission::SelectPersonalisedAds),
        "GPC should revoke the granted necessary.operations.storage and advertising_marketing.first_party.targeted baseline"
    );
}

#[test]
fn a_provider_left_off_the_list_does_not_run() {
    let consent = ConsentContext {
        gpc: true,
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();

    let everything = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        !everything.is_set(Permission::StoreOnDevice),
        "with every provider running, the header takes storage away"
    );

    let pruned = assembled(&consent, GeoStatus::Located(&geo), &all_but("gpc"));
    assert!(
        pruned.is_set(Permission::StoreOnDevice),
        "a publisher who does not want to act on Global Privacy Control removes it from \
         the list, and the provider that read the header then does not run"
    );
}

#[test]
fn removing_one_opt_out_leaves_the_others_working() {
    // The reason the three opt-outs are separate providers rather than one.
    let consent = ConsentContext {
        us_privacy: Some(us_privacy_opted_out()),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_but("gpc"));
    assert!(
        !state.is_set(Permission::StoreOnDevice),
        "dropping Global Privacy Control must not drop the US Privacy opt-out with it"
    );
}

#[test]
fn gpc_suppresses_storage_even_when_us_privacy_reports_no_opt_out() {
    let consent = ConsentContext {
        gpc: true,
        us_privacy: Some(UsPrivacy {
            opt_out_sale: PrivacyFlag::No,
            ..us_privacy_opted_out()
        }),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        !state.is_set(Permission::StoreOnDevice),
        "any one opt-out provider should suppress, whatever the others say"
    );
}

// ----------------------------------------------------------------------
// Opt-out and prompt precedence.
//
// The providers are asked in order and each amends what the ones before it
// settled, so a later provider can amend an opt-out. The default order asks
// Global Privacy Control first, being a browser setting with no interface of
// its own, and the schemes carrying a choice someone made through an
// interface after, which is why an answer given at a prompt amends the
// header the visitor arrived with. A deployment wanting the opposite puts
// the provider it wants to win last.
// ----------------------------------------------------------------------

#[test]
fn a_prompt_answer_applies_over_a_gpc_signal() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[1, 4])),
        gpc: true,
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        state.is_set(Permission::StoreOnDevice),
        "the visitor answered a prompt after arriving with GPC set, and under the \
         default order the answer they gave is applied over the header they sent"
    );
}

#[test]
fn a_prompt_answer_applies_over_a_us_privacy_opt_out_signal() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[1, 4])),
        us_privacy: Some(us_privacy_opted_out()),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        state.is_set(Permission::StoreOnDevice),
        "the visitor answered a prompt after arriving with a US Privacy opt-out, and under \
         the default order the answer they gave amends the signal they sent"
    );
}

#[test]
fn a_prompt_answer_applies_over_a_gpp_sale_opt_out_signal() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[1, 4])),
        gpp: Some(gpp_sale_opted_out()),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        state.is_set(Permission::StoreOnDevice),
        "the visitor answered a prompt after arriving with a GPP sale opt-out, and under \
         the default order the answer they gave amends the signal they sent"
    );
}

#[test]
fn the_opt_out_wins_when_a_deployment_puts_it_last() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[1, 4])),
        gpc: true,
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let providers = configured(&["tcf", "gpc"]);
    let state = assembled(&consent, GeoStatus::Located(&geo), &providers);
    assert!(
        !state.is_set(Permission::StoreOnDevice),
        "the same request, with the order reversed in configuration, lets the header win"
    );
}

// ----------------------------------------------------------------------
// The TCF mapping, now the TCF crate's, still reaches every purpose.
// ----------------------------------------------------------------------

#[test]
fn tcf_resolves_every_mapped_purpose_not_just_storage_and_ads() {
    // Consent to all purposes except Purpose 7 (measure ad performance), in a
    // US opt-out state where the baseline granted them all, so a revoke is
    // observable as a drop.
    let consented: Vec<usize> = (1..=11).filter(|&purpose| purpose != 7).collect();
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&consented)),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());

    assert!(
        state.is_set(Permission::SelectBasicAds),
        "Purpose 2 consent should set advertising_marketing.first_party.contextual"
    );
    assert!(
        !state.is_set(Permission::MeasureAdPerformance),
        "Purpose 7 refusal should revoke analytics.ad_reporting.measure_ad_performance"
    );
    assert!(
        state.is_set(Permission::StoreOnDevice) && state.is_set(Permission::SelectPersonalisedAds),
        "Purposes 1 and 4 remain resolved from the TCF record"
    );
}

// ----------------------------------------------------------------------
// Withdrawal scoping: only a TCF storage refusal withdraws, and only where
// the baseline did not grant storage outright. Opt-outs suppress use but
// never destroy an already-issued identifier.
//
// No location resolves at the policy's top node, the gdpr-eu group, where
// storage requires a signal. A US opt-out state grants it outright.
// ----------------------------------------------------------------------

#[test]
fn tcf_storage_refusal_withdraws_under_a_requires_signal_baseline() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[4])),
        ..ConsentContext::default()
    };
    let state = assembled(&consent, GeoStatus::NoLocation, &all_four());
    assert!(
        state.storage_withdrawn(),
        "refusing the signal storage depends on should withdraw"
    );
}

#[test]
fn tcf_storage_refusal_does_not_withdraw_under_a_granted_baseline() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[4])),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        !state.storage_withdrawn(),
        "storage never depended on the record here, so refusal suppresses without destroying"
    );
}

#[test]
fn tcf_storage_consent_is_not_a_withdrawal() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[1])),
        ..ConsentContext::default()
    };
    let state = assembled(&consent, GeoStatus::NoLocation, &all_four());
    assert!(
        !state.storage_withdrawn(),
        "a consenting record is not a withdrawal"
    );
}

#[test]
fn gpc_alone_never_withdraws() {
    let consent = ConsentContext {
        gpc: true,
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    assert!(
        !assembled(&consent, GeoStatus::Located(&geo), &all_four()).storage_withdrawn()
            && !assembled(&consent, GeoStatus::NoLocation, &all_four()).storage_withdrawn(),
        "GPC suppresses use for the request but never destroys the identifier"
    );
}

#[test]
fn us_style_opt_outs_never_withdraw() {
    let consent = ConsentContext {
        us_privacy: Some(us_privacy_opted_out()),
        gpp: Some(gpp_sale_opted_out()),
        ..ConsentContext::default()
    };
    let state = assembled(&consent, GeoStatus::NoLocation, &all_four());
    assert!(
        !state.storage_withdrawn(),
        "sale opt-outs suppress use but never destroy the identifier"
    );
}

#[test]
fn no_signal_is_not_a_withdrawal() {
    let state = assembled(
        &ConsentContext::default(),
        GeoStatus::NoLocation,
        &all_four(),
    );
    assert!(
        !state.storage_withdrawn(),
        "absence of a signal must never destroy an identifier"
    );
}

#[test]
fn a_withdrawal_needs_the_tcf_provider_to_be_running() {
    // The withdrawal is TCF's answer, so a deployment that removed the TCF
    // provider from the list has no scheme left that can withdraw.
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[4])),
        ..ConsentContext::default()
    };
    let state = assembled(&consent, GeoStatus::NoLocation, &all_but("tcf"));
    assert!(
        !state.storage_withdrawn(),
        "a scheme that does not run cannot withdraw, whatever the request carries"
    );
}

// ----------------------------------------------------------------------
// Unreadable and expired records, assembled with the real providers.
// ----------------------------------------------------------------------

#[test]
fn a_malformed_tcf_record_blocks_baseline_grants() {
    let consent = ConsentContext {
        raw_tc_string: Some("not-a-tc-string".to_owned()),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        !state.is_set(Permission::StoreOnDevice),
        "an unreadable record should block the granted baseline, not vanish"
    );
    assert!(
        !state.storage_withdrawn(),
        "and it fails closed by suppression, never destructively"
    );
}

#[test]
fn a_readable_tcf_record_does_not_cure_an_unreadable_gpp_string() {
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[1, 4])),
        raw_gpp_string: Some("this is not a GPP string".to_owned()),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        !state.is_set(Permission::StoreOnDevice),
        "one scheme arriving unreadable is not cured by another scheme arriving readable"
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
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        state.is_set(Permission::StoreOnDevice),
        "expiry is its own explicit state, deliberately distinct from malformed"
    );
}
