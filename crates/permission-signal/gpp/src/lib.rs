//! The GPP US sale opt-out as a permission signal module.
//!
//! Reads the `__gpp` string and the `__gpp_sid` hint from the request, decodes
//! the string, and answers from the US sale opt-out it carries. It says
//! nothing about the EU section a GPP string may also carry, because that is
//! the TCF module's scheme, which asks this crate for the raw section through
//! [`section`].
//!
//! This module lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

mod decode;

pub use decode::{
    GppConsent, GppDecodeError, MAX_GPP_STRING_LEN, decode_gpp_string, parse_gpp_sid_cookie,
    section, section_ids_of,
};

use trusted_server_core::consent::{ConsentContext, SignalRecord, SignalStatus};
use trusted_server_core::consent_config::ConsentMode;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permission_signal::{PermissionSignalModule, SignalInput};
use trusted_server_core::permissions::{ConsentSignal, Permission, ValidSignal};
use trusted_server_core::settings::Settings;

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
pub const SCHEME: &str = "gpp";

/// The name this scheme is listed under in `signals.us_opt_out.sources` of
/// the permission policy.
pub const OPT_OUT_SOURCE: &str = "gpp_sale_opt_out";

/// The cookie carrying the GPP string.
pub const COOKIE_GPP: &str = "__gpp";

/// The cookie carrying the string's section identifiers, as a hint.
pub const COOKIE_GPP_SID: &str = "__gpp_sid";

/// The GPP string and the section hint on a request, as received.
#[must_use]
pub fn strings(evidence: &dyn RequestInfo) -> (Option<&str>, Option<&str>) {
    (evidence.cookie(COOKIE_GPP), evidence.cookie(COOKIE_GPP_SID))
}

/// The GPP string the request carried, as received, when this module read
/// one and vouched for it.
#[must_use]
pub fn gpp_string(consent: &ConsentContext) -> Option<&str> {
    consent.value(SCHEME)
}

/// The section identifiers of the request's GPP signal, when it carried any.
#[must_use]
pub fn section_ids(consent: &ConsentContext) -> Option<&[u16]> {
    consent
        .record(SCHEME)
        .map(SignalRecord::sections)
        .filter(|sections| !sections.is_empty())
}

/// The record this module reports for a decoded string, for a caller that
/// builds a consent context without a request.
#[must_use]
pub fn record(decoded: GppConsent, raw: &str) -> SignalRecord {
    SignalRecord::new(SCHEME, SignalStatus::Decoded)
        .with_value(raw)
        .with_sections(decoded.section_ids.clone())
        .with_decoded(decoded)
}

/// A GPP US sale opt-out, read from the `__gpp` string.
#[derive(Debug, Default, Clone, Copy)]
pub struct GppSaleOptOutModule;

impl GppSaleOptOutModule {
    /// A new module.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Reads the string and the hint from the request and reports what they
    /// carry.
    fn read_record(&self, evidence: &dyn RequestInfo, settings: &Settings) -> Option<SignalRecord> {
        let (raw, hint) = strings(evidence);
        let hinted = hint.and_then(parse_gpp_sid_cookie);
        let Some(raw) = raw else {
            // A hint without a string still says which sections the visitor
            // answered for, which a bid request carries.
            return hinted.map(|sections| {
                SignalRecord::new(SCHEME, SignalStatus::Absent).with_sections(sections)
            });
        };
        if settings.consent.mode == ConsentMode::Proxy {
            return Some(
                SignalRecord::new(SCHEME, SignalStatus::NotDecoded)
                    .with_value(raw)
                    .with_sections(hinted.unwrap_or_default()),
            );
        }
        match decode_gpp_string(raw) {
            Ok(decoded) => Some(record(decoded, raw)),
            Err(error) => {
                log::warn!("Failed to decode GPP string: {error}");
                Some(
                    SignalRecord::new(SCHEME, SignalStatus::Unreadable)
                        .with_value(raw)
                        .with_sections(hinted.unwrap_or_default()),
                )
            }
        }
    }

    /// The signal this scheme vouches for, when the string decoded.
    fn read_signal(&self, consent: &ConsentContext) -> Option<ValidSignal> {
        let record = consent.record(SCHEME)?;
        if record.status() != SignalStatus::Decoded {
            return None;
        }
        Some(ValidSignal::new(short(), SCHEME, record.value()?))
    }
}

