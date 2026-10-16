//! Shared request policy and response reconciliation for authenticated capture handoffs.

use core::fmt;
use std::collections::BTreeMap;

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Response, header};

use crate::consent::{RawConsentSignals, identity_capture_consent_allowed};
use crate::consent_config::ConsentConfig;
use crate::constants::COOKIE_TS_EIDS_SERVER;
use crate::integrations::IntegrationRegistry;
use crate::openrtb::Eid;
use crate::response_privacy::enforce_set_cookie_cache_privacy;
use crate::settings::Settings;

use super::eids::{ResolvedPartnerId, to_eids};
use super::identity::IdentityEffects;
use super::identity_handoff_codec::{self as codec, HANDOFF_TTL_SECS, HandoffEntry};
use super::kv_types::KvPartnerId;
use super::partner::normalize_partner_source_domain;
use super::registry::PartnerRegistry;
use super::{EcContext, EcKvSnapshot, checked_current_timestamp};

/// Authenticated incoming observations, never raw cookie data in diagnostics.
#[derive(Clone, Default)]
pub(crate) struct HandoffState {
    present: bool,
    ec_id: Option<String>,
    entries: Vec<HandoffEntry>,
    consent_config: Option<ConsentConfig>,
    pub(crate) supported: bool,
    pub(crate) proxy_replay_allowed: bool,
    pub(crate) proxy_replay_denied: bool,
}

impl fmt::Debug for HandoffState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HandoffState")
            .field("present", &self.present)
            .field("entries", &self.entries.len())
            .field("supported", &self.supported)
            .finish()
    }
}

impl HandoffState {
    pub(crate) fn from_cookie(
        settings: &Settings,
        ec_id: Option<&str>,
        cookie: Option<&str>,
    ) -> Self {
        let mut state = Self {
            present: cookie.is_some(),
            ..Self::default()
        };
        let Some(value) = cookie else {
            return state;
        };
        let Some(ec_id) = ec_id else {
            return state;
        };
        let Some(now) = checked_current_timestamp() else {
            return state;
        };
        let Some(entries) = codec::decode(
            &settings.ec.identity_handoff,
            &settings.publisher.domain,
            ec_id,
            value,
            now,
        ) else {
            return state;
        };
        state.entries = entries
            .into_iter()
            .filter(|entry| source_allowed(settings, entry))
            .collect();
        state.ec_id = Some(ec_id.to_owned());
        state.consent_config = Some(settings.consent.clone());
        state
    }

    pub(crate) fn matching_capture(
        &self,
        context: &EcContext,
        module: &str,
        source: &str,
        uid: &str,
        now: u64,
    ) -> Option<&HandoffEntry> {
        if !self.allowed(context) {
            return None;
        }
        self.entries.iter().find(|entry| {
            entry.module == module && entry.source == source && entry.uid == uid && live(entry, now)
        })
    }

    fn allowed(&self, context: &EcContext) -> bool {
        self.supported
            && !self.proxy_replay_denied
            && context.ec_was_present()
            && !context.ec_generated()
            && context.ec_value() == self.ec_id.as_deref()
            && self.consent_config.as_ref().is_some_and(|config| {
                identity_capture_consent_allowed(
                    context.consent(),
                    &RawConsentSignals::default(),
                    config,
                )
            })
    }
}

fn source_allowed(settings: &Settings, entry: &HandoffEntry) -> bool {
    settings.ec.partners.iter().any(|partner| {
        normalize_partner_source_domain(&partner.source_domain)
            .ok()
            .as_deref()
            == Some(entry.source.as_str())
            && partner.identity_owner.as_deref() == Some(entry.module.as_str())
    }) && IntegrationRegistry::capture_claim_enabled(settings, &entry.module, &entry.source)
}

fn live(entry: &HandoffEntry, now: u64) -> bool {
    now < entry.deadline && entry.expires_at.is_none_or(|expiry| now < expiry)
}

