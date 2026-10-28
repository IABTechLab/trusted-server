//! Identifiers a module learns for a request, and their sync into the
//! identity graph.
//!
//! An [`IdentifierModule`] hands core the identifiers it learned for the
//! request, such as the ones a browser-side script persisted in a cookie the
//! module owns. Core asks every running module once, when it reads the Edge
//! Cookie state, and carries the answers on the request's
//! [`EcContext`](super::EcContext) to response finalization, which offers
//! them to the identity graph, and to the auction, which forwards them when
//! the request body carries none.
//!
//! Core names no cookie here. A module reads its own, through the request's
//! evidence and the public prefix the services carry, and returns what it
//! found. A module is offered through
//! [`IntegrationRegistrationBuilder::with_identifier_module`](crate::integrations::IntegrationRegistrationBuilder::with_identifier_module).

use std::sync::Arc;

use crate::module_context::{ModuleCall, ModuleContext, Withheld};
use crate::openrtb::{Eid, Uid};
use crate::permissions::PermissionSet;

use super::kv::PartnerIdUpdate;
use super::kv_types::MAX_UID_LENGTH;
use super::registry::PartnerRegistry;

/// Learns identifiers for a request.
///
/// Each call is handed a [`ModuleCall`] carrying the request's evidence,
/// settings, services, consent and resolved permissions, as every module is,
/// and an implementation hands its own function to [`ModuleCall::inject`]
/// naming what it reads. A module reads what the request carries and retains
/// nothing.
pub trait IdentifierModule: Send + Sync + core::fmt::Debug {
    /// The module's name, for the call and the logs.
    fn id(&self) -> &'static str;

    /// The permissions this module declares, which decide whether a gated
    /// value is passed to it. None by default.
    fn required_permissions(&self) -> PermissionSet {
        PermissionSet::none()
    }

