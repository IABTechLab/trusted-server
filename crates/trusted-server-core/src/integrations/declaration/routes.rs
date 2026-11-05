use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use error_stack::Report;
use http::Method;
use matchit::{InsertError, Router};

use super::super::registry::IntegrationEndpoint;
use crate::error::TrustedServerError;

/// A fixed application route that an integration cannot replace.
///
/// The owning adapter supplies its actual route declarations. Paths stay private
/// to validation; diagnostics report only the trusted resource identifier.
#[derive(Clone)]
pub struct ReservedRouteClaim {
    pub(crate) resource: &'static str,
    pub(crate) endpoint: IntegrationEndpoint,
}

impl ReservedRouteClaim {
    /// Declare one method and path from an application's fixed route table.
    ///
    /// # Examples
    ///
    /// ```
    /// use http::Method;
    /// use trusted_server_core::integrations::ReservedRouteClaim;
    /// let claim = ReservedRouteClaim::new("application.auction", Method::POST, "/auction");
    /// ```
    #[must_use]
    pub fn new(resource: &'static str, method: Method, path: impl Into<String>) -> Self {
        Self {
            resource,
            endpoint: IntegrationEndpoint::new(method, path),
        }
    }
}

impl fmt::Debug for ReservedRouteClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReservedRouteClaim")
            .field("resource", &self.resource)
            .field("method", &self.endpoint.method)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
enum RouteOwner {
    Integration(&'static str),
    Reserved(&'static str),
}

impl fmt::Display for RouteOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integration(id) => write!(formatter, "integration {id}"),
            Self::Reserved(resource) => write!(formatter, "reserved route {resource}"),
        }
    }
}

struct AcceptedRoute {
    owner: RouteOwner,
    endpoint: IntegrationEndpoint,
}

/// The same method-partitioned insertion check serves pure declarations and
/// executable registry construction. It never attaches matchit's pathful errors.
#[derive(Default)]
pub(crate) struct RouteClaims {
    routers: BTreeMap<String, Router<usize>>,
    accepted: Vec<AcceptedRoute>,
}

pub(crate) fn router_path(path: &str) -> Cow<'_, str> {
    match path.strip_suffix("/*") {
        Some(prefix) => Cow::Owned(format!("{prefix}/{{*rest}}")),
        None => Cow::Borrowed(path),
    }
}

pub(crate) fn supports_integration_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET
            | Method::POST
            | Method::PUT
            | Method::DELETE
            | Method::PATCH
            | Method::HEAD
            | Method::OPTIONS
    )
}

impl RouteClaims {
    pub(crate) fn with_reserved(
        claims: &[ReservedRouteClaim],
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut result = Self::default();
        for claim in claims {
            result.insert(RouteOwner::Reserved(claim.resource), &claim.endpoint)?;
        }
        Ok(result)
    }

    pub(crate) fn integration(
        &mut self,
        id: &'static str,
        endpoint: &IntegrationEndpoint,
    ) -> Result<(), Report<TrustedServerError>> {
        // Match the existing registry's supported methods. Unsupported proxy
        // methods are inactive, not a new source-validation restriction.
        if supports_integration_method(&endpoint.method) {
            self.insert(RouteOwner::Integration(id), endpoint)?;
        }
        Ok(())
    }

    fn insert(
        &mut self,
        owner: RouteOwner,
        endpoint: &IntegrationEndpoint,
    ) -> Result<(), Report<TrustedServerError>> {
        let router = self
            .routers
            .entry(endpoint.method.as_str().to_owned())
            .or_default();
        if let Err(error) = router.insert(router_path(&endpoint.path), self.accepted.len()) {
            let message = if matches!(error, InsertError::Conflict { .. }) {
                let previous = self
                    .accepted
                    .iter()
                    .find(|accepted| {
                        if accepted.endpoint.method != endpoint.method {
                            return false;
                        }
                        // Error text can unescape braces. Use the router itself to
                        // find the owner instead of comparing its formatted error.
                        let mut pair = Router::new();
                        pair.insert(router_path(&accepted.endpoint.path), ())
                            .expect("should reinsert a previously validated route");
                        matches!(
                            pair.insert(router_path(&endpoint.path), ()),
                            Err(InsertError::Conflict { .. })
                        )
                    })
                    .expect("should identify the previously accepted conflicting route");
                format!(
                    "Integration route conflict between {} and {owner} for {}",
                    previous.owner, endpoint.method
                )
            } else {
                format!("Invalid integration route declaration for {owner}")
            };
            return Err(Report::new(TrustedServerError::Configuration { message }));
        }
        self.accepted.push(AcceptedRoute {
            owner,
            endpoint: endpoint.clone(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_claims_use_runtime_partition_and_wildcard_rules() {
        let mut claims = RouteClaims::default();
        claims
            .integration("first", &IntegrationEndpoint::get("/assets/*"))
            .expect("should accept wildcard");
        claims
            .integration("second", &IntegrationEndpoint::post("/assets/{*rest}"))
            .expect("should partition methods");
        let error = claims
            .integration("third", &IntegrationEndpoint::get("/assets/{*tail}"))
            .expect_err("should reject equivalent catch-all routes");
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("first") && diagnostic.contains("third"),
            "should identify both owners"
        );
        assert!(
            !diagnostic.contains("/assets"),
            "should not disclose configured paths"
        );
    }

    #[test]
    fn reserved_claims_are_inserted_before_integration_routes() {
        let reserved = [ReservedRouteClaim::new(
            "application.auction",
            Method::POST,
            "/credential-sentinel/{id}",
        )];
        let mut claims =
            RouteClaims::with_reserved(&reserved).expect("should register fixed route");
        let error = claims
            .integration(
                "asset-owner",
                &IntegrationEndpoint::post("/credential-sentinel/{other}"),
            )
            .expect_err("should reject reserved route collision");
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("application.auction") && diagnostic.contains("asset-owner"),
            "should identify fixed resource and integration"
        );
        assert!(
            !diagnostic.contains("credential-sentinel"),
            "should not disclose the route path"
        );
        assert!(
            !format!("{reserved:?}").contains("credential-sentinel"),
            "should redact reserved-claim debug output"
        );
    }

    #[test]
    fn static_routes_and_parameter_routes_can_coexist() {
        let mut claims = RouteClaims::default();
        claims
            .integration("static-owner", &IntegrationEndpoint::get("/assets/static"))
            .expect("should accept static path");
        claims
            .integration("parameter-owner", &IntegrationEndpoint::get("/assets/{id}"))
            .expect("should preserve matchit precedence");
    }

    #[test]
    fn escaped_literal_conflicts_identify_owners_without_paths() {
        let mut claims = RouteClaims::default();
        let endpoint = IntegrationEndpoint::get("/credential-sentinel/{{literal}}");
        claims
            .integration("first", &endpoint)
            .expect("should accept escaped braces");
        let error = claims
            .integration("second", &endpoint)
            .expect_err("should reject duplicate escaped path");
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("first") && diagnostic.contains("second"),
            "should identify both owners"
        );
        assert!(
            !diagnostic.contains("credential-sentinel"),
            "should omit the configured path"
        );
    }

    #[test]
    fn invalid_route_diagnostics_do_not_attach_matchit_errors() {
        let mut claims = RouteClaims::default();
        let error = claims
            .integration("owner", &IntegrationEndpoint::get("/credential-sentinel/{"))
            .expect_err("should reject invalid pattern");
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("owner"),
            "should identify declaration owner"
        );
        assert!(
            !diagnostic.contains("credential-sentinel"),
            "should omit invalid configured path"
        );
    }
}
