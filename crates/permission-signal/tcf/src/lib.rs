//! IAB TCF v2 as a permission signal module.
//!
//! Reads the TC String from the `euconsent-v2` cookie and the EU section of
//! the request's GPP string, decodes them, settles which record is in force
//! when both are present, expires a record older than the settings allow, and
//! answers from that record for the purposes this crate maps to each
//! permission. It is the one place that knows what a TCF purpose is.
//!
//! The record is read once, when the consent context is built, and every
//! reader of the request sees the same record, so the module answers the same
//! as the bid request builder and the identity graph about expiry and which
//! string is in force.
//!
//! This module lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

mod decode;
mod mapping;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub use decode::{TcfConsent, TcfDecodeError, decode_tc_string};
pub use mapping::purpose_for;

use serde::Deserialize;
use trusted_server_core::consent::{ConsentContext, SignalRecord, SignalStatus};
use trusted_server_core::consent_config::ConsentMode;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permission_signal::{PermissionSignalModule, SignalInput};
use trusted_server_core::permissions::{
    ConsentSignal, Permission, PermissionSet, SignalPolicy, ValidSignal,
};
use trusted_server_core::settings::Settings;
use trusted_server_permission_signal_gpp as gpp;
use web_time::{SystemTime, UNIX_EPOCH};

/// The name `[permission-signal] modules` selects this module by, from its
/// crate folder.
#[must_use]
pub fn name() -> &'static str {
    trusted_server_core::module_name!()
}

/// The name the page is told a signal came from, being the name without the
/// type folder.
fn short() -> &'static str {
    trusted_server_core::module_name::short_form(
        trusted_server_core::permission_signal::MODULE_TYPE,
        name(),
    )
}

/// The scheme, as a [`ValidSignal`] and a [`SignalRecord`] name it, and as
/// `signals.tcf` in the permission policy names it.
pub const SCHEME: &str = "tcf";

/// The cookie carrying the TC String.
pub const COOKIE_EUCONSENT_V2: &str = "euconsent-v2";

/// The identifier of the EU TCF v2 section in a GPP string.
pub const GPP_SECTION_ID: u16 = 2;

/// Deciseconds in one day, the unit a TC String keeps its timestamps in.
const DECISECONDS_PER_DAY: u64 = 86_400 * 10;

static SETTINGS_WARNING_LOGGED: AtomicBool = AtomicBool::new(false);

/// What this module reads from the `[consent]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TcfSettings {
    /// Whether a record older than `max_consent_age_days` is treated as
    /// expired.
    pub check_expiration: bool,
    /// The oldest a record may be, in days. The specification recommends
    /// 13 months, and a value outside `1..=3650` is clamped.
    pub max_consent_age_days: u32,
    /// How a standalone TC String and the EU section of a GPP string are
    /// settled when both are present and disagree.
    pub conflict_resolution: ConflictResolution,
}

impl Default for TcfSettings {
    fn default() -> Self {
        Self {
            check_expiration: true,
            max_consent_age_days: 395,
            conflict_resolution: ConflictResolution::default(),
        }
    }
}

impl TcfSettings {
    /// The longest a record may live, in deciseconds, with the days clamped
    /// to the range the setting accepts.
    fn max_age_deciseconds(&self) -> u64 {
        u64::from(self.max_consent_age_days.clamp(1, 3650)) * DECISECONDS_PER_DAY
    }
}

/// The `conflict_resolution` table of the `[consent]` settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ConflictResolution {
    /// Which record wins when the two disagree.
    pub mode: ConflictMode,
    /// How many days newer one record must be to win under `newest`.
    pub freshness_threshold_days: u32,
}

impl Default for ConflictResolution {
    fn default() -> Self {
        Self {
            mode: ConflictMode::Restrictive,
            freshness_threshold_days: 30,
        }
    }
}

/// How two disagreeing records are settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictMode {
    /// The record that denies wins.
    Restrictive,
    /// The newer record wins, when it is newer by the freshness threshold,
    /// otherwise the record that denies.
    Newest,
    /// The record that grants wins.
    Permissive,
}

/// What this module stores in the consent context for a decoded request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcfRecord {
    /// The record in force: the standalone TC String's, or the EU section's
    /// of a GPP string, after the settings' conflict rule.
    pub effective: TcfConsent,
}

/// The TC String the request carried, as received, when this module read
/// one and vouched for it.
#[must_use]
pub fn tc_string(consent: &ConsentContext) -> Option<&str> {
    consent.value(SCHEME)
}

/// Whether the request carries an EU TCF signal, as a standalone TC String
/// or as the EU section of a GPP string, which is what a bid request carries
/// as the regulation's applicability flag.
#[must_use]
pub fn applies(consent: &ConsentContext) -> bool {
    consent.record(SCHEME).is_some_and(SignalRecord::applies)
}

