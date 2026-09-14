#![doc = include_str!("README.md")]

use std::sync::Arc;

use error_stack::Report;

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::evidence::RequestInfo;
use crate::permissions::{Acquisition, ConsentSignal, Permission, SignalPolicy};
use crate::settings::Settings;
use crate::tdl::Tdl;

/// What a signal provider may read about a request.
///
/// A struct rather than a parameter list, so a provider needing something new
/// does not change every implementation.
pub struct SignalInput<'a> {
    /// The decoded consent record for this request.
    ///
    /// Offered because the schemes that ship by default already decode into
    /// it, core keeps it against the Edge Cookie identifier between requests,
    /// and the rest of the system reads it. A provider is not required to use
    /// it, and a scheme core has never heard of will not appear in it. Such a
    /// provider reads [`evidence`](Self::evidence) instead and decodes whatever
    /// its scheme needs.
    pub consent: &'a ConsentContext,
    /// The request itself, as evidence.
    ///
    /// This is the open half of the seam, and it is [`RequestInfo`], the same
    /// abstraction the Edge Cookie and device providers already read. A
    /// provider asks for the header, cookie, path or query parameter its own
    /// scheme uses, so core holds no list of which evidence a scheme may read.
    pub evidence: &'a dyn RequestInfo,
    /// The policy from `permissions.yaml`, which decides what a signal means
    /// rather than leaving each provider to invent its own meaning.
    pub policy: &'a SignalPolicy,
    /// What the country and region rules say about this permission, before any
    /// provider is asked. A provider amends this rather than deciding alone.
    pub baseline: Acquisition,
    /// What the providers asked before this one settled on.
    ///
    /// [`ConsentSignal::Neutral`] means none of them had an opinion, so the
    /// baseline still stands.
    pub settled: ConsentSignal,
    /// Every provider in configured order, this one included.
    providers: &'a [Arc<dyn PermissionSignalProvider>],
    /// Where in that order the provider being asked sits.
    position: usize,
    /// Whether this provider may consult a peer.
    ///
    /// False while answering a consultation, which is what stops two providers
    /// that consult each other from looping.
    may_ask: bool,
}

impl<'a> SignalInput<'a> {
    /// An input for a provider asked on its own, outside an ordered run.
    #[must_use]
    pub fn new(
        consent: &'a ConsentContext,
        evidence: &'a dyn RequestInfo,
        policy: &'a SignalPolicy,
        baseline: Acquisition,
    ) -> Self {
        Self {
            consent,
            evidence,
            policy,
            baseline,
            settled: ConsentSignal::Neutral,
            providers: &[],
            position: 0,
            may_ask: true,
        }
    }

    /// Every provider in configured order, this one included.
    ///
    /// A provider consults the list to decide whether a peer it cares about is
    /// configured at all, and where it sits relative to this one.
    #[must_use]
    pub fn providers(&self) -> &[Arc<dyn PermissionSignalProvider>] {
        self.providers
    }

    /// Where the provider being asked sits in that order.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Whether a provider with this identifier is configured.
    #[must_use]
    pub fn has(&self, id: &str) -> bool {
        self.providers.iter().any(|provider| provider.id() == id)
    }

    /// What a peer makes of `permission`, asked directly.
    ///
    /// The peer answers as if it were first, so the reply is that peer's own
    /// opinion rather than what the run has settled on so far. That is the
    /// useful question, because a provider wanting to know whether the prior
    /// value came from a particular peer asks that peer what it says.
    ///
    /// Returns `None` when no provider carries the identifier, when the caller
    /// names itself, and when this input is itself answering a consultation.
    /// A provider must handle `None` rather than assume a peer is present.
    #[must_use]
    pub fn ask(&self, id: &str, permission: Permission) -> Option<ConsentSignal> {
        if !self.may_ask {
            return None;
        }
        let (position, provider) = self
            .providers
            .iter()
            .enumerate()
            .find(|(position, provider)| provider.id() == id && *position != self.position)?;
        let input = Self {
            consent: self.consent,
            evidence: self.evidence,
            policy: self.policy,
            baseline: self.baseline,
            settled: ConsentSignal::Neutral,
            providers: self.providers,
            position,
            may_ask: false,
        };
        Some(provider.signal(permission, &input))
    }
}

