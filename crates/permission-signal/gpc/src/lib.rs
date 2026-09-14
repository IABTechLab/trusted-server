//! Global Privacy Control as a permission signal provider.
//!
//! Answers from the `Sec-GPC` request header, which core reads into the
//! consent record's `gpc` flag. Separate from the GPP and US Privacy providers
//! so that a publisher who does not act on Global Privacy Control can leave
//! this one out of the configured list without also losing the other two
//! opt-outs.
//!
//! This provider lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

use trusted_server_core::permission_signal::{PermissionSignalProvider, SignalInput};
use trusted_server_core::permissions::{ConsentSignal, OptOutSource, Permission};

/// The stable identifier this provider answers to in `[permission_signal]`
/// `sources`, in logs, and when a peer consults it.
pub const ID: &str = "gpc";

/// The `Sec-GPC` request header, Global Privacy Control.
#[derive(Debug, Default, Clone, Copy)]
pub struct GpcProvider;

impl GpcProvider {
    /// A new provider.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl PermissionSignalProvider for GpcProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        // The policy decides whether this scheme counts at all and what an
        // opt-out takes away, so a provider the policy does not list stays
        // silent even when configuration names it.
        if !input.policy.opt_out_sources().contains(&OptOutSource::Gpc) {
            return ConsentSignal::Neutral;
        }
        if input.consent.gpc && input.policy.opt_out_revokes(permission) {
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
    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::permission_signal::SignalInput;
    use trusted_server_core::permissions::{Acquisition, PermissionMaps, SignalPolicy};

    /// The shipped policy lists this scheme as an opt-out and revokes device
    /// storage on it, and leaves ad measurement alone.
    fn shipped_policy() -> &'static SignalPolicy {
        PermissionMaps::standard().signals()
    }

    fn with_header(set: bool) -> ConsentContext {
        ConsentContext {
            gpc: set,
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
        GpcProvider::new().signal(permission, &input)
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(GpcProvider::new().id(), ID);
    }

    #[test]
    fn revokes_a_listed_permission_when_the_header_is_set() {
        assert_eq!(
            answer(
                &with_header(true),
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
                &with_header(true),
                shipped_policy(),
                Permission::MeasureAdPerformance
            ),
            ConsentSignal::Neutral,
            "what an opt-out takes away is the policy's decision, and measurement is not listed"
        );
    }

    #[test]
    fn is_silent_when_the_header_is_absent() {
        assert_eq!(
            answer(
                &with_header(false),
                shipped_policy(),
                Permission::StoreOnDevice
            ),
            ConsentSignal::Neutral,
            "an absent signal is silence, never a refusal"
        );
    }

    #[test]
    fn is_silent_when_the_policy_does_not_list_this_scheme() {
        // A policy declaring no opt-out sources at all.
        let unlisted = SignalPolicy::default();
        assert_eq!(
            answer(&with_header(true), &unlisted, Permission::StoreOnDevice),
            ConsentSignal::Neutral,
            "a scheme the policy does not count stays silent even when the header is set"
        );
    }

    #[test]
    fn never_withdraws() {
        let consent = with_header(true);
        let evidence = OwnedRequestInfo::default();
        let input = SignalInput::new(
            &consent,
            &evidence,
            shipped_policy(),
            Acquisition::RequiresSignal,
        );
        assert!(
            !GpcProvider::new().withdraws(Permission::StoreOnDevice, &input),
            "a browser setting suppresses use for the request and never destroys an identifier"
        );
    }
}