/// The record in force for the request, when one decoded and has not
/// expired.
#[must_use]
pub fn effective(consent: &ConsentContext) -> Option<&TcfConsent> {
    consent
        .decoded::<TcfRecord>(SCHEME)
        .map(|record| &record.effective)
}

/// The record this module reports for a decoded TC String, for a caller that
/// builds a consent context without a request. `raw` is the string as
/// received, or `None` for a record carried in a GPP string.
#[must_use]
pub fn record(effective: TcfConsent, raw: Option<&str>) -> SignalRecord {
    let record = SignalRecord::new(SCHEME, SignalStatus::Decoded)
        .with_applies(true)
        .with_consented(consented(&effective))
        .with_decoded(TcfRecord { effective });
    match raw {
        Some(raw) => record.with_value(raw),
        None => record,
    }
}

/// TCF v2, when the policy says TCF answers for this deployment.
///
/// The mapping from permission to purpose is this crate's, in
/// [`purpose_for`], so core carries no table of another scheme's numbers. A
/// permission no purpose maps to gets silence, not a refusal.
#[derive(Debug, Default, Clone, Copy)]
pub struct TcfModule;

impl TcfModule {
    /// A new module.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The module's settings from the `[consent]` table, or the defaults
    /// when the table cannot be read as them, which is said once in the log.
    fn settings(settings: &Settings) -> TcfSettings {
        match settings.consent.read::<TcfSettings>() {
            Ok(read) => read,
            Err(error) => {
                if !SETTINGS_WARNING_LOGGED.swap(true, Ordering::Relaxed) {
                    log::warn!(
                        "The TCF module cannot read its settings and is using its defaults: {error:?}"
                    );
                }
                TcfSettings::default()
            }
        }
    }

    /// Reads the TC String and the EU section of the GPP string from the
    /// request and reports the record in force.
    fn read_record(&self, evidence: &dyn RequestInfo, settings: &Settings) -> Option<SignalRecord> {
        let raw_tc = evidence.cookie(COOKIE_EUCONSENT_V2);
        let (raw_gpp, raw_sid) = gpp::strings(evidence);
        let section_ids = gpp::section_ids_of(raw_gpp, raw_sid);
        let in_gpp = section_ids.is_some_and(|ids| ids.contains(&GPP_SECTION_ID));
        let applies = raw_tc.is_some() || in_gpp;
        let with_value = |record: SignalRecord| match raw_tc {
            Some(raw) => record.with_value(raw),
            None => record,
        };
        let unreadable = || {
            Some(with_value(
                SignalRecord::new(SCHEME, SignalStatus::Unreadable).with_applies(applies),
            ))
        };

        if settings.consent.mode == ConsentMode::Proxy {
            return match raw_tc {
                Some(_) => Some(with_value(
                    SignalRecord::new(SCHEME, SignalStatus::NotDecoded).with_applies(applies),
                )),
                None => applies
                    .then(|| SignalRecord::new(SCHEME, SignalStatus::Absent).with_applies(true)),
            };
        }

        let tcf_settings = Self::settings(settings);
        let standalone = match raw_tc.map(decode_tc_string) {
            None => None,
            Some(Ok(decoded)) => Some(decoded),
            Some(Err(error)) => {
                log::warn!("Failed to decode TC String: {error}");
                return unreadable();
            }
        };
        // The EU section a GPP string carries, or its hint names, is a TC
        // String too, so one that cannot be read is unreadable as a
        // standalone string is, whatever the standalone string says.
        let embedded = match raw_gpp {
            Some(raw) if in_gpp => {
                match gpp::section(raw, GPP_SECTION_ID).map(|section| decode_tc_string(&section)) {
                    Some(Ok(decoded)) => Some(decoded),
                    Some(Err(error)) => {
                        log::warn!(
                            "GPP contains an EU TCF section that could not be decoded: {error}"
                        );
                        return unreadable();
                    }
                    None => {
                        log::warn!(
                            "The EU TCF section the GPP string names could not be read from it"
                        );
                        return unreadable();
                    }
                }
            }
            _ => None,
        };
        let effective = match (standalone, embedded) {
            (Some(standalone), Some(embedded)) => {
                resolve_conflict(standalone, embedded, &tcf_settings.conflict_resolution)
            }
            (Some(standalone), None) => standalone,
            (None, Some(embedded)) => embedded,
            (None, None) => {
                return applies
                    .then(|| SignalRecord::new(SCHEME, SignalStatus::Absent).with_applies(true));
            }
        };
        if tcf_settings.check_expiration {
            let age = now_deciseconds().saturating_sub(effective.last_updated_ds);
            if age > tcf_settings.max_age_deciseconds() {
                log::warn!(
                    "TCF consent expired: age_days={}",
                    age / DECISECONDS_PER_DAY
                );
                return Some(with_value(
                    SignalRecord::new(SCHEME, SignalStatus::Expired).with_applies(applies),
                ));
            }
        }
        Some(with_value(
            SignalRecord::new(SCHEME, SignalStatus::Decoded)
                .with_applies(applies)
                .with_consented(consented(&effective))
                .with_decoded(TcfRecord { effective }),
        ))
    }

