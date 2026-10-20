//! Global Privacy Control as a permission signal module.
//!
//! Answers from the `Sec-GPC` request header, which core reads into the
//! consent record's `gpc` flag. Separate from the GPP and US Privacy modules
//! so that a publisher who does not act on Global Privacy Control can leave
//! this one out of the configured list without also losing the other two
//! opt-outs.
//!
//! This module lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

use trusted_server_core::consent::ConsentContext;
use trusted_server_core::evidence::RequestInfo;
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

/// The `Sec-GPC` request header, Global Privacy Control.
#[derive(Debug, Default, Clone, Copy)]
pub struct GpcModule;

impl GpcModule {
    /// A new module.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl PermissionSignalModule for GpcModule {
    fn id(&self) -> &'static str {
        name()
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        // The policy decides whether this scheme counts at all and what an
        // opt-out takes away, so a module the policy does not list stays
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

    /// The header's one value, when it was sent. There is nothing to
    /// decode, so a sent header is always valid.
    fn valid_signal(
        &self,
        consent: &ConsentContext,
        _evidence: &dyn RequestInfo,
    ) -> Option<ValidSignal> {
        consent.gpc.then(|| ValidSignal::new(short(), "gpc", "1"))
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
        GpcModule::new().signal(permission, &input)
    }

    #[test]
    fn vouches_for_the_header_only_when_it_was_sent() {
        let evidence = OwnedRequestInfo::default();
        assert_eq!(
            GpcModule::new().valid_signal(&with_header(true), &evidence),
            Some(ValidSignal::new(short(), "gpc", "1")),
            "a sent header is the one value it can carry"
        );
        assert_eq!(
            GpcModule::new().valid_signal(&with_header(false), &evidence),
            None,
            "no header is no signal"
        );
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(
            GpcModule::new().id(),
            name(),
            "the module answers to the identifier configuration names"
        );
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
            !GpcModule::new().withdraws(Permission::StoreOnDevice, &input),
            "a browser setting suppresses use for the request and never destroys an identifier"
        );
    }
}