impl PermissionSignalModule for GppSaleOptOutModule {
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
            .decoded::<GppConsent>()
            .and_then(|decoded| decoded.us_sale_opt_out)
            == Some(true);
        if (opted_out || unreadable) && input.policy.opt_out_revokes(permission) {
            return ConsentSignal::Revoke;
        }
        ConsentSignal::Neutral
    }

    /// The GPP string, when it decoded.
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

    fn with_sale_opt_out(value: Option<bool>) -> ConsentContext {
        ConsentContext::default().with_record(record(
            GppConsent {
                section_ids: vec![7],
                us_sale_opt_out: value,
            },
            "DBABMA~CPreadable",
        ))
    }

    fn unreadable() -> ConsentContext {
        ConsentContext::default().with_record(
            SignalRecord::new(SCHEME, SignalStatus::Unreadable).with_value("not a GPP string"),
        )
    }

    fn answer(
        consent: &ConsentContext,
        policy: &SignalPolicy,
        permission: Permission,
    ) -> ConsentSignal {
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(consent, &evidence, policy, Acquisition::Granted);
        GppSaleOptOutModule::new().signal(permission, &input)
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
        GppSaleOptOutModule::new().read_record(&evidence(cookie), settings)
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(
            GppSaleOptOutModule::new().id(),
            name(),
            "the module answers to the identifier configuration names"
        );
    }

    #[test]
    fn reads_and_decodes_the_string_from_the_request() {
        let settings = Settings::default();
        let record = read_with("__gpp=DBABLA~BVQqAAAAAgA.QA", &settings).expect("should read");
        assert_eq!(record.status(), SignalStatus::Decoded);
        assert_eq!(record.value(), Some("DBABLA~BVQqAAAAAgA.QA"));
        assert_eq!(record.sections(), &[7], "the sections come from the string");
        assert_eq!(
            record.decoded::<GppConsent>().map(|d| d.us_sale_opt_out),
            Some(Some(false))
        );
        assert!(
            read_with("other=1", &settings).is_none(),
            "no string, no record"
        );
    }

    #[test]
    fn an_unreadable_string_is_recorded_with_the_hinted_sections() {
        let settings = Settings::default();
        let record =
            read_with("__gpp=not-a-gpp-string; __gpp_sid=2,6", &settings).expect("should read");
        assert_eq!(record.status(), SignalStatus::Unreadable);
        assert_eq!(record.sections(), &[2, 6], "the hint stands in");
        assert!(record.decoded::<GppConsent>().is_none());
    }

    #[test]
    fn a_hint_alone_is_recorded_as_absent_with_its_sections() {
        let record = read_with("__gpp_sid=2,6", &Settings::default()).expect("should read");
        assert_eq!(record.status(), SignalStatus::Absent);
        assert_eq!(record.value(), None);
        assert_eq!(record.sections(), &[2, 6]);
    }

    #[test]
    fn proxy_mode_leaves_the_string_as_received() {
        let mut settings = Settings::default();
        settings.consent.mode = ConsentMode::Proxy;
        let record =
            read_with("__gpp=DBABLA~BVQqAAAAAgA.QA; __gpp_sid=7", &settings).expect("should read");
        assert_eq!(record.status(), SignalStatus::NotDecoded);
        assert_eq!(record.value(), Some("DBABLA~BVQqAAAAAgA.QA"));
        assert_eq!(record.sections(), &[7], "only the hint is read");
        assert!(record.decoded::<GppConsent>().is_none());
    }

    #[test]
    fn an_unreadable_gpp_string_is_read_as_the_opt_out_it_may_have_carried() {
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
        let not_decoded = ConsentContext::default().with_record(
            SignalRecord::new(SCHEME, SignalStatus::NotDecoded).with_value("DBABMA~x"),
        );
        assert_eq!(
            answer(&not_decoded, sample_policy(), Permission::StoreOnDevice),
            ConsentSignal::Revoke,
            "a string left as received is a preference this module cannot see"
        );
    }

    #[test]
    fn a_gpp_string_that_cannot_be_read_gives_no_consent_to_what_an_opt_out_covers() {
        let read = read_with("__gpp=not-a-gpp-string", &Settings::default()).expect("should read");
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
    fn vouches_for_the_gpp_string_only_when_it_decoded() {
        assert_eq!(
            GppSaleOptOutModule::new().read_signal(&with_sale_opt_out(Some(false))),
            Some(ValidSignal::new(short(), SCHEME, "DBABMA~CPreadable")),
            "a decoded string is vouched for as received, whatever it says"
        );
        assert_eq!(
            GppSaleOptOutModule::new().read_signal(&unreadable()),
            None,
            "an unreadable string is not"
        );
        assert_eq!(
            gpp_string(&with_sale_opt_out(None)),
            Some("DBABMA~CPreadable"),
            "and a reader asks this crate for the string"
        );
        assert_eq!(section_ids(&with_sale_opt_out(None)), Some(&[7][..]));
        assert_eq!(section_ids(&ConsentContext::default()), None);
    }

    #[test]
    fn revokes_a_listed_permission_on_a_sale_opt_out() {
        assert_eq!(
            answer(
                &with_sale_opt_out(Some(true)),
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
                &with_sale_opt_out(Some(true)),
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
                &with_sale_opt_out(Some(false)),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a section present and not opting out is not an opt-out"
        );
        assert_eq!(
            answer(
                &with_sale_opt_out(None),
                sample_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "and a section carrying no sale flag is silence"
        );
    }

    #[test]
    fn is_silent_when_no_gpp_string_arrived() {
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
                &with_sale_opt_out(Some(true)),
                &unlisted,
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a scheme the policy does not count stays silent even on an opt-out"
        );
    }

    #[test]
    fn never_withdraws() {
        let consent = with_sale_opt_out(Some(true));
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(
            &consent,
            &evidence,
            sample_policy(),
            Acquisition::RequiresSignal,
        );
        assert!(
            !GppSaleOptOutModule::new().withdraws(Permission::StoreOnDevice, &input),
            "a sale opt-out suppresses use for the request and never destroys an identifier"
        );
    }
}