    /// The identifiers the module learned for this request, in the order it
    /// offers them, so that where two carry the same partner the later one is
    /// the candidate. Empty when the request carries none.
    ///
    /// # Errors
    ///
    /// [`Withheld`] when the module names a value the call does not carry.
    fn identifiers(&self, call: ModuleCall<'_>) -> Result<Vec<Eid>, Withheld>;
}

/// Asks each of `modules` for the identifiers it learned, in order.
///
/// A module whose call withholds a value it names contributes nothing and is
/// logged, so one module's fault does not lose another's identifiers.
pub(crate) fn learn(
    context: &ModuleContext<'_>,
    modules: &[Arc<dyn IdentifierModule>],
) -> Vec<Eid> {
    let mut learned = Vec::new();
    for module in modules {
        match module.identifiers(context.call(module.id(), module.required_permissions())) {
            Ok(eids) => learned.extend(eids),
            Err(withheld) => log::warn!(
                "identifier module `{}` contributed nothing: {withheld}",
                module.id()
            ),
        }
    }
    learned
}

/// Collects validated request-local partner updates without performing KV I/O.
///
/// `module_eids` are what the identifier modules learned, applied first.
/// `body_eids` are the current request's own EIDs (e.g. an `/auction` JSON
/// body), when the route captured any, applied after so they win on
/// conflicts: a cookie a module read is size-capped by the browser and may
/// be missing partners the request body still carries in full. The
/// `sharedId` cookie is applied last, unaffected by `body_eids`.
pub(crate) fn collect_eid_updates(
    module_eids: &[Eid],
    sharedid_cookie: Option<&str>,
    body_eids: Option<&[Eid]>,
    registry: &PartnerRegistry,
) -> Vec<PartnerIdUpdate> {
    if registry.is_empty() {
        return Vec::new();
    }

    let mut updates = collect_eid_updates_from_eids(module_eids, registry);
    if let Some(eids) = body_eids {
        updates.extend(collect_eid_updates_from_eids(eids, registry));
    }
    if let Some(cookie) = sharedid_cookie
        && let Some(update) = collect_sharedid_update(cookie, registry)
    {
        updates.push(update);
    }
    dedupe_partner_updates(updates)
}

fn collect_eid_updates_from_eids(eids: &[Eid], registry: &PartnerRegistry) -> Vec<PartnerIdUpdate> {
    let mut updates = Vec::new();
    for eid in eids {
        // An EID's source is what the reader sent, so no log line carries
        // it. A line names the partner's configured domain instead.
        let Some(partner) = registry.find_by_source_domain(&eid.source) else {
            log::debug!("EIDs: skipped an EID whose source matches no partner");
            continue;
        };

        // KV stores one UID per partner. Sync the first valid UID under each
        // source, skipping malformed candidates instead of dropping the whole
        // source.
        let Some(uid) = first_valid_uid(&eid.uids) else {
            log::debug!(
                "EIDs: no valid uid for source_domain '{}'",
                partner.source_domain
            );
            continue;
        };

        updates.push(PartnerIdUpdate::new(&partner.source_domain, &uid.id));
    }

    updates
}

fn dedupe_partner_updates(updates: Vec<PartnerIdUpdate>) -> Vec<PartnerIdUpdate> {
    let mut latest = std::collections::BTreeMap::new();
    for update in updates {
        latest.insert(update.partner_id, update.uid);
    }

    latest
        .into_iter()
        .map(|(partner_id, uid)| PartnerIdUpdate::new(partner_id, uid))
        .collect()
}

fn first_valid_uid(uids: &[Uid]) -> Option<&Uid> {
    uids.iter().find(|uid| is_valid_eid_uid(&uid.id))
}

fn is_valid_eid_uid(uid: &str) -> bool {
    !uid.trim().is_empty() && !eid_id_exceeds_size_limit(uid)
}

/// `SharedID` EID source domain used for partner registry lookup.
const SHAREDID_SOURCE_DOMAIN: &str = "sharedid.org";

fn collect_sharedid_update(
    cookie_value: &str,
    registry: &PartnerRegistry,
) -> Option<PartnerIdUpdate> {
    let cookie_value = cookie_value.trim();
    if cookie_value.is_empty() {
        return None;
    }

    if sharedid_cookie_exceeds_size_limit(cookie_value) {
        log::debug!(
            "SharedID: cookie exceeds MAX_UID_LENGTH ({} bytes)",
            cookie_value.len()
        );
        return None;
    }

    let Some(partner) = registry.find_by_source_domain(SHAREDID_SOURCE_DOMAIN) else {
        log::debug!("SharedID: no partner configured for source '{SHAREDID_SOURCE_DOMAIN}'");
        return None;
    };

    Some(PartnerIdUpdate::new(&partner.source_domain, cookie_value))
}

fn eid_id_exceeds_size_limit(uid: &str) -> bool {
    uid.len() > MAX_UID_LENGTH
}

fn sharedid_cookie_exceeds_size_limit(cookie_value: &str) -> bool {
    cookie_value.len() > MAX_UID_LENGTH
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ec::registry::PartnerRegistry;
    use crate::evidence::{OwnedRequestInfo, RequestInfo};
    use crate::module_context::test_support::request;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    fn make_test_partner(source_domain: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Some(Redacted::new(format!(
                "token-{source_domain}-32-bytes-minimum-value"
            ))),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    fn make_registry(source_domains: &[&str]) -> PartnerRegistry {
        let partners: Vec<_> = source_domains
            .iter()
            .map(|source_domain| make_test_partner(source_domain))
            .collect();
        PartnerRegistry::from_config(&partners).expect("should build partner registry")
    }

    fn eid(source: &str, ids: &[&str]) -> Eid {
        Eid {
            source: source.to_owned(),
            uids: ids
                .iter()
                .map(|id| Uid {
                    id: (*id).to_owned(),
                    atype: Some(1),
                    ext: None,
                })
                .collect(),
        }
    }

    /// A module that answers the identifiers it was built with, under a
    /// fixed name as every module has.
    #[derive(Debug)]
    struct Fixed(Vec<Eid>);

    impl IdentifierModule for Fixed {
        fn id(&self) -> &'static str {
            "testing.fixed"
        }

        fn identifiers(&self, _call: ModuleCall<'_>) -> Result<Vec<Eid>, Withheld> {
            Ok(self.0.clone())
        }
    }

    /// A module that reads one cookie from the request's evidence, the way a
    /// module owning a cookie does.
    #[derive(Debug)]
    struct FromCookie;

    impl FromCookie {
        fn read(&self, evidence: &dyn RequestInfo) -> Vec<Eid> {
            evidence
                .cookie("fixture-id")
                .map(|id| vec![eid("fixture.example", &[id])])
                .unwrap_or_default()
        }
    }

    impl IdentifierModule for FromCookie {
        fn id(&self) -> &'static str {
            "testing.from-cookie"
        }

        fn identifiers(&self, call: ModuleCall<'_>) -> Result<Vec<Eid>, Withheld> {
            call.inject(self, Self::read)
        }
    }