    /// The signal this scheme vouches for: the standalone TC String, when it
    /// decoded and has not expired. A record carried inside a GPP string is
    /// the GPP string's, which the GPP module vouches for.
    fn read_signal(&self, consent: &ConsentContext) -> Option<ValidSignal> {
        let record = consent.record(SCHEME)?;
        if record.status() != SignalStatus::Decoded {
            return None;
        }
        Some(ValidSignal::new(short(), SCHEME, record.value()?))
    }
}

impl PermissionSignalModule for TcfModule {
    fn id(&self) -> &'static str {
        name()
    }

    fn read(&self, call: ModuleCall<'_>) -> Option<SignalRecord> {
        call.inject(self, Self::read_record).ok().flatten()
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        // The policy still says whether a TCF record answers for this
        // deployment at all. What it does not say is which purpose grants
        // which permission, because that is this scheme's own knowledge.
        if !input.policy.authoritative(SCHEME) {
            return ConsentSignal::Neutral;
        }
        let Some(purpose) = mapping::purpose_for(permission) else {
            // TCF has nothing to say about this Data Use, so it says nothing.
            return ConsentSignal::Neutral;
        };
        let Some(record) = input.consent.record(SCHEME) else {
            // No TCF record on the request. Silence, not refusal, because
            // reading an absent scheme as a refusal would revoke on every
            // request that did not carry it.
            return ConsentSignal::Neutral;
        };
        if matches!(
            record.status(),
            SignalStatus::Unreadable | SignalStatus::NotDecoded
        ) {
            // A TC String arrived and could not be read, or was left as
            // received. The visitor expressed a preference this module cannot
            // see, which is not the same as no preference, so it fails closed
            // on everything it maps rather than leaving the place baseline
            // standing. An expired record is not this case, expiry being its
            // own explicit state.
            return ConsentSignal::Revoke;
        }
        let Some(effective) = record.decoded::<TcfRecord>() else {
            return ConsentSignal::Neutral;
        };
        if effective
            .effective
            .has_purpose_consent(usize::from(purpose))
        {
            ConsentSignal::Grant
        } else {
            // A purpose the visitor did not consent to is a refusal. Reading it
            // as silence would leave the country baseline standing and grant
            // what they declined.
            ConsentSignal::Revoke
        }
    }

    /// Every Data Use a purpose maps to, when the policy lets a record answer,
    /// and nothing when it does not, because a silenced record grants nothing
    /// and a page must not wait for it.
    fn grants(&self, policy: &SignalPolicy) -> PermissionSet {
        if !policy.authoritative(SCHEME) {
            return PermissionSet::none();
        }
        mapped()
    }

    fn valid_signal(&self, call: ModuleCall<'_>) -> Option<ValidSignal> {
        call.inject(self, Self::read_signal).ok().flatten()
    }

    /// Only a TCF record refusing storage withdraws, because only TCF records
    /// a visitor declining the very signal storage depended on. A US-style
    /// opt-out suppresses use for the request and never destroys an identifier,
    /// so the other modules leave this at its default.
    ///
    /// Whether the refusal is destructive at all is core's to decide from the
    /// jurisdiction's storage baseline, which is why this answers the narrow
    /// question only. It does not consult the policy's authoritative flag, so
    /// a record refusing storage withdraws whether or not the policy lets the
    /// record grant anything.
    fn withdraws(&self, permission: Permission, input: &SignalInput<'_>) -> bool {
        if permission != Permission::StoreOnDevice {
            return false;
        }
        effective(input.consent).is_some_and(|record| !record.has_storage_consent())
    }
}

/// Every Data Use a purpose maps to, computed once rather than on every
/// request, because the mapping is a constant of this crate.
fn mapped() -> PermissionSet {
    static MAPPED: OnceLock<PermissionSet> = OnceLock::new();
    *MAPPED.get_or_init(|| {
        Permission::all()
            .filter(|permission| mapping::purpose_for(*permission).is_some())
            .collect()
    })
}

/// Every Data Use whose purpose `record` consents to. The record reports
/// these whatever the policy makes of it, so a reader that asks for the
/// visitor's consent itself, as the server-side auction gate does, sees it.
fn consented(record: &TcfConsent) -> PermissionSet {
    mapped()
        .iter()
        .filter(|permission| {
            mapping::purpose_for(*permission)
                .is_some_and(|purpose| record.has_purpose_consent(usize::from(purpose)))
        })
        .collect()
}

