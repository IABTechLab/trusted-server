//! The US Privacy string sale opt-out as a permission signal module.
//!
//! Reads the four character `us_privacy` cookie, decodes it and answers from
//! the sale opt-out it carries. Where no string arrived and the request
//! carries a Global Privacy Control header in a US state, this module
//! constructs the string from the deployment's declared defaults, when the
//! settings say to, and records it as a policy default rather than something
//! the visitor sent.
//!
//! This module lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

mod decode;

use std::sync::atomic::{AtomicBool, Ordering};

pub use decode::{PrivacyFlag, UsPrivacy, UsPrivacyDecodeError, decode_us_privacy};

use serde::Deserialize;
use trusted_server_core::consent::jurisdiction::Jurisdiction;
use trusted_server_core::consent::{ConsentContext, ConsentSource, SignalRecord, SignalStatus};
use trusted_server_core::consent_config::ConsentMode;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permission_signal::{PermissionSignalModule, SignalInput};
use trusted_server_core::permissions::{ConsentSignal, Permission, ValidSignal};
use trusted_server_core::settings::Settings;
use trusted_server_permission_signal_gpc as gpc;

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

/// The scheme, as a [`ValidSignal`] and a [`SignalRecord`] name it.
pub const SCHEME: &str = "us_privacy";

/// The name this scheme is listed under in `signals.us_opt_out.sources` of
/// the permission policy.
pub const OPT_OUT_SOURCE: &str = "us_privacy_opt_out";

/// The cookie carrying the string.
pub const COOKIE_US_PRIVACY: &str = "us_privacy";

static SETTINGS_WARNING_LOGGED: AtomicBool = AtomicBool::new(false);

/// What this module reads from the `[consent]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct UsPrivacySettings {
    /// The deployment's declared posture, used to construct a string from a
    /// Global Privacy Control header.
    pub us_privacy_defaults: UsPrivacyDefaults,
}

/// The `us_privacy_defaults` table of the `[consent]` settings: the
/// deployment's own compliance posture, which is policy and not a protocol
/// requirement.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct UsPrivacyDefaults {
    /// Whether the deployment has shown the visitor a notice.
    pub notice_given: bool,
    /// Whether the deployment is subject to the Limited Service Provider
    /// Agreement.
    pub lspa_covered: bool,
    /// Whether a `Sec-GPC: 1` header is read as an opt-out of sale.
    pub gpc_implies_optout: bool,
}

impl Default for UsPrivacyDefaults {
    fn default() -> Self {
        Self {
            notice_given: true,
            lspa_covered: false,
            gpc_implies_optout: true,
        }
    }
}

/// The US Privacy string the request carried, or this module constructed,
/// as it reads, when this module vouched for it.
#[must_use]
pub fn us_privacy_string(consent: &ConsentContext) -> Option<&str> {
    consent.value(SCHEME)
}

/// The decoded string, when one decoded or was constructed.
#[must_use]
pub fn decoded(consent: &ConsentContext) -> Option<&UsPrivacy> {
    consent.decoded::<UsPrivacy>(SCHEME)
}

/// The record this module reports for a decoded string, for a caller that
/// builds a consent context without a request.
#[must_use]
pub fn record(decoded: UsPrivacy, raw: &str) -> SignalRecord {
    SignalRecord::new(SCHEME, SignalStatus::Decoded)
        .with_value(raw)
        .with_decoded(decoded)
}

/// A US Privacy string sale opt-out, read from `us_privacy`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UsPrivacyModule;

impl UsPrivacyModule {
    /// A new module.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The module's settings from the `[consent]` table, or the defaults
    /// when the table cannot be read as them, which is said once in the log.
    fn settings(settings: &Settings) -> UsPrivacySettings {
        match settings.consent.read::<UsPrivacySettings>() {
            Ok(read) => read,
            Err(error) => {
                if !SETTINGS_WARNING_LOGGED.swap(true, Ordering::Relaxed) {
                    log::warn!(
                        "The US Privacy module cannot read its settings and is using its defaults: {error:?}"
                    );
                }
                UsPrivacySettings::default()
            }
        }
    }