/// Allows auction fallback for absent or known-expired IDs, not empty records.
///
/// A matching UID needs a refreshed known expiry; omission cannot revive it.
fn stored_source_allows_handoff(
    record: Option<&KvPartnerId>,
    entry: &HandoffEntry,
    now: u64,
) -> bool {
    record.is_none_or(|record| {
        !record.uid.is_empty()
            && record.expires_at.is_some_and(|expiry| now >= expiry)
            && (record.uid != entry.uid || entry.expires_at.is_some_and(|expiry| now < expiry))
    })
}

/// Resolves handoff IDs for absent or expired sources in the current live root.
pub(crate) fn resolve_eids(
    context: &EcContext,
    snapshot: &EcKvSnapshot,
    registry: &PartnerRegistry,
    now: Option<u64>,
) -> Vec<Eid> {
    if !context.identity_handoff.allowed(context) {
        return Vec::new();
    }
    let Some(now) = now else {
        return Vec::new();
    };
    let Some(ec_id) = context.ec_value() else {
        return Vec::new();
    };
    let Some(root) = snapshot.entry_for(ec_id).filter(|root| root.consent.ok) else {
        return Vec::new();
    };
    let ids = context
        .identity_handoff
        .entries
        .iter()
        .filter_map(|entry| {
            let partner = registry.get(&entry.source)?;
            (live(entry, now)
                && partner.bidstream_enabled
                && partner.identity_owner.as_deref() == Some(entry.module.as_str())
                && stored_source_allows_handoff(root.ids.get(&entry.source), entry, now))
            .then(|| ResolvedPartnerId {
                source_domain: entry.source.clone(),
                uid: entry.uid.clone(),
                writer: Some(entry.module.clone()),
                openrtb_atype: partner.openrtb_atype,
            })
        })
        .collect::<Vec<_>>();
    to_eids(&ids)
}

/// Prevents ordinary browser ingestion from stripping a trusted pending record's metadata.
pub(crate) fn protects_source(context: &EcContext, source: &str) -> bool {
    let Some(now) = checked_current_timestamp() else {
        return false;
    };
    context.identity_handoff.allowed(context)
        && context
            .identity_handoff
            .entries
            .iter()
            .any(|entry| entry.source == source && live(entry, now))
}

