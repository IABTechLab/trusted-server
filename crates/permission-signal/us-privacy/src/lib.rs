//! The US Privacy string sale opt-out as a permission signal module.
//!
//! Answers from the sale opt-out carried in the four character `us_privacy`
//! string, which core decodes into the consent record. Core also constructs
//! that string from a Global Privacy Control header in a US state when the
//! deployment's consent settings say to, and this module sees the result
//! the same way, because it reads the record and not the wire.
//!
//! This module lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

use trusted_server_core::consent::{ConsentContext, PrivacyFlag};
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permission_signal::{PermissionSignalModule, SignalInput};
use trusted_server_core::permissions::{ConsentSignal, OptOutSource, Permission, ValidSignal};

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

/// A US Privacy string sale opt-out, read from `us_privacy`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UsPrivacyModule;

impl UsPrivacyModule {
    /// A new module.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The signal this scheme reads from the request's consent record, when
    /// it can use one.
    fn read_signal(&self, consent: &ConsentContext) -> Option<ValidSignal> {
        consent.us_privacy.as_ref()?;
        let raw = consent.raw_us_privacy.as_deref()?;
        Some(ValidSignal::new(short(), "us_privacy", raw))
    }
}

impl PermissionSignalModule for UsPrivacyModule {
    fn id(&self) -> &'static str {
        name()
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        // The policy decides whether this scheme counts at all and what an
        // opt-out takes away, so a module the policy does not list stays
        // silent even when configuration names it.
        if !input
            .policy
            .opt_out_sources()
            .contains(&OptOutSource::UsPrivacyOptOut)
        {
            return ConsentSignal::Neutral;
        }
        // A US Privacy string that arrived and could not be read is treated
        // as the opt-out it may have carried, because a preference this
        // module cannot see is not the same as no preference.
        let unreadable =
            input.consent.raw_us_privacy.is_some() && input.consent.us_privacy.is_none();
        let opted_out = input
            .consent
            .us_privacy
            .as_ref()
            .is_some_and(|usp| usp.opt_out_sale == PrivacyFlag::Yes);
        if (opted_out || unreadable) && input.policy.opt_out_revokes(permission) {
            return ConsentSignal::Revoke;
        }
        // Silence rather than refusal. Reading an absent signal as a refusal
        // would revoke the permission on every request that did not carry this
        // scheme, which is most of them.
        ConsentSignal::Neutral
    }

    /// The US Privacy string, when it decoded.
    fn valid_signal(&self, call: ModuleCall<'_>) -> Option<ValidSignal> {
        call.inject(self, Self::read_signal).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::consent::ConsentContext;
    use trusted_server_core::consent::types::UsPrivacy;
    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::permission_signal::SignalInput;
    use trusted_server_core::permissions::{Acquisition, PermissionMaps, SignalPolicy};

    fn shipped_policy() -> &'static SignalPolicy {
        PermissionMaps::standard().signals()
    }

    fn with_sale_flag(opt_out_sale: PrivacyFlag) -> ConsentContext {
        ConsentContext {
            us_privacy: Some(UsPrivacy {
                version: 1,
                notice_given: PrivacyFlag::Yes,
                opt_out_sale,
                lspa_covered: PrivacyFlag::NotApplicable,
            }),
            ..ConsentContext::default()
        }
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

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(
            UsPrivacyModule::new().id(),
            name(),
            "the module answers to the identifier configuration names"
        );
    }

    #[test]
    fn an_unreadable_us_privacy_string_is_read_as_the_opt_out_it_may_have_carried() {
        let unreadable = ConsentContext {
            raw_us_privacy: Some("bogus".to_owned()),
            ..ConsentContext::default()
        };
        assert_eq!(
            answer(&unreadable, shipped_policy(), Permission::StoreOnDevice),
            ConsentSignal::Revoke,
            "an unreadable string refuses what the policy lets an opt-out revoke"
        );
        assert_eq!(
            answer(
                &unreadable,
                shipped_policy(),
                Permission::MeasureAdPerformance
            ),
            ConsentSignal::Neutral,
            "and nothing the policy leaves alone"
        );
    }

    #[test]
    fn vouches_for_the_us_privacy_string_only_when_it_decoded() {
        let decoded = ConsentContext {
            raw_us_privacy: Some("1YNN".to_owned()),
            ..with_sale_flag(PrivacyFlag::No)
        };
        assert_eq!(
            UsPrivacyModule::new().read_signal(&decoded),
            Some(ValidSignal::new(short(), "us_privacy", "1YNN")),
            "a decoded string is vouched for as received, whatever it says"
        );
        let unreadable = ConsentContext {
            raw_us_privacy: Some("bogus".to_owned()),
            ..ConsentContext::default()
        };
        assert_eq!(
            UsPrivacyModule::new().read_signal(&unreadable),
            None,
            "an unreadable string is not"
        );
    }

    #[test]
    fn revokes_a_listed_permission_on_a_sale_opt_out() {
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::Yes),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Revoke,
            "the shipped policy revokes device storage on an opt-out"
        );
    }

    #[test]
    fn is_silent_for_a_permission_the_policy_does_not_revoke() {
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::Yes),
                shipped_policy(),
                Permission::MeasureAdPerformance
            ),
            ConsentSignal::Neutral,
            "measurement is not on the shipped revoke list"
        );
    }

    #[test]
    fn is_silent_when_the_string_does_not_opt_out() {
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::No),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a string present and not opting out is not an opt-out"
        );
        assert_eq!(
            answer(
                &with_sale_flag(PrivacyFlag::NotApplicable),
                shipped_policy(),
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
                shipped_policy(),
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
            shipped_policy(),
            Acquisition::RequiresSignal,
        );
        assert!(
            !UsPrivacyModule::new().withdraws(Permission::StoreOnDevice, &input),
            "a sale opt-out suppresses use for the request and never destroys an identifier"
        );
    }
}
