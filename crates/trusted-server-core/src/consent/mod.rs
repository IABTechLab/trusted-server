//! The request's consent, read by the permission signal modules.
//!
//! Core decodes no scheme. Each configured module reads its own signal from
//! the request, once, through [`PermissionSignalModule::read`], and reports a
//! [`SignalRecord`]. The records, with the request's jurisdiction, are the
//! [`ConsentContext`] the permission assembly, the bid request builders and the
//! identity graph read. What a record means for a permission is the module's
//! to say when the permissions are assembled.

pub mod jurisdiction;
pub mod types;

pub use types::{ConsentContext, ConsentSource, SignalRecord, SignalStatus};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::geo::GeoInfo;
use crate::module_context::ModuleContext;
use crate::permission_signal::PermissionSignalModule;
use crate::permissions::{Permission, PermissionMaps, PermissionState};

static MISSING_GEO_WARNING_LOGGED: AtomicBool = AtomicBool::new(false);

/// Reads the request's consent, asking each of `modules` in order for the
/// signal of its scheme.
///
/// `context` carries the request's evidence and the deployment's settings,
/// which is what a module reads. It is offered to the modules carrying a
/// consent context with no records under `jurisdiction`, so a module whose
/// reading depends on the jurisdiction can see it. The records every module
/// reported, in module order, make the returned context.
#[must_use]
pub fn read_consent(
    context: &ModuleContext<'_>,
    modules: &[Arc<dyn PermissionSignalModule>],
    jurisdiction: jurisdiction::Jurisdiction,
) -> ConsentContext {
    let empty = ConsentContext::new(jurisdiction.clone());
    let context = (*context).with_consent(&empty);
    let mut consent = ConsentContext::new(jurisdiction);
    for module in modules {
        let call = context.call(module.id(), module.required_permissions());
        if let Some(record) = module.read(call) {
            consent = consent.with_record(record);
        }
    }
    log_consent_context(&consent);
    consent
}

/// The jurisdiction for a request: detected from its location, in the
/// policy whose rules are `maps`, when one resolved, otherwise the
/// declaration the caller supplied.
///
/// Jurisdiction is detected from geolocation, so with no location every
/// request would be [`Jurisdiction::Unknown`](jurisdiction::Jurisdiction::Unknown)
/// and the consent gates would fail closed even where the permission policy
/// declares what to do. `default` carries that declaration (see
/// [`PermissionMaps::default_jurisdiction`](crate::permissions::PermissionMaps::default_jurisdiction)),
/// and is `Unknown` where no declaration applies, for example after a failed
/// geo lookup.
#[must_use]
pub fn request_jurisdiction(
    maps: &PermissionMaps,
    geo: Option<&GeoInfo>,
    default: jurisdiction::Jurisdiction,
) -> jurisdiction::Jurisdiction {
    match geo {
        Some(_) => jurisdiction::detect_jurisdiction(maps, geo),
        None => {
            log_missing_geo_warning_once();
            default
        }
    }
}

/// Whether a server-side auction may be dispatched for this request.
///
/// Fails closed where the request's jurisdiction is GDPR or unknown, or a
/// record says the regulation its scheme answers for applies. The auction
/// then runs only when a record that applies holds the visitor's consent to
/// store and access information on the device,
/// [`Permission::StoreOnDevice`]. A request from a known jurisdiction outside
/// GDPR with no record that applies runs freely.
///
/// The gate reads the records and the jurisdiction, not the resolved
/// permissions, so the place rules, the permission policy, an opt-out and a
/// module that grants without a record that applies leave it unchanged.
#[must_use]
pub fn consent_allows_server_side_auction(consent: &ConsentContext) -> bool {
    let requires_consent = consent.applies()
        || matches!(
            consent.jurisdiction,
            jurisdiction::Jurisdiction::Gdpr | jurisdiction::Jurisdiction::Unknown
        );
    if !requires_consent {
        return true;
    }
    consent
        .records
        .iter()
        .any(|record| record.applies() && record.consented().contains(Permission::StoreOnDevice))
}

