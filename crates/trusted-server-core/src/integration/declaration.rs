//! Pure source-visible integration facts and shared conflict checks.
//!
//! These inputs do not construct clients or read secrets. Executable registration
//! and source validation use the same route insertion and source-transition rules.

mod routes;
mod scripts;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use error_stack::Report;

pub use routes::ReservedRouteClaim;
pub(crate) use routes::{RouteClaims, router_path};
pub use scripts::{
    ScriptAssetPolicy, ScriptSourceAction, ScriptSourceActionKind, ScriptSourceClaim,
    ScriptSourceContext, ScriptSourceKind, ScriptSourceOutcome, ScriptSourceStep,
    ScriptSourceTransition, evaluate_script_sources, validate_script_asset_policy,
};

use super::registry::IntegrationEndpoint;
use crate::error::TrustedServerError;

/// A reserved request input, identified by its name rather than any value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedIntegrationInput {
    /// A Cookie pair reserved by fixed request normalization.
    Cookie(&'static str),
    /// A query parameter reserved by fixed request normalization.
    QueryParameter(&'static str),
}

/// Source-visible facts for one enabled catalog definition.
///
/// This is validation input, not executable capability state. Route paths and
/// script transition values are intentionally absent from its debug output.
#[derive(Clone)]
pub struct IntegrationDeclaration {
    /// Static catalog owner identifier.
    pub integration_id: &'static str,
    /// Number of enabled provider instances owned by this definition.
    pub provider_count: usize,
    /// Profile capabilities declared by this definition, at most one.
    pub openrtb_profiles: Vec<&'static str>,
    /// Number of mediator capabilities, at most one.
    pub mediator_count: usize,
    /// Browser modules owned by this definition, in definition-local order.
    pub browser_modules: Vec<&'static str>,
    /// Renderer types with a handler owned by this definition.
    pub renderer_types: Vec<&'static str>,
    /// Fixed Cookie/query inputs needed by request processing.
    pub reserved_inputs: Vec<ReservedIntegrationInput>,
    /// Source-visible native/proxy routes, in definition-local order.
    pub routes: Vec<IntegrationEndpoint>,
    /// Source-visible script transitions, in execution order.
    pub script_sources: Vec<ScriptSourceClaim>,
}

impl IntegrationDeclaration {
    /// Start an empty declaration for one static catalog owner.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::integration::IntegrationDeclaration;
    /// let declaration = IntegrationDeclaration::new("example");
    /// assert_eq!(declaration.integration_id, "example");
    /// ```
    #[must_use]
    pub fn new(integration_id: &'static str) -> Self {
        Self {
            integration_id,
            provider_count: 0,
            openrtb_profiles: Vec::new(),
            mediator_count: 0,
            browser_modules: Vec::new(),
            renderer_types: Vec::new(),
            reserved_inputs: Vec::new(),
            routes: Vec::new(),
            script_sources: Vec::new(),
        }
    }
}

impl fmt::Debug for IntegrationDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IntegrationDeclaration")
            .field("integration_id", &self.integration_id)
            .field("provider_count", &self.provider_count)
            .field("openrtb_profiles", &self.openrtb_profiles)
            .field("mediator_count", &self.mediator_count)
            .field("browser_modules", &self.browser_modules)
            .field("renderer_types", &self.renderer_types)
            .field("route_count", &self.routes.len())
            .field("script_sources", &self.script_sources)
            .finish_non_exhaustive()
    }
}