/// Whether a record allows identifiers to travel in the bidstream, which is
/// what the two records are compared on when they disagree.
fn allows_eid_transmission(record: &TcfConsent) -> bool {
    record.has_storage_consent() && record.has_personalized_ads_consent()
}

/// Settles a standalone TC String against the EU section of a GPP string.
fn resolve_conflict(
    standalone: TcfConsent,
    embedded: TcfConsent,
    rule: &ConflictResolution,
) -> TcfConsent {
    let standalone_allows = allows_eid_transmission(&standalone);
    let embedded_allows = allows_eid_transmission(&embedded);
    if standalone_allows == embedded_allows {
        return standalone;
    }
    let select_embedded = match rule.mode {
        ConflictMode::Restrictive => !embedded_allows,
        ConflictMode::Permissive => embedded_allows,
        // When neither is newer by the threshold, the record that denies
        // wins.
        ConflictMode::Newest => select_newer(&standalone, &embedded, rule.freshness_threshold_days)
            .unwrap_or(!embedded_allows),
    };
    log::info!(
        "TCF conflict detected; mode={:?}, selected={}",
        rule.mode,
        if select_embedded { "gpp" } else { "standalone" }
    );
    if select_embedded {
        embedded
    } else {
        standalone
    }
}

/// Whether the embedded record wins under `newest`: `Some(true)` when it is
/// newer than the standalone by the threshold, `Some(false)` when the
/// standalone is, and `None` when neither is.
fn select_newer(
    standalone: &TcfConsent,
    embedded: &TcfConsent,
    threshold_days: u32,
) -> Option<bool> {
    let threshold_ds = u64::from(threshold_days) * DECISECONDS_PER_DAY;
    if standalone
        .last_updated_ds
        .saturating_sub(embedded.last_updated_ds)
        > threshold_ds
    {
        return Some(false);
    }
    if embedded
        .last_updated_ds
        .saturating_sub(standalone.last_updated_ds)
        > threshold_ds
    {
        return Some(true);
    }
    None
}

