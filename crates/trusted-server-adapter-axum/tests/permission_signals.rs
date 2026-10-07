//! The four shipped signal modules assembled together, as a deployment
//! runs them.
//!
//! Each module crate tests its own scheme in isolation, in its own unit
//! tests. What is tested here is what only shows when multiple modules
//! run in order through core's assembly: an answer to a prompt applying
//! over an opt-out, one opt-out standing when another is removed, a scheme
//! left off the list not running at all, and withdrawal being TCF's alone
//! and scoped to the place. This sits in the Axum adapter's tests because
//! it is the first crate that links all four, and core deliberately links
//! none. The names a deployment writes in configuration are checked here for
//! the same reason, against the identifiers the real crates answer to.
//!
//! The consent records here are built by hand, so nothing in core's consent
//! pipeline runs. In a deployment that pipeline also synthesizes a US Privacy
//! opt-out from a Global Privacy Control header in a US state when the consent
//! settings say to, and the `us_privacy` module then acts on it, which is
//! why removing `gpc` from the list alone does not make that header inert.

use std::sync::Arc;

use trusted_server_core::consent::types::{GppConsent, TcfConsent, UsPrivacy};
use trusted_server_core::consent::{ConsentContext, PrivacyFlag};
use trusted_server_core::ec::consent::{GeoStatus, assemble_permissions};
use trusted_server_core::evidence::OwnedRequestInfo;
use trusted_server_core::permission_signal::{
    PermissionSignalModule, build_permission_signal_modules,
};
use trusted_server_core::permissions::{Permission, PermissionState};
use trusted_server_core::platform::GeoInfo;
use trusted_server_core::settings::Settings;
use trusted_server_permission_signal_gpc::GpcModule;
use trusted_server_permission_signal_gpp::GppSaleOptOutModule;
use trusted_server_permission_signal_mtm::MtmModule;
use trusted_server_permission_signal_tcf::TcfModule;
use trusted_server_permission_signal_us_privacy::UsPrivacyModule;

/// The four IAB modules an adapter offers, in the default order. MTM is
/// offered after them and is tested with them below.
fn all_four() -> Vec<Arc<dyn PermissionSignalModule>> {
    vec![
        Arc::new(GpcModule::new()),
        Arc::new(GppSaleOptOutModule::new()),
        Arc::new(UsPrivacyModule::new()),
        Arc::new(TcfModule::new()),
    ]
}

/// All five, as an adapter offers them.
fn all_five() -> Vec<Arc<dyn PermissionSignalModule>> {
    let mut modules = all_four();
    modules.push(Arc::new(MtmModule::new()));
    modules
}

/// Settings naming these identifiers in `[permission-signal] modules`.
fn settings_naming(names: &[&str]) -> Settings {
    let mut settings = Settings::default();
    settings.permission_signal.modules =
        Some(names.iter().map(|name| (*name).to_owned()).collect());
    settings
}

/// The modules a deployment gets from naming these identifiers in
/// `[permission-signal] modules`, through the same entry point an adapter's
/// composition root uses.
fn configured(names: &[&str]) -> Arc<[Arc<dyn PermissionSignalModule>]> {
    build_permission_signal_modules(&settings_naming(names), &all_four())
        .expect("should select modules this build offers")
}

/// Every module except the one named, in the default order, as a
/// publisher removes one from configuration.
fn all_but(excluded: &str) -> Arc<[Arc<dyn PermissionSignalModule>]> {
    let names: Vec<&str> = all_four()
        .iter()
        .map(|module| trusted_server_core::permission_signal::short_name(module.as_ref()))
        .filter(|name| *name != excluded)
        .collect();
    configured(&names)
}

fn no_evidence() -> OwnedRequestInfo {
    OwnedRequestInfo::default()
}