    /// Reads the string from the request, or constructs one from a Global
    /// Privacy Control header in a US state when the settings say to.
    fn read_record(
        &self,
        evidence: &dyn RequestInfo,
        settings: &Settings,
        consent: &ConsentContext,
    ) -> Option<SignalRecord> {
        let raw = evidence.cookie(COOKIE_US_PRIVACY);
        if settings.consent.mode == ConsentMode::Proxy {
            return raw
                .map(|raw| SignalRecord::new(SCHEME, SignalStatus::NotDecoded).with_value(raw));
        }
        let read = raw.map(|raw| match decode_us_privacy(raw) {
            Ok(decoded) => record(decoded, raw),
            Err(error) => {
                log::warn!("Failed to decode US Privacy string: {error}");
                SignalRecord::new(SCHEME, SignalStatus::Unreadable).with_value(raw)
            }
        });
        if read
            .as_ref()
            .is_some_and(|record| record.status() == SignalStatus::Decoded)
        {
            return read;
        }
        // No readable string. A Global Privacy Control header in a US state
        // is read as the opt-out the deployment's defaults declare.
        if !gpc::header_set(evidence) || !matches!(consent.jurisdiction, Jurisdiction::UsState(_)) {
            return read;
        }
        let defaults = Self::settings(settings).us_privacy_defaults;
        if !defaults.gpc_implies_optout {
            return read;
        }
        if defaults.notice_given {
            log::warn!(
                "GPC-constructed US Privacy string asserts notice_given=true; \
                 verify that the publisher's privacy notice satisfies this claim"
            );
        }
        let constructed = UsPrivacy {
            version: 1,
            notice_given: PrivacyFlag::from(defaults.notice_given),
            opt_out_sale: PrivacyFlag::Yes,
            lspa_covered: PrivacyFlag::from(defaults.lspa_covered),
        };
        log::info!("Constructed US Privacy string from GPC: {constructed}");
        Some(
            record(constructed.clone(), &constructed.to_string())
                .with_source(ConsentSource::PolicyDefault),
        )
    }

    /// The signal this scheme vouches for, when the string decoded or was
    /// constructed.
    fn read_signal(&self, consent: &ConsentContext) -> Option<ValidSignal> {
        let record = consent.record(SCHEME)?;
        if record.status() != SignalStatus::Decoded {
            return None;
        }
        Some(ValidSignal::new(short(), SCHEME, record.value()?))
    }
}