    #[test]
    fn learn_asks_each_module_in_order_and_keeps_their_order() {
        let modules: Vec<Arc<dyn IdentifierModule>> = vec![
            Arc::new(Fixed(vec![eid("id5-sync.com", &["ID5_abc"])])),
            Arc::new(Fixed(vec![eid("liveramp.com", &["LR_xyz"])])),
        ];

        let learned = learn(ModuleContext::empty(), &modules);

        assert_eq!(
            learned
                .iter()
                .map(|eid| eid.source.as_str())
                .collect::<Vec<_>>(),
            vec!["id5-sync.com", "liveramp.com"],
            "should keep the modules' order"
        );
    }

    #[test]
    fn learn_passes_the_evidence_a_module_names() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "cookie",
            "other=1; fixture-id=from-cookie"
                .parse()
                .expect("should build a cookie header"),
        );
        let evidence = OwnedRequestInfo::new(String::new(), headers);
        let context = ModuleContext::new(request("/")).with_evidence(&evidence);
        let modules: Vec<Arc<dyn IdentifierModule>> = vec![Arc::new(FromCookie)];

        let learned = learn(&context, &modules);

        assert_eq!(learned.len(), 1, "should read the cookie the module names");
        assert_eq!(learned[0].uids[0].id, "from-cookie");
    }

    #[test]
    fn learn_skips_a_module_whose_call_withholds_what_it_names() {
        let modules: Vec<Arc<dyn IdentifierModule>> = vec![
            Arc::new(FromCookie),
            Arc::new(Fixed(vec![eid("id5-sync.com", &["ID5_abc"])])),
        ];

        let learned = learn(ModuleContext::empty(), &modules);

        assert_eq!(
            learned.len(),
            1,
            "a module handed no evidence should contribute nothing, and the next still answers"
        );
        assert_eq!(learned[0].source, "id5-sync.com");
    }

    #[test]
    fn sharedid_cookie_rejects_values_larger_than_uid_limit() {
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let exact_limit = "x".repeat(MAX_UID_LENGTH);

        assert!(
            sharedid_cookie_exceeds_size_limit(&oversized),
            "should reject sharedId values larger than MAX_UID_LENGTH"
        );
        assert!(
            !sharedid_cookie_exceeds_size_limit(&exact_limit),
            "should allow sharedId values exactly at MAX_UID_LENGTH"
        );
    }

    #[test]
    fn eid_uid_rejects_values_larger_than_uid_limit() {
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let exact_limit = "x".repeat(MAX_UID_LENGTH);

        assert!(
            eid_id_exceeds_size_limit(&oversized),
            "should reject EID values larger than MAX_UID_LENGTH"
        );
        assert!(
            !eid_id_exceeds_size_limit(&exact_limit),
            "should allow EID values exactly at MAX_UID_LENGTH"
        );
    }

    #[test]
    fn first_valid_uid_skips_oversized_and_uses_later_valid_uid() {
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let uids = vec![
            Uid {
                id: oversized,
                atype: Some(1),
                ext: None,
            },
            Uid {
                id: "valid-uid".to_owned(),
                atype: Some(1),
                ext: None,
            },
        ];

        let uid = first_valid_uid(&uids).expect("should find later valid UID");
        assert_eq!(uid.id, "valid-uid", "should skip oversized first UID");
    }

    #[test]
    fn first_valid_uid_rejects_whitespace_only_ids() {
        let uids = vec![Uid {
            id: "   ".to_owned(),
            atype: Some(1),
            ext: None,
        }];

        assert!(
            first_valid_uid(&uids).is_none(),
            "should reject whitespace UID"
        );
    }

    #[test]
    fn collect_eid_updates_collects_multiple_partner_matches() {
        let registry = make_registry(&["id5-sync.com", "liveramp.com"]);
        let module_eids = vec![
            eid("id5-sync.com", &["ID5_abc"]),
            eid("liveramp.com", &["LR_xyz"]),
        ];

        let updates = collect_eid_updates(&module_eids, None, None, &registry);

        assert_eq!(updates.len(), 2, "should collect both partner matches");
        assert_eq!(updates[0], PartnerIdUpdate::new("id5-sync.com", "ID5_abc"));
        assert_eq!(updates[1], PartnerIdUpdate::new("liveramp.com", "LR_xyz"));
    }

    #[test]
    fn collect_eid_updates_skips_unknown_sources() {
        let registry = make_registry(&["id5-sync.com"]);
        let module_eids = vec![eid("unknown.example", &["unknown"])];

        let updates = collect_eid_updates(&module_eids, None, None, &registry);

        assert!(
            updates.is_empty(),
            "should skip EIDs without configured source-domain partners"
        );
    }

    /// Each EID skipped is logged without the source as the reader sent it,
    /// and an EID a partner owns is named by the partner's configured domain.
    #[test]
    fn no_log_line_carries_the_source_a_reader_sent() {
        let registry = make_registry(&["id5-sync.com"]);
        let module_eids = vec![
            eid("reader-sent.example", &["unmatched"]),
            eid("ID5-Sync.COM.", &["   "]),
        ];

        let (updates, lines) = futures::executor::block_on(crate::test_support::logs::logged_at(
            log::Level::Debug,
            async { collect_eid_updates(&module_eids, None, None, &registry) },
        ));

        assert!(updates.is_empty(), "neither EID should give an update");
        assert_eq!(
            lines.len(),
            2,
            "each skipped EID should be logged: {lines:?}"
        );
        for sent in ["reader-sent.example", "ID5-Sync.COM."] {
            assert!(
                lines.iter().all(|line| !line.contains(sent)),
                "no line should carry the source `{sent}`: {lines:?}"
            );
        }
        assert!(
            lines[1].contains("'id5-sync.com'"),
            "the partner's EID should be named by its configured domain: {lines:?}"
        );
    }

    #[test]
    fn collect_eid_updates_uses_later_valid_uid_candidate() {
        let registry = make_registry(&["id5-sync.com"]);
        let oversized = "x".repeat(MAX_UID_LENGTH + 1);
        let module_eids = vec![eid("id5-sync.com", &[&oversized, "ID5_valid"])];

        let updates = collect_eid_updates(&module_eids, None, None, &registry);

        assert_eq!(
            updates,
            vec![PartnerIdUpdate::new("id5-sync.com", "ID5_valid")]
        );
    }

    #[test]
    fn collect_sharedid_update_maps_configured_sharedid_partner() {
        let registry = make_registry(&["sharedid.org"]);

        let update = collect_sharedid_update(" shared-cookie-id ", &registry)
            .expect("should collect sharedId update");

        assert_eq!(
            update,
            PartnerIdUpdate::new("sharedid.org", "shared-cookie-id")
        );
    }

    #[test]
    fn collect_eid_updates_merges_module_eids_and_sharedid_without_kv() {
        let registry = make_registry(&["id5-sync.com", "sharedid.org"]);
        let module_eids = vec![eid("id5-sync.com", &["ID5_abc"])];

        let updates = collect_eid_updates(&module_eids, Some(" shared-1 "), None, &registry);

        assert_eq!(
            updates.len(),
            2,
            "should collect module and sharedId matches"
        );
        assert!(updates.contains(&PartnerIdUpdate::new("id5-sync.com", "ID5_abc")));
        assert!(updates.contains(&PartnerIdUpdate::new("sharedid.org", "shared-1")));
    }

    #[test]
    fn collect_eid_updates_body_eids_ingest_partners_a_module_trimmed() {
        // A module's cookie the browser trimmed to fit its size cap dropped
        // the `liveramp.com` source, while the `/auction` request body still
        // carried it, and the newer `id5-sync.com` uid, in full.
        let registry = make_registry(&["id5-sync.com", "liveramp.com"]);
        let module_eids = vec![eid("id5-sync.com", &["ID5_stale"])];
        let body_eids = vec![
            eid("id5-sync.com", &["ID5_fresh"]),
            eid("liveramp.com", &["LR_xyz"]),
        ];

        let updates = collect_eid_updates(&module_eids, None, Some(&body_eids), &registry);

        assert_eq!(
            updates,
            vec![
                PartnerIdUpdate::new("id5-sync.com", "ID5_fresh"),
                PartnerIdUpdate::new("liveramp.com", "LR_xyz"),
            ],
            "body EIDs should ingest every configured partner present in the \
             request, including the one the module's cookie trimmed, and should \
             win over a stale value for a partner both carry"
        );
    }

    #[test]
    fn collect_eid_updates_empty_registry_returns_no_updates() {
        let registry = PartnerRegistry::empty();
        let module_eids = vec![eid("id5-sync.com", &["ID5_abc"])];

        let updates = collect_eid_updates(&module_eids, Some("shared-1"), None, &registry);

        assert!(
            updates.is_empty(),
            "an empty registry matches no partners and touches no KV"
        );
    }

    #[test]
    fn dedupe_partner_updates_uses_last_partner_value() {
        let updates = vec![
            PartnerIdUpdate::new("sharedid.org", "module-shared"),
            PartnerIdUpdate::new("id5-sync.com", "id5-uid"),
            PartnerIdUpdate::new("sharedid.org", "cookie-shared"),
        ];

        let deduped = dedupe_partner_updates(updates);

        assert_eq!(deduped.len(), 2, "should keep one update per partner");
        assert_eq!(
            deduped,
            vec![
                PartnerIdUpdate::new("id5-sync.com", "id5-uid"),
                PartnerIdUpdate::new("sharedid.org", "cookie-shared"),
            ],
            "should keep the last value for duplicate partners"
        );
    }

    #[test]
    fn collect_eid_updates_combines_sources() {
        let registry = make_registry(&["id5-sync.com", "liveramp.com", "sharedid.org"]);
        let module_eids = vec![
            eid("id5-sync.com", &["ID5_abc"]),
            eid("liveramp.com", &["LR_xyz"]),
        ];

        let updates = collect_eid_updates(&module_eids, Some("shared-cookie-id"), None, &registry);

        assert_eq!(
            updates,
            vec![
                PartnerIdUpdate::new("id5-sync.com", "ID5_abc"),
                PartnerIdUpdate::new("liveramp.com", "LR_xyz"),
                PartnerIdUpdate::new("sharedid.org", "shared-cookie-id"),
            ],
            "should collect every matched update in one batch"
        );
    }

    #[test]
    fn collect_eid_updates_preserves_an_opaque_envelope() {
        let registry = make_registry(&["liveramp.com"]);
        let module_eids = vec![eid("liveramp.com", &["opaque-test-envelope"])];

        let updates = collect_eid_updates(&module_eids, None, None, &registry);

        assert_eq!(
            updates,
            vec![PartnerIdUpdate::new("liveramp.com", "opaque-test-envelope")],
            "should preserve the opaque envelope without decoding it"
        );
    }

    #[test]
    fn collect_eid_updates_prefers_direct_sharedid_cookie() {
        let registry = make_registry(&["sharedid.org"]);
        let module_eids = vec![eid("sharedid.org", &["module-shared"])];

        let updates = collect_eid_updates(&module_eids, Some("cookie-shared"), None, &registry);

        assert_eq!(
            updates,
            vec![PartnerIdUpdate::new("sharedid.org", "cookie-shared")],
            "should apply the direct sharedId cookie after the modules' EIDs"
        );
    }

    #[test]
    fn collect_eid_updates_ignores_unknown_sources() {
        let registry = make_registry(&["id5-sync.com"]);
        let module_eids = vec![eid("unknown.example", &["unknown"])];

        let updates = collect_eid_updates(&module_eids, None, None, &registry);

        assert!(
            updates.is_empty(),
            "should ignore unmatched partner updates"
        );
    }
}