/// Sets a new capture handoff or reconciles an existing one without opening KV.
///
/// A `NotRead` snapshot may stage replay, but only the post-send mutation can
/// authorize persistence against an existing live row. Auction reads separately
/// require an already loaded live root.
pub(crate) fn finalize_response(
    settings: &Settings,
    partners: &PartnerRegistry,
    context: &mut EcContext,
    response: &mut Response<EdgeBody>,
) {
    if !context.identity_handoff.supported {
        return;
    }
    let withdrawal = response
        .extensions()
        .get::<IdentityEffects>()
        .is_some_and(|effects| effects.withdraws(context))
        || super::consent::ec_consent_withdrawn(context.consent());
    let Some(now) = checked_current_timestamp() else {
        clear_if_present(settings, context, response);
        return;
    };
    if withdrawal
        || !settings.ec.identity_handoff.enabled
        || !context.ec_allowed()
        || context.ec_generated()
    {
        clear_if_present(settings, context, response);
        return;
    }
    let Some(ec_id) = context.ec_value().map(str::to_owned) else {
        clear_if_present(settings, context, response);
        return;
    };
    let new_entries = response
        .extensions()
        .get::<IdentityEffects>()
        .map(|effects| effects.handoff_entries(context, now))
        .unwrap_or_default();
    let mut pending = BTreeMap::new();
    let mut replay = Vec::new();
    if context.identity_handoff.allowed(context) {
        for entry in &context.identity_handoff.entries {
            if !live(entry, now) || !source_allowed(settings, entry) {
                continue;
            }
            let partner_owned = partners.get(&entry.source).is_some_and(|partner| {
                partner.identity_owner.as_deref() == Some(entry.module.as_str())
            });
            if !partner_owned {
                continue;
            }
            let source_absent = match context.kv_snapshot() {
                EcKvSnapshot::Present { .. } => {
                    let Some(root) = context
                        .kv_snapshot()
                        .entry_for(&ec_id)
                        .filter(|root| root.consent.ok)
                    else {
                        continue;
                    };
                    if !stored_source_allows_handoff(root.ids.get(&entry.source), entry, now) {
                        continue;
                    }
                    // An expired record permits auction fallback, not persistence replay.
                    !root.ids.contains_key(&entry.source)
                }
                EcKvSnapshot::Missing { .. } => continue,
                EcKvSnapshot::Failed { .. } => {
                    // A failed lookup cannot authorize replay, but need not
                    // destroy the browser's bounded chance on a later request.
                    pending.insert(entry.source.clone(), entry.clone());
                    continue;
                }
                EcKvSnapshot::NotRead => true,
            };
            pending.insert(entry.source.clone(), entry.clone());
            if source_absent
                && (context.eid_sync_source().is_some()
                    || context.identity_handoff.proxy_replay_allowed)
            {
                replay.push(entry.clone());
            }
        }
    }
    let capture_sources = new_entries
        .iter()
        .map(|entry| entry.source.clone())
        .collect::<Vec<_>>();
    replay.retain(|entry| !capture_sources.contains(&entry.source));
    for entry in new_entries {
        if !live(&entry, now) || !source_allowed(settings, &entry) {
            continue;
        }
        match context.kv_snapshot() {
            EcKvSnapshot::Present { .. } => {
                let Some(root) = context
                    .kv_snapshot()
                    .entry_for(&ec_id)
                    .filter(|root| root.consent.ok)
                else {
                    continue;
                };
                if !stored_source_allows_handoff(root.ids.get(&entry.source), &entry, now) {
                    continue;
                }
            }
            EcKvSnapshot::Missing { .. } | EcKvSnapshot::Failed { .. } => continue,
            EcKvSnapshot::NotRead => {}
        }
        pending.insert(entry.source.clone(), entry);
    }
    if !replay.is_empty() {
        if let Some(effects) = response.extensions_mut().get_mut::<IdentityEffects>() {
            effects.add_handoff_replay(replay);
        } else {
            response
                .extensions_mut()
                .insert(IdentityEffects::handoff_replay(context, replay));
        }
    }
    let entries = pending.into_values().collect::<Vec<_>>();
    if entries.is_empty() {
        clear_if_present(settings, context, response);
        return;
    }
    if let Some(value) = codec::encode(
        &settings.ec.identity_handoff,
        &settings.publisher.domain,
        &ec_id,
        entries,
        now,
    ) && let Some(retained) = codec::decode(
        &settings.ec.identity_handoff,
        &settings.publisher.domain,
        &ec_id,
        &value,
        now,
    ) {
        let max_age = retained
            .iter()
            .map(|entry| entry.deadline)
            .max()
            .unwrap_or(now)
            .saturating_sub(now)
            .min(HANDOFF_TTL_SECS);
        append_cookie(settings, response, &value, max_age);
    } else {
        clear_if_present(settings, context, response);
    }
}

fn clear_if_present(
    settings: &Settings,
    context: &mut EcContext,
    response: &mut Response<EdgeBody>,
) {
    if context.identity_handoff.present {
        append_cookie(settings, response, "", 0);
    }
    context.identity_handoff.entries.clear();
}

fn append_cookie(
    settings: &Settings,
    response: &mut Response<EdgeBody>,
    value: &str,
    max_age: u64,
) {
    let cookie = format!(
        "{COOKIE_TS_EIDS_SERVER}={value}; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}",
        settings.publisher.ec_cookie_domain()
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
        enforce_set_cookie_cache_privacy(response);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
}

#[cfg(test)]
#[path = "identity_handoff_tests.rs"]
mod tests;
