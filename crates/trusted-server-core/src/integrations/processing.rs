//! Neutral request-local integration lifecycle contracts.
//!
//! Requirements only restrict existing cache decisions. They never select a
//! buffered body route or authorize sharing rejected by another gate.

use std::sync::Arc;

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Request, Response};

use super::IntegrationDocumentState;
use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Increasing restrictions on origin reuse and representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum OriginRequirement {
    /// Preserve existing origin and cache decisions.
    #[default]
    Ordinary,
    /// Veto shared templates and origin readthrough.
    BypassShared,
    /// Also request unconditional, complete HTML document representations.
    FullHtmlRepresentation,
    /// Also reject an unexpected origin 304 instead of forwarding it.
    FullHtmlRepresentationStrict,
}

/// Increasing scopes of response cache privacy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResponsePrivacyScope {
    /// Preserve existing response privacy.
    #[default]
    Unchanged,
    /// Make synthesized body-carrying HTML private.
    SynthesizedHtml,
    /// Make every response private, including non-HTML and bodiless responses.
    AllResponses,
}

/// Independent monotonic restrictions supplied before cache and origin selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestProcessingRequirements {
    /// Origin freshness and sharing restriction.
    pub origin: OriginRequirement,
    /// Scope at which response privacy is enforced.
    pub privacy: ResponsePrivacyScope,
}

impl RequestProcessingRequirements {
    /// Combine restrictions without weakening either input.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// requirements.restrict(additional_requirements);
    /// ```
    pub fn restrict(&mut self, other: Self) {
        self.origin = self.origin.max(other.origin);
        self.privacy = self.privacy.max(other.privacy);
    }

    /// Whether shared-template and origin-readthrough reuse must be vetoed.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// if requirements.bypass_shared() { skip_shared_cache(); }
    /// ```
    #[must_use]
    pub fn bypass_shared(self) -> bool {
        self.origin >= OriginRequirement::BypassShared
    }

    /// Whether an HTML document requires an unconditional full representation.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// if requirements.requires_full_html() { strip_conditional_headers(&mut request); }
    /// ```
    #[must_use]
    pub fn requires_full_html(self) -> bool {
        self.origin >= OriginRequirement::FullHtmlRepresentation
    }

    /// Whether an unexpected origin 304 must fail closed for an HTML document.
    ///
    /// This is an origin-representation restriction, independent of response privacy.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let reject_304 = requirements.rejects_not_modified_html();
    /// ```
    #[must_use]
    pub fn rejects_not_modified_html(self) -> bool {
        self.origin == OriginRequirement::FullHtmlRepresentationStrict
    }
}

/// Immutable opaque decision carried only by the current request.
///
/// Mutable document buffers belong in `initialize_document`, never in this
/// shared decision or a registration factory.
pub trait PreparedIntegrationRequest: Send + Sync {
    /// Restrictions to join before cache/origin selection.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// requirements.restrict(prepared.requirements());
    /// ```
    fn requirements(&self) -> RequestProcessingRequirements {
        RequestProcessingRequirements::default()
    }

    /// Seed a freshly allocated document state with this request's opaque facts.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// prepared.initialize_document(&document_state);
    /// ```
    fn initialize_document(&self, _state: &IntegrationDocumentState) {}

    /// Request-only markup immediately before the unified tag.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// if let Some(markup) = prepared.before_unified() { head.push_str(&markup); }
    /// ```
    fn before_unified(&self) -> Option<String> {
        None
    }

    /// Synchronous request-only markup immediately after the unified tag.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// if let Some(markup) = prepared.after_unified() { head.push_str(&markup); }
    /// ```
    fn after_unified(&self) -> Option<String> {
        None
    }

    /// Whether the request enables document trace evidence.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let enabled = prepared.document_trace_enabled();
    /// ```
    fn document_trace_enabled(&self) -> bool {
        false
    }

    /// Apply owner-specific response mutations at the existing finalization point.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// prepared.finalize_response(&mut response);
    /// ```
    fn finalize_response(&self, _response: &mut Response<EdgeBody>) {}
}

/// Stateless factory for a request-local opaque decision.
pub trait IntegrationRequestLifecycle: Send + Sync {
    /// Stable owner identifier.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let owner = lifecycle.integration_id();
    /// ```
    fn integration_id(&self) -> &'static str;