/// Filters Extended User IDs through the resolved permission state.
///
/// EIDs are partner identifiers carried in the bidstream, so they ride on two
/// permissions: [`Permission::StoreOnDevice`] (the identifier must be readable)
/// and [`Permission::SelectPersonalisedAds`] (it may be shared to select ads).
/// They are transmitted only when both are set in the resolved
/// [`PermissionState`]; otherwise every EID is stripped.
///
/// The country/region map and the signal modules already decided those bits
/// when the permission state was assembled, so this reads one source of truth
/// rather than re-inspecting consent. Called by `handle_auction` after
/// resolving partner EIDs from the KV identity graph, before attaching them to
/// the outbound bid request.
#[must_use]
pub fn gate_eids_by_permissions<T>(
    eids: Option<Vec<T>>,
    permissions: &PermissionState,
) -> Option<Vec<T>> {
    let eids = eids?;
    if eids.is_empty() {
        return None;
    }

    if permissions.is_set(Permission::StoreOnDevice)
        && permissions.is_set(Permission::SelectPersonalisedAds)
    {
        Some(eids)
    } else {
        log::info!(
            "EIDs stripped: necessary.operations.storage or advertising_marketing.first_party.targeted is not set in the resolved permissions"
        );
        None
    }
}

/// Logs a one-time warning when request geolocation is unavailable.
///
/// It says what happens next rather than only that geo is missing, because an
/// operator reading it wants to know whether identity and advertising are
/// still running. With no location the caller supplies the declared
/// jurisdiction, which is the permission policy's top node, so they are. A
/// lookup that fails is the different case, and that one does fail closed.
fn log_missing_geo_warning_once() {
    if MISSING_GEO_WARNING_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }

    log::warn!(
        "Geo lookup returned no location, so the jurisdiction is the one the          permission policy declares rather than one resolved from the request.          Country and region rules cannot match. A geo lookup that fails, as          opposed to resolving nothing, is treated as unknown and fails closed."
    );
}