fn assembled(
    consent: &ConsentContext,
    geo: GeoStatus<'_>,
    modules: &[Arc<dyn PermissionSignalModule>],
) -> PermissionState {
    assemble_permissions(consent, &no_evidence(), geo, modules)
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
// Which modules run.
// ----------------------------------------------------------------------

#[test]
fn the_documented_names_select_every_shipped_module_in_order() {
    // The names the guide and the example configuration list, which must be
    // the names the shipped crates answer to within the section.
    let documented = ["gpc", "gpp", "us-privacy", "tcf"];
    let selected: Vec<&str> = configured(&documented)
        .iter()
        .map(|module| trusted_server_core::permission_signal::short_name(module.as_ref()))
        .collect();
    assert_eq!(
        selected, documented,
        "each documented name selects the shipped module it names, in the order written"
    );
}

#[test]
fn an_old_name_is_refused_naming_the_names_available() {
    for old in ["gpp_sale_opt_out", "us_privacy"] {
        let Err(error) = build_permission_signal_modules(&settings_naming(&[old]), &all_four())
        else {
            panic!("should refuse the old name `{old}`");
        };
        let message = format!("{error:?}");
        assert!(
            message.contains(&format!("`{old}` is not available in this build"))
                && message.contains("Available modules are gpc, gpp, us-privacy, tcf"),
            "the refusal names the old name and the names to write instead: {message}"
        );
    }
}

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
fn a_module_left_off_the_list_does_not_run() {
    let consent = ConsentContext {
        gpc: true,
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();

    let everything = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        !everything.is_set(Permission::StoreOnDevice),
        "with every module running, the header takes storage away"
    );

    let pruned = assembled(&consent, GeoStatus::Located(&geo), &all_but("gpc"));
    assert!(
        pruned.is_set(Permission::StoreOnDevice),
        "a publisher who does not want to act on Global Privacy Control removes it from \
         the list, and the module that read the header then does not run"
    );
}

#[test]
fn removing_one_opt_out_leaves_the_others_working() {
    // The reason the three opt-outs are separate modules rather than one.
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
        "any one opt-out module should suppress, whatever the others say"
    );
}

// ----------------------------------------------------------------------
// Opt-out and prompt precedence.
//
// The modules are asked in order and each amends what the ones before it
// settled, so a later module can amend an opt-out. The default order asks
// Global Privacy Control first, being a browser setting with no interface of
// its own, and the schemes carrying a choice someone made through an
// interface after, which is why an answer given at a prompt amends the
// header the visitor arrived with. A deployment wanting the opposite puts
// the module it wants to win last.
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
    let modules = configured(&["tcf", "gpc"]);
    let state = assembled(&consent, GeoStatus::Located(&geo), &modules);
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
fn a_withdrawal_needs_the_tcf_module_to_be_running() {
    // The withdrawal is TCF's answer, so a deployment that removed the TCF
    // module from the list has no scheme left that can withdraw.
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
// Unreadable and expired records, assembled with the real modules.
// ----------------------------------------------------------------------

#[test]
fn an_unreadable_tcf_record_is_the_tcf_modules_refusal() {
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
    assert!(
        state.signals().is_empty(),
        "and the string is not a valid signal, so nothing downstream sees it"
    );
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_but("tcf"));
    assert!(
        state.is_set(Permission::StoreOnDevice),
        "a scheme that does not run answers nothing, readable or not"
    );
}