/// Validate pure declarations before constructing their executable capabilities.
///
/// Input order is preserved; lookup maps are used only for duplicate detection.
/// Fixed route claims enter the same method-partitioned router before integration
/// claims. Static catalog IDs and field names are the only diagnostic labels.
///
/// # Errors
///
/// Returns an error for duplicate IDs/modules/renderers, invalid single-valued
/// capabilities, inconsistent claim ownership or route insertion conflicts.
/// Configured paths, URLs and transition implementations are never attached.
///
/// # Panics
///
/// Panics only if the pinned router cannot reproduce a previously validated
/// conflicting route during owner identification, violating its invariant.
///
/// # Examples
///
/// ```
/// use trusted_server_core::integration::{IntegrationDeclaration, validate_integration_declarations};
/// validate_integration_declarations(&[IntegrationDeclaration::new("example")], &[])
///     .expect("should accept an empty definition");
/// ```
pub fn validate_integration_declarations(
    declarations: &[IntegrationDeclaration],
    reserved_routes: &[ReservedRouteClaim],
) -> Result<(), Report<TrustedServerError>> {
    let mut ids = BTreeSet::new();
    let mut modules = BTreeMap::new();
    let mut renderers = BTreeMap::new();
    let mut routes = RouteClaims::with_reserved(reserved_routes)?;
    for declaration in declarations {
        let id = declaration.integration_id;
        if !ids.insert(id) {
            return Err(configuration_error(format!(
                "Duplicate integration ID {id}"
            )));
        }
        if declaration.openrtb_profiles.len() > 1
            || (declaration.provider_count > 0 && declaration.openrtb_profiles.len() != 1)
        {
            return Err(configuration_error(format!(
                "Integration {id} must declare exactly one OpenRTB profile when it owns providers and at most one otherwise"
            )));
        }
        if declaration.mediator_count > 1 {
            return Err(configuration_error(format!(
                "Integration {id} declares multiple mediator capabilities"
            )));
        }
        for (capability, owners, values) in [
            ("browser module", &mut modules, &declaration.browser_modules),
            ("renderer type", &mut renderers, &declaration.renderer_types),
        ] {
            for value in values {
                if let Some(previous) = owners.insert(*value, id) {
                    return Err(configuration_error(format!(
                        "Duplicate {capability} {value} between integrations {previous} and {id}"
                    )));
                }
            }
        }
        for claim in &declaration.script_sources {
            if claim.integration_id != id {
                return Err(configuration_error(format!(
                    "Script-source claim owner {} does not match integration {id}",
                    claim.integration_id
                )));
            }
        }
        for route in &declaration.routes {
            routes.integration(id, route)?;
        }
    }
    Ok(())
}

fn configuration_error(message: String) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration { message })
}

#[cfg(test)]
mod tests {
    use http::Method;

    use super::*;

    #[test]
    fn declaration_validation_rejects_duplicate_single_valued_capabilities() {
        for (profiles, mediators, providers) in [
            (vec!["one", "two"], 0, 0),
            (vec![], 0, 1),
            (vec!["one"], 2, 0),
        ] {
            let mut declaration = IntegrationDeclaration::new("owner");
            declaration.openrtb_profiles = profiles;
            declaration.mediator_count = mediators;
            declaration.provider_count = providers;
            let _error = validate_integration_declarations(&[declaration], &[])
                .expect_err("should reject invalid single-valued capability facts");
        }
        let mut declaration = IntegrationDeclaration::new("owner");
        declaration.openrtb_profiles.push("profile");
        declaration.provider_count = 2;
        validate_integration_declarations(&[declaration], &[]).expect(
            "should allow several provider instances to share their owner's single profile",
        );
    }

    #[test]
    fn declaration_validation_rejects_duplicate_ids_modules_and_renderers() {
        let first = IntegrationDeclaration::new("owner");
        let _error = validate_integration_declarations(&[first.clone(), first], &[])
            .expect_err("should reject duplicate integration ID");
        for renderer in [false, true] {
            let mut first = IntegrationDeclaration::new("first");
            let mut second = IntegrationDeclaration::new("second");
            if renderer {
                first.renderer_types.push("renderer");
                second.renderer_types.push("renderer");
            } else {
                first.browser_modules.push("module");
                second.browser_modules.push("module");
            }
            let _error = validate_integration_declarations(&[first, second], &[])
                .expect_err("should reject duplicate capability ownership");
        }
    }

    #[test]
    fn declaration_routes_obey_reserved_claims_without_disclosing_paths() {
        let mut declaration = IntegrationDeclaration::new("owner");
        declaration
            .routes
            .push(IntegrationEndpoint::get("/credential-sentinel/{name}"));
        let reserved = [ReservedRouteClaim::new(
            "application.discovery",
            Method::GET,
            "/credential-sentinel/{id}",
        )];
        let error = validate_integration_declarations(&[declaration.clone()], &reserved)
            .expect_err("should reject reserved-route collision in pure validation");
        assert!(
            !format!("{error:?}").contains("credential-sentinel"),
            "should redact configured route"
        );
        assert!(
            !format!("{declaration:?}").contains("credential-sentinel"),
            "should redact declaration route values"
        );
    }
}