/// Logs a summary of what the modules read, one entry per scheme.
fn log_consent_context(ctx: &ConsentContext) {
    if ctx.records.is_empty() {
        log::debug!("No consent signals read: jurisdiction={}", ctx.jurisdiction);
        return;
    }
    let records: Vec<String> = ctx
        .records
        .iter()
        .map(|record| {
            format!(
                "{}={}{}",
                record.scheme(),
                record.status(),
                if record.source() == ConsentSource::PolicyDefault {
                    " (policy default)"
                } else {
                    ""
                }
            )
        })
        .collect();
    log::info!(
        "Consent context: jurisdiction={}, {}, applies={}",
        ctx.jurisdiction,
        records.join(", "),
        ctx.applies(),
    );
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::jurisdiction::Jurisdiction;
    use super::*;
    use crate::evidence::{OwnedRequestInfo, RequestInfo};
    use crate::module_context::{ModuleCall, test_support};
    use crate::permission_signal::SignalInput;
    use crate::permissions::{ConsentSignal, PermissionMaps};

    /// A module that reads one cookie and reports it decoded, standing in for
    /// a scheme that decodes a cookie.
    struct Reading(&'static str, &'static str);

    impl Reading {
        fn record(&self, evidence: &dyn RequestInfo) -> Option<SignalRecord> {
            let value = evidence.cookie(self.1)?;
            Some(SignalRecord::new(self.0, SignalStatus::Decoded).with_value(value))
        }
    }

    impl PermissionSignalModule for Reading {
        fn id(&self) -> &'static str {
            self.0
        }

        fn signal(&self, _permission: Permission, _input: &SignalInput<'_>) -> ConsentSignal {
            ConsentSignal::Neutral
        }

        fn read(&self, call: ModuleCall<'_>) -> Option<SignalRecord> {
            call.inject(self, Self::record).ok().flatten()
        }
    }

    /// A module that reports the jurisdiction it was offered, standing in
    /// for a scheme whose reading depends on it.
    struct SeeingJurisdiction;

    impl SeeingJurisdiction {
        fn record(&self, consent: &ConsentContext) -> Option<SignalRecord> {
            Some(
                SignalRecord::new("seeing", SignalStatus::Absent)
                    .with_applies(consent.jurisdiction == jurisdiction::Jurisdiction::Gdpr),
            )
        }
    }

    impl PermissionSignalModule for SeeingJurisdiction {
        fn id(&self) -> &'static str {
            "seeing"
        }

        fn signal(&self, _permission: Permission, _input: &SignalInput<'_>) -> ConsentSignal {
            ConsentSignal::Neutral
        }

        fn read(&self, call: ModuleCall<'_>) -> Option<SignalRecord> {
            call.inject(self, Self::record).ok().flatten()
        }
    }

    fn evidence(cookie: &str) -> OwnedRequestInfo {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            cookie.parse().expect("should build a cookie header"),
        );
        OwnedRequestInfo::new(String::new(), headers)
    }

    #[test]
    fn each_module_reads_its_own_signal_in_order() {
        let evidence = evidence("b=2; a=1");
        let context = ModuleContext::new(test_support::request("/")).with_evidence(&evidence);
        let modules: Vec<Arc<dyn PermissionSignalModule>> = vec![
            Arc::new(Reading("first", "a")),
            Arc::new(Reading("second", "b")),
            Arc::new(Reading("third", "c")),
        ];
        let consent = read_consent(&context, &modules, jurisdiction::Jurisdiction::NonRegulated);
        let schemes: Vec<&str> = consent.records.iter().map(SignalRecord::scheme).collect();
        assert_eq!(
            schemes,
            vec!["first", "second"],
            "a module with nothing to read reports no record, and the rest come in order"
        );
        assert_eq!(consent.value("first"), Some("1"));
        assert_eq!(consent.value("second"), Some("2"));
        assert_eq!(
            consent.jurisdiction,
            jurisdiction::Jurisdiction::NonRegulated,
            "the jurisdiction given is the context's"
        );
    }

    #[test]
    fn a_module_reading_sees_the_jurisdiction() {
        let context = ModuleContext::new(test_support::request("/"));
        let modules: Vec<Arc<dyn PermissionSignalModule>> = vec![Arc::new(SeeingJurisdiction)];
        let consent = read_consent(&context, &modules, jurisdiction::Jurisdiction::Gdpr);
        assert!(
            consent.record("seeing").is_some_and(SignalRecord::applies),
            "the module read the jurisdiction the context will carry"
        );
    }

    #[test]
    fn no_modules_read_nothing() {
        let context = ModuleContext::new(test_support::request("/"));
        let consent = read_consent(&context, &[], jurisdiction::Jurisdiction::Unknown);
        assert!(consent.records.is_empty());
        assert!(consent.is_empty());
    }

    #[test]
    fn a_located_request_takes_the_jurisdiction_of_its_place() {
        let geo = GeoInfo {
            city: String::new(),
            country: "DE".to_owned(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        };
        assert_eq!(
            request_jurisdiction(
                PermissionMaps::sample(),
                Some(&geo),
                jurisdiction::Jurisdiction::Unknown
            ),
            jurisdiction::Jurisdiction::Gdpr,
            "the place decides, not the default"
        );
    }

    #[test]
    fn an_unlocated_request_takes_the_default_jurisdiction() {
        assert_eq!(
            request_jurisdiction(
                PermissionMaps::sample(),
                None,
                jurisdiction::Jurisdiction::Unknown
            ),
            jurisdiction::Jurisdiction::Unknown,
            "missing geo should keep the default the caller supplied"
        );
        assert_eq!(
            request_jurisdiction(
                PermissionMaps::sample(),
                None,
                jurisdiction::Jurisdiction::Gdpr
            ),
            jurisdiction::Jurisdiction::Gdpr
        );
    }

    /// A record of a regulation that applies, holding the visitor's consent
    /// to `consented`, as a module reports a consent string it decoded.
    fn consenting(consented: &[Permission]) -> SignalRecord {
        SignalRecord::new("consent", SignalStatus::Decoded)
            .with_value("consent-string")
            .with_applies(true)
            .with_consented(consented.iter().copied().collect())
    }

    /// The permissions the shipped policy resolves in a place where the
    /// modules grant `granted` and revoke `revoked`.
    fn resolved(
        country: &str,
        region: Option<&str>,
        granted: &[Permission],
        revoked: &[Permission],
    ) -> PermissionState {
        PermissionMaps::sample().resolve_with(Some(country), region, |permission, _| {
            if granted.contains(&permission) {
                ConsentSignal::Grant
            } else if revoked.contains(&permission) {
                ConsentSignal::Revoke
            } else {
                ConsentSignal::Neutral
            }
        })
    }

    /// Whether the gate passes a request in `jurisdiction` carrying
    /// `records`, whose modules resolved `permissions`.
    fn gate(
        jurisdiction: Jurisdiction,
        records: Vec<SignalRecord>,
        permissions: PermissionState,
    ) -> bool {
        let consent = records.into_iter().fold(
            ConsentContext::new(jurisdiction),
            ConsentContext::with_record,
        );
        crate::ec::EcContext::new_for_test(None, consent)
            .with_permissions_for_test(permissions)
            .server_side_auction_allowed()
    }

    fn california() -> Jurisdiction {
        Jurisdiction::UsState("CA".to_owned())
    }

    #[test]
    fn the_auction_runs_freely_only_outside_gdpr_with_no_record_that_applies() {
        let allows = |consent: ConsentContext| consent_allows_server_side_auction(&consent);
        assert!(
            allows(ConsentContext::new(california())),
            "a US state with no record that applies runs the auction"
        );
        assert!(allows(ConsentContext::new(Jurisdiction::NonRegulated)));
        assert!(
            !allows(ConsentContext::new(Jurisdiction::Gdpr)),
            "GDPR with no consent fails closed"
        );
        assert!(
            !allows(ConsentContext::new(Jurisdiction::Unknown)),
            "an unknown jurisdiction with no consent fails closed"
        );
        assert!(
            allows(
                ConsentContext::new(Jurisdiction::Gdpr)
                    .with_record(consenting(&[Permission::StoreOnDevice]))
            ),
            "consent to storage from a record that applies runs it"
        );
        assert!(
            !allows(
                ConsentContext::new(Jurisdiction::Gdpr)
                    .with_record(consenting(&[Permission::StoreOnDevice]).with_applies(false))
            ),
            "and only from a record that applies"
        );
    }

    // Each request below is one whose resolved permissions answer the other
    // way, so each test shows the gate reading the records and the
    // jurisdiction, not the permissions.

    #[test]
    fn the_gate_asks_for_consent_to_storage_not_to_basic_ad_selection() {
        assert!(
            gate(
                Jurisdiction::Gdpr,
                vec![consenting(&[Permission::StoreOnDevice])],
                resolved(
                    "FR",
                    None,
                    &[Permission::StoreOnDevice],
                    &[Permission::SelectBasicAds]
                ),
            ),
            "consent to storage alone runs the auction, though basic ad selection is unset"
        );
        assert!(
            !gate(
                Jurisdiction::Gdpr,
                vec![consenting(&[Permission::SelectBasicAds])],
                resolved(
                    "FR",
                    None,
                    &[Permission::SelectBasicAds],
                    &[Permission::StoreOnDevice]
                ),
            ),
            "consent to basic ad selection alone does not, though it is set"
        );
    }

    #[test]
    fn an_expired_record_that_applies_shuts_the_gate_in_every_place() {
        let expired = SignalRecord::new("consent", SignalStatus::Expired)
            .with_value("consent-string")
            .with_applies(true);
        assert!(
            !gate(
                california(),
                vec![expired],
                resolved("US", Some("CA"), &[], &[])
            ),
            "an expired record answers nothing, and the auction still waits for consent \
             where the place grants basic ad selection outright"
        );
    }

    #[test]
    fn the_gate_reads_a_record_whatever_the_policy_lets_it_answer() {
        // A policy that silences the record leaves every permission at the
        // place's baseline, and the record's own consent still decides.
        assert!(
            gate(
                Jurisdiction::Gdpr,
                vec![consenting(&[Permission::StoreOnDevice])],
                resolved("FR", None, &[], &[]),
            ),
            "a silenced record consenting to storage runs the auction"
        );
        assert!(
            !gate(
                california(),
                vec![consenting(&[])],
                resolved("US", Some("CA"), &[], &[])
            ),
            "and a silenced record refusing it shuts the gate where the place grants \
             basic ad selection"
        );
    }

    #[test]
    fn a_grant_from_a_module_with_no_record_that_applies_leaves_the_gate_shut() {
        // A module answering a preference of its own, with no record that
        // applies, grants storage and basic ad selection in a GDPR place.
        let granted = [Permission::StoreOnDevice, Permission::SelectBasicAds];
        assert!(
            !gate(
                Jurisdiction::Gdpr,
                vec![],
                resolved("FR", None, &granted, &[])
            ),
            "the grant does not stand in for consent from a record that applies"
        );
        assert!(
            !gate(
                Jurisdiction::Gdpr,
                vec![consenting(&[])],
                resolved("FR", None, &granted, &[])
            ),
            "nor stand over a record that applies and refuses storage"
        );
    }

    #[test]
    fn the_gate_follows_the_jurisdiction_not_the_place_rules_or_an_opt_out() {
        assert!(
            gate(
                Jurisdiction::NonRegulated,
                vec![],
                PermissionState::default()
            ),
            "a place outside GDPR whose rules leave basic ad selection unset still runs it"
        );
        // An opt-out whose policy revokes basic ad selection, as `revokes: all`
        // does.
        let opted_out = SignalRecord::new("opt-out", SignalStatus::Decoded).with_value("opted-out");
        assert!(
            gate(
                california(),
                vec![opted_out],
                resolved("US", Some("CA"), &[], &[Permission::SelectBasicAds]),
            ),
            "and an opt-out does not stop it"
        );
    }

    #[test]
    fn a_record_that_applies_with_nothing_decoded_shuts_the_gate_in_every_place() {
        // A module that sees its regulation applies with no string it could
        // decode, such as a hint naming a section with no string beside it.
        let absent = SignalRecord::new("consent", SignalStatus::Absent).with_applies(true);
        assert!(
            !gate(
                california(),
                vec![absent],
                resolved("US", Some("CA"), &[], &[])
            ),
            "the regulation applies, so the auction waits for consent to storage"
        );
    }

    #[test]
    fn gate_eids_keeps_eids_when_required_permissions_are_set() {
        // US maps to us-opt-out, where necessary.operations.storage and
        // advertising_marketing.first_party.targeted are granted with no
        // signal, so bidstream EIDs are transmitted.
        let permissions = PermissionMaps::sample().resolve(Some("US"), |_| false);
        let eids = Some(vec!["eid-1".to_owned()]);
        assert!(
            gate_eids_by_permissions(eids, &permissions).is_some(),
            "EIDs should pass when necessary.operations.storage and advertising_marketing.first_party.targeted are set"
        );
    }

    #[test]
    fn gate_eids_strips_eids_when_a_required_permission_is_unset() {
        // FR maps to gdpr-eu, where every purpose is requires_signal, so with no
        // signal neither required permission is set and EIDs are stripped.
        let permissions = PermissionMaps::sample().resolve(Some("FR"), |_| false);
        let eids = Some(vec!["eid-1".to_owned()]);
        assert!(
            gate_eids_by_permissions(eids, &permissions).is_none(),
            "EIDs should be stripped when a required permission is not set"
        );
    }

    #[test]
    fn gate_eids_returns_none_for_empty_input() {
        let permissions = PermissionMaps::sample().resolve(Some("US"), |_| false);
        assert!(
            gate_eids_by_permissions::<String>(None, &permissions).is_none(),
            "no EIDs should resolve to None"
        );
        assert!(
            gate_eids_by_permissions(Some(Vec::<String>::new()), &permissions).is_none(),
            "an empty EID list should resolve to None"
        );
    }
}
