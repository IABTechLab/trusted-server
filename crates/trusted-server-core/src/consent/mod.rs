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
use crate::permissions::{Permission, PermissionState};

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

/// The jurisdiction for a request: detected from its location when one
/// resolved, otherwise the declaration the caller supplied.
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
    geo: Option<&GeoInfo>,
    default: jurisdiction::Jurisdiction,
) -> jurisdiction::Jurisdiction {
    match geo {
        Some(_) => jurisdiction::detect_jurisdiction(geo),
        None => {
            log_missing_geo_warning_once();
            default
        }
    }
}

/// Whether a server-side auction may be dispatched for this request.
///
/// Reads the resolved permission state rather than any scheme's record, so
/// the place baseline and every configured module have already had their
/// say. The auction selects advertising from the request, so it runs when
/// [`Permission::SelectBasicAds`] is set, which under a baseline that requires
/// a signal means a module granted it, and under a baseline that grants it
/// outright means no module revoked it.
#[must_use]
pub fn permissions_allow_server_side_auction(permissions: &PermissionState) -> bool {
    permissions.is_set(Permission::SelectBasicAds)
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
            request_jurisdiction(Some(&geo), jurisdiction::Jurisdiction::Unknown),
            jurisdiction::Jurisdiction::Gdpr,
            "the place decides, not the default"
        );
    }

    #[test]
    fn an_unlocated_request_takes_the_default_jurisdiction() {
        assert_eq!(
            request_jurisdiction(None, jurisdiction::Jurisdiction::Unknown),
            jurisdiction::Jurisdiction::Unknown,
            "missing geo should keep the default the caller supplied"
        );
        assert_eq!(
            request_jurisdiction(None, jurisdiction::Jurisdiction::Gdpr),
            jurisdiction::Jurisdiction::Gdpr
        );
    }

    #[test]
    fn the_auction_runs_where_basic_ad_selection_is_set() {
        // US/CA maps to us-opt-out, where advertising_marketing.first_party.contextual
        // is granted with no signal.
        let permissions = PermissionMaps::standard().resolve(Some("US"), |_| false);
        assert!(
            permissions_allow_server_side_auction(&permissions),
            "a granted baseline with no signal allows the auction"
        );
        // FR maps to gdpr-eu, where every purpose requires a signal, so with
        // none the auction fails closed.
        let permissions = PermissionMaps::standard().resolve(Some("FR"), |_| false);
        assert!(
            !permissions_allow_server_side_auction(&permissions),
            "a requires-signal baseline with no signal fails closed"
        );
        // A signal granting basic ad selection opens it.
        let permissions = PermissionMaps::standard().resolve(Some("FR"), |permission| {
            permission == Permission::SelectBasicAds
        });
        assert!(
            permissions_allow_server_side_auction(&permissions),
            "a module granting basic ad selection allows the auction"
        );
        // An unresolved place takes the top node, which requires a signal.
        let permissions = PermissionMaps::standard().resolve(None, |_| false);
        assert!(
            !permissions_allow_server_side_auction(&permissions),
            "an unknown place without a signal fails closed"
        );
    }

    #[test]
    fn gate_eids_keeps_eids_when_required_permissions_are_set() {
        // US maps to us-opt-out, where necessary.operations.storage and
        // advertising_marketing.first_party.targeted are granted with no
        // signal, so bidstream EIDs are transmitted.
        let permissions = PermissionMaps::standard().resolve(Some("US"), |_| false);
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
        let permissions = PermissionMaps::standard().resolve(Some("FR"), |_| false);
        let eids = Some(vec!["eid-1".to_owned()]);
        assert!(
            gate_eids_by_permissions(eids, &permissions).is_none(),
            "EIDs should be stripped when a required permission is not set"
        );
    }

    #[test]
    fn gate_eids_returns_none_for_empty_input() {
        let permissions = PermissionMaps::standard().resolve(Some("US"), |_| false);
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