/// A provider of permission signals.
///
/// An implementation answers for one signaling scheme. It reads the request,
/// applies whatever the policy says about its own scheme, and returns how it
/// would amend one permission.
///
/// # Contract
///
/// Answer [`ConsentSignal::Neutral`] for a permission this provider has no
/// opinion on, including when the signal it reads is absent from the request.
/// Returning [`ConsentSignal::Revoke`] for an absent signal would turn silence
/// into refusal and revoke the permission on every request that did not carry
/// this scheme.
pub trait PermissionSignalProvider: Send + Sync {
    /// Stable identifier, used in configuration, in logs, and by a peer
    /// looking this provider up through [`SignalInput::ask`].
    fn id(&self) -> &'static str;

    /// How this provider would amend `permission` for this request.
    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal;

    /// Whether the request carries an explicit withdrawal of `permission`
    /// under this scheme, as opposed to merely not granting it.
    ///
    /// The difference is destructive. A withdrawal of storage expires the
    /// browser cookie and writes the authoritative tombstone against the
    /// identifier, where a permission that is simply not set for this request
    /// strips the response headers and leaves the identifier alone, so that a
    /// returning visitor is not permanently withdrawn before they ever get to
    /// answer. Most schemes have no such notion, a browser setting and a sale
    /// opt-out included, and leave this at its default of `false`. Whether a
    /// withdrawal is acted on at all is core's decision from the jurisdiction's
    /// baseline, so a refusal only withdraws where the baseline did not grant
    /// the permission outright, because where it did the permission never
    /// depended on the record.
    fn withdraws(&self, _permission: Permission, _input: &SignalInput<'_>) -> bool {
        false
    }

    /// The terms documents this provider says the request's data is
    /// available under, empty when it declares none.
    ///
    /// A locator tells whoever receives the data what terms cover it, so
    /// they can decide whether those are terms they accept and whether they
    /// may pass the data on. The four schemes that ship carry no terms of
    /// their own and leave this at its default, and a provider for a terms
    /// scheme returns the document that applies to this request. Model Terms
    /// for Marketing (MTM) is the first such scheme and one of many rather
    /// than the only one. Core does
    /// not read the documents, it carries the locators, so what a document
    /// says stays between the parties bound by it.
    ///
    /// A locator must point at a document that is never edited once
    /// published, which [`Tdl`] documents and cannot enforce.
    fn tdls(&self, _consent: &ConsentContext, _evidence: &dyn RequestInfo) -> Vec<Tdl> {
        Vec::new()
    }
}

/// Asks every provider in order and returns what they settle on together.
///
/// See the module documentation for the layering and why the order is the
/// configuration.
#[must_use]
pub(crate) fn combine(
    providers: &[Arc<dyn PermissionSignalProvider>],
    permission: Permission,
    consent: &ConsentContext,
    evidence: &dyn RequestInfo,
    policy: &SignalPolicy,
    baseline: Acquisition,
) -> ConsentSignal {
    let mut settled = ConsentSignal::Neutral;
    for (position, provider) in providers.iter().enumerate() {
        let input = SignalInput {
            consent,
            evidence,
            policy,
            baseline,
            settled,
            providers,
            position,
            may_ask: true,
        };
        // Every provider is asked, because a later one may amend what an
        // earlier one settled. Stopping at the first answer would make the
        // order mean the opposite of what it says.
        match provider.signal(permission, &input) {
            ConsentSignal::Neutral => {}
            answer => settled = answer,
        }
    }
    settled
}

/// Whether the request explicitly withdraws `permission`, scoped to the
/// jurisdiction's baseline for it.
///
/// A withdrawal only counts where the baseline did not grant the permission
/// outright. Under a `requires_signal` baseline a refusal is the visitor
/// declining the very signal the permission depended on, so it is
/// destructive. Under a `granted` baseline the permission never depended on
/// the record, so the same refusal suppresses use for the request without
/// destroying anything. Any one provider answering [`withdraws`] is enough,
/// and no provider answering it, or none configured, is never a withdrawal.
///
/// [`withdraws`]: PermissionSignalProvider::withdraws
#[must_use]
pub(crate) fn withdrawn(
    providers: &[Arc<dyn PermissionSignalProvider>],
    permission: Permission,
    consent: &ConsentContext,
    evidence: &dyn RequestInfo,
    policy: &SignalPolicy,
    baseline: Acquisition,
) -> bool {
    if matches!(baseline, Acquisition::Granted) {
        return false;
    }
    providers.iter().enumerate().any(|(position, provider)| {
        let input = SignalInput {
            consent,
            evidence,
            policy,
            baseline,
            settled: ConsentSignal::Neutral,
            providers,
            position,
            may_ask: true,
        };
        provider.withdraws(permission, &input)
    })
}

