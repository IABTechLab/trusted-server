//! The US Privacy string sale opt-out as a permission signal provider.
//!
//! Answers from the sale opt-out carried in the four character `us_privacy`
//! string, which core decodes into the consent record. Core also constructs
//! that string from a Global Privacy Control header in a US state when the
//! deployment's consent settings say to, and this provider sees the result
//! the same way, because it reads the record and not the wire.
//!
//! This provider lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

use trusted_server_core::consent::PrivacyFlag;
use trusted_server_core::permission_signal::{PermissionSignalProvider, SignalInput};
use trusted_server_core::permissions::{ConsentSignal, OptOutSource, Permission};

/// The stable identifier this provider answers to in `[permission_signal]`
/// `sources`, in logs, and when a peer consults it.
pub const ID: &str = "us-privacy";

/// A US Privacy string sale opt-out, read from `us_privacy`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UsPrivacyProvider;

impl UsPrivacyProvider {
    /// A new provider.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl PermissionSignalProvider for UsPrivacyProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        // The policy decides whether this scheme counts at all and what an
        // opt-out takes away, so a provider the policy does not list stays
        // silent even when configuration names it.
        if !input
            .policy
            .opt_out_sources()
            .contains(&OptOutSource::UsPrivacyOptOut)
        {
            return ConsentSignal::Neutral;
        }
        let opted_out = input
            .consent
            .us_privacy
            .as_ref()
            .is_some_and(|usp| usp.opt_out_sale == PrivacyFlag::Yes);
        if opted_out && input.policy.opt_out_revokes(permission) {
            return ConsentSignal::Revoke;
        }
        // Silence rather than refusal. Reading an absent signal as a refusal
        // would revoke the permission on every request that did not carry this
        // scheme, which is most of them.
        ConsentSignal::Neutral
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
        UsPrivacyProvider::new().signal(permission, &input)
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(UsPrivacyProvider::new().id(), ID);
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
            !UsPrivacyProvider::new().withdraws(Permission::StoreOnDevice, &input),
            "a sale opt-out suppresses use for the request and never destroys an identifier"
        );
    }
}