#[test]
fn an_unreadable_gpp_string_is_the_gpp_modules_opt_out_and_the_order_decides() {
    // The GPP module reads its own unreadable string as the opt-out it
    // may have carried. Asked after it, a readable TCF record consenting to
    // storage amends that, because the order is the policy. Nothing in
    // core answers ahead of the modules.
    let consent = ConsentContext {
        tcf: Some(tcf_with_purposes(&[1, 4])),
        raw_tc_string: Some("CPreadable".to_owned()),
        raw_gpp_string: Some("this is not a GPP string".to_owned()),
        ..ConsentContext::default()
    };
    let geo = us_ca_geo();
    assert!(
        !assembled(&consent, GeoStatus::Located(&geo), &all_but("tcf"))
            .is_set(Permission::StoreOnDevice),
        "without TCF the GPP module's reading of its unreadable string stands"
    );
    let state = assembled(&consent, GeoStatus::Located(&geo), &all_four());
    assert!(
        state.is_set(Permission::StoreOnDevice),
        "asked last, the readable TCF record amends the GPP module's answer"
    );
    let schemes: Vec<&str> = state.signals().iter().map(|s| s.scheme).collect();
    assert_eq!(
        schemes,
        vec!["tcf"],
        "only the readable string is vouched for, so only it goes any further"
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

#[test]
fn a_visitor_in_the_eu_with_no_record_is_awaiting_what_tcf_could_grant() {
    let geo = GeoInfo {
        city: String::new(),
        country: "FR".to_owned(),
        continent: String::new(),
        latitude: 0.0,
        longitude: 0.0,
        metro_code: 0,
        region: None,
        asn: None,
    };
    let state = assembled(
        &ConsentContext::default(),
        GeoStatus::Located(&geo),
        &all_four(),
    );
    assert!(
        state.is_awaited(Permission::StoreOnDevice)
            && state.is_awaited(Permission::SelectPersonalisedAds),
        "a prompt that has not run leaves the TCF-mapped Data Uses awaited"
    );
    let email = Permission::all()
        .find(|permission| permission.as_str() == "advertising_marketing.communications.email")
        .expect("the taxonomy should carry the email channel");
    assert!(
        !state.is_awaited(email),
        "a channel no module can grant is not awaited"
    );
    let state = assembled(
        &ConsentContext {
            tcf: Some(tcf_with_purposes(&[1])),
            ..ConsentContext::default()
        },
        GeoStatus::Located(&geo),
        &all_four(),
    );
    assert!(
        state.is_set(Permission::StoreOnDevice) && !state.is_awaited(Permission::StoreOnDevice),
        "an answered prompt settles what it granted"
    );
    assert!(
        !state.is_awaited(Permission::SelectPersonalisedAds)
            && !state.is_set(Permission::SelectPersonalisedAds),
        "a record refusing a purpose is a refusal, not an awaited answer"
    );
}

#[test]
fn a_pmp_answer_in_the_eu_settles_what_the_model_terms_cover_and_declares_them() {
    use trusted_server_core::constants::COOKIE_MTM_PREF;

    let geo = GeoInfo {
        city: String::new(),
        country: "FR".to_owned(),
        continent: String::new(),
        latitude: 0.0,
        longitude: 0.0,
        metro_code: 0,
        region: None,
        asn: None,
    };
    let evidence = |word: &str| {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "cookie",
            format!("{COOKIE_MTM_PREF}={word}")
                .parse()
                .expect("should build a cookie header"),
        );
        OwnedRequestInfo::new(String::new(), headers)
    };
    let standard = assemble_permissions(
        &ConsentContext::default(),
        &evidence("standard"),
        GeoStatus::Located(&geo),
        &all_five(),
    );
    assert!(
        standard.is_set(Permission::StoreOnDevice)
            && standard.is_set(Permission::SelectBasicAds)
            && !standard.is_set(Permission::SelectPersonalisedAds)
            && !standard.is_awaited(Permission::SelectPersonalisedAds),
        "standard grants storage and contextual advertising and refuses targeting"
    );
    assert_eq!(
        standard.signals(),
        &[trusted_server_core::permissions::ValidSignal::new(
            "mtm", "mtm", "standard"
        )],
        "the word is the valid signal, as received"
    );
    assert_eq!(
        standard
            .tdls()
            .iter()
            .map(trusted_server_core::tdl::Tdl::as_str)
            .collect::<Vec<_>>(),
        vec!["https://m4ow.uk/mtm/2.txt"],
        "an answer is given under the versioned Model Terms"
    );
    let personalized = assemble_permissions(
        &ConsentContext::default(),
        &evidence("personalized"),
        GeoStatus::Located(&geo),
        &all_five(),
    );
    assert!(
        personalized.is_set(Permission::SelectPersonalisedAds),
        "personalized grants targeting too"
    );
    let unanswered = assemble_permissions(
        &ConsentContext::default(),
        &no_evidence(),
        GeoStatus::Located(&geo),
        &all_five(),
    );
    assert!(
        unanswered.is_awaited(Permission::SelectPersonalisedAds) && unanswered.tdls().is_empty(),
        "with no answer the question is still open and no terms are declared"
    );
}