/// The terms documents the configured providers declare for this request.
///
/// Asked in the same order the providers answer in, so the list reads the
/// way the deployment is configured, and a document named by two providers
/// is carried once. No provider declaring anything leaves the list empty,
/// which says no terms were declared rather than that any terms apply.
#[must_use]
pub(crate) fn tdls(
    providers: &[Arc<dyn PermissionSignalProvider>],
    consent: &ConsentContext,
    evidence: &dyn RequestInfo,
) -> Arc<[Tdl]> {
    let mut declared: Vec<Tdl> = Vec::new();
    for provider in providers {
        for tdl in provider.tdls(consent, evidence) {
            if !declared.contains(&tdl) {
                declared.push(tdl);
            }
        }
    }
    Arc::from(declared)
}

/// The providers a deployment named, in the order it named them, drawn from
/// the ones the build makes available.
///
/// `None` means nothing was configured, which runs every available provider
/// in the order the adapter offered them. That is deliberate, so a publisher
/// gets every scheme the build knows about until they say otherwise and a
/// signal is never quietly ignored because someone forgot to list it. An
/// empty list is a publisher acting on no signal at all, and is accepted.
///
/// # Errors
///
/// A name matching no available provider, or a name given twice, is refused
/// rather than ignored, so a typo cannot silently stop a scheme being honored
/// and a scheme cannot run twice at two places in the order.
pub(crate) fn select(
    available: &[Arc<dyn PermissionSignalProvider>],
    configured: Option<&[String]>,
) -> Result<Vec<Arc<dyn PermissionSignalProvider>>, Report<TrustedServerError>> {
    let Some(names) = configured else {
        return Ok(available.to_vec());
    };
    let mut selected = Vec::with_capacity(names.len());
    for (position, name) in names.iter().enumerate() {
        if names[..position].contains(name) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Permission signal provider `{name}` is named more than once in \
                     [permission_signal] sources. Each provider runs once, at one place \
                     in the order"
                ),
            }));
        }
        let Some(provider) = available.iter().find(|provider| provider.id() == name) else {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Permission signal provider `{name}` is not available in this build. \
                     Available providers are {}",
                    ids(available).join(", ")
                ),
            }));
        };
        selected.push(Arc::clone(provider));
    }
    Ok(selected)
}

/// The available providers a configured list leaves out, for the startup log.
#[must_use]
pub(crate) fn omitted<'a>(
    available: &'a [Arc<dyn PermissionSignalProvider>],
    configured: Option<&[String]>,
) -> Vec<&'a str> {
    let Some(names) = configured else {
        return Vec::new();
    };
    available
        .iter()
        .map(|provider| provider.id())
        .filter(|id| !names.iter().any(|name| name == id))
        .collect()
}

/// The identifiers of `providers`, in order.
#[must_use]
pub(crate) fn ids(providers: &[Arc<dyn PermissionSignalProvider>]) -> Vec<&str> {
    providers.iter().map(|provider| provider.id()).collect()
}