/// Now, in deciseconds since the Unix epoch, the unit a TC String keeps its
/// timestamps in.
fn now_deciseconds() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_secs() * 10 + u64::from(duration.subsec_millis()) / 100
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::*;
    use crate::decode::tests::encode_tc_string;
    use trusted_server_core::consent::ConsentContext;
    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::permission_signal::SignalInput;
    use trusted_server_core::permissions::{Acquisition, PermissionMaps, SignalPolicy};

    /// The repository's sample policy, under which a TCF record answers.
    fn sample_policy() -> &'static SignalPolicy {
        PermissionMaps::sample().signals()
    }

    /// A decoded record consenting to the given 1-indexed purposes, with
    /// everything else refused.
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

    fn with_record(consented: &[usize]) -> ConsentContext {
        ConsentContext::default().with_record(record(tcf_with_purposes(consented), None))
    }

    fn with_status(status: SignalStatus) -> ConsentContext {
        ConsentContext::default()
            .with_record(SignalRecord::new(SCHEME, status).with_value("CPsomething"))
    }

    fn answer(
        consent: &ConsentContext,
        policy: &SignalPolicy,
        permission: Permission,
    ) -> ConsentSignal {
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(consent, &evidence, policy, Acquisition::RequiresSignal);
        TcfModule::new().signal(permission, &input)
    }

    fn withdraws(consent: &ConsentContext, policy: &SignalPolicy, permission: Permission) -> bool {
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(consent, &evidence, policy, Acquisition::RequiresSignal);
        TcfModule::new().withdraws(permission, &input)
    }

    fn evidence(cookie: &str) -> OwnedRequestInfo {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            cookie.parse().expect("should build a cookie header"),
        );
        OwnedRequestInfo::new(String::new(), headers)
    }

    fn read_with(cookie: &str, settings: &Settings) -> Option<SignalRecord> {
        TcfModule::new().read_record(&evidence(cookie), settings)
    }

    /// A TC String consenting to purposes 1 and 4, last updated now.
    fn fresh_tc_string() -> String {
        encode_tc_string(&[true, false, false, true], now_deciseconds())
    }

    fn with_consent_settings(json: serde_json::Value) -> Settings {
        let mut settings = Settings::default();
        settings.consent = serde_json::from_value(json).expect("should build the consent table");
        settings
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(
            TcfModule::new().id(),
            name(),
            "the module answers to the identifier configuration names"
        );
    }

    #[test]
    fn reads_and_decodes_the_tc_string_from_the_request() {
        let tc = fresh_tc_string();
        let record =
            read_with(&format!("euconsent-v2={tc}"), &Settings::default()).expect("should read");
        assert_eq!(record.status(), SignalStatus::Decoded);
        assert_eq!(record.value(), Some(tc.as_str()));
        assert!(record.applies(), "a TC String says the regulation applies");
        let decoded = record
            .decoded::<TcfRecord>()
            .expect("should carry the record");
        assert!(decoded.effective.has_purpose_consent(1));
        assert!(decoded.effective.has_purpose_consent(4));
        assert!(!decoded.effective.has_purpose_consent(2));
        assert!(
            read_with("other=1", &Settings::default()).is_none(),
            "no string and no section, no record"
        );
    }

    #[test]
    fn a_decoded_record_reports_what_its_purposes_consent_to() {
        let tc = encode_tc_string(&[true], now_deciseconds());
        let read =
            read_with(&format!("euconsent-v2={tc}"), &Settings::default()).expect("should read");
        assert_eq!(
            read.consented(),
            PermissionSet::none().with(Permission::StoreOnDevice),
            "Purpose 1 alone is consent to device storage and to nothing else"
        );
        let consent = ConsentContext::default().with_record(read);
        assert_eq!(
            answer(
                &consent,
                &SignalPolicy::default(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a policy that silences the record leaves the module silent"
        );
        assert!(
            consent
                .record(SCHEME)
                .is_some_and(|held| held.consented().contains(Permission::StoreOnDevice)),
            "while the record still holds the consent the visitor gave"
        );
        let contextual = record(tcf_with_purposes(&[2]), None).consented();
        assert!(
            contextual.contains(Permission::SelectBasicAds)
                && !contextual.contains(Permission::StoreOnDevice),
            "Purpose 2 alone is consent to basic ad selection and not to storage"
        );
        let old = encode_tc_string(&[true], 0);
        let expired =
            read_with(&format!("euconsent-v2={old}"), &Settings::default()).expect("should read");
        assert!(
            expired.consented().is_empty(),
            "an expired record holds no consent"
        );
    }

    #[test]
    fn an_unreadable_tc_string_is_recorded_as_such() {
        let record =
            read_with("euconsent-v2=not-a-tc-string", &Settings::default()).expect("should read");
        assert_eq!(record.status(), SignalStatus::Unreadable);
        assert_eq!(record.value(), Some("not-a-tc-string"));
        assert!(record.applies());
        assert!(record.decoded::<TcfRecord>().is_none());
    }

    #[test]
    fn a_stale_tc_string_is_recorded_as_expired() {
        let old = encode_tc_string(&[true], 0);
        let record =
            read_with(&format!("euconsent-v2={old}"), &Settings::default()).expect("should read");
        assert_eq!(record.status(), SignalStatus::Expired);
        assert_eq!(
            record.value(),
            Some(old.as_str()),
            "the string stays on record"
        );
        assert!(
            record.decoded::<TcfRecord>().is_none(),
            "and the record is cleared"
        );

        let unchecked = with_consent_settings(serde_json::json!({ "check_expiration": false }));
        let record = read_with(&format!("euconsent-v2={old}"), &unchecked).expect("should read");
        assert_eq!(
            record.status(),
            SignalStatus::Decoded,
            "with expiry unchecked the record stands"
        );
    }

    #[test]
    fn the_eu_section_of_a_gpp_string_is_read_when_no_tc_string_arrived() {
        // Header "DBACNY" carries sections 2 and 6, and section 2 is a TC
        // String consenting to purposes 1 and 4.
        let tc = fresh_tc_string();
        let record = read_with(&format!("__gpp=DBACNY~{tc}~1YNN"), &Settings::default())
            .expect("should read");
        assert_eq!(record.status(), SignalStatus::Decoded);
        assert_eq!(record.value(), None, "the string is the GPP module's");
        assert!(record.applies(), "section 2 says the regulation applies");
        let decoded = record
            .decoded::<TcfRecord>()
            .expect("should carry the record");
        assert!(decoded.effective.has_purpose_consent(4));

        // With a standalone string beside it, the restrictive default keeps
        // the record that denies.
        let denying = encode_tc_string(&[true], now_deciseconds());
        let record = read_with(
            &format!("euconsent-v2={denying}; __gpp=DBACNY~{tc}~1YNN"),
            &Settings::default(),
        )
        .expect("should read");
        assert_eq!(record.value(), Some(denying.as_str()));
        let decoded = record
            .decoded::<TcfRecord>()
            .expect("should carry the record");
        assert!(
            !decoded.effective.has_purpose_consent(4),
            "the standalone record denies and the restrictive rule keeps it"
        );
    }

    #[test]
    fn a_section_hint_naming_the_eu_section_says_the_regulation_applies() {
        let record = read_with("__gpp_sid=2,6", &Settings::default()).expect("should read");
        assert_eq!(record.status(), SignalStatus::Absent);
        assert!(record.applies());
        assert!(
            read_with("__gpp_sid=7", &Settings::default()).is_none(),
            "a hint naming no EU section is nothing for this module"
        );
    }

    #[test]
    fn a_tc_string_that_cannot_be_read_consents_to_nothing_it_governs() {
        let read =
            read_with("euconsent-v2=not-a-tc-string", &Settings::default()).expect("should read");
        assert!(read.consented().is_empty(), "the record holds no consent");
        let consent = ConsentContext::default().with_record(read);
        for permission in mapped().iter() {
            assert_eq!(
                answer(&consent, sample_policy(), permission),
                ConsentSignal::Revoke,
                "every purpose the module maps is refused, {} included",
                permission.as_str()
            );
        }
    }

    #[test]
    fn an_eu_section_in_a_gpp_string_that_cannot_be_read_is_unreadable() {
        let settings = Settings::default();
        // Header "DBACNY" carries sections 2 and 6, and section 2 here is not
        // a TC String.
        let unreadable_section = "__gpp=DBACNY~not-a-tc-string~1YNN";
        let read = read_with(unreadable_section, &settings).expect("should read");
        assert_eq!(read.status(), SignalStatus::Unreadable);
        assert!(read.applies(), "section 2 says the regulation applies");
        let read =
            read_with("__gpp=not-a-gpp-string; __gpp_sid=2,6", &settings).expect("should read");
        assert_eq!(
            read.status(),
            SignalStatus::Unreadable,
            "so is a string the hint says carries the section and that cannot be parsed"
        );
        let tc = fresh_tc_string();
        let read = read_with(
            &format!("euconsent-v2={tc}; {unreadable_section}"),
            &settings,
        )
        .expect("should read");
        assert_eq!(
            read.status(),
            SignalStatus::Unreadable,
            "whatever a readable standalone string beside it says"
        );
        let consent = ConsentContext::default().with_record(read);
        for permission in mapped().iter() {
            assert_eq!(
                answer(&consent, sample_policy(), permission),
                ConsentSignal::Revoke,
                "every purpose the module maps is refused, {} included",
                permission.as_str()
            );
        }
        assert_eq!(
            TcfModule::new().read_signal(&consent),
            None,
            "and the standalone string is not vouched for"
        );
        assert_eq!(
            read_with("__gpp_sid=2", &settings)
                .as_ref()
                .map(SignalRecord::status),
            Some(SignalStatus::Absent),
            "a hint with no string beside it leaves nothing to read"
        );
    }

    #[test]
    fn the_auction_gate_reads_the_record_this_module_reports() {
        use trusted_server_core::consent::consent_allows_server_side_auction;
        use trusted_server_core::consent::jurisdiction::Jurisdiction;

        let settings = Settings::default();
        let gate = |cookie: &str, jurisdiction: Jurisdiction| {
            let consent = ConsentContext::new(jurisdiction);
            let consent = match read_with(cookie, &settings) {
                Some(read) => consent.with_record(read),
                None => consent,
            };
            consent_allows_server_side_auction(&consent)
        };
        let california = || Jurisdiction::UsState("CA".to_owned());
        let storage_only = encode_tc_string(&[true], now_deciseconds());
        let contextual_only = encode_tc_string(&[false, true], now_deciseconds());
        let expired = encode_tc_string(&[true], 0);
        assert!(
            gate(&format!("euconsent-v2={storage_only}"), Jurisdiction::Gdpr),
            "Purpose 1 alone runs the auction in a GDPR place"
        );
        assert!(
            !gate(
                &format!("euconsent-v2={contextual_only}"),
                Jurisdiction::Gdpr
            ),
            "Purpose 2 alone does not"
        );
        assert!(
            !gate(&format!("euconsent-v2={expired}"), california()),
            "an expired string shuts the gate outside GDPR too"
        );
        assert!(
            !gate("__gpp_sid=2", california()),
            "so does an EU section named only by a hint"
        );
        assert!(
            !gate("euconsent-v2=not-a-tc-string", california()),
            "and a TC String that cannot be read"
        );
        assert!(
            !gate("__gpp=DBACNY~not-a-tc-string~1YNN", california()),
            "and an EU section of a GPP string that cannot be read"
        );
        assert!(
            gate("other=1", california()),
            "a US state with no EU signal runs it"
        );
    }

    #[test]
    fn proxy_mode_leaves_the_string_as_received() {
        let tc = fresh_tc_string();
        let settings = with_consent_settings(serde_json::json!({ "mode": "proxy" }));
        let record = read_with(&format!("euconsent-v2={tc}"), &settings).expect("should read");
        assert_eq!(record.status(), SignalStatus::NotDecoded);
        assert_eq!(record.value(), Some(tc.as_str()));
        assert!(record.applies());
        let record = read_with("__gpp_sid=2", &settings).expect("should read");
        assert_eq!(record.status(), SignalStatus::Absent);
        assert!(record.applies());
    }

    #[test]
    fn unreadable_settings_fall_back_to_the_defaults() {
        let settings = with_consent_settings(serde_json::json!({ "max_consent_age_days": "x" }));
        let old = encode_tc_string(&[true], 0);
        let record = read_with(&format!("euconsent-v2={old}"), &settings).expect("should read");
        assert_eq!(
            record.status(),
            SignalStatus::Expired,
            "the default expiry check still runs"
        );
    }

    #[test]
    fn settings_read_the_table_and_clamp_the_age() {
        let settings = with_consent_settings(serde_json::json!({
            "check_expiration": false,
            "max_consent_age_days": 9999,
            "conflict_resolution": { "mode": "newest", "freshness_threshold_days": 15 },
            "us_privacy_defaults": { "notice_given": false }
        }));
        let read = TcfModule::settings(&settings);
        assert!(!read.check_expiration);
        assert_eq!(read.max_consent_age_days, 9999);
        assert_eq!(read.max_age_deciseconds(), 3650 * DECISECONDS_PER_DAY);
        assert_eq!(read.conflict_resolution.mode, ConflictMode::Newest);
        assert_eq!(read.conflict_resolution.freshness_threshold_days, 15);
        assert_eq!(
            TcfModule::settings(&Settings::default()),
            TcfSettings::default()
        );
    }

    fn allowing(last_updated_ds: u64, allows_eids: bool) -> TcfConsent {
        let mut record = tcf_with_purposes(if allows_eids { &[1, 4] } else { &[1] });
        record.last_updated_ds = last_updated_ds;
        record
    }

    #[test]
    fn conflict_resolution_restrictive_prefers_denial() {
        let settled = resolve_conflict(
            allowing(10, true),
            allowing(20, false),
            &ConflictResolution::default(),
        );
        assert!(
            !settled.has_personalized_ads_consent(),
            "restrictive mode should choose the denying record"
        );
    }

    #[test]
    fn conflict_resolution_permissive_prefers_grant() {
        let rule = ConflictResolution {
            mode: ConflictMode::Permissive,
            ..ConflictResolution::default()
        };
        let settled = resolve_conflict(allowing(10, true), allowing(20, false), &rule);
        assert!(
            settled.has_personalized_ads_consent(),
            "permissive mode should choose the granting record"
        );
    }

    #[test]
    fn conflict_resolution_newest_prefers_fresher_record() {
        let rule = ConflictResolution {
            mode: ConflictMode::Newest,
            freshness_threshold_days: 1,
        };
        let settled = resolve_conflict(
            allowing(2 * DECISECONDS_PER_DAY, true),
            allowing(0, false),
            &rule,
        );
        assert!(
            settled.has_personalized_ads_consent(),
            "newest mode should pick the record that is newer than the threshold"
        );
        let settled = resolve_conflict(allowing(0, true), allowing(0, false), &rule);
        assert!(
            !settled.has_personalized_ads_consent(),
            "neither newer by the threshold, so the denying record wins"
        );
    }

    #[test]
    fn records_that_agree_keep_the_standalone() {
        let settled = resolve_conflict(
            allowing(10, true),
            allowing(20, true),
            &ConflictResolution::default(),
        );
        assert_eq!(settled.last_updated_ds, 10);
    }

    #[test]
    fn grants_a_permission_whose_purpose_the_record_consents_to() {
        assert_eq!(
            answer(
                &with_record(&[1]),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Grant,
            "Purpose 1 consent grants device storage"
        );
    }

    #[test]
    fn revokes_a_permission_whose_purpose_the_record_refuses() {
        assert_eq!(
            answer(
                &with_record(&[1]),
                sample_policy(),
                Permission::SelectPersonalisedAds
            ),
            ConsentSignal::Revoke,
            "a purpose the visitor did not consent to is a refusal, not silence"
        );
    }

    #[test]
    fn is_silent_for_a_data_use_no_purpose_grants() {
        let sale = Permission::from_identifier("disclosure.sale")
            .expect("should be a known Data Use with no TCF purpose");
        assert_eq!(
            answer(&with_record(&[1]), sample_policy(), sale),
            ConsentSignal::Neutral,
            "TCF has nothing to say about a Data Use none of its purposes grant"
        );
    }

    #[test]
    fn is_silent_when_no_record_arrived() {
        assert_eq!(
            answer(
                &ConsentContext::default(),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "an absent record is silence, never a refusal"
        );
    }

    #[test]
    fn an_unreadable_tc_string_is_this_modules_refusal_and_an_expired_one_is_not() {
        let unreadable = with_status(SignalStatus::Unreadable);
        assert_eq!(
            answer(&unreadable, sample_policy(), Permission::StoreOnDevice),
            ConsentSignal::Revoke,
            "an unreadable record refuses the Data Uses this scheme covers"
        );
        let email = Permission::all()
            .find(|permission| permission.as_str() == "advertising_marketing.communications.email")
            .expect("the taxonomy should carry the email channel");
        assert_eq!(
            answer(&unreadable, sample_policy(), email),
            ConsentSignal::Neutral,
            "and says nothing about a Data Use no purpose maps to"
        );
        assert_eq!(
            answer(
                &unreadable,
                &SignalPolicy::default(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a silenced scheme says nothing, readable or not"
        );
        assert_eq!(
            answer(
                &with_status(SignalStatus::NotDecoded),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Revoke,
            "a string left as received is a preference this module cannot see"
        );
        assert_eq!(
            answer(
                &with_status(SignalStatus::Expired),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "expiry is its own explicit state and not an unreadable record"
        );
    }

    #[test]
    fn vouches_for_the_standalone_tc_string_only_when_it_decoded() {
        let decoded = ConsentContext::default()
            .with_record(record(tcf_with_purposes(&[1]), Some("CPreadable")));
        assert_eq!(
            TcfModule::new().read_signal(&decoded),
            Some(ValidSignal::new(short(), SCHEME, "CPreadable")),
            "a decoded record vouches for the string as received"
        );
        assert_eq!(tc_string(&decoded), Some("CPreadable"));
        assert!(applies(&decoded));
        assert_eq!(
            TcfModule::new().read_signal(&with_status(SignalStatus::Unreadable)),
            None,
            "an unreadable string is not vouched for"
        );
        assert_eq!(
            TcfModule::new().read_signal(&with_status(SignalStatus::Expired)),
            None,
            "an expired record is not one this module uses, so it is not vouched for"
        );
        assert_eq!(
            TcfModule::new().read_signal(&with_record(&[1])),
            None,
            "a record carried in a GPP string is the GPP string's to vouch for"
        );
        assert_eq!(
            TcfModule::new().read_signal(&ConsentContext::default()),
            None,
            "and no record is no signal"
        );
        assert!(!applies(&ConsentContext::default()));
    }

    #[test]
    fn declares_exactly_the_data_uses_a_purpose_maps_to() {
        let declared = TcfModule::new().grants(sample_policy());
        assert!(
            declared.contains(Permission::StoreOnDevice),
            "purpose 1 maps to storage, so storage is grantable"
        );
        let email = Permission::all()
            .find(|permission| permission.as_str() == "advertising_marketing.communications.email")
            .expect("the taxonomy should carry the email channel");
        assert!(
            !declared.contains(email),
            "no purpose maps to a marketing channel, so it is not grantable"
        );
        assert_eq!(
            declared.iter().count(),
            Permission::all()
                .filter(|permission| purpose_for(*permission).is_some())
                .count(),
            "should declare each mapped Data Use once and nothing else"
        );
        assert!(
            TcfModule::new().grants(&SignalPolicy::default()).is_empty(),
            "a silenced record grants nothing"
        );
    }

    #[test]
    fn a_non_authoritative_policy_silences_the_record_but_not_the_withdrawal() {
        let silenced = SignalPolicy::default();
        assert!(
            !silenced.authoritative(SCHEME),
            "the fixture must not be authoritative"
        );
        assert_eq!(
            answer(
                &with_record(&[4]),
                &silenced,
                Permission::SelectPersonalisedAds
            ),
            ConsentSignal::Neutral,
            "a record the policy does not let answer stays silent"
        );
        assert!(
            withdraws(&with_record(&[4]), &silenced, Permission::StoreOnDevice),
            "but a record refusing storage still withdraws"
        );
    }

    #[test]
    fn withdraws_only_for_storage_and_only_when_refused() {
        assert!(
            withdraws(
                &with_record(&[4]),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            "refusing Purpose 1 withdraws storage"
        );
        assert!(
            !withdraws(
                &with_record(&[1]),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            "consenting to Purpose 1 is not a withdrawal"
        );
        assert!(
            !withdraws(
                &with_record(&[]),
                sample_policy(),
                Permission::SelectPersonalisedAds
            ),
            "no other permission is ever withdrawn, refused or not"
        );
        assert!(
            !withdraws(
                &ConsentContext::default(),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            "and no record is never a withdrawal"
        );
        assert!(
            !withdraws(
                &with_status(SignalStatus::Expired),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            "an expired record is no record"
        );
    }
}
