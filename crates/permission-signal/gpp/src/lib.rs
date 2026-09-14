//! The GPP US sale opt-out as a permission signal provider.
//!
//! Answers from the US sale opt-out carried in the `__gpp` string, which core
//! decodes into the consent record. It says nothing about the EU TCF section a
//! GPP string may also carry, because that is the TCF provider's scheme.
//!
//! This provider lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

use trusted_server_core::permission_signal::{PermissionSignalProvider, SignalInput};
use trusted_server_core::permissions::{ConsentSignal, OptOutSource, Permission};

/// The stable identifier this provider answers to in `[permission_signal]`
/// `sources`, in logs, and when a peer consults it.
pub const ID: &str = "gpp-sale-opt-out";

/// A GPP US sale opt-out, read from the `__gpp` string.
#[derive(Debug, Default, Clone, Copy)]
pub struct GppSaleOptOutProvider;

impl GppSaleOptOutProvider {
    /// A new provider.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl PermissionSignalProvider for GppSaleOptOutProvider {
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
            .contains(&OptOutSource::GppSaleOptOut)
        {
            return ConsentSignal::Neutral;
        }
        let opted_out = input
            .consent
            .gpp
            .as_ref()
            .and_then(|gpp| gpp.us_sale_opt_out)
            == Some(true);
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
    use trusted_server_core::consent::types::GppConsent;
    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::permission_signal::SignalInput;
    use trusted_server_core::permissions::{Acquisition, PermissionMaps, SignalPolicy};

    fn shipped_policy() -> &'static SignalPolicy {
        PermissionMaps::standard().signals()
    }

    fn with_sale_opt_out(value: Option<bool>) -> ConsentContext {
        ConsentContext {
            gpp: Some(GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: value,
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
        GppSaleOptOutProvider::new().signal(permission, &input)
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(GppSaleOptOutProvider::new().id(), ID);
    }

    #[test]
    fn revokes_a_listed_permission_on_a_sale_opt_out() {
        assert_eq!(
            answer(
                &with_sale_opt_out(Some(true)),
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
                &with_sale_opt_out(Some(true)),
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
                &with_sale_opt_out(Some(false)),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "a section present and not opting out is not an opt-out"
        );
        assert_eq!(
            answer(
                &with_sale_opt_out(None),
                shipped_policy(),
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
            shipped_policy(),
            Acquisition::RequiresSignal,
        );
        assert!(
            !GppSaleOptOutProvider::new().withdraws(Permission::StoreOnDevice, &input),
            "a sale opt-out suppresses use for the request and never destroys an identifier"
        );
    }
}