/// Selects the providers a deployment runs from the ones an adapter makes
/// available, and says so in the log once at startup.
///
/// This is the adapter's composition point. Core supplies no provider of its
/// own, so an adapter hands in every scheme crate it links, in the order that
/// stands when configuration names none, and receives back the ordered list
/// the request path asks. The list is shared rather than owned, so handing it
/// to every request's services is one reference count and no copy.
///
/// # Errors
///
/// A configured name that matches nothing available, or a name given twice,
/// fails startup with a message naming what is available, so a typo cannot
/// silently stop a scheme being honored.
pub fn build_permission_signal_providers(
    settings: &Settings,
    available: &[Arc<dyn PermissionSignalProvider>],
) -> Result<Arc<[Arc<dyn PermissionSignalProvider>]>, Report<TrustedServerError>> {
    let configured = settings.permission_signal.sources.as_deref();
    let selected = select(available, configured)?;
    match configured {
        None => log::info!(
            "Permission signals: acting on every provider this build offers, [{}], no \
             [permission_signal] sources configured",
            ids(&selected).join(", ")
        ),
        Some([]) => log::info!(
            "Permission signals: acting on no provider, [permission_signal] sources is \
             empty, so every permission stays at its country and region baseline"
        ),
        Some(_) => log::info!(
            "Permission signals: acting on [{}], asked in that order",
            ids(&selected).join(", ")
        ),
    }
    let left_out = omitted(available, configured);
    if !left_out.is_empty() {
        log::warn!(
            "Permission signals: not acting on [{}], which are not in [permission_signal] \
             sources. A signal this deployment does not act on is read from the request \
             and then ignored",
            left_out.join(", ")
        );
    }
    Ok(Arc::from(selected))
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::*;
    use crate::evidence::OwnedRequestInfo;

    /// A provider that always answers the same thing, for testing the rule
    /// rather than any particular scheme.
    struct Fixed(&'static str, ConsentSignal);

    impl PermissionSignalProvider for Fixed {
        fn id(&self) -> &'static str {
            self.0
        }

        fn signal(&self, _permission: Permission, _input: &SignalInput<'_>) -> ConsentSignal {
            self.1
        }
    }

    /// A provider that answers by consulting a peer, which is the behavior the
    /// peer visibility exists for.
    struct Consulting {
        id: &'static str,
        peer: &'static str,
    }

    impl PermissionSignalProvider for Consulting {
        fn id(&self) -> &'static str {
            self.id
        }

        fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
            match input.ask(self.peer, permission) {
                // The peer refused, and this provider takes the opposite view
                // of the same request, which is the override the seam allows.
                Some(ConsentSignal::Revoke) => ConsentSignal::Grant,
                _ => ConsentSignal::Neutral,
            }
        }
    }

    /// A provider that declares a terms document, which is what a scheme like
    /// Model Terms for Marketing does and none of the four that ship do.
    struct Declaring(&'static str, &'static str);

    impl PermissionSignalProvider for Declaring {
        fn id(&self) -> &'static str {
            self.0
        }

        fn signal(&self, _permission: Permission, _input: &SignalInput<'_>) -> ConsentSignal {
            ConsentSignal::Neutral
        }

        fn tdls(&self, _consent: &ConsentContext, _evidence: &dyn RequestInfo) -> Vec<Tdl> {
            vec![Tdl::new(self.1).expect("should accept the test locator")]
        }
    }

    /// A provider that withdraws storage, for testing the scoping rule.
    struct Withdrawing;

    impl PermissionSignalProvider for Withdrawing {
        fn id(&self) -> &'static str {
            "withdrawing"
        }

        fn signal(&self, _permission: Permission, _input: &SignalInput<'_>) -> ConsentSignal {
            ConsentSignal::Revoke
        }

        fn withdraws(&self, permission: Permission, _input: &SignalInput<'_>) -> bool {
            permission == Permission::StoreOnDevice
        }
    }

    fn no_evidence() -> OwnedRequestInfo {
        OwnedRequestInfo::new(String::new(), HeaderMap::new())
    }

    fn fixed(id: &'static str, signal: ConsentSignal) -> Arc<dyn PermissionSignalProvider> {
        Arc::new(Fixed(id, signal))
    }

    #[test]
    fn a_provider_declaring_no_terms_leaves_the_list_empty() {
        let consent = ConsentContext::default();
        let declared = tdls(
            &[fixed("quiet", ConsentSignal::Grant)],
            &consent,
            &no_evidence(),
        );
        assert!(
            declared.is_empty(),
            "should declare nothing, because the four shipped schemes carry no terms"
        );
    }

    #[test]
    fn terms_are_collected_in_the_order_the_providers_are_asked() {
        let consent = ConsentContext::default();
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![
            Arc::new(Declaring("first", "https://terms.example.com/a/1.txt")),
            fixed("quiet", ConsentSignal::Neutral),
            Arc::new(Declaring("second", "https://terms.example.com/b/1.txt")),
        ];
        let declared = tdls(&providers, &consent, &no_evidence());
        let addresses: Vec<&str> = declared.iter().map(Tdl::as_str).collect();
        assert_eq!(
            addresses,
            vec![
                "https://terms.example.com/a/1.txt",
                "https://terms.example.com/b/1.txt"
            ],
            "should read in the configured order, so the list matches the deployment"
        );
    }

    #[test]
    fn one_document_named_by_two_providers_is_carried_once() {
        let consent = ConsentContext::default();
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![
            Arc::new(Declaring("first", "https://terms.example.com/a/1.txt")),
            Arc::new(Declaring("second", "https://terms.example.com/a/1.txt")),
        ];
        let declared = tdls(&providers, &consent, &no_evidence());
        assert_eq!(
            declared.len(),
            1,
            "should carry the same document once, not once per provider naming it"
        );
    }

    #[test]
    fn versions_of_one_document_are_both_carried() {
        let consent = ConsentContext::default();
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![
            Arc::new(Declaring("first", "https://terms.example.com/a/1.txt")),
            Arc::new(Declaring("second", "https://terms.example.com/a/2.txt")),
        ];
        let declared = tdls(&providers, &consent, &no_evidence());
        assert_eq!(
            declared.len(),
            2,
            "should keep both, because a recipient agreed to one version and not the other"
        );
    }

    #[test]
    fn no_providers_declare_nothing() {
        let consent = ConsentContext::default();
        assert!(
            tdls(&[], &consent, &no_evidence()).is_empty(),
            "should declare nothing when no provider runs, rather than implying terms"
        );
    }

    fn combined(providers: &[Arc<dyn PermissionSignalProvider>]) -> ConsentSignal {
        let consent = ConsentContext::default();
        let policy = SignalPolicy::default();
        combine(
            providers,
            Permission::StoreOnDevice,
            &consent,
            &no_evidence(),
            &policy,
            Acquisition::RequiresSignal,
        )
    }

    /// The same, for a run whose providers differ only in what they answer.
    fn combined_signals(signals: &[ConsentSignal]) -> ConsentSignal {
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = signals
            .iter()
            .map(|signal| fixed("test", *signal))
            .collect();
        combined(&providers)
    }

    fn withdrawn_under(
        providers: &[Arc<dyn PermissionSignalProvider>],
        baseline: Acquisition,
    ) -> bool {
        let consent = ConsentContext::default();
        let policy = SignalPolicy::default();
        withdrawn(
            providers,
            Permission::StoreOnDevice,
            &consent,
            &no_evidence(),
            &policy,
            baseline,
        )
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    // ------------------------------------------------------------------
    // Combining answers in order.
    // ------------------------------------------------------------------

    #[test]
    fn no_providers_leaves_the_place_baseline_alone() {
        assert_eq!(combined_signals(&[]), ConsentSignal::Neutral);
    }

    #[test]
    fn the_last_provider_with_an_opinion_decides() {
        assert_eq!(
            combined_signals(&[ConsentSignal::Revoke, ConsentSignal::Grant]),
            ConsentSignal::Grant,
            "a visitor who answers a prompt after arriving with an opt-out header has \
             their answer applied over it, which is why the order is the policy"
        );
        assert_eq!(
            combined_signals(&[ConsentSignal::Grant, ConsentSignal::Revoke]),
            ConsentSignal::Revoke,
            "and a deployment that wants the opt-out to win puts it last"
        );
    }

    #[test]
    fn silence_leaves_an_earlier_answer_standing() {
        // The failure this guards is a later provider overwriting a settled
        // answer with its own absence, which would let adding a provider nobody
        // uses undo the one that was working.
        assert_eq!(
            combined_signals(&[ConsentSignal::Grant, ConsentSignal::Neutral]),
            ConsentSignal::Grant
        );
        assert_eq!(
            combined_signals(&[ConsentSignal::Revoke, ConsentSignal::Neutral]),
            ConsentSignal::Revoke
        );
    }

    #[test]
    fn silence_from_every_provider_is_not_a_refusal() {
        // The failure this guards is a provider that reads an absent signal as
        // a refusal. It would revoke the permission on every request that did
        // not carry that scheme, which is most of them.
        assert_eq!(
            combined_signals(&[ConsentSignal::Neutral, ConsentSignal::Neutral]),
            ConsentSignal::Neutral
        );
    }

    #[test]
    fn a_provider_sees_what_the_earlier_ones_settled() {
        struct Recording;

        impl PermissionSignalProvider for Recording {
            fn id(&self) -> &'static str {
                "recording"
            }

            fn signal(&self, _permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
                assert_eq!(
                    input.settled,
                    ConsentSignal::Revoke,
                    "a provider is asked with the value the providers before it settled on"
                );
                assert_eq!(input.position(), 1, "and with its own place in the order");
                assert_eq!(input.providers().len(), 2, "and with the whole list");
                assert_eq!(
                    input.baseline,
                    Acquisition::RequiresSignal,
                    "and with what the place rules said before anyone was asked"
                );
                ConsentSignal::Neutral
            }
        }

        let providers: Vec<Arc<dyn PermissionSignalProvider>> =
            vec![fixed("opt-out", ConsentSignal::Revoke), Arc::new(Recording)];
        assert_eq!(combined(&providers), ConsentSignal::Revoke);
    }

    #[test]
    fn a_provider_can_override_a_peer_by_consulting_it() {
        // The worked example from the README: a later provider overrides a
        // refusal because of who made it, not merely that one was made.
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![
            fixed("gpc", ConsentSignal::Revoke),
            Arc::new(Consulting {
                id: "prompt",
                peer: "gpc",
            }),
        ];
        assert_eq!(combined(&providers), ConsentSignal::Grant);

        // The same provider leaves the refusal alone when it came from a peer
        // it was not told to override.
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![
            fixed("other", ConsentSignal::Revoke),
            Arc::new(Consulting {
                id: "prompt",
                peer: "gpc",
            }),
        ];
        assert_eq!(combined(&providers), ConsentSignal::Revoke);
    }

    #[test]
    fn asking_reaches_a_peer_wherever_it_sits_in_the_order() {
        // A provider may consult one configured after it, not only before, so
        // a reordering does not silently change what a provider can see.
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![
            Arc::new(Consulting {
                id: "prompt",
                peer: "gpc",
            }),
            fixed("gpc", ConsentSignal::Revoke),
        ];
        assert_eq!(
            combined(&providers),
            ConsentSignal::Revoke,
            "the consultation succeeded, and the later opt-out then settled it"
        );
    }

    #[test]
    fn asking_for_a_provider_that_is_not_configured_answers_nothing() {
        struct Absent;

        impl PermissionSignalProvider for Absent {
            fn id(&self) -> &'static str {
                "absent"
            }

            fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
                assert!(
                    input.ask("not-configured", permission).is_none(),
                    "a provider must be able to tell a missing peer from a silent one"
                );
                assert!(!input.has("not-configured"));
                assert!(input.has("absent"), "and can see itself in the list");
                ConsentSignal::Neutral
            }
        }

        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![Arc::new(Absent)];
        assert_eq!(combined(&providers), ConsentSignal::Neutral);
    }

    #[test]
    fn a_provider_cannot_consult_itself() {
        struct SelfAsking;

        impl PermissionSignalProvider for SelfAsking {
            fn id(&self) -> &'static str {
                "self-asking"
            }

            fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
                // Without the guard this recurses until the stack is gone.
                assert!(input.ask("self-asking", permission).is_none());
                ConsentSignal::Grant
            }
        }

        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![Arc::new(SelfAsking)];
        assert_eq!(combined(&providers), ConsentSignal::Grant);
    }

    #[test]
    fn two_providers_that_consult_each_other_do_not_loop() {
        // Each consults the other, and the one answering a consultation is
        // refused a consultation of its own, so the pair settles instead of
        // recursing.
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![
            Arc::new(Consulting {
                id: "first",
                peer: "second",
            }),
            Arc::new(Consulting {
                id: "second",
                peer: "first",
            }),
        ];
        assert_eq!(combined(&providers), ConsentSignal::Neutral);
    }

    // ------------------------------------------------------------------
    // Withdrawal scoping.
    // ------------------------------------------------------------------

    #[test]
    fn a_withdrawal_counts_only_where_the_baseline_did_not_grant() {
        let providers: Vec<Arc<dyn PermissionSignalProvider>> = vec![Arc::new(Withdrawing)];
        assert!(
            withdrawn_under(&providers, Acquisition::RequiresSignal),
            "refusing the signal the permission depended on is destructive"
        );
        assert!(
            withdrawn_under(&providers, Acquisition::Denied),
            "and so is refusing under a baseline that never allowed it"
        );
        assert!(
            !withdrawn_under(&providers, Acquisition::Granted),
            "where the permission never depended on the record, the refusal suppresses \
             without destroying"
        );
    }

    #[test]
    fn a_provider_that_merely_revokes_does_not_withdraw() {
        // Revoke and withdraw are different questions. A sale opt-out revokes
        // and must never destroy an identifier.
        let providers: Vec<Arc<dyn PermissionSignalProvider>> =
            vec![fixed("opt-out", ConsentSignal::Revoke)];
        assert!(!withdrawn_under(&providers, Acquisition::RequiresSignal));
    }

    #[test]
    fn no_providers_never_withdraw() {
        assert!(!withdrawn_under(&[], Acquisition::RequiresSignal));
    }

    // ------------------------------------------------------------------
    // Selecting which providers run.
    // ------------------------------------------------------------------

    fn four() -> Vec<Arc<dyn PermissionSignalProvider>> {
        vec![
            fixed("gpc", ConsentSignal::Neutral),
            fixed("gpp-sale-opt-out", ConsentSignal::Neutral),
            fixed("us-privacy", ConsentSignal::Neutral),
            fixed("tcf", ConsentSignal::Neutral),
        ]
    }

    #[test]
    fn naming_nothing_runs_every_available_provider_in_the_offered_order() {
        let selected = select(&four(), None).expect("should accept no configuration");
        assert_eq!(
            ids(&selected),
            vec!["gpc", "gpp-sale-opt-out", "us-privacy", "tcf"],
            "a publisher who configures nothing acts on every scheme the build knows, so \
             one is never ignored because they forgot to list it"
        );
    }

    #[test]
    fn the_configured_order_is_the_order_they_run_in() {
        let reversed = names(&["tcf", "us-privacy", "gpp-sale-opt-out", "gpc"]);
        let selected = select(&four(), Some(&reversed)).expect("should accept known names");
        assert_eq!(
            ids(&selected),
            vec!["tcf", "us-privacy", "gpp-sale-opt-out", "gpc"],
            "the list is the order, not merely the membership"
        );
    }

    #[test]
    fn an_empty_list_is_acting_on_no_signal_and_is_accepted() {
        let selected = select(&four(), Some(&[])).expect("should accept an empty list");
        assert!(selected.is_empty());
        assert_eq!(
            omitted(&four(), Some(&[])).len(),
            4,
            "and every provider is reported left out"
        );
    }

    #[test]
    fn a_name_matching_no_available_provider_is_refused() {
        // Matched rather than `expect_err`, because the success type holds
        // trait objects that are deliberately not `Debug`.
        let Err(error) = select(&four(), Some(&names(&["gpc", "not-a-provider"]))) else {
            panic!("should refuse a name this build does not offer");
        };
        let message = format!("{error:?}");
        assert!(
            message.contains("not-a-provider") && message.contains("gpc, gpp-sale-opt-out"),
            "the refusal names the bad entry and what is available: {message}"
        );
    }

    #[test]
    fn naming_a_provider_twice_is_refused() {
        let Err(error) = select(&four(), Some(&names(&["gpc", "tcf", "gpc"]))) else {
            panic!("should refuse a provider named twice");
        };
        assert!(
            format!("{error:?}").contains("more than once"),
            "a provider runs once, at one place in the order"
        );
    }

    #[test]
    fn a_left_out_provider_is_reported_as_omitted() {
        let configured = names(&["gpc", "tcf"]);
        assert_eq!(
            omitted(&four(), Some(&configured)),
            vec!["gpp-sale-opt-out", "us-privacy"]
        );
        assert!(
            omitted(&four(), None).is_empty(),
            "configuring nothing omits nothing"
        );
    }
}