impl PermissionSignalModule for UsPrivacyModule {
    fn id(&self) -> &'static str {
        name()
    }

    fn read(&self, call: ModuleCall<'_>) -> Option<SignalRecord> {
        call.inject(self, Self::read_record).ok().flatten()
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        // The policy decides whether this scheme counts at all and what an
        // opt-out takes away, so a module the policy does not list stays
        // silent even when configuration names it.
        if !input.policy.is_opt_out_source(OPT_OUT_SOURCE) {
            return ConsentSignal::Neutral;
        }
        let Some(record) = input.consent.record(SCHEME) else {
            // Silence rather than refusal. Reading an absent signal as a
            // refusal would revoke the permission on every request that did
            // not carry this scheme, which is most of them.
            return ConsentSignal::Neutral;
        };
        // A string that arrived and could not be read, or was left as
        // received, is treated as the opt-out it may have carried, because a
        // preference this module cannot see is not the same as no preference.
        let unreadable = matches!(
            record.status(),
            SignalStatus::Unreadable | SignalStatus::NotDecoded
        );
        let opted_out = record
            .decoded::<UsPrivacy>()
            .is_some_and(|decoded| decoded.opt_out_sale == PrivacyFlag::Yes);
        if (opted_out || unreadable) && input.policy.opt_out_revokes(permission) {
            return ConsentSignal::Revoke;
        }
        ConsentSignal::Neutral
    }

    /// The US Privacy string, when it decoded.
    fn valid_signal(&self, call: ModuleCall<'_>) -> Option<ValidSignal> {
        call.inject(self, Self::read_signal).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::*;
    use trusted_server_core::consent::ConsentContext;
    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::permission_signal::SignalInput;
    use trusted_server_core::permissions::{Acquisition, PermissionMaps, SignalPolicy};

    fn sample_policy() -> &'static SignalPolicy {
        PermissionMaps::sample().signals()
    }

    fn with_sale_flag(opt_out_sale: PrivacyFlag) -> ConsentContext {
        let decoded = UsPrivacy {
            version: 1,
            notice_given: PrivacyFlag::Yes,
            opt_out_sale,
            lspa_covered: PrivacyFlag::NotApplicable,
        };
        let raw = decoded.to_string();
        ConsentContext::default().with_record(record(decoded, &raw))
    }

    fn unreadable() -> ConsentContext {
        ConsentContext::default()
            .with_record(SignalRecord::new(SCHEME, SignalStatus::Unreadable).with_value("bogus"))
    }

    fn answer(
        consent: &ConsentContext,
        policy: &SignalPolicy,
        permission: Permission,
    ) -> ConsentSignal {
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(consent, &evidence, policy, Acquisition::Granted);
        UsPrivacyModule::new().signal(permission, &input)
    }

    fn evidence(cookie: Option<&str>, gpc: bool) -> OwnedRequestInfo {
        let mut headers = HeaderMap::new();
        if let Some(cookie) = cookie {
            headers.insert(
                "cookie",
                cookie.parse().expect("should build a cookie header"),
            );
        }
        if gpc {
            headers.insert("sec-gpc", "1".parse().expect("should build the header"));
        }
        OwnedRequestInfo::new(String::new(), headers)
    }

    fn in_jurisdiction(jurisdiction: Jurisdiction) -> ConsentContext {
        ConsentContext::new(jurisdiction)
    }

    fn with_consent_settings(json: serde_json::Value) -> Settings {
        let mut settings = Settings::default();
        settings.consent = serde_json::from_value(json).expect("should build the consent table");
        settings
    }

    fn read_with(
        cookie: Option<&str>,
        gpc: bool,
        settings: &Settings,
        jurisdiction: Jurisdiction,
    ) -> Option<SignalRecord> {
        UsPrivacyModule::new().read_record(
            &evidence(cookie, gpc),
            settings,
            &in_jurisdiction(jurisdiction),
        )
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(
            UsPrivacyModule::new().id(),
            name(),
            "the module answers to the identifier configuration names"
        );
    }

    #[test]
    fn reads_and_decodes_the_string_from_the_request() {
        let record = read_with(
            Some("us_privacy=1YNN"),
            false,
            &Settings::default(),
            Jurisdiction::NonRegulated,
        )
        .expect("should read");
        assert_eq!(record.status(), SignalStatus::Decoded);
        assert_eq!(record.value(), Some("1YNN"));
        assert_eq!(record.source(), ConsentSource::Cookie);
        assert_eq!(
            record.decoded::<UsPrivacy>().map(|d| d.opt_out_sale),
            Some(PrivacyFlag::No)
        );
        assert!(
            read_with(
                None,
                false,
                &Settings::default(),
                Jurisdiction::NonRegulated
            )
            .is_none(),
            "no string, no record"
        );
    }

    #[test]
    fn an_unreadable_string_is_recorded_as_such() {
        let record = read_with(
            Some("us_privacy=bogus"),
            false,
            &Settings::default(),
            Jurisdiction::NonRegulated,
        )
        .expect("should read");
        assert_eq!(record.status(), SignalStatus::Unreadable);
        assert!(record.decoded::<UsPrivacy>().is_none());
    }

    #[test]
    fn proxy_mode_leaves_the_string_as_received_and_constructs_nothing() {
        let settings = with_consent_settings(serde_json::json!({ "mode": "proxy" }));
        let record = read_with(
            Some("us_privacy=1YNN"),
            true,
            &settings,
            Jurisdiction::UsState("CA".to_owned()),
        )
        .expect("should read");
        assert_eq!(record.status(), SignalStatus::NotDecoded);
        assert!(
            read_with(
                None,
                true,
                &settings,
                Jurisdiction::UsState("CA".to_owned())
            )
            .is_none(),
            "nothing is constructed in proxy mode"
        );
    }

    #[test]
    fn a_gpc_header_in_a_us_state_constructs_the_declared_opt_out() {
        let record = read_with(
            None,
            true,
            &Settings::default(),
            Jurisdiction::UsState("CA".to_owned()),
        )
        .expect("should construct");
        assert_eq!(record.status(), SignalStatus::Decoded);
        assert_eq!(
            record.value(),
            Some("1YYN"),
            "notice given, opted out, no LSPA"
        );
        assert_eq!(
            record.source(),
            ConsentSource::PolicyDefault,
            "a constructed string is the deployment's declaration, not the visitor's"
        );
        assert_eq!(
            UsPrivacyModule::new().read_signal(&ConsentContext::default().with_record(record)),
            Some(ValidSignal::new(short(), SCHEME, "1YYN")),
            "and it is vouched for"
        );
    }

    #[test]
    fn the_constructed_string_follows_the_declared_defaults() {
        let settings = with_consent_settings(serde_json::json!({
            "us_privacy_defaults": { "notice_given": false, "lspa_covered": true },
            "check_expiration": false
        }));
        let record = read_with(
            None,
            true,
            &settings,
            Jurisdiction::UsState("CA".to_owned()),
        )
        .expect("should construct");
        assert_eq!(record.value(), Some("1NYY"));
        let off = with_consent_settings(serde_json::json!({
            "us_privacy_defaults": { "gpc_implies_optout": false }
        }));
        assert!(
            read_with(None, true, &off, Jurisdiction::UsState("CA".to_owned())).is_none(),
            "a deployment that does not read the header as an opt-out constructs nothing"
        );
    }

    #[test]
    fn nothing_is_constructed_outside_a_us_state_or_without_the_header() {
        assert!(
            read_with(None, true, &Settings::default(), Jurisdiction::Gdpr).is_none(),
            "the header means something else outside a US state"
        );
        assert!(read_with(None, true, &Settings::default(), Jurisdiction::NonRegulated).is_none());
        assert!(
            read_with(
                None,
                false,
                &Settings::default(),
                Jurisdiction::UsState("CA".to_owned())
            )
            .is_none(),
            "no header, nothing to read"
        );
    }

    #[test]
    fn a_received_string_stands_over_the_header() {
        let record = read_with(
            Some("us_privacy=1YNN"),
            true,
            &Settings::default(),
            Jurisdiction::UsState("CA".to_owned()),
        )
        .expect("should read");
        assert_eq!(record.value(), Some("1YNN"), "the visitor's string is kept");
        assert_eq!(record.source(), ConsentSource::Cookie);
    }

    #[test]
    fn settings_read_the_table_and_leave_the_rest() {
        let settings = with_consent_settings(serde_json::json!({
            "us_privacy_defaults": { "notice_given": false, "lspa_covered": true },
            "max_consent_age_days": 180
        }));
        let read = UsPrivacyModule::settings(&settings);
        assert!(!read.us_privacy_defaults.notice_given);
        assert!(read.us_privacy_defaults.lspa_covered);
        assert!(read.us_privacy_defaults.gpc_implies_optout);
        let unreadable = with_consent_settings(serde_json::json!({
            "us_privacy_defaults": { "notice_given": "yes" }
        }));
        assert_eq!(
            UsPrivacyModule::settings(&unreadable),
            UsPrivacySettings::default(),
            "a table that cannot be read gives the defaults"
        );
    }

    #[test]
    fn an_unreadable_us_privacy_string_is_read_as_the_opt_out_it_may_have_carried() {
        assert_eq!(
            answer(&unreadable(), sample_policy(), Permission::StoreOnDevice),
            ConsentSignal::Revoke,
            "an unreadable string refuses what the policy lets an opt-out revoke"
        );
        assert_eq!(
            answer(
                &unreadable(),
                sample_policy(),
                Permission::MeasureAdPerformance
            ),
            ConsentSignal::Neutral,
            "and nothing the policy leaves alone"
        );
    }

    #[test]
    fn a_us_privacy_string_that_cannot_be_read_gives_no_consent_to_what_an_opt_out_covers() {
        let read = read_with(
            Some("us_privacy=bogus"),
            false,
            &Settings::default(),
            Jurisdiction::UsState("CA".to_owned()),
        )
        .expect("should read");
        assert_eq!(read.status(), SignalStatus::Unreadable);
        let consent = ConsentContext::default().with_record(read);
        let policy = sample_policy();
        let covered: Vec<Permission> = Permission::all()
            .filter(|permission| policy.opt_out_revokes(*permission))
            .collect();
        assert!(!covered.is_empty(), "the shipped opt-out covers something");
        for permission in covered {
            assert_eq!(
                answer(&consent, policy, permission),
                ConsentSignal::Revoke,
                "{} is refused, as an opt-out would refuse it",
                permission.as_str()
            );
        }
    }

    #[test]
    fn vouches_for_the_us_privacy_string_only_when_it_decoded() {
        assert_eq!(
            UsPrivacyModule::new().read_signal(&with_sale_flag(PrivacyFlag::No)),
            Some(ValidSignal::new(short(), SCHEME, "1YN-")),
            "a decoded string is vouched for as received, whatever it says"
        );
        assert_eq!(
            us_privacy_string(&with_sale_flag(PrivacyFlag::No)),
            Some("1YN-")
        );
        assert_eq!(
            decoded(&with_sale_flag(PrivacyFlag::No)).map(|d| d.opt_out_sale),
            Some(PrivacyFlag::No)
        );
        assert_eq!(
            UsPrivacyModule::new().read_signal(&unreadable()),
            None,
            "an unreadable string is not"
        );
    }

    #[test]
    fn revokes_a_listed_permission_on_a_sale_opt_out() {
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::Yes),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Revoke,
            "the sample policy revokes device storage on an opt-out"
        );
    }

    #[test]
    fn is_silent_for_a_permission_the_policy_does_not_revoke() {
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::Yes),
                sample_policy(),
                Permission::MeasureAdPerformance
            ),
            ConsentSignal::Neutral,
            "measurement is not on the sample's revoke list"
        );
    }

    #[test]
    fn is_silent_when_the_string_does_not_opt_out() {
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::No),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a string present and not opting out is not an opt-out"
        );
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::NotApplicable),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "and a string saying the flag does not apply is silence"
        );
    }

    #[test]
    fn is_silent_when_no_string_arrived() {
        assert_eq!(
            answer(
                &ConsentContext::default(),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "an absent signal is silence, never a refusal"
        );
    }

    #[test]
    fn is_silent_when_the_policy_does_not_list_this_scheme() {
        let unlisted = SignalPolicy::default();
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::Yes),
                &unlisted,
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a scheme the policy does not count stays silent even on an opt-out"
        );
    }

    #[test]
    fn never_withdraws() {
        let consent = with_sale_flag(PrivacyFlag::Yes);
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(
            &consent,
            &evidence,
            sample_policy(),
            Acquisition::RequiresSignal,
        );
        assert!(
            !UsPrivacyModule::new().withdraws(Permission::StoreOnDevice, &input),
            "a sale opt-out suppresses use for the request and never destroys an identifier"
        );
    }
}
