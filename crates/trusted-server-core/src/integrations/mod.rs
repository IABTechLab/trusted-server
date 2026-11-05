//! Integration module registry and sample implementations.

use std::sync::Arc;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};

use crate::auction::AuctionPlan;
use crate::error::TrustedServerError;
use crate::integration::processing::RequestProcessingState;
use crate::integration::{IntegrationRegistration, IntegrationRegistry};
use crate::settings::Settings;

pub mod adserver_mock;
pub mod aps;
pub mod datadome;
pub mod didomi;
pub mod google_tag_manager;
pub mod gpt;
pub mod gpt_diagnostics;
pub mod js_asset_proxy;
pub mod lockr;
pub mod nextjs;
pub(crate) mod openrtb;
pub mod osano;
pub mod permutive;
pub mod prebid;
mod script_url;
pub mod sourcepoint;
pub mod testlight;

/// Sole schema-one profile catalog until application composition moves owners.
pub(crate) fn profile_definitions() -> &'static [crate::auction::profile::OpenRtbProfileDefinition]
{
    &[
        openrtb::DEFINITION,
        prebid::profile::DEFINITION,
        aps::profile::DEFINITION,
    ]
}

/// Normalize reserved ingress inputs before integration request preparation.
///
/// Settings-only startup failures retain the same reserved-input handling even
/// when no executable registry is available. The legacy catalog owns concrete
/// normalizer selection until the application composition root moves.
///
/// # Errors
///
/// Returns an integration error when reserved-input normalization or request
/// preparation fails.
///
/// # Examples
///
/// ```
/// use edgezero_core::body::Body;
/// use error_stack::Report;
/// use trusted_server_core::{error::TrustedServerError, settings::Settings,
///     integration::processing::RequestProcessingState, integrations::prepare_request};
/// fn prepare(settings: &Settings, request: &mut http::Request<Body>)
///     -> Result<RequestProcessingState, Report<TrustedServerError>> {
///     prepare_request(settings, None, request)
/// }
/// ```
pub fn prepare_request(
    settings: &Settings,
    registry: Option<&IntegrationRegistry>,
    request: &mut http::Request<EdgeBody>,
) -> Result<RequestProcessingState, Report<TrustedServerError>> {
    gpt_diagnostics::normalize_reserved_inputs(request)?;
    if let Some(registry) = registry {
        registry.prepare_request(settings, request)
    } else {
        gpt_diagnostics::prepare_request(settings, request)?;
        Ok(RequestProcessingState::from_request(request))
    }
}

/// Sole schema-one mediator catalog, removed when application composition moves.
pub(crate) fn mediator_definitions() -> &'static [crate::auction::mediator::MediatorDefinition] {
    &[adserver_mock::mediator::DEFINITION]
}

pub(crate) fn compile_legacy_mediator(
    settings: &Settings,
    selected: Option<&str>,
) -> Result<Option<Arc<dyn crate::auction::mediator::CompiledMediator>>, Report<TrustedServerError>>
{
    let Some(id) = selected else {
        return Ok(None);
    };
    let definition = mediator_definitions()
        .iter()
        .find(|definition| definition.id == id)
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!("unknown mediator `{id}`"),
            })
        })?;
    let config = settings.integration_config::<adserver_mock::AdServerMockConfig>(id)?
        .ok_or_else(|| Report::new(TrustedServerError::Configuration { message: format!("auction mediator `{id}` must reference a separately registered enabled integration with the exact same ID") }))?;
    let config =
        serde_json::to_value(config).change_context(TrustedServerError::Configuration {
            message: "cannot compile mediator configuration".to_string(),
        })?;
    (definition.compile)(&config).map(Some)
}

impl IntegrationRegistry {
    /// Build a registry and auction plan from the provided settings for tests.
    ///
    /// Runtime adapters should compile one plan and pass it to [`Self::with_plan`].
    ///
    /// # Errors
    ///
    /// Returns an error if the auction plan or integration registry is invalid.
    #[cfg(test)]
    pub fn new(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        let plan = Arc::new(crate::auction::compile_auction_plan(settings)?);
        Self::with_plan(settings, plan)
    }

    /// Build registrations from the existing catalog around a shared plan.
    ///
    /// Runtime adapters compile one plan and reuse its allocation. Catalog
    /// construction stays here while the registry only assembles its inputs.
    ///
    /// # Errors
    ///
    /// Returns an error when catalog construction, route registration or
    /// asynchronous HTML completion validation fails.
    ///
    /// # Panics
    ///
    /// See [`Self::from_registrations`] for its route-validation invariant.
    pub fn with_plan(
        settings: &Settings,
        plan: Arc<AuctionPlan>,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut registrations = Vec::new();
        if let Some(registration) = prebid::register_for_plan(settings, &plan)? {
            registrations.push(registration);
        }
        if let Some(registration) = aps::register_for_plan(settings, &plan)? {
            registrations.push(registration);
        }
        for builder in builders() {
            if let Some(registration) = (builder.build)(settings)? {
                debug_assert_eq!(
                    registration.integration_id, builder.id,
                    "integration builder ID should match registration ID"
                );
                registrations.push(registration);
            }
        }
        Self::from_registrations(settings, plan, registrations, &[]).map(|registry| {
            registry
                .with_reserved_input_normalizers(vec![gpt_diagnostics::normalize_reserved_inputs])
        })
    }
}

type IntegrationBuilderFn =
    fn(&Settings) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>>;

pub(crate) struct IntegrationBuilder {
    id: &'static str,
    build: IntegrationBuilderFn,
}

pub(crate) fn builders() -> &'static [IntegrationBuilder] {
    &[
        // This must remain first: attribute rewriters chain replacements and short-circuit removals.
        IntegrationBuilder {
            id: js_asset_proxy::JS_ASSET_PROXY_INTEGRATION_ID,
            build: js_asset_proxy::register,
        },
        #[cfg(feature = "test-utils")]
        IntegrationBuilder {
            id: crate::test_support::async_html_finalization::ID,
            build: crate::test_support::async_html_finalization::register,
        },
        IntegrationBuilder {
            id: "testlight",
            build: testlight::register,
        },
        // Next.js must precede GTM: GTM reads the Flight-protection snapshot
        // that the Next.js script stage records for the same fragment.
        IntegrationBuilder {
            id: "nextjs",
            build: nextjs::register,
        },
        IntegrationBuilder {
            id: "permutive",
            build: permutive::register,
        },
        IntegrationBuilder {
            id: "lockr",
            build: lockr::register,
        },
        IntegrationBuilder {
            id: "didomi",
            build: didomi::register,
        },
        IntegrationBuilder {
            id: "sourcepoint",
            build: sourcepoint::register,
        },
        IntegrationBuilder {
            id: "osano",
            build: osano::register,
        },
        IntegrationBuilder {
            id: "google_tag_manager",
            build: google_tag_manager::register,
        },
        IntegrationBuilder {
            id: "datadome",
            build: datadome::register,
        },
        IntegrationBuilder {
            id: "gpt",
            build: gpt::register,
        },
        IntegrationBuilder {
            id: "gpt_diagnostics",
            build: gpt_diagnostics::register,
        },
    ]
}

#[cfg(test)]
pub(crate) fn registered_builder_ids() -> impl Iterator<Item = &'static str> {
    builders().iter().map(|builder| builder.id)
}