    /// Prepare a request after fixed reserved-input normalization.
    ///
    /// # Errors
    ///
    /// Returns the owner's existing preparation error.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let prepared = lifecycle.prepare(&settings, &mut request)?;
    /// ```
    fn prepare(
        &self,
        settings: &Settings,
        request: &mut Request<EdgeBody>,
    ) -> Result<Arc<dyn PreparedIntegrationRequest>, Report<TrustedServerError>>;
}

/// Static idempotent ingress normalizer installed independently of enablement.
///
/// The normalizer captures typed owner facts in [`ReservedInputState`] and
/// removes reserved input before generic request processing.
pub type ReservedInputNormalizer =
    fn(&mut Request<EdgeBody>) -> Result<(), Report<TrustedServerError>>;

/// Marks a successful fixed ingress-normalization pass, not lifecycle completion.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReservedInputsNormalized;

/// Typed owner data captured by static ingress normalization, even when disabled.
#[derive(Clone, Debug, Default)]
pub struct ReservedInputState(pub IntegrationDocumentState);

/// Immutable request-local decisions; cloning does not share document buffers.
#[derive(Clone, Default)]
pub struct RequestProcessingState {
    requirements: RequestProcessingRequirements,
    prepared: Vec<(&'static str, Arc<dyn PreparedIntegrationRequest>)>,
}

impl std::fmt::Debug for RequestProcessingState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestProcessingState")
            .field("requirements", &self.requirements)
            .field(
                "owners",
                &self
                    .prepared
                    .iter()
                    .map(|(owner, _)| owner)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl RequestProcessingState {
    /// Read decisions frozen on the current request, defaulting to ordinary.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let state = RequestProcessingState::from_request(&request);
    /// ```
    #[must_use]
    pub fn from_request(request: &Request<EdgeBody>) -> Self {
        request
            .extensions()
            .get::<Self>()
            .cloned()
            .unwrap_or_default()
    }

    /// Read the restrictions accumulated by all owners.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let requirements = state.requirements();
    /// ```
    #[must_use]
    pub fn requirements(&self) -> RequestProcessingRequirements {
        self.requirements
    }

    /// Whether this owner already prepared the request.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// if !state.contains("example") { state.insert("example", prepared); }
    /// ```
    #[must_use]
    pub fn contains(&self, owner: &str) -> bool {
        self.prepared.iter().any(|(existing, _)| *existing == owner)
    }

    /// Add one immutable owner decision, retaining the first preparation.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// state.insert("example", prepared);
    /// ```
    pub fn insert(&mut self, owner: &'static str, prepared: Arc<dyn PreparedIntegrationRequest>) {
        if self.prepared.iter().any(|(existing, _)| *existing == owner) {
            return;
        }
        self.requirements.restrict(prepared.requirements());
        self.prepared.push((owner, prepared));
    }

    /// Attach a filter's decision without discarding earlier owner restrictions.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// RequestProcessingState::attach(&mut request, "example", prepared);
    /// ```
    pub fn attach(
        request: &mut Request<EdgeBody>,
        owner: &'static str,
        prepared: Arc<dyn PreparedIntegrationRequest>,
    ) {
        let mut state = Self::from_request(request);
        state.insert(owner, prepared);
        request.extensions_mut().insert(state);
    }

    /// Seed request facts into a fresh document's mutable state.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let document_state = IntegrationDocumentState::default();
    /// state.initialize_document(&document_state);
    /// ```
    pub fn initialize_document(&self, state: &IntegrationDocumentState) {
        for (_, prepared) in &self.prepared {
            prepared.initialize_document(state);
        }
    }

    /// Markup in the fixed synchronous phase before the unified tag.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// head.push_str(&state.before_unified());
    /// ```
    #[must_use]
    pub fn before_unified(&self) -> String {
        self.prepared
            .iter()
            .filter_map(|(_, prepared)| prepared.before_unified())
            .collect()
    }

    /// Markup in the fixed synchronous phase after the unified tag.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// head.push_str(&state.after_unified());
    /// ```
    #[must_use]
    pub fn after_unified(&self) -> String {
        self.prepared
            .iter()
            .filter_map(|(_, prepared)| prepared.after_unified())
            .collect()
    }

    /// Whether this request permits document trace evidence.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let eligible = trace_gate.document_eligible(state.document_trace_enabled());
    /// ```
    #[must_use]
    pub fn document_trace_enabled(&self) -> bool {
        self.prepared
            .iter()
            .any(|(_, prepared)| prepared.document_trace_enabled())
    }

    /// Apply request-local response hooks. Privacy is enforced separately by core.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// state.finalize_response(&mut response);
    /// ```
    pub fn finalize_response(&self, response: &mut Response<EdgeBody>) {
        for (_, prepared) in &self.prepared {
            prepared.finalize_response(response);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{IntegrationRegistration, IntegrationRegistry};
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Prepared;
    impl PreparedIntegrationRequest for Prepared {
        fn requirements(&self) -> RequestProcessingRequirements {
            RequestProcessingRequirements {
                origin: OriginRequirement::FullHtmlRepresentation,
                privacy: ResponsePrivacyScope::SynthesizedHtml,
            }
        }
        fn initialize_document(&self, state: &IntegrationDocumentState) {
            state.get_or_insert_with("fixture", || Mutex::new(Vec::<u8>::new()));
        }
    }

    #[derive(Clone)]
    struct NormalizerProbe {
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    fn normalize_probe(request: &mut Request<EdgeBody>) -> Result<(), Report<TrustedServerError>> {
        let probe = request
            .extensions()
            .get::<NormalizerProbe>()
            .expect("should install normalizer probe");
        probe.calls.fetch_add(1, Ordering::SeqCst);
        if probe.fail {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "example normalizer failure".to_owned(),
            }));
        }
        Ok(())
    }

    #[test]
    fn failed_normalization_does_not_mark_completion_or_skip_lifecycle_on_reentry() {
        let settings = crate::test_support::tests::create_test_settings();
        let plan = Arc::new(
            crate::auction::compile_auction_plan(&settings).expect("should compile test plan"),
        );
        let normalizer_calls = Arc::new(AtomicUsize::new(0));
        let lifecycle_calls = Arc::new(AtomicUsize::new(0));
        let static_registry =
            IntegrationRegistry::from_registrations(&settings, Arc::clone(&plan), [], &[])
                .expect("should assemble static-only boundary")
                .with_reserved_input_normalizers(vec![normalize_probe]);
        let mut request = Request::new(EdgeBody::empty());
        request.extensions_mut().insert(NormalizerProbe {
            calls: Arc::clone(&normalizer_calls),
            fail: true,
        });
        assert!(
            static_registry
                .prepare_request(&settings, &mut request)
                .is_err(),
            "should preserve normalization failure"
        );
        assert!(
            request
                .extensions()
                .get::<ReservedInputsNormalized>()
                .is_none(),
            "should never mark a failed pass complete"
        );
        request
            .extensions_mut()
            .get_mut::<NormalizerProbe>()
            .expect("should retain probe")
            .fail = false;
        static_registry
            .prepare_request(&settings, &mut request)
            .expect("should retry a failed pass");
        assert!(
            request
                .extensions()
                .get::<ReservedInputsNormalized>()
                .is_some(),
            "should mark only successful normalization"
        );
        let registration = IntegrationRegistration::builder("fixture")
            .with_request_lifecycle(Arc::new(Lifecycle(Arc::clone(&lifecycle_calls))))
            .build();
        let registry =
            IntegrationRegistry::from_registrations(&settings, plan, [registration], &[])
                .expect("should install required lifecycle")
                .with_reserved_input_normalizers(vec![normalize_probe]);
        registry
            .prepare_request(&settings, &mut request)
            .expect("should run hooks after normalized ingress");
        registry
            .prepare_request(&settings, &mut request)
            .expect("should preserve first hook preparation");
        assert_eq!(
            normalizer_calls.load(Ordering::SeqCst),
            2,
            "should skip only the successful static pass on reentry"
        );
        assert_eq!(
            lifecycle_calls.load(Ordering::SeqCst),
            1,
            "should not confuse static normalization with lifecycle completion"
        );
    }

    struct Lifecycle(Arc<AtomicUsize>);
    impl IntegrationRequestLifecycle for Lifecycle {
        fn integration_id(&self) -> &'static str {
            "fixture"
        }
        fn prepare(
            &self,
            _settings: &Settings,
            _request: &mut Request<EdgeBody>,
        ) -> Result<Arc<dyn PreparedIntegrationRequest>, Report<TrustedServerError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(Prepared))
        }
    }

    #[test]
    fn registry_prepares_once_per_request_without_sharing_mutable_document_state() {
        let settings = crate::test_support::tests::create_test_settings();
        let calls = Arc::new(AtomicUsize::new(0));
        let registration = IntegrationRegistration::builder("fixture")
            .with_request_lifecycle(Arc::new(Lifecycle(Arc::clone(&calls))))
            .with_standalone_js("fixture")
            .with_auction_correlation()
            .build();
        let plan = Arc::new(
            crate::auction::compile_auction_plan(&settings).expect("should compile test plan"),
        );
        let registry =
            IntegrationRegistry::from_registrations(&settings, plan, [registration], &[])
                .expect("should assemble neutral lifecycle");
        let mut first = Request::new(EdgeBody::empty());
        let mut second = Request::new(EdgeBody::empty());
        let first_state = registry
            .prepare_request(&settings, &mut first)
            .expect("should prepare first request");
        let repeated = registry
            .prepare_request(&settings, &mut first)
            .expect("should preserve reentered request");
        let second_state = registry
            .prepare_request(&settings, &mut second)
            .expect("should prepare independent request");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "should prepare once for each request, not each entry point"
        );
        assert_eq!(
            first_state.requirements(),
            repeated.requirements(),
            "should retain monotonic restrictions on reentry"
        );
        assert!(
            registry.auction_correlation_enabled(),
            "should expose deployment capability without activation"
        );
        assert!(
            registry.serves_standalone_js("fixture"),
            "should declare standalone assets without core ID checks"
        );
        let a = IntegrationDocumentState::default();
        let b = IntegrationDocumentState::default();
        first_state.initialize_document(&a);
        second_state.initialize_document(&b);
        let a = a
            .get::<Mutex<Vec<u8>>>("fixture")
            .expect("should initialize first document");
        let b = b
            .get::<Mutex<Vec<u8>>>("fixture")
            .expect("should initialize second document");
        assert!(
            !Arc::ptr_eq(&a, &b),
            "should allocate fresh mutable state for each document"
        );
    }

    #[test]
    fn processing_requirements_join_is_monotonic_on_independent_axes() {
        for origin in [
            OriginRequirement::Ordinary,
            OriginRequirement::BypassShared,
            OriginRequirement::FullHtmlRepresentation,
            OriginRequirement::FullHtmlRepresentationStrict,
        ] {
            for privacy in [
                ResponsePrivacyScope::Unchanged,
                ResponsePrivacyScope::SynthesizedHtml,
                ResponsePrivacyScope::AllResponses,
            ] {
                let mut value = RequestProcessingRequirements { origin, privacy };
                value.restrict(RequestProcessingRequirements {
                    origin: OriginRequirement::BypassShared,
                    privacy: ResponsePrivacyScope::SynthesizedHtml,
                });
                assert!(
                    value.origin >= origin,
                    "should never relax origin requirements"
                );
                assert!(
                    value.privacy >= privacy,
                    "should never relax response privacy"
                );
                value.restrict(RequestProcessingRequirements::default());
                assert!(
                    value.origin >= origin && value.privacy >= privacy,
                    "should ignore ordinary requirements after a veto"
                );
            }
        }
    }

    #[test]
    fn processing_decision_clones_seed_fresh_mutable_document_state() {
        let mut prepared = RequestProcessingState::default();
        prepared.insert("fixture", Arc::new(Prepared));
        let first = IntegrationDocumentState::default();
        let second = IntegrationDocumentState::default();
        prepared.initialize_document(&first);
        prepared.clone().initialize_document(&second);
        let first = first
            .get::<Mutex<Vec<u8>>>("fixture")
            .expect("should initialize first document");
        let second = second
            .get::<Mutex<Vec<u8>>>("fixture")
            .expect("should initialize second document");
        first.lock().expect("should lock first document").push(1);
        assert!(
            second
                .lock()
                .expect("should lock second document")
                .is_empty(),
            "should never reuse mutable buffers across documents"
        );
    }
}
