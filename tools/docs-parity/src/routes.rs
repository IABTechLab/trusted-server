//! Closed adapter-route records and Cloudflare builder extraction.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde::Deserialize;
use syn::visit::{self, Visit};
use syn::{Attribute, Block, Expr, Item, Lit, Pat, Stmt};

use crate::integrations::IntegrationInventory;
use crate::markdown::{GeneratedRegion, GeneratedRow};
use crate::repository::{NormalizedRelativePath, Repository};

const MAX_ROUTE_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ROUTE_ENTRIES: usize = 4096;
const MAX_ROUTE_STRING_BYTES: usize = 16 * 1024;
type RawRouteIdentity = (String, String, String, bool);
type RouteGroupKey = (String, String, RouteShape, String, RouteStatus, bool);

#[derive(Default)]
struct RouteAccumulator {
    identities: BTreeSet<RawRouteIdentity>,
    groups: BTreeMap<RouteGroupKey, RouteRecord>,
}

impl RouteAccumulator {
    fn insert(&mut self, record: &RouteRecord) -> Result<(), Report<RouteError>> {
        for method in &record.methods {
            let identity = (
                record.adapter.clone(),
                record.path.clone(),
                method.clone(),
                record.startup_router,
            );
            if !self.identities.insert(identity) {
                return Err(invalid(format!(
                    "duplicate expanded route semantic (raw identity): {} {method} {} startup={}",
                    record.adapter, record.path, record.startup_router
                )));
            }
            let key = (
                record.adapter.clone(),
                record.path.clone(),
                record.shape,
                record.predicate.clone(),
                record.status,
                record.startup_router,
            );
            self.groups
                .entry(key)
                .and_modify(|existing| {
                    existing.methods.insert(method.clone());
                })
                .or_insert_with(|| {
                    RouteRecord::new(
                        &record.adapter,
                        &record.path,
                        [method],
                        record.shape,
                        &record.predicate,
                        record.status,
                        record.startup_router,
                    )
                });
        }
        Ok(())
    }

    fn extend(
        &mut self,
        records: impl IntoIterator<Item = RouteRecord>,
    ) -> Result<(), Report<RouteError>> {
        for record in records {
            self.insert(&record)?;
        }
        Ok(())
    }

    fn finish(self) -> BTreeSet<RouteRecord> {
        self.groups.into_values().collect()
    }
}

/// Route-inventory validation failure.
#[derive(Debug, derive_more::Display)]
pub enum RouteError {
    /// A checked or extracted route uses an unsupported shape.
    #[display("invalid route inventory: {detail}")]
    Invalid { detail: String },
    /// A particular route semantic differs from its checked record.
    #[display("route {axis} inventory differs")]
    Drift { axis: &'static str },
}

impl core::error::Error for RouteError {}

/// How a route path is obtained.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum RouteShape {
    /// A fixed path literal or named path constant.
    Literal,
    /// A router path template.
    Template,
    /// A path or prefix obtained from operator configuration.
    ConfigDerived,
    /// A route registered only under a named configuration predicate.
    Conditional,
}

/// Observable routing disposition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    /// The adapter serves the real handler.
    Real,
    /// The adapter deliberately returns not-supported.
    Unsupported,
    /// The adapter deliberately denies or guards the route.
    Guarded,
    /// The request is forwarded through publisher fallback.
    PublisherFallback,
    /// The route returns the adapter's startup error.
    StartupError,
}

/// One exact adapter route contract.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct RouteRecord {
    /// Stable adapter identifier.
    pub adapter: String,
    /// Literal, template, or symbolic config-derived path.
    pub path: String,
    /// Exact registered methods.
    pub methods: BTreeSet<String>,
    /// Path provenance.
    pub shape: RouteShape,
    /// Exact registration/configuration predicate.
    pub predicate: String,
    /// Observable handler disposition.
    pub status: RouteStatus,
    /// Whether this record belongs to the startup-error router.
    pub startup_router: bool,
}

impl RouteRecord {
    /// Construct a normalized route record.
    #[must_use]
    pub fn new<M>(
        adapter: &str,
        path: &str,
        methods: M,
        shape: RouteShape,
        predicate: &str,
        status: RouteStatus,
        startup_router: bool,
    ) -> Self
    where
        M: IntoIterator,
        M::Item: AsRef<str>,
    {
        Self {
            adapter: adapter.to_owned(),
            path: path.to_owned(),
            methods: methods
                .into_iter()
                .map(|method| method.as_ref().to_owned())
                .collect(),
            shape,
            predicate: predicate.to_owned(),
            status,
            startup_router,
        }
    }
}

/// Adapter source files that jointly define the complete route inventory.
pub struct RouteSources<'a> {
    /// Core publisher source that owns shared route-path constants.
    pub publisher_routes: &'a str,
    /// Core EC admin source that owns the portable unsupported response.
    pub admin_routes: &'a str,
    /// Fastly router source.
    pub fastly_app: &'a str,
    /// Fastly pre-router entrypoint source.
    pub fastly_entrypoint: &'a str,
    /// Axum router source.
    pub axum_app: &'a str,
    /// Cloudflare inline router source.
    pub cloudflare_app: &'a str,
    /// Spin router source.
    pub spin_app: &'a str,
}

/// Parsed and expanded checked route manifest.
#[derive(Debug)]
pub struct RouteManifest {
    routes: BTreeSet<RouteRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRouteManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    routes: Vec<RawRoute>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    adapters: Vec<String>,
    path: String,
    methods: Vec<String>,
    shape: RouteShape,
    predicate: String,
    status: RouteStatus,
    #[serde(default)]
    startup_router: bool,
}

impl RouteManifest {
    /// Parse and expand grouped adapter rows from a reviewed manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed data, unknown fields, unsupported
    /// versions, an absent review attestation, or duplicate expanded records.
    pub fn parse(source: &str) -> Result<Self, Report<RouteError>> {
        ensure_route_input_bound("route manifest", source)?;
        let manifest = toml::from_str::<RawRouteManifest>(source)
            .map_err(|error| invalid(format!("malformed route manifest: {error}")))?;
        if manifest.version != 1 {
            return Err(invalid("route manifest version must equal 1"));
        }
        if !manifest.reviewed {
            return Err(invalid("route manifest reviewed must be true"));
        }
        if manifest.routes.len() > MAX_ROUTE_ENTRIES {
            return Err(invalid("route row cardinality limit exceeded"));
        }
        let mut routes = RouteAccumulator::default();
        for row in manifest.routes {
            if row.path.len() > MAX_ROUTE_STRING_BYTES
                || row.predicate.len() > MAX_ROUTE_STRING_BYTES
            {
                return Err(invalid("route row contains an oversized string"));
            }
            let adapters = unique_route_values(row.adapters, "adapter")?;
            let methods = unique_route_values(row.methods, "method")?;
            if adapters.is_empty() {
                return Err(invalid("route row adapters must not be empty"));
            }
            if methods.is_empty() {
                return Err(invalid("route row methods must not be empty"));
            }
            for adapter in adapters {
                if !matches!(adapter.as_str(), "fastly" | "axum" | "cloudflare" | "spin") {
                    return Err(invalid(format!("unknown route adapter: {adapter}")));
                }
                let record = RouteRecord::new(
                    &adapter,
                    &row.path,
                    &methods,
                    row.shape,
                    &row.predicate,
                    row.status,
                    row.startup_router,
                );
                routes.insert(&record)?;
            }
        }
        Ok(Self {
            routes: routes.finish(),
        })
    }

    /// Expanded exact route set.
    #[must_use]
    pub fn routes(&self) -> &BTreeSet<RouteRecord> {
        &self.routes
    }
}

/// One manually reviewed adapter operational-support record.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct AdapterSupportRecord {
    id: String,
    release_status: String,
    owner: String,
    reviewed_at: String,
    health: String,
    startup_status: u16,
    startup_health: bool,
    provider_fanout: String,
    trusted_client_ip: String,
    request_normalization: String,
}

/// Checked adapter support records.
#[derive(Debug)]
pub struct AdapterSupportManifest {
    adapters: BTreeSet<AdapterSupportRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAdapterSupportManifest {
    version: u32,
    reviewed: bool,
    #[serde(default)]
    adapters: Vec<AdapterSupportRecord>,
}

impl AdapterSupportManifest {
    /// Parse manually owned adapter support records.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or unreviewed records and duplicate IDs.
    pub fn parse(source: &str) -> Result<Self, Report<RouteError>> {
        ensure_route_input_bound("adapter support manifest", source)?;
        let manifest = toml::from_str::<RawAdapterSupportManifest>(source)
            .map_err(|error| invalid(format!("malformed adapter support manifest: {error}")))?;
        if manifest.version != 1 || !manifest.reviewed {
            return Err(invalid(
                "adapter support manifest requires version 1 and reviewed=true",
            ));
        }
        if manifest.adapters.len() > MAX_ROUTE_ENTRIES {
            return Err(invalid("adapter support row cardinality limit exceeded"));
        }
        let mut ids = BTreeSet::new();
        let adapters = manifest
            .adapters
            .into_iter()
            .map(|record| {
                if [
                    &record.id,
                    &record.release_status,
                    &record.owner,
                    &record.reviewed_at,
                    &record.health,
                    &record.provider_fanout,
                    &record.trusted_client_ip,
                    &record.request_normalization,
                ]
                .into_iter()
                .any(|value| value.len() > MAX_ROUTE_STRING_BYTES)
                {
                    return Err(invalid("adapter support row contains an oversized string"));
                }
                if !ids.insert(record.id.clone()) {
                    return Err(invalid(format!(
                        "duplicate adapter support row: {}",
                        record.id
                    )));
                }
                if record.owner.trim().is_empty()
                    || !is_review_date(&record.reviewed_at)
                    || !matches!(
                        record.release_status.as_str(),
                        "production" | "development" | "experimental"
                    )
                {
                    return Err(invalid(format!(
                        "invalid manual ownership/status for adapter {}",
                        record.id
                    )));
                }
                Ok(record)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self { adapters })
    }
}

/// Validate the exact four adapter startup/fan-out facts while leaving release
/// maturity manually owned.
///
/// # Errors
///
/// Returns an error when an adapter is missing/extra or a source-backed support
/// fact differs from the known runtime contract.
pub fn validate_adapter_support(
    manifest: &AdapterSupportManifest,
) -> Result<(), Report<RouteError>> {
    let facts = manifest
        .adapters
        .iter()
        .map(|record| {
            (
                record.id.as_str(),
                record.health.as_str(),
                record.startup_status,
                record.startup_health,
                record.provider_fanout.as_str(),
                record.trusted_client_ip.as_str(),
                record.request_normalization.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        (
            "axum",
            "real",
            500,
            false,
            "multiple",
            "outermost_sanitize",
            "none",
        ),
        (
            "cloudflare",
            "absent",
            500,
            false,
            "single",
            "outermost_sanitize",
            "none",
        ),
        (
            "fastly",
            "pre_router",
            500,
            true,
            "multiple",
            "entry_point_resolve_and_sanitize",
            "none",
        ),
        (
            "spin",
            "real",
            503,
            true,
            "single",
            "outermost_sanitize",
            "innermost_spin_headers",
        ),
    ]);
    if facts != expected {
        return Err(Report::new(RouteError::Drift {
            axis: "adapter-support",
        }));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct DocumentationManifest {
    #[serde(default)]
    regions: Vec<DocumentationRegion>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentationRegion {
    name: String,
    path: String,
    columns: Vec<String>,
    #[serde(default)]
    rows: Vec<DocumentationRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentationRow {
    key: String,
    cells: Vec<String>,
}

const API_REFERENCE_PATH: &str = "docs/guide/api-reference.md";
const API_ROUTE_REGION: &str = "api-route-availability";
const API_INTEGRATION_REGION: &str = "api-integration-route-families";
const API_ADAPTER_REGION: &str = "api-adapter-support";

/// Build the API reference's generated regions directly from checked route,
/// integration, and adapter-support records.
#[must_use]
pub fn documentation_regions(
    routes: &RouteManifest,
    support: &AdapterSupportManifest,
    integrations: &IntegrationInventory,
) -> Vec<GeneratedRegion> {
    vec![
        adapter_documentation_region(support),
        route_documentation_region(routes, support),
        integration_route_documentation_region(integrations),
    ]
}

/// Require the pages manifest to declare each API region exactly once at the
/// canonical document and with no hand-maintained rows.
///
/// # Errors
///
/// Returns an error when a required declaration is absent, duplicated,
/// redirected, has different columns, or contains copied route data.
pub fn validate_documentation_contract(
    routes: &RouteManifest,
    support: &AdapterSupportManifest,
    integrations: &IntegrationInventory,
    pages_source: &str,
) -> Result<(), Report<RouteError>> {
    ensure_route_input_bound("pages manifest", pages_source)?;
    let manifest = toml::from_str::<DocumentationManifest>(pages_source)
        .map_err(|error| invalid(format!("malformed API documentation manifest: {error}")))?;
    let mut declarations = BTreeMap::new();
    for region in manifest.regions {
        if declarations.insert(region.name.clone(), region).is_some() {
            return Err(invalid("duplicate API documentation region declaration"));
        }
    }
    for expected in documentation_regions(routes, support, integrations) {
        let declaration = declarations
            .get(&expected.name)
            .ok_or_else(|| invalid(format!("missing {} region", expected.name)))?;
        if declaration.path != API_REFERENCE_PATH || declaration.columns != expected.columns {
            return Err(invalid(format!(
                "{} path or columns differ from the checked record",
                expected.name
            )));
        }
        if !declaration.rows.is_empty() {
            let first = declaration
                .rows
                .first()
                .map(|row| format!("{} ({} cells)", row.key, row.cells.len()))
                .unwrap_or_default();
            return Err(invalid(format!(
                "{} rows must be generated from checked records, found {first}",
                expected.name
            )));
        }
    }
    Ok(())
}

pub(crate) fn repository_documentation_regions(
    repository: &Repository,
) -> Result<Vec<GeneratedRegion>, Report<RouteError>> {
    let routes = RouteManifest::parse(&read_utf8(
        repository,
        "tools/docs-parity/manifests/routes.toml",
    )?)?;
    let support = AdapterSupportManifest::parse(&read_utf8(
        repository,
        "tools/docs-parity/manifests/adapter-support.toml",
    )?)?;
    validate_adapter_support(&support)?;
    let integrations = IntegrationInventory::parse(&read_utf8(
        repository,
        "tools/docs-parity/manifests/integrations.toml",
    )?)
    .map_err(|error| invalid(format!("cannot parse integration routes: {error}")))?;
    Ok(documentation_regions(&routes, &support, &integrations))
}

fn adapter_documentation_region(support: &AdapterSupportManifest) -> GeneratedRegion {
    let rows = support
        .adapters
        .iter()
        .map(|adapter| GeneratedRow {
            key: adapter.id.clone(),
            cells: vec![
                format!("`{}`", adapter.id),
                adapter.release_status.clone(),
                adapter.health.replace('_', " "),
                format!("`{}`", adapter.startup_status),
                if adapter.startup_health { "yes" } else { "no" }.to_owned(),
                adapter.provider_fanout.clone(),
                match adapter.trusted_client_ip.as_str() {
                    "entry_point_resolve_and_sanitize" => "entry-point resolve + sanitize",
                    "outermost_sanitize" => "outermost sanitize",
                    _ => "invalid",
                }
                .to_owned(),
                match adapter.request_normalization.as_str() {
                    "none" => "none",
                    "innermost_spin_headers" => "innermost Spin-header derivation",
                    _ => "invalid",
                }
                .to_owned(),
            ],
        })
        .collect();
    GeneratedRegion {
        name: API_ADAPTER_REGION.to_owned(),
        columns: vec![
            "Adapter",
            "Release status",
            "Health",
            "Startup status",
            "Startup health",
            "Provider fan-out",
            "Trusted-client-IP handling",
            "Request normalization",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        rows,
    }
}

fn route_documentation_region(
    routes: &RouteManifest,
    support: &AdapterSupportManifest,
) -> GeneratedRegion {
    type RouteKey = (bool, String, BTreeSet<String>, RouteShape, String);
    let mut grouped = BTreeMap::<RouteKey, BTreeMap<String, RouteStatus>>::new();
    for route in &routes.routes {
        grouped
            .entry((
                route.startup_router,
                route.path.clone(),
                route.methods.clone(),
                route.shape,
                route.predicate.clone(),
            ))
            .or_default()
            .insert(route.adapter.clone(), route.status);
    }
    let startup_status = support
        .adapters
        .iter()
        .map(|adapter| (adapter.id.as_str(), adapter.startup_status))
        .collect::<BTreeMap<_, _>>();
    let rows = grouped
        .into_iter()
        .map(|((startup, path, methods, shape, predicate), adapters)| {
            let key = format!(
                "{}|{path}|{}|{}|{predicate}",
                if startup { "startup" } else { "normal" },
                methods.iter().cloned().collect::<Vec<_>>().join(","),
                route_shape_name(shape)
            );
            let mut cells = vec![
                if startup { "startup" } else { "normal" }.to_owned(),
                format!("`{path}`"),
                methods
                    .iter()
                    .map(|method| format!("`{method}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                route_shape_name(shape).to_owned(),
                format!("`{predicate}`"),
            ];
            for adapter in ["fastly", "axum", "cloudflare", "spin"] {
                cells.push(match adapters.get(adapter) {
                    Some(RouteStatus::StartupError) => format!(
                        "startup error (`{}`)",
                        startup_status
                            .get(adapter)
                            .expect("support validation requires every adapter")
                    ),
                    Some(status) => route_status_name(*status).to_owned(),
                    None => "—".to_owned(),
                });
            }
            GeneratedRow { key, cells }
        })
        .collect();
    GeneratedRegion {
        name: API_ROUTE_REGION.to_owned(),
        columns: [
            "Router",
            "Path",
            "Methods",
            "Shape",
            "Predicate",
            "Fastly",
            "Axum",
            "Cloudflare",
            "Spin",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        rows,
    }
}

fn integration_route_documentation_region(integrations: &IntegrationInventory) -> GeneratedRegion {
    let rows = integrations
        .capabilities
        .iter()
        .flat_map(|capability| {
            let routes = if capability.proxy_routes.is_empty() {
                vec!["None".to_owned()]
            } else {
                capability
                    .proxy_routes
                    .iter()
                    .map(|route| format!("`{route}`"))
                    .collect()
            };
            routes.into_iter().map(|route| GeneratedRow {
                key: format!("{}|{}|{route}", capability.id, capability.predicate),
                cells: vec![
                    format!("`{}`", capability.id),
                    format!("`{}`", capability.predicate),
                    route,
                ],
            })
        })
        .collect();
    GeneratedRegion {
        name: API_INTEGRATION_REGION.to_owned(),
        columns: ["Integration", "Registration predicate", "HTTP routes"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        rows,
    }
}

const fn route_shape_name(shape: RouteShape) -> &'static str {
    match shape {
        RouteShape::Literal => "literal",
        RouteShape::Template => "template",
        RouteShape::ConfigDerived => "config derived",
        RouteShape::Conditional => "conditional",
    }
}

const fn route_status_name(status: RouteStatus) -> &'static str {
    match status {
        RouteStatus::Real => "real",
        RouteStatus::Unsupported => "unsupported",
        RouteStatus::Guarded => "guarded",
        RouteStatus::PublisherFallback => "publisher fallback",
        RouteStatus::StartupError => "startup error",
    }
}

/// Extract all fixed, fallback, config-derived, and degraded adapter routes.
///
/// # Errors
///
/// Returns an error when a named collection or required routing decision leaves
/// the closed grammar.
pub fn extract_repository_routes(
    sources: &RouteSources<'_>,
) -> Result<BTreeSet<RouteRecord>, Report<RouteError>> {
    for (label, source) in [
        ("Fastly app", sources.fastly_app),
        ("Fastly entrypoint", sources.fastly_entrypoint),
        ("Axum app", sources.axum_app),
        ("Cloudflare app", sources.cloudflare_app),
        ("Spin app", sources.spin_app),
        ("Core publisher", sources.publisher_routes),
        ("Core EC admin", sources.admin_routes),
    ] {
        ensure_route_input_bound(label, source)?;
    }
    let authoritative_paths = publisher_route_constants(sources.publisher_routes)?;
    let mut routes = RouteAccumulator::default();
    routes.extend(extract_named_routes_with_constants(
        "fastly",
        sources.fastly_app,
        &authoritative_paths,
    )?)?;
    routes.extend(extract_named_routes_with_constants(
        "axum",
        sources.axum_app,
        &authoritative_paths,
    )?)?;
    routes.extend(extract_named_routes_with_constants(
        "spin",
        sources.spin_app,
        &authoritative_paths,
    )?)?;
    routes.extend(extract_cloudflare_routes_with_constants(
        sources.cloudflare_app,
        &authoritative_paths,
    )?)?;

    validate_special_route_sources(sources, &authoritative_paths)?;

    let fallback_methods = ["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"];
    for adapter in ["fastly", "axum", "spin"] {
        add_record(
            &mut routes,
            adapter,
            "/",
            &fallback_methods,
            RouteShape::Literal,
            "publisher_fallback",
            RouteStatus::PublisherFallback,
            false,
        )?;
        add_record(
            &mut routes,
            adapter,
            "/{*rest}",
            &fallback_methods,
            RouteShape::Template,
            "publisher_fallback",
            RouteStatus::PublisherFallback,
            false,
        )?;
    }
    for adapter in ["fastly", "axum", "spin"] {
        add_record(
            &mut routes,
            adapter,
            "/health",
            &["GET"],
            RouteShape::Literal,
            "always",
            RouteStatus::Real,
            false,
        )?;
    }
    add_record(
        &mut routes,
        "fastly",
        "/_ts/debug/ja4",
        &["GET"],
        RouteShape::Conditional,
        "settings.debug.ja4_endpoint_enabled",
        RouteStatus::Real,
        false,
    )?;
    for adapter in ["fastly", "axum", "cloudflare", "spin"] {
        add_record(
            &mut routes,
            adapter,
            "/static/tsjs=<file>",
            &["GET"],
            RouteShape::Template,
            "path.starts_with(/static/tsjs=)",
            RouteStatus::Real,
            false,
        )?;
    }
    add_record(
        &mut routes,
        "fastly",
        "<proxy.asset_routes[].prefix>{*rest}",
        &["GET", "HEAD"],
        RouteShape::ConfigDerived,
        "settings.proxy.asset_routes[]",
        RouteStatus::Real,
        false,
    )?;
    for adapter in ["fastly", "axum", "cloudflare", "spin"] {
        for (path, shape) in [
            ("/", RouteShape::Literal),
            ("/{*rest}", RouteShape::Template),
        ] {
            add_record(
                &mut routes,
                adapter,
                path,
                &fallback_methods,
                shape,
                "startup_error",
                RouteStatus::StartupError,
                true,
            )?;
        }
    }
    for adapter in ["fastly", "spin"] {
        add_record(
            &mut routes,
            adapter,
            "/health",
            &["GET"],
            RouteShape::Literal,
            "startup_error",
            RouteStatus::Real,
            true,
        )?;
    }
    Ok(routes.finish())
}

/// Check route and adapter-support manifests against repository sources.
///
/// # Errors
///
/// Returns an error for repository access, unsupported route source grammar,
/// exact set drift, or invalid adapter support facts.
pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<RouteError>> {
    let route_manifest = RouteManifest::parse(&read_utf8(
        repository,
        "tools/docs-parity/manifests/routes.toml",
    )?)?;
    let support_manifest = AdapterSupportManifest::parse(&read_utf8(
        repository,
        "tools/docs-parity/manifests/adapter-support.toml",
    )?)?;
    let fastly_app = read_utf8(
        repository,
        "crates/trusted-server-adapter-fastly/src/app.rs",
    )?;
    let fastly_entrypoint = read_utf8(
        repository,
        "crates/trusted-server-adapter-fastly/src/main.rs",
    )?;
    let axum_app = read_utf8(repository, "crates/trusted-server-adapter-axum/src/app.rs")?;
    let cloudflare_app = read_utf8(
        repository,
        "crates/trusted-server-adapter-cloudflare/src/app.rs",
    )?;
    let spin_app = read_utf8(repository, "crates/trusted-server-adapter-spin/src/app.rs")?;
    let publisher_routes = read_utf8(repository, "crates/trusted-server-core/src/publisher.rs")?;
    let admin_routes = read_utf8(repository, "crates/trusted-server-core/src/ec/admin.rs")?;
    let observed = extract_repository_routes(&RouteSources {
        publisher_routes: &publisher_routes,
        admin_routes: &admin_routes,
        fastly_app: &fastly_app,
        fastly_entrypoint: &fastly_entrypoint,
        axum_app: &axum_app,
        cloudflare_app: &cloudflare_app,
        spin_app: &spin_app,
    })?;
    validate_routes(route_manifest.routes(), &observed)?;
    validate_adapter_support(&support_manifest)?;
    validate_middleware_sources(&fastly_entrypoint, &axum_app, &cloudflare_app, &spin_app)?;
    let integrations = IntegrationInventory::parse(&read_utf8(
        repository,
        "tools/docs-parity/manifests/integrations.toml",
    )?)
    .map_err(|error| invalid(format!("cannot parse integration routes: {error}")))?;
    let pages = read_utf8(repository, "tools/docs-parity/manifests/pages.toml")?;
    validate_documentation_contract(&route_manifest, &support_manifest, &integrations, &pages)?;
    if crate::markdown::generate(repository, false)
        .map_err(|error| invalid(format!("cannot check generated API reference: {error}")))?
    {
        return Err(invalid("generated API reference has drift"));
    }
    Ok(())
}

/// Validate the adapter middleware facts rendered in the API support table.
///
/// # Errors
///
/// Returns an error when trusted-client-IP sanitization/resolution or Spin
/// request normalization is missing or ordered differently.
pub fn validate_middleware_sources(
    fastly_entrypoint: &str,
    axum_app: &str,
    cloudflare_app: &str,
    spin_app: &str,
) -> Result<(), Report<RouteError>> {
    let fastly = parse_route_source("Fastly entrypoint", fastly_entrypoint)?;
    let fastly_entrypoint = exact_top_function(&fastly, "edgezero_main")?;
    if !exact_fastly_resolver_source(&fastly_entrypoint.block) {
        return Err(invalid(
            "Fastly trusted-client-IP entry-point contract differs",
        ));
    }
    if named_call_occurrences(&fastly_entrypoint.block, "NormalizeMiddleware") != 0 {
        return Err(invalid("Fastly request normalization contract differs"));
    }

    for (adapter, source) in [("Axum", axum_app), ("Cloudflare", cloudflare_app)] {
        let file = parse_route_source(&format!("{adapter} app"), source)?;
        let build_router = exact_top_function(&file, "build_router")?;
        if middleware_order(&build_router.block)
            != [
                Some("SanitizeRequestMiddleware"),
                Some("FinalizeResponseMiddleware"),
                Some("AuthMiddleware"),
            ]
            || named_call_occurrences(&build_router.block, "resolve_and_sanitize_client_ip") != 0
        {
            return Err(invalid(format!("{adapter} middleware contract differs")));
        }
    }

    let spin = parse_route_source("Spin app", spin_app)?;
    let build_router = exact_top_function(&spin, "build_router")?;
    let normalize = exact_top_function(&spin, "normalize_spin_request")?;
    if middleware_order(&build_router.block)
        != [
            Some("SanitizeRequestMiddleware"),
            Some("FinalizeResponseMiddleware"),
            Some("AuthMiddleware"),
            Some("NormalizeMiddleware"),
        ]
        || named_call_occurrences(&build_router.block, "resolve_and_sanitize_client_ip") != 0
        || !exact_spin_client_addr_source(&normalize.block)
    {
        return Err(invalid("Spin trusted request normalization differs"));
    }
    Ok(())
}

fn middleware_order(block: &Block) -> Vec<Option<&'static str>> {
    #[derive(Default)]
    struct MiddlewareOrder {
        names: Vec<Option<&'static str>>,
    }
    impl<'ast> Visit<'ast> for MiddlewareOrder {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            visit::visit_expr_method_call(self, call);
            if call.method == "middleware" {
                self.names
                    .push(call.args.first().and_then(exact_middleware_constructor));
            }
        }
    }
    let mut order = MiddlewareOrder::default();
    order.visit_block(block);
    order.names
}

fn exact_middleware_constructor(expression: &Expr) -> Option<&'static str> {
    let Expr::Call(call) = strip_parens(expression) else {
        return None;
    };
    for name in [
        "SanitizeRequestMiddleware",
        "FinalizeResponseMiddleware",
        "AuthMiddleware",
    ] {
        if exact_route_call_path(call, &[name, "new"])
            && call.args.len() == 1
            && call.args.first().is_some_and(exact_arc_clone_settings)
        {
            return Some(name);
        }
    }
    if exact_route_call_path(call, &["NormalizeMiddleware", "new"]) && call.args.is_empty() {
        return Some("NormalizeMiddleware");
    }
    None
}

fn exact_arc_clone_settings(expression: &Expr) -> bool {
    matches!(strip_parens(expression), Expr::Call(call)
        if exact_route_call_path(call, &["Arc", "clone"])
            && call.args.len() == 1
            && exact_reference_field(call.args.first(), "state", "settings"))
}

fn exact_fastly_resolver_source(block: &Block) -> bool {
    block
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Local(local) if exact_local_binding(local, "resolved_client_ip", false) => {
                local.init.as_ref().map(|init| init.expr.as_ref())
            }
            _ => None,
        })
        .filter(|expression| {
            matches!(strip_parens(expression), Expr::Call(call)
                if matches!(call.func.as_ref(), Expr::Path(path)
                    if path.qself.is_none()
                        && path.path.leading_colon.is_none()
                        && path.path.segments.iter().map(|segment| segment.ident.to_string()).collect::<Vec<_>>()
                            == ["compat", "resolve_and_sanitize_client_ip"])
                    && call.args.len() == 2
                    && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                        if reference.mutability.is_some() && is_ident(&reference.expr, "req"))
                    && call.args.iter().nth(1).is_some_and(|argument| {
                        is_ident(argument, "trusted_client_ip")
                    }))
        })
        .count()
        == 1
}

fn named_call_occurrences(block: &Block, name: &str) -> usize {
    struct Calls<'a> {
        name: &'a str,
        count: usize,
    }
    impl<'ast> Visit<'ast> for Calls<'_> {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            self.count += usize::from(matches!(call.func.as_ref(), Expr::Path(path)
                if path.qself.is_none()
                    && path.path.segments.iter().any(|segment| segment.ident == self.name)));
            visit::visit_expr_call(self, call);
        }
    }
    let mut calls = Calls { name, count: 0 };
    calls.visit_block(block);
    calls.count
}

fn exact_spin_client_addr_source(block: &Block) -> bool {
    let candidates = block
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Local(local) if exact_local_binding(local, "trusted_client_addr", false) => {
                local.init.as_ref().map(|init| init.expr.as_ref())
            }
            _ => None,
        })
        .filter(|expression| exact_spin_client_addr_initializer(expression))
        .count();
    candidates == 1
}

fn exact_spin_client_addr_initializer(expression: &Expr) -> bool {
    let Expr::MethodCall(parse) = strip_parens(expression) else {
        return false;
    };
    let exact_parser = parse.method == "and_then"
        && parse.args.len() == 1
        && parse
            .args
            .first()
            .is_some_and(|argument| is_ident(argument, "parse_client_addr"));
    let Expr::MethodCall(convert) = strip_parens(&parse.receiver) else {
        return false;
    };
    let exact_conversion = convert.method == "and_then"
        && convert.args.len() == 1
        && matches!(convert.args.first().map(strip_parens), Some(Expr::Closure(closure))
        if matches!(closure.inputs.iter().collect::<Vec<_>>().as_slice(),
            [Pat::Ident(binding)] if binding.ident == "v" && binding.subpat.is_none())
            && exact_zero_arg_method(&closure.body, "ok", |receiver| {
                exact_zero_arg_method(receiver, "to_str", |receiver| is_ident(receiver, "v"))
            }));
    let Expr::MethodCall(last) = strip_parens(&convert.receiver) else {
        return false;
    };
    let Expr::MethodCall(iter) = strip_parens(&last.receiver) else {
        return false;
    };
    let Expr::MethodCall(get_all) = strip_parens(&iter.receiver) else {
        return false;
    };
    let Expr::MethodCall(headers) = strip_parens(&get_all.receiver) else {
        return false;
    };
    exact_parser
        && exact_conversion
        && last.method == "next_back"
        && last.args.is_empty()
        && iter.method == "iter"
        && iter.args.is_empty()
        && get_all.method == "get_all"
        && get_all.args.len() == 1
        && get_all
            .args
            .first()
            .and_then(literal_string_value)
            .as_deref()
            == Some("spin-client-addr")
        && headers.method == "headers"
        && headers.args.is_empty()
        && is_ident(&headers.receiver, "req")
}

#[allow(clippy::too_many_arguments)]
fn add_record(
    routes: &mut RouteAccumulator,
    adapter: &str,
    path: &str,
    methods: &[&str],
    shape: RouteShape,
    predicate: &str,
    status: RouteStatus,
    startup_router: bool,
) -> Result<(), Report<RouteError>> {
    routes.insert(&RouteRecord::new(
        adapter,
        path,
        methods,
        shape,
        predicate,
        status,
        startup_router,
    ))
}

fn validate_special_route_sources(
    sources: &RouteSources<'_>,
    authoritative_paths: &BTreeMap<String, String>,
) -> Result<(), Report<RouteError>> {
    let fastly = parse_route_source("Fastly app", sources.fastly_app)?;
    let entrypoint = parse_route_source("Fastly entrypoint", sources.fastly_entrypoint)?;
    let axum = parse_route_source("Axum app", sources.axum_app)?;
    let cloudflare = parse_route_source("Cloudflare app", sources.cloudflare_app)?;
    let spin = parse_route_source("Spin app", sources.spin_app)?;
    let publisher = parse_route_source("Core publisher", sources.publisher_routes)?;
    let admin = parse_route_source("Core EC admin", sources.admin_routes)?;
    validate_fastly_pre_router(&entrypoint)?;
    validate_fastly_health_call(&entrypoint)?;
    validate_fastly_dispatch_tsjs(&fastly)?;
    validate_fastly_early_named_dispatch(&fastly)?;
    validate_nested_dispatch_binding("cloudflare", &cloudflare)?;
    validate_nested_dispatch_binding("spin", &spin)?;
    validate_named_handler_dispatch("fastly", &fastly)?;
    validate_named_handler_dispatch("axum", &axum)?;
    validate_named_discovery_arm("fastly", &fastly)?;
    validate_named_discovery_arm("axum", &axum)?;
    validate_named_inventory_registration("fastly", &fastly)?;
    validate_named_inventory_registration("axum", &axum)?;
    validate_healthy_fallback_handler("fastly", &fastly)?;
    validate_healthy_fallback_handler("axum", &axum)?;
    validate_unsupported_response("cloudflare", &cloudflare)?;
    validate_unsupported_response("spin", &spin)?;
    validate_guarded_response_bodies(&publisher, &admin, &fastly, &axum, &cloudflare, &spin)?;
    validate_dynamic_tsjs_guard("fastly", &fastly)?;
    validate_dynamic_tsjs_guard("axum", &axum)?;
    validate_dynamic_tsjs_guard("cloudflare", &cloudflare)?;
    validate_dynamic_tsjs_guard("spin", &spin)?;

    for (adapter, file, needs_health) in [
        ("fastly", &fastly, false),
        ("axum", &axum, true),
        ("cloudflare", &cloudflare, false),
        ("spin", &spin, true),
    ] {
        if needs_health {
            validate_health_registration(adapter, file)?;
        }
        validate_fallback_routes(adapter, file, false)?;
        let methods = exact_method_list(file, "publisher_fallback_methods")?;
        let expected = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if methods != expected {
            return Err(invalid(format!(
                "{adapter} fallback method AST receipt differs"
            )));
        }
        validate_fallback_routes(adapter, file, true)?;
        if adapter == "spin" {
            validate_spin_startup_health(file)?;
        }
    }
    validate_asset_route_guard(&fastly)?;
    validate_cloudflare_local_handlers(&cloudflare)?;
    validate_cloudflare_discovery_handler(&cloudflare)?;
    validate_spin_named_handlers(&spin, authoritative_paths)?;
    Ok(())
}

fn validate_named_discovery_arm(adapter: &str, file: &syn::File) -> Result<(), Report<RouteError>> {
    let function = exact_top_function(
        file,
        if adapter == "fastly" {
            "run_named_route"
        } else {
            "named_route_handler"
        },
    )?;
    struct DiscoveryArms<'a>(Vec<&'a Expr>);
    impl<'ast> Visit<'ast> for DiscoveryArms<'ast> {
        fn visit_arm(&mut self, arm: &'ast syn::Arm) {
            if matches!(&arm.pat, Pat::Path(path)
            if path.qself.is_none()
                && path.path.leading_colon.is_none()
                && path.path.segments.len() == 2
                && path.path.segments[0].ident == "NamedRouteHandler"
                && path.path.segments[1].ident == "TrustedServerDiscovery"
                && path.path.segments.iter().all(|segment| {
                    matches!(segment.arguments, syn::PathArguments::None)
                }))
            {
                self.0.push(&arm.body);
            }
            visit::visit_arm(self, arm);
        }
    }
    let mut arms = DiscoveryArms(Vec::new());
    arms.visit_block(&function.block);
    let [expression] = arms.0.as_slice() else {
        return Err(invalid(format!(
            "{adapter} discovery handler must have one live arm"
        )));
    };
    let Expr::Block(body) = strip_parens(expression) else {
        return Err(invalid(format!(
            "{adapter} discovery handler arm shape differs"
        )));
    };
    let [Stmt::Expr(Expr::Call(call), None)] = body.block.stmts.as_slice() else {
        return Err(invalid(format!(
            "{adapter} discovery handler arm is not one terminal call"
        )));
    };
    let exact_services = if adapter == "fastly" {
        call.args
            .iter()
            .nth(1)
            .is_some_and(|value| is_ident(value, "services"))
    } else {
        call.args.iter().nth(1).is_some_and(|value| {
            matches!(strip_parens(value), Expr::Reference(reference)
                if reference.mutability.is_none() && is_ident(&reference.expr, "services"))
        })
    };
    if !call_named_route(call, "handle_trusted_server_discovery")
        || call.args.len() != 3
        || call.args.first().is_none_or(|value| {
            !matches!(strip_parens(value), Expr::Reference(reference)
                if reference.mutability.is_none()
                    && exact_field_receiver(&reference.expr, "state", "settings"))
        })
        || !exact_services
        || call
            .args
            .iter()
            .nth(2)
            .is_none_or(|value| !is_ident(value, "req"))
    {
        return Err(invalid(format!(
            "{adapter} discovery handler call arguments differ"
        )));
    }
    Ok(())
}

fn validate_named_inventory_registration(
    adapter: &str,
    file: &syn::File,
) -> Result<(), Report<RouteError>> {
    let block = if adapter == "fastly" {
        exact_impl_method(file, "TrustedServerApp", "routes_for_state")?
    } else {
        &exact_top_function(file, "build_router")?.block
    };
    let loops = block
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Expr(Expr::ForLoop(loop_expression), _)
                if named_inventory_iterator(&loop_expression.expr, adapter) =>
            {
                Some(loop_expression)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let [routes] = loops.as_slice() else {
        return Err(invalid(format!(
            "{adapter} named inventory must have one live registration loop"
        )));
    };
    if !matches!(&*routes.pat, Pat::Ident(binding)
        if binding.ident == "route" && binding.subpat.is_none())
        || routes.body.stmts.len() != 2
        || !exact_primary_named_registration(&routes.body.stmts[0])
        || !exact_named_fallback_registration(
            &routes.body.stmts[1],
            if adapter == "fastly" {
                "fallback_handler"
            } else {
                "fallback"
            },
        )
    {
        return Err(invalid(format!(
            "{adapter} named inventory registration arguments differ"
        )));
    }
    Ok(())
}

fn named_inventory_iterator(expression: &Expr, adapter: &str) -> bool {
    if adapter == "fastly" {
        return is_ident(expression, "NAMED_ROUTES");
    }
    matches!(strip_parens(expression), Expr::Call(call)
        if call_named_route(call, "named_routes") && call.args.is_empty())
}

fn exact_primary_named_registration(statement: &Stmt) -> bool {
    let Stmt::Expr(Expr::ForLoop(methods), _) = statement else {
        return false;
    };
    if !matches!(&*methods.pat, Pat::Ident(binding)
        if binding.ident == "method" && binding.subpat.is_none())
        || !exact_field_receiver(&methods.expr, "route", "primary_methods")
    {
        return false;
    }
    let [Stmt::Expr(Expr::Assign(assignment), _)] = methods.body.stmts.as_slice() else {
        return false;
    };
    let Expr::MethodCall(route) = strip_parens(&assignment.right) else {
        return false;
    };
    is_ident(&assignment.left, "router")
        && route.method == "route"
        && is_ident(&route.receiver, "router")
        && route.args.len() == 3
        && route
            .args
            .first()
            .is_some_and(|value| exact_field_receiver(value, "route", "path"))
        && route.args.iter().nth(1).is_some_and(|value| {
            matches!(strip_parens(value), Expr::MethodCall(clone)
                if clone.method == "clone" && clone.args.is_empty()
                    && is_ident(&clone.receiver, "method"))
        })
        && route.args.iter().nth(2).is_some_and(|value| {
            matches!(strip_parens(value), Expr::Call(handler)
            if call_named_route(handler, "named_route_handler")
                && handler.args.len() == 2
                && handler.args.first().is_some_and(exact_arc_clone_state)
                && handler.args.iter().nth(1).is_some_and(|argument| {
                    exact_field_receiver(argument, "route", "handler")
                }))
        })
}

fn exact_named_fallback_registration(statement: &Stmt, fallback: &str) -> bool {
    let Stmt::Expr(Expr::ForLoop(methods), _) = statement else {
        return false;
    };
    if !matches!(&*methods.pat, Pat::Ident(binding)
        if binding.ident == "method" && binding.subpat.is_none())
        || !matches!(strip_parens(&methods.expr), Expr::Call(call)
            if call_named_route(call, "publisher_fallback_methods") && call.args.is_empty())
    {
        return false;
    }
    let [Stmt::Expr(Expr::If(branch), _)] = methods.body.stmts.as_slice() else {
        return false;
    };
    let [Stmt::Expr(Expr::Assign(assignment), _)] = branch.then_branch.stmts.as_slice() else {
        return false;
    };
    let Expr::MethodCall(route) = strip_parens(&assignment.right) else {
        return false;
    };
    branch.else_branch.is_none()
        && is_ident(&assignment.left, "router")
        && route.method == "route"
        && is_ident(&route.receiver, "router")
        && route.args.len() == 3
        && route
            .args
            .first()
            .is_some_and(|value| exact_field_receiver(value, "route", "path"))
        && route
            .args
            .iter()
            .nth(1)
            .is_some_and(|value| is_ident(value, "method"))
        && route.args.iter().nth(2).is_some_and(|value| {
            matches!(strip_parens(value), Expr::MethodCall(clone)
                if clone.method == "clone" && clone.args.is_empty()
                    && is_ident(&clone.receiver, fallback))
        })
}

fn exact_field_receiver(expression: &Expr, base: &str, field: &str) -> bool {
    matches!(strip_parens(expression), Expr::Field(value)
        if is_ident(&value.base, base)
            && matches!(&value.member, syn::Member::Named(name) if name == field))
}

fn exact_arc_clone_state(expression: &Expr) -> bool {
    matches!(strip_parens(expression), Expr::Call(call)
        if exact_route_call_path(call, &["Arc", "clone"])
            && call.args.len() == 1
            && call.args.first().is_some_and(|argument| is_ident(argument, "state")))
}

fn validate_healthy_fallback_handler(
    adapter: &str,
    file: &syn::File,
) -> Result<(), Report<RouteError>> {
    let name = if adapter == "fastly" {
        "fallback_route_handler"
    } else {
        "fallback_handler"
    };
    let function = exact_top_function(file, name)?;
    let [Stmt::Expr(Expr::Closure(handler), None)] = function.block.stmts.as_slice() else {
        return Err(invalid(format!(
            "{adapter} healthy fallback factory shape differs"
        )));
    };
    let Expr::Block(body) = strip_parens(&handler.body) else {
        return Err(invalid(format!(
            "{adapter} healthy fallback closure shape differs"
        )));
    };
    if body.block.stmts.len() != 2
        || !matches!(&body.block.stmts[0], Stmt::Local(local)
            if matches!(&local.pat, Pat::Ident(binding) if binding.ident == "state")
                && local.init.as_ref().is_some_and(|init| exact_arc_clone(&init.expr, "state")))
    {
        return Err(invalid(format!(
            "{adapter} healthy fallback state binding differs"
        )));
    }
    let Some(terminal) = body
        .block
        .stmts
        .last()
        .and_then(statement_terminal_expression)
    else {
        return Err(invalid(format!(
            "{adapter} healthy fallback terminal expression differs"
        )));
    };
    let Expr::Call(pin) = strip_parens(terminal) else {
        return Err(invalid(format!(
            "{adapter} healthy fallback terminal expression differs"
        )));
    };
    if !exact_route_call_path(pin, &["Box", "pin"]) || pin.args.len() != 1 {
        return Err(invalid(format!(
            "{adapter} healthy fallback terminal expression differs"
        )));
    }
    let dispatch = pin.args.first().expect("Box::pin should have one argument");
    let exact = if adapter == "fastly" {
        matches!(strip_parens(dispatch), Expr::Call(call)
            if call_named_route(call, "execute_fallback")
                && call.args.len() == 2
                && call.args.first().is_some_and(|value| is_ident(value, "state"))
                && call.args.iter().nth(1).is_some_and(|value| is_ident(value, "ctx")))
    } else {
        exact_axum_fallback_dispatch(dispatch)
    };
    if !exact {
        return Err(invalid(format!(
            "{adapter} healthy fallback dispatch binding differs"
        )));
    }
    Ok(())
}

fn exact_arc_clone(expression: &Expr, name: &str) -> bool {
    matches!(strip_parens(expression), Expr::Call(call)
        if exact_route_call_path(call, &["Arc", "clone"])
            && call.args.len() == 1
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if is_ident(&reference.expr, name)))
}

fn exact_axum_fallback_dispatch(expression: &Expr) -> bool {
    let Expr::Call(execute) = strip_parens(expression) else {
        return false;
    };
    if !call_named_route(execute, "execute_handler")
        || execute.args.len() != 3
        || execute
            .args
            .first()
            .is_none_or(|value| !is_ident(value, "state"))
        || execute
            .args
            .iter()
            .nth(1)
            .is_none_or(|value| !is_ident(value, "ctx"))
    {
        return false;
    }
    let Some(Expr::Closure(dispatch)) = execute.args.iter().nth(2).map(strip_parens) else {
        return false;
    };
    let inputs = dispatch
        .inputs
        .iter()
        .map(|input| match input {
            Pat::Ident(binding) => Some(binding.ident.to_string()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    if inputs.as_deref() != Some(&["state".to_owned(), "services".to_owned(), "req".to_owned()]) {
        return false;
    }
    let Expr::Async(future) = strip_parens(&dispatch.body) else {
        return false;
    };
    let [Stmt::Expr(Expr::Await(awaited), None)] = future.block.stmts.as_slice() else {
        return false;
    };
    matches!(strip_parens(&awaited.base), Expr::Call(call)
        if call_named_route(call, "dispatch_fallback")
            && call.args.len() == 3
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if is_ident(&reference.expr, "state"))
            && matches!(call.args.iter().nth(1).map(strip_parens), Some(Expr::Reference(reference))
                if is_ident(&reference.expr, "services"))
            && call.args.iter().nth(2).is_some_and(|value| is_ident(value, "req")))
}

fn parse_route_source(label: &str, source: &str) -> Result<syn::File, Report<RouteError>> {
    syn::parse_file(source).map_err(|error| invalid(format!("invalid {label} Rust: {error}")))
}

fn validate_health_registration(adapter: &str, file: &syn::File) -> Result<(), Report<RouteError>> {
    if adapter == "spin" {
        validate_spin_health_response(file)?;
    }
    let block = &exact_top_function(file, "build_router")?.block;
    let authority = if adapter == "axum" {
        "router"
    } else {
        "builder"
    };
    if !block_returns_build(block, authority) {
        return Err(invalid(format!(
            "{adapter} build_router must return its health-route authority"
        )));
    }
    let mut count = 0;
    for block in direct_anonymous_blocks(block) {
        for statement in &block.stmts {
            match statement {
                Stmt::Expr(Expr::Assign(assignment), _) if adapter == "axum" => {
                    let Expr::MethodCall(call) = strip_parens(&assignment.right) else {
                        continue;
                    };
                    if is_ident(&assignment.left, "router")
                        && exact_health_route_call(call, "router")
                    {
                        count += 1;
                    }
                }
                Stmt::Local(local)
                    if adapter == "spin"
                        && matches!(&local.pat, Pat::Ident(binding) if binding.ident == "builder")
                        && local
                            .init
                            .as_ref()
                            .is_some_and(|init| method_chain_health_count(&init.expr) == 1) =>
                {
                    count += 1;
                }
                _ => {}
            }
        }
    }
    if count != 1 {
        return Err(invalid(format!(
            "{adapter} health registration AST receipt differs"
        )));
    }
    Ok(())
}

fn exact_health_route_call(call: &syn::ExprMethodCall, receiver: &str) -> bool {
    call.method == "route"
        && is_ident(&call.receiver, receiver)
        && call.args.len() == 3
        && call.args.first().and_then(literal_string_value).as_deref() == Some("/health")
        && call.args.iter().nth(1).is_some_and(expression_is_get)
        && call
            .args
            .iter()
            .nth(2)
            .is_some_and(axum_health_handler_is_ok)
}

fn method_chain_health_count(expression: &Expr) -> usize {
    match strip_parens(expression) {
        Expr::MethodCall(call) => {
            usize::from(
                call.method == "get"
                    && call.args.len() == 2
                    && call.args.first().and_then(literal_string_value).as_deref()
                        == Some("/health")
                    && call.args.iter().nth(1).is_some_and(spin_health_handler),
            ) + method_chain_health_count(&call.receiver)
        }
        _ => 0,
    }
}

fn status_is_ok(expression: &Expr) -> bool {
    exact_route_path(expression, &["StatusCode", "OK"])
}

fn axum_health_handler_is_ok(expression: &Expr) -> bool {
    let Expr::Closure(closure) = strip_parens(expression) else {
        return false;
    };
    let Expr::Async(body) = strip_parens(&closure.body) else {
        return false;
    };
    let [Stmt::Expr(Expr::Call(ok), None)] = body.block.stmts.as_slice() else {
        return false;
    };
    call_named_route(ok, "Ok")
        && ok.args.len() == 1
        && ok.args.first().is_some_and(exact_axum_health_response)
}

fn spin_health_handler(expression: &Expr) -> bool {
    let Expr::Closure(closure) = strip_parens(expression) else {
        return false;
    };
    let Expr::Async(body) = strip_parens(&closure.body) else {
        return false;
    };
    matches!(body.block.stmts.as_slice(), [Stmt::Expr(Expr::Call(ok), None)]
        if call_named_route(ok, "Ok")
            && ok.args.len() == 1
            && matches!(ok.args.first().map(strip_parens), Some(Expr::Call(health))
                if call_named_route(health, "health_response") && health.args.is_empty()))
}

fn validate_spin_health_response(file: &syn::File) -> Result<(), Report<RouteError>> {
    let response = exact_top_function(file, "health_response")?;
    if response.block.stmts.len() != 4
        || !matches!(&response.block.stmts[0], Stmt::Local(local)
            if matches!(&local.pat, Pat::Ident(binding) if binding.ident == "resp")
                && local.init.as_ref().is_some_and(|init| exact_spin_health_initializer(&init.expr)))
        || !matches!(&response.block.stmts[1], Stmt::Expr(Expr::Assign(assignment), _)
            if status_is_ok(&assignment.right)
                && matches!(strip_parens(&assignment.left), Expr::Unary(unary)
                    if matches!(unary.op, syn::UnOp::Deref(_))
                        && matches!(strip_parens(&unary.expr), Expr::MethodCall(call)
                            if call.method == "status_mut" && is_ident(&call.receiver, "resp"))))
        || !matches!(&response.block.stmts[2], Stmt::Expr(Expr::MethodCall(insert), _)
            if exact_content_type_insert(insert, "resp", "text/plain"))
        || !matches!(&response.block.stmts[3], Stmt::Expr(value, None) if is_ident(value, "resp"))
    {
        return Err(invalid("Spin health response AST receipt differs"));
    }
    Ok(())
}

fn exact_axum_health_response(expression: &Expr) -> bool {
    let Expr::MethodCall(expect) = strip_parens(expression) else {
        return false;
    };
    if expect.method != "expect"
        || expect.args.len() != 1
        || expect
            .args
            .first()
            .and_then(literal_string_value)
            .as_deref()
            != Some("should build health response")
    {
        return false;
    }
    let Expr::MethodCall(body) = strip_parens(&expect.receiver) else {
        return false;
    };
    let exact_body = body.method == "body"
        && body.args.len() == 1
        && body.args.first().is_some_and(|value| {
            matches!(strip_parens(value), Expr::Call(call)
                if exact_route_call_path(call, &["edgezero_core", "body", "Body", "from"])
                    && call.args.len() == 1
                    && call.args.first().and_then(literal_string_value).as_deref() == Some("ok"))
        });
    let Expr::MethodCall(header) = strip_parens(&body.receiver) else {
        return false;
    };
    let Expr::MethodCall(status) = strip_parens(&header.receiver) else {
        return false;
    };
    let exact_status = status.method == "status"
        && status.args.len() == 1
        && status.args.first().is_some_and(status_is_ok);
    let exact_builder = matches!(strip_parens(&status.receiver), Expr::Call(call)
        if exact_route_call_path(call, &["edgezero_core", "http", "response_builder"])
            && call.args.is_empty());
    exact_body
        && header.method == "header"
        && header.args.len() == 2
        && header
            .args
            .first()
            .is_some_and(|value| exact_route_path(value, &["header", "CONTENT_TYPE"]))
        && header
            .args
            .iter()
            .nth(1)
            .is_some_and(|value| exact_header_value(value, "text/plain"))
        && exact_status
        && exact_builder
}

fn exact_spin_health_initializer(expression: &Expr) -> bool {
    matches!(strip_parens(expression), Expr::Call(response)
        if exact_route_call_path(response, &["Response", "new"])
            && response.args.len() == 1
            && matches!(response.args.first().map(strip_parens), Some(Expr::Call(body))
                if exact_route_call_path(body, &["edgezero_core", "body", "Body", "from"])
                    && body.args.len() == 1
                    && body.args.first().and_then(literal_string_value).as_deref() == Some("ok")))
}

fn exact_route_path(expression: &Expr, expected: &[&str]) -> bool {
    matches!(strip_parens(expression), Expr::Path(path)
    if path.qself.is_none()
        && path.path.leading_colon.is_none()
        && path.path.segments.len() == expected.len()
        && path.path.segments.iter().zip(expected).all(|(segment, name)| {
            segment.ident == *name
                && matches!(segment.arguments, syn::PathArguments::None)
        }))
}

fn exact_header_value(expression: &Expr, value: &str) -> bool {
    matches!(strip_parens(expression), Expr::Call(call)
        if exact_route_call_path(call, &["HeaderValue", "from_static"])
            && call.args.len() == 1
            && call.args.first().and_then(literal_string_value).as_deref() == Some(value))
}

fn exact_content_type_insert(call: &syn::ExprMethodCall, response: &str, value: &str) -> bool {
    call.method == "insert"
        && call.args.len() == 2
        && matches!(strip_parens(&call.receiver), Expr::MethodCall(headers)
            if headers.method == "headers_mut"
                && headers.args.is_empty()
                && is_ident(&headers.receiver, response))
        && call
            .args
            .first()
            .is_some_and(|argument| exact_route_path(argument, &["header", "CONTENT_TYPE"]))
        && call
            .args
            .iter()
            .nth(1)
            .is_some_and(|argument| exact_header_value(argument, value))
}

fn direct_anonymous_blocks(block: &Block) -> Vec<&Block> {
    let mut blocks = vec![block];
    for statement in &block.stmts {
        if let Stmt::Expr(Expr::Block(expression), _) = statement {
            blocks.extend(direct_anonymous_blocks(&expression.block));
        }
    }
    blocks
}

fn validate_spin_startup_health(file: &syn::File) -> Result<(), Report<RouteError>> {
    validate_spin_health_response(file)?;
    let startup = exact_top_function(file, "startup_error_router")?;
    let mut registrations = 0;
    for statement in &startup.block.stmts {
        let Stmt::Expr(Expr::Assign(assignment), _) = statement else {
            continue;
        };
        let Expr::MethodCall(call) = strip_parens(&assignment.right) else {
            continue;
        };
        if call.method == "get"
            && is_ident(&assignment.left, "builder")
            && is_ident(&call.receiver, "builder")
            && call.args.first().and_then(literal_string_value).as_deref() == Some("/health")
            && call.args.iter().nth(1).is_some_and(spin_health_handler)
        {
            registrations += 1;
        }
    }
    if registrations != 1 {
        return Err(invalid("Spin startup health AST receipt differs"));
    }
    Ok(())
}

fn validate_asset_route_guard(file: &syn::File) -> Result<(), Report<RouteError>> {
    let function = exact_top_function(file, "dispatch_fallback")?;
    let matches = function
        .block
        .stmts
        .iter()
        .filter(|statement| {
            let Stmt::Local(local) = statement else {
                return false;
            };
            if !matches!(&local.pat, Pat::Ident(binding) if binding.ident == "result") {
                return false;
            }
            let Some(init) = &local.init else {
                return false;
            };
            let Expr::If(branches) = strip_parens(&init.expr) else {
                return false;
            };
            final_else_block(branches).is_some_and(exact_asset_dispatch)
        })
        .count();
    if matches != 1 {
        return Err(invalid(
            "Fastly asset GET/HEAD dispatch AST receipt differs",
        ));
    }
    Ok(())
}

fn exact_asset_dispatch(block: &Block) -> bool {
    let bindings = block
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| {
            let Stmt::Local(asset) = statement else {
                return None;
            };
            matches!(&asset.pat, Pat::Ident(binding) if binding.ident == "matched_asset_route")
                .then_some((index, asset))
        })
        .collect::<Vec<_>>();
    let [(index, asset)] = bindings.as_slice() else {
        return false;
    };
    asset
        .init
        .as_ref()
        .is_some_and(|init| exact_asset_initializer(&init.expr))
        && block
            .stmts
            .get(index + 1)
            .is_some_and(exact_asset_dispatch_branch)
}

fn exact_asset_dispatch_branch(statement: &Stmt) -> bool {
    let Stmt::Expr(Expr::If(branch), _) = statement else {
        return false;
    };
    let Expr::Let(condition) = strip_parens(&branch.cond) else {
        return false;
    };
    let exact_pattern = matches!(condition.pat.as_ref(), Pat::TupleStruct(pattern)
        if pattern.path.segments.last().is_some_and(|segment| segment.ident == "Some")
            && pattern.elems.len() == 1
            && matches!(pattern.elems.first(), Some(Pat::Ident(binding)) if binding.ident == "asset_route"));
    exact_pattern
        && is_ident(&condition.expr, "matched_asset_route")
        && branch.else_branch.is_none()
        && matches!(branch.then_branch.stmts.as_slice(), [Stmt::Expr(Expr::Return(value), _)]
        if value.expr.as_deref().is_some_and(|value| {
            matches!(strip_parens(value), Expr::Await(awaited)
                if matches!(strip_parens(&awaited.base), Expr::Call(call)
                    if call_named_route(call, "dispatch_asset_fallback")
                        && call.args.len() == 5
                        && call.args.first().is_some_and(|value| is_ident(value, "state"))
                        && call.args.iter().nth(1).is_some_and(|value| is_ident(value, "services"))
                        && call.args.iter().nth(2).is_some_and(|value| is_ident(value, "req"))
                        && call.args.iter().nth(3).is_some_and(|value| is_ident(value, "asset_route"))
                        && matches!(call.args.iter().nth(4).map(strip_parens), Some(Expr::Reference(reference))
                            if reference.mutability.is_none() && is_ident(&reference.expr, "effects"))))
        }))
}

fn exact_asset_initializer(expression: &Expr) -> bool {
    let Expr::MethodCall(flatten) = strip_parens(expression) else {
        return false;
    };
    if flatten.method != "flatten" || !flatten.args.is_empty() {
        return false;
    }
    let Expr::MethodCall(then) = strip_parens(&flatten.receiver) else {
        return false;
    };
    let Expr::Macro(methods) = strip_parens(&then.receiver) else {
        return false;
    };
    let tokens = methods
        .mac
        .tokens
        .to_string()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if !methods.mac.path.is_ident("matches")
        || tokens != "method,Method::GET|Method::HEAD"
        || then.method != "then"
        || then.args.len() != 1
    {
        return false;
    }
    let Some(Expr::Closure(lookup_closure)) = then.args.first() else {
        return false;
    };
    let Expr::MethodCall(lookup) = strip_parens(&lookup_closure.body) else {
        return false;
    };
    lookup.method == "asset_route_for_path"
        && matches!(strip_parens(&lookup.receiver), Expr::Field(settings)
            if is_ident(&settings.base, "state")
                && matches!(&settings.member, syn::Member::Named(member) if member == "settings"))
        && lookup.args.len() == 1
        && matches!(lookup.args.first(), Some(Expr::Reference(reference)) if is_ident(&reference.expr, "path"))
}

fn final_else_block(mut expression: &syn::ExprIf) -> Option<&Block> {
    loop {
        let (_, alternative) = expression.else_branch.as_ref()?;
        match strip_parens(alternative) {
            Expr::If(next) => expression = next,
            Expr::Block(block) => return Some(&block.block),
            _ => return None,
        }
    }
}

fn validate_fallback_routes(
    adapter: &str,
    file: &syn::File,
    startup: bool,
) -> Result<(), Report<RouteError>> {
    let name = if startup {
        "startup_error_router"
    } else if adapter == "fastly" {
        "routes_for_state"
    } else {
        "build_router"
    };
    let block = if !startup && adapter == "fastly" {
        exact_impl_method(file, "TrustedServerApp", name)?
    } else {
        &exact_top_function(file, name)?.block
    };
    validate_router_construction_reachability(block, adapter, startup)?;
    let (receiver, handler) = if startup {
        match adapter {
            "fastly" => ("router", "make"),
            "axum" => ("router", "make_handler"),
            "cloudflare" => ("router", "make"),
            "spin" => ("builder", "handler"),
            _ => return Err(invalid("unknown fallback adapter")),
        }
    } else {
        match adapter {
            "fastly" => ("router", "fallback_handler"),
            "axum" | "cloudflare" => ("router", "fallback"),
            "spin" => ("builder", "fallback"),
            _ => return Err(invalid("unknown fallback adapter")),
        }
    };
    let facts = exact_fallback_call_facts(block, receiver, handler);
    if facts != (1, 1) {
        return Err(invalid(format!(
            "{adapter} {} fallback route AST receipt differs",
            if startup { "startup" } else { "healthy" }
        )));
    }
    if startup && !startup_status_is_bound(block, adapter) {
        return Err(invalid(format!(
            "{adapter} startup status AST receipt differs"
        )));
    }
    if !block_returns_build(block, receiver) {
        return Err(invalid(format!(
            "{adapter} {} fallback routes are not bound to the returned authority",
            if startup { "startup" } else { "healthy" }
        )));
    }
    validate_router_authority_continuity(block, receiver, adapter, startup)?;
    Ok(())
}

fn validate_router_construction_reachability(
    block: &Block,
    adapter: &str,
    startup: bool,
) -> Result<(), Report<RouteError>> {
    let active = if !startup && matches!(adapter, "cloudflare" | "spin") {
        let [Stmt::Expr(Expr::Block(body), None)] = block.stmts.as_slice() else {
            return Err(invalid(format!(
                "{adapter} router construction must use one lexical block"
            )));
        };
        &body.block
    } else {
        block
    };

    struct TerminatingControl {
        found: bool,
    }
    impl<'ast> Visit<'ast> for TerminatingControl {
        fn visit_expr_return(&mut self, _: &'ast syn::ExprReturn) {
            self.found = true;
        }

        fn visit_expr_break(&mut self, _: &'ast syn::ExprBreak) {
            self.found = true;
        }

        fn visit_expr_continue(&mut self, _: &'ast syn::ExprContinue) {
            self.found = true;
        }

        fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}

        fn visit_expr_async(&mut self, _: &'ast syn::ExprAsync) {}

        fn visit_item(&mut self, _: &'ast Item) {}
    }

    let mut terminating = TerminatingControl { found: false };
    for statement in &active.stmts {
        terminating.visit_stmt(statement);
    }
    let unmodeled = active.stmts.iter().any(|statement| match statement {
        Stmt::Expr(
            Expr::If(_)
            | Expr::Match(_)
            | Expr::While(_)
            | Expr::Loop(_)
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::Return(_),
            _,
        ) => true,
        Stmt::Expr(Expr::Macro(value), _) => !exact_log_error_macro(&value.mac),
        Stmt::Expr(Expr::ForLoop(loop_expression), _) => {
            !router_loop_is_modeled(&loop_expression.expr, adapter, startup)
        }
        Stmt::Macro(value) => !exact_log_error_macro(&value.mac),
        _ => false,
    });
    let consumed_authorities: &[&str] = match adapter {
        "fastly" => &[
            "publisher_fallback_methods",
            "NAMED_ROUTES",
            "LEGACY_ADMIN_DENY_METHODS",
            "degraded_routes",
        ],
        "axum" => &[
            "publisher_fallback_methods",
            "named_routes",
            "LEGACY_ADMIN_DENY_METHODS",
            "degraded_routes",
        ],
        "cloudflare" => &[
            "publisher_fallback_methods",
            "PAGE_BIDS_PATH",
            "PAGE_BIDS_LEGACY_PATH",
            "degraded_routes",
        ],
        "spin" => &[
            "publisher_fallback_methods",
            "named_fallback_paths",
            "LEGACY_ADMIN_DENY_METHODS",
            "degraded_routes",
        ],
        _ => &[],
    };
    if terminating.found || unmodeled || block_shadows_route_authority(active, consumed_authorities)
    {
        return Err(invalid(format!(
            "{adapter} {} router construction has a reachable bypass",
            if startup { "startup" } else { "healthy" }
        )));
    }
    Ok(())
}

fn block_shadows_route_authority(block: &Block, names: &[&str]) -> bool {
    struct Shadows<'a> {
        names: &'a [&'a str],
        found: bool,
    }
    impl<'ast> Visit<'ast> for Shadows<'_> {
        fn visit_item(&mut self, item: &'ast Item) {
            let name = match item {
                Item::Fn(value) => Some(&value.sig.ident),
                Item::Const(value) => Some(&value.ident),
                Item::Static(value) => Some(&value.ident),
                Item::Struct(value) => Some(&value.ident),
                _ => None,
            };
            if name.is_some_and(|ident| self.names.iter().any(|name| ident == name))
                || matches!(item, Item::Use(value)
                    if self.names.iter().any(|name| route_use_tree_binds_name(&value.tree, name)))
            {
                self.found = true;
                return;
            }
            visit::visit_item(self, item);
        }

        fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
            if self.names.iter().any(|name| pattern.ident == name) {
                self.found = true;
                return;
            }
            visit::visit_pat_ident(self, pattern);
        }
    }
    let mut shadows = Shadows {
        names,
        found: false,
    };
    shadows.visit_block(block);
    shadows.found
}

fn route_use_tree_binds_name(tree: &syn::UseTree, name: &str) -> bool {
    match tree {
        syn::UseTree::Name(value) => value.ident == name,
        syn::UseTree::Rename(value) => value.rename == name,
        syn::UseTree::Path(value) => route_use_tree_binds_name(&value.tree, name),
        syn::UseTree::Group(value) => value
            .items
            .iter()
            .any(|item| route_use_tree_binds_name(item, name)),
        syn::UseTree::Glob(_) => true,
    }
}

fn exact_log_error_macro(value: &syn::Macro) -> bool {
    value.path.leading_colon.is_none()
        && value.path.segments.len() == 2
        && value.path.segments[0].ident == "log"
        && value.path.segments[1].ident == "error"
        && value
            .path
            .segments
            .iter()
            .all(|segment| matches!(segment.arguments, syn::PathArguments::None))
}

fn router_loop_is_modeled(expression: &Expr, adapter: &str, startup: bool) -> bool {
    if matches!(strip_parens(expression), Expr::Call(call)
        if call_named_route(call, "publisher_fallback_methods") && call.args.is_empty())
    {
        return true;
    }
    if startup {
        return false;
    }
    match (adapter, strip_parens(expression)) {
        ("fastly", expression) => is_ident(expression, "NAMED_ROUTES"),
        ("axum", Expr::Call(call)) => {
            call_named_route(call, "named_routes") && call.args.is_empty()
        }
        ("cloudflare", Expr::Array(array)) => {
            array.elems.len() == 2
                && array
                    .elems
                    .first()
                    .is_some_and(|value| is_ident(value, "PAGE_BIDS_PATH"))
                && array
                    .elems
                    .iter()
                    .nth(1)
                    .is_some_and(|value| is_ident(value, "PAGE_BIDS_LEGACY_PATH"))
        }
        ("spin", expression) if is_ident(expression, "LEGACY_ADMIN_DENY_METHODS") => true,
        ("spin", Expr::Call(call)) => {
            call_named_route(call, "named_fallback_paths") && call.args.is_empty()
        }
        _ => false,
    }
}

fn exact_fallback_call_facts(block: &Block, receiver: &str, handler: &str) -> (usize, usize) {
    let mut root = 0;
    let mut rest = 0;
    for loop_expression in direct_fallback_loops(block) {
        for statement in &loop_expression.body.stmts {
            let call = match statement {
                Stmt::Expr(Expr::Assign(assignment), _) => {
                    if !is_ident(&assignment.left, receiver) {
                        continue;
                    }
                    let Expr::MethodCall(call) = strip_parens(&assignment.right) else {
                        continue;
                    };
                    call
                }
                _ => continue,
            };
            if call.method != "route"
                || !is_ident(&call.receiver, receiver)
                || call.args.len() != 3
                || call
                    .args
                    .iter()
                    .nth(1)
                    .is_none_or(|value| !exact_loop_method(value))
                || call
                    .args
                    .iter()
                    .nth(2)
                    .is_none_or(|value| !exact_handler(value, handler))
            {
                continue;
            }
            match call.args.first().and_then(literal_string_value).as_deref() {
                Some("/") => root += 1,
                Some("/{*rest}") => rest += 1,
                _ => {}
            }
        }
    }
    (root, rest)
}

fn validate_router_authority_continuity(
    block: &Block,
    authority: &str,
    adapter: &str,
    startup: bool,
) -> Result<(), Report<RouteError>> {
    struct Audit<'a> {
        authority: &'a str,
        authority_bindings: usize,
        builder_bindings: usize,
        authorized_route_calls: usize,
        invalid: bool,
    }

    impl<'ast> Visit<'ast> for Audit<'_> {
        fn visit_local(&mut self, local: &'ast syn::Local) {
            let binding_is_authority = matches!(&local.pat, Pat::Ident(binding)
                if binding.ident == self.authority);
            self.authority_bindings += usize::from(binding_is_authority);

            if let Some(init) = &local.init {
                let route_calls = expression_route_method_count(&init.expr);
                if is_router_builder_chain(&init.expr) {
                    self.builder_bindings += usize::from(binding_is_authority);
                    if !binding_is_authority
                        || (route_calls != 0
                            && !route_calls_have_root(&init.expr, self.authority, true))
                    {
                        self.invalid = true;
                    } else {
                        self.authorized_route_calls += route_calls;
                    }
                } else if route_calls != 0 {
                    self.invalid = true;
                }
            }
            visit::visit_local(self, local);
        }

        fn visit_expr_assign(&mut self, assignment: &'ast syn::ExprAssign) {
            let assigns_authority = is_ident(&assignment.left, self.authority);
            let route_calls = expression_route_method_count(&assignment.right);
            if assigns_authority {
                if route_calls == 0
                    || !route_calls_have_root(&assignment.right, self.authority, false)
                {
                    self.invalid = true;
                } else {
                    self.authorized_route_calls += route_calls;
                }
            } else if route_calls != 0 {
                self.invalid = true;
            }
            visit::visit_expr_assign(self, assignment);
        }

        fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
            if expression_route_method_count(&closure.body) != 0
                || expression_references_ident(&closure.body, self.authority)
            {
                self.invalid = true;
            }
            visit::visit_expr_closure(self, closure);
        }
    }

    let mut audit = Audit {
        authority,
        authority_bindings: 0,
        builder_bindings: 0,
        authorized_route_calls: 0,
        invalid: false,
    };
    audit.visit_block(block);
    let total_route_calls = block_route_method_count(block);
    if block_builder_count(block) != 1
        || audit.authority_bindings != 1
        || audit.builder_bindings != 1
        || audit.authorized_route_calls != total_route_calls
        || audit.invalid
    {
        return Err(invalid(format!(
            "{adapter} {} router authority continuity differs",
            if startup { "startup" } else { "healthy" }
        )));
    }
    Ok(())
}

fn route_calls_have_root(expression: &Expr, authority: &str, allow_builder: bool) -> bool {
    struct Roots<'a> {
        authority: &'a str,
        allow_builder: bool,
        seen: usize,
        valid: bool,
    }

    impl Visit<'_> for Roots<'_> {
        fn visit_expr_method_call(&mut self, call: &syn::ExprMethodCall) {
            if route_method_call(call) {
                self.seen += 1;
                self.valid &=
                    route_receiver_root(&call.receiver, self.authority, self.allow_builder);
            }
            visit::visit_expr_method_call(self, call);
        }
    }

    let mut roots = Roots {
        authority,
        allow_builder,
        seen: 0,
        valid: true,
    };
    roots.visit_expr(expression);
    roots.seen != 0 && roots.valid
}

fn route_receiver_root(expression: &Expr, authority: &str, allow_builder: bool) -> bool {
    match strip_parens(expression) {
        Expr::Path(_) => is_ident(expression, authority),
        Expr::MethodCall(call) => route_receiver_root(&call.receiver, authority, allow_builder),
        Expr::Call(call) => {
            allow_builder
                && exact_route_call_path(call, &["RouterService", "builder"])
                && call.args.is_empty()
        }
        _ => false,
    }
}

fn block_route_method_count(block: &Block) -> usize {
    struct Counter(usize);
    impl<'ast> Visit<'ast> for Counter {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            self.0 += usize::from(route_method_call(call));
            visit::visit_expr_method_call(self, call);
        }
    }
    let mut counter = Counter(0);
    counter.visit_block(block);
    counter.0
}

fn route_method_call(call: &syn::ExprMethodCall) -> bool {
    let method = call.method.to_string();
    method == "route"
        || (matches!(
            method.as_str(),
            "get" | "post" | "head" | "options" | "put" | "patch" | "delete"
        ) && call.args.len() == 2)
}

fn direct_fallback_loops(block: &Block) -> Vec<&syn::ExprForLoop> {
    let mut loops = Vec::new();
    for statement in &block.stmts {
        match statement {
            Stmt::Expr(Expr::ForLoop(expression), _) if matches!(strip_parens(&expression.expr), Expr::Call(call) if call_named_route(call, "publisher_fallback_methods")) =>
            {
                loops.push(expression);
            }
            Stmt::Expr(Expr::Block(expression), _) => {
                loops.extend(direct_fallback_loops(&expression.block));
            }
            _ => {}
        }
    }
    loops
}

fn call_named_route(call: &syn::ExprCall, expected: &str) -> bool {
    matches!(call.func.as_ref(), Expr::Path(path)
    if path.qself.is_none()
        && path.path.leading_colon.is_none()
        && path.path.segments.len() == 1
        && path.path.segments[0].ident == expected
        && path.path.segments.iter().all(|segment| {
            matches!(segment.arguments, syn::PathArguments::None)
                || matches!(expected, "Ok" | "Some")
        }))
}

fn exact_loop_method(expression: &Expr) -> bool {
    is_ident(expression, "method")
        || matches!(strip_parens(expression), Expr::MethodCall(call) if call.method == "clone" && call.args.is_empty() && is_ident(&call.receiver, "method"))
}

fn exact_handler(expression: &Expr, expected: &str) -> bool {
    match strip_parens(expression) {
        Expr::Path(path) => path.path.is_ident(expected),
        Expr::MethodCall(call) => {
            call.method == "clone" && call.args.is_empty() && is_ident(&call.receiver, expected)
        }
        Expr::Call(call) => {
            matches!(call.func.as_ref(), Expr::Path(path) if path.path.is_ident(expected))
        }
        _ => false,
    }
}

fn startup_status_is_bound(block: &Block, adapter: &str) -> bool {
    let expected_handler = if adapter == "spin" {
        "handler"
    } else if adapter == "axum" {
        "make_handler"
    } else {
        "make"
    };
    let exact_source = |expression: &Expr| {
        matches!(strip_parens(expression), Expr::MethodCall(status)
            if status.method == "status_code"
                && status.args.is_empty()
                && matches!(strip_parens(&status.receiver), Expr::MethodCall(context)
                    if context.method == "current_context"
                        && context.args.is_empty()
                        && is_ident(&context.receiver, "e")))
    };
    let source_count = block
        .stmts
        .iter()
        .filter(|statement| {
            matches!(statement, Stmt::Local(local)
                if matches!(&local.pat, Pat::Ident(binding) if binding.ident == "status")
                    && local.init.as_ref().is_some_and(|init| exact_source(&init.expr)))
        })
        .count();
    if (adapter == "spin" && source_count != 0) || (adapter != "spin" && source_count != 1) {
        return false;
    }

    let handlers = block
        .stmts
        .iter()
        .filter_map(|statement| {
            let Stmt::Local(local) = statement else {
                return None;
            };
            matches!(&local.pat, Pat::Ident(binding) if binding.ident == expected_handler)
                .then(|| local.init.as_ref().map(|init| &*init.expr))
        })
        .collect::<Vec<_>>();
    let [Some(handler)] = handlers.as_slice() else {
        return false;
    };
    exact_startup_handler(handler, adapter)
}

fn exact_startup_handler(expression: &Expr, adapter: &str) -> bool {
    let Expr::Closure(outer) = strip_parens(expression) else {
        return false;
    };
    let response_block = if adapter == "spin" {
        let Expr::Block(block) = strip_parens(&outer.body) else {
            return false;
        };
        &block.block
    } else {
        let Expr::Block(block) = strip_parens(&outer.body) else {
            return false;
        };
        let [Stmt::Expr(Expr::Closure(inner), None)] = block.block.stmts.as_slice() else {
            return false;
        };
        let Expr::Block(block) = strip_parens(&inner.body) else {
            return false;
        };
        &block.block
    };

    if response_block.stmts.len() != 5
        || !matches!(&response_block.stmts[0], Stmt::Local(local)
            if matches!(&local.pat, Pat::Ident(binding) if binding.ident == "body")
                && local.init.as_ref().is_some_and(|init| matches!(strip_parens(&init.expr), Expr::Call(call)
                    if exact_route_call_path(call, &["edgezero_core", "body", "Body", "from"])
                        && call.args.len() == 1)))
        || !matches!(&response_block.stmts[1], Stmt::Local(local)
            if matches!(&local.pat, Pat::Ident(binding) if binding.ident == "resp")
                && local.init.as_ref().is_some_and(|init| matches!(strip_parens(&init.expr), Expr::Call(call)
                    if exact_route_call_path(call, &["Response", "new"])
                        && call.args.len() == 1
                        && call.args.first().is_some_and(|argument| is_ident(argument, "body")))))
        || !matches!(&response_block.stmts[2], Stmt::Expr(Expr::Assign(assignment), _)
            if exact_status_assignment(assignment, adapter))
        || !matches!(&response_block.stmts[3], Stmt::Expr(Expr::MethodCall(insert), _)
            if insert.method == "insert"
                && insert.args.len() == 2
                && matches!(strip_parens(&insert.receiver), Expr::MethodCall(headers)
                    if headers.method == "headers_mut"
                        && headers.args.is_empty()
                        && is_ident(&headers.receiver, "resp")))
    {
        return false;
    }
    matches!(&response_block.stmts[4], Stmt::Expr(Expr::Async(future), None)
        if matches!(future.block.stmts.as_slice(), [Stmt::Expr(Expr::Call(ok), None)]
            if call_named_route(ok, "Ok")
                && ok.args.len() == 1
                && ok.args.first().is_some_and(|argument| is_ident(argument, "resp"))))
}

fn exact_status_assignment(assignment: &syn::ExprAssign, adapter: &str) -> bool {
    let exact_receiver = matches!(strip_parens(&assignment.left), Expr::Unary(unary)
        if matches!(unary.op, syn::UnOp::Deref(_))
            && matches!(strip_parens(&unary.expr), Expr::MethodCall(call)
                if call.method == "status_mut"
                    && call.args.is_empty()
                    && is_ident(&call.receiver, "resp")));
    let exact_value = if adapter == "spin" {
        matches!(strip_parens(&assignment.right), Expr::Path(path)
            if path.path.segments.len() == 2
                && path.path.segments[0].ident == "StatusCode"
                && path.path.segments[1].ident == "SERVICE_UNAVAILABLE")
    } else {
        is_ident(&assignment.right, "status")
    };
    exact_receiver && exact_value
}

fn exact_method_list(file: &syn::File, name: &str) -> Result<BTreeSet<String>, Report<RouteError>> {
    let function = exact_top_function(file, name)?;
    let [Stmt::Expr(Expr::Array(array), None)] = function.block.stmts.as_slice() else {
        return Err(invalid(format!(
            "unsupported adapter method function: {name}"
        )));
    };
    Ok(parse_method_array(array)?.into_iter().collect())
}

fn literal_string_value(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Lit(literal) => match &literal.lit {
            Lit::Str(value) => Some(value.value()),
            _ => None,
        },
        _ => None,
    }
}

fn expression_is_get(expression: &Expr) -> bool {
    exact_route_path(expression, &["Method", "GET"])
        || exact_route_path(expression, &["FastlyMethod", "GET"])
}

fn validate_fastly_pre_router(file: &syn::File) -> Result<(), Report<RouteError>> {
    let health = exact_top_function(file, "health_response")?;
    let health_if = health
        .block
        .stmts
        .iter()
        .find_map(statement_if)
        .ok_or_else(|| {
            invalid("Fastly health_response is missing its exact controlling condition")
        })?;
    if health.block.stmts.len() != 2
        || !exact_get_path_condition(&health_if.cond, "/health")
        || !block_returns_health_200_ok(&health_if.then_branch)
        || !matches!(&health.block.stmts[1], Stmt::Expr(value, None)
            if exact_route_path(value, &["None"]))
    {
        return Err(invalid("Fastly health route AST receipt differs"));
    }
    let entry = exact_top_function(file, "edgezero_main")?;
    let ja4 = entry
        .block
        .stmts
        .iter()
        .filter_map(statement_if)
        .find(|expression| exact_get_path_condition(&expression.cond, "/_ts/debug/ja4"))
        .ok_or_else(|| invalid("Fastly JA4 route condition AST receipt differs"))?;
    if !exact_ja4_branch(&ja4.then_branch) {
        return Err(invalid(
            "Fastly JA4 predicate is not bound to the JA4 branch",
        ));
    }
    Ok(())
}

fn validate_fastly_health_call(file: &syn::File) -> Result<(), Report<RouteError>> {
    let main = exact_top_function(file, "main")?;
    let exact_request = matches!(main.block.stmts.first(), Some(Stmt::Local(local))
        if local.attrs.is_empty()
            && matches!(&local.pat, Pat::Ident(binding)
                if binding.ident == "req" && binding.mutability.is_none() && binding.subpat.is_none())
            && local.init.as_ref().is_some_and(|init| matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["FastlyRequest", "from_client"])
                    && call.args.is_empty())));
    let exact_health = main.block.stmts.get(1).is_some_and(|statement| {
        let Stmt::Expr(Expr::If(branch), _) = statement else {
            return false;
        };
        let Expr::Let(condition) = strip_parens(&branch.cond) else {
            return false;
        };
        let Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
            return false;
        };
        pattern.path.is_ident("Some")
            && pattern.elems.len() == 1
            && matches!(pattern.elems.first(), Some(Pat::Ident(binding))
                if binding.ident == "response" && binding.subpat.is_none())
            && matches!(strip_parens(&condition.expr), Expr::Call(call)
                if matches!(call.func.as_ref(), Expr::Path(path) if path.path.is_ident("health_response"))
                    && call.args.len() == 1
                    && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                        if reference.mutability.is_none() && is_ident(&reference.expr, "req")))
            && branch.else_branch.is_none()
            && branch.then_branch.stmts.len() == 2
            && matches!(&branch.then_branch.stmts[0], Stmt::Expr(Expr::MethodCall(call), _)
                if call.method == "send_to_client" && call.args.is_empty()
                    && is_ident(&call.receiver, "response"))
            && matches!(&branch.then_branch.stmts[1], Stmt::Expr(Expr::Return(value), _)
                if value.expr.is_none())
    });
    if !exact_request || !exact_health {
        return Err(invalid(
            "Fastly main does not dispatch through the exact health response",
        ));
    }
    Ok(())
}

fn exact_ja4_branch(block: &Block) -> bool {
    if block.stmts.len() != 2 {
        return false;
    }
    let Stmt::Expr(Expr::Match(routes), _) = &block.stmts[0] else {
        return false;
    };
    let guarded = routes.arms.iter().filter(|arm| {
        arm.guard.as_ref().is_some_and(|(_, guard)| {
            matches!(strip_parens(guard), Expr::Field(field)
                if matches!(&field.member, syn::Member::Named(name) if name == "ja4_endpoint_enabled")
                    && matches!(strip_parens(&field.base), Expr::Field(debug)
                        if is_ident(&debug.base, "settings")
                            && matches!(&debug.member, syn::Member::Named(name) if name == "debug")))
        }) && matches!(strip_parens(&arm.body), Expr::Block(body)
            if body.block.stmts.first().is_some_and(|statement| matches!(statement, Stmt::Expr(Expr::MethodCall(send), _)
                if send.method == "send_to_client"
                    && matches!(strip_parens(&send.receiver), Expr::Call(call)
                        if call_named_route(call, "build_ja4_debug_response")
                            && call.args.len() == 1
                            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                                if reference.mutability.is_none() && is_ident(&reference.expr, "req"))))))
    }).count();
    guarded == 1
        && matches!(&block.stmts[1], Stmt::Expr(Expr::Return(value), _)
            if value.expr.is_none())
}

fn validate_fastly_dispatch_tsjs(file: &syn::File) -> Result<(), Report<RouteError>> {
    let dispatch = exact_top_function(file, "dispatch_fallback")?;
    struct ResultBindings {
        bindings: usize,
        assignments: usize,
    }
    impl<'ast> Visit<'ast> for ResultBindings {
        fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
            self.bindings += usize::from(pattern.ident == "result");
            visit::visit_pat_ident(self, pattern);
        }

        fn visit_expr_assign(&mut self, assignment: &'ast syn::ExprAssign) {
            self.assignments += usize::from(is_ident(&assignment.left, "result"));
            visit::visit_expr_assign(self, assignment);
        }
    }
    let mut authority = ResultBindings {
        bindings: 0,
        assignments: 0,
    };
    authority.visit_block(&dispatch.block);
    if dispatch.block.stmts.len() < 3 {
        return Err(invalid(
            "Fastly returned dispatcher is disconnected from TSJS",
        ));
    }
    let result_index = dispatch.block.stmts.len() - 3;
    let exact_result = matches!(&dispatch.block.stmts[result_index], Stmt::Local(local)
    if local.attrs.is_empty()
        && matches!(&local.pat, Pat::Ident(binding)
            if binding.ident == "result" && binding.mutability.is_none() && binding.subpat.is_none())
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::If(branch)
                if matches!(strip_parens(&branch.cond), Expr::Call(call)
                    if call_named_route(call, "uses_dynamic_tsjs_fallback")
                        && call.args.len() == 2
                        && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                            if reference.mutability.is_none() && is_ident(&reference.expr, "method"))
                        && matches!(call.args.iter().nth(1).map(strip_parens), Some(Expr::Reference(reference))
                            if reference.mutability.is_none() && is_ident(&reference.expr, "path")))
                    && exact_tsjs_branch_result(&branch.then_branch, false))
        }));
    if !exact_result
        || authority.bindings != 1
        || authority.assignments != 0
        || !exact_fastly_result_response(&dispatch.block.stmts[result_index + 1])
        || !exact_fastly_dispatch_return(&dispatch.block.stmts[result_index + 2])
    {
        return Err(invalid(
            "Fastly returned dispatcher is disconnected from TSJS",
        ));
    }
    Ok(())
}

fn exact_fastly_result_response(statement: &Stmt) -> bool {
    let Stmt::Local(local) = statement else {
        return false;
    };
    if !local.attrs.is_empty()
        || !matches!(&local.pat, Pat::Ident(binding)
            if binding.ident == "response" && binding.mutability.is_none() && binding.subpat.is_none())
    {
        return false;
    }
    let Some(init) = &local.init else {
        return false;
    };
    let Expr::MethodCall(unwrap) = strip_parens(&init.expr) else {
        return false;
    };
    if unwrap.method != "unwrap_or_else"
        || !is_ident(&unwrap.receiver, "result")
        || unwrap.args.len() != 1
    {
        return false;
    }
    let Some(Expr::Closure(error)) = unwrap.args.first().map(strip_parens) else {
        return false;
    };
    error.inputs.len() == 1
        && matches!(error.inputs.first(), Some(Pat::Ident(binding))
            if binding.ident == "e" && binding.subpat.is_none())
        && matches!(strip_parens(&error.body), Expr::Call(call)
            if call_named_route(call, "http_error")
                && call.args.len() == 1
                && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                    if reference.mutability.is_none() && is_ident(&reference.expr, "e")))
}

fn exact_fastly_dispatch_return(statement: &Stmt) -> bool {
    let Stmt::Expr(Expr::Call(call), None) = statement else {
        return false;
    };
    call_named_route(call, "attach_dispatch_extensions")
        && call.args.len() == 3
        && call
            .args
            .first()
            .is_some_and(|argument| is_ident(argument, "response"))
        && call
            .args
            .iter()
            .nth(1)
            .is_some_and(|argument| is_ident(argument, "ec"))
        && call
            .args
            .iter()
            .nth(2)
            .is_some_and(|argument| is_ident(argument, "effects"))
}

fn validate_named_handler_dispatch(
    adapter: &str,
    file: &syn::File,
) -> Result<(), Report<RouteError>> {
    let function = exact_top_function(
        file,
        if adapter == "fastly" {
            "run_named_route"
        } else {
            "named_route_handler"
        },
    )?;
    struct Arms<'a> {
        values: Vec<(String, &'a Expr)>,
    }
    impl<'ast> Visit<'ast> for Arms<'ast> {
        fn visit_arm(&mut self, arm: &'ast syn::Arm) {
            if let Some(variant) = match &arm.pat {
                Pat::Path(path)
                    if path.qself.is_none()
                        && path.path.leading_colon.is_none()
                        && path.path.segments.len() == 2
                        && path.path.segments[0].ident == "NamedRouteHandler"
                        && path.path.segments.iter().all(|segment| {
                            matches!(segment.arguments, syn::PathArguments::None)
                        }) =>
                {
                    Some(path.path.segments[1].ident.to_string())
                }
                _ => None,
            } {
                self.values.push((variant, &arm.body));
            }
            visit::visit_arm(self, arm);
        }
    }
    let mut arms = Arms { values: Vec::new() };
    arms.visit_block(&function.block);
    let expected: &[(&str, &str)] = if adapter == "fastly" {
        &[
            ("TrustedServerDiscovery", "handle_trusted_server_discovery"),
            ("VerifySignature", "handle_verify_signature"),
            ("RotateKey", "handle_rotate_key"),
            ("DeactivateKey", "handle_deactivate_key"),
            ("LegacyAdminDenied", "legacy_admin_alias_denied"),
            ("SetTester", "handle_set_tester"),
            ("ClearTester", "handle_clear_tester"),
            ("Auction", "handle_auction"),
            ("FirstPartyProxy", "handle_first_party_proxy"),
            ("FirstPartyClick", "handle_first_party_click"),
            ("FirstPartySign", "handle_first_party_proxy_sign"),
            ("FirstPartyProxyRebuild", "handle_first_party_proxy_rebuild"),
        ]
    } else {
        &[
            ("TrustedServerDiscovery", "handle_trusted_server_discovery"),
            ("VerifySignature", "handle_verify_signature"),
            ("AdminEcNotSupported", "admin_ec_lookup_not_supported"),
            ("AdminEidsLookup", "handle_admin_eids_lookup"),
            ("LegacyAdminDenied", "legacy_admin_alias_denied"),
            ("Auction", "handle_auction"),
            ("FirstPartyProxy", "handle_first_party_proxy"),
            ("FirstPartyClick", "handle_first_party_click"),
            ("FirstPartySign", "handle_first_party_proxy_sign"),
            ("FirstPartyProxyRebuild", "handle_first_party_proxy_rebuild"),
        ]
    };
    let mut audited_behaviors = expected
        .iter()
        .map(|(_, behavior)| *behavior)
        .collect::<Vec<_>>();
    audited_behaviors.extend(["handle_page_bids", "page_bids_preflight_denied"]);
    if block_shadows_route_authority(&function.block, &audited_behaviors) {
        return Err(invalid(format!(
            "{adapter} named handler authority is lexically shadowed"
        )));
    }
    for (variant, behavior) in expected {
        let matching = arms
            .values
            .iter()
            .filter(|(actual, expression)| {
                actual == variant
                    && terminal_behavior_name(expression).as_deref() == Some(*behavior)
                    && exact_named_handler_call(adapter, variant, behavior, expression)
            })
            .count();
        if matching != 1 {
            return Err(invalid(format!(
                "{adapter} named handler behavior differs for {variant}"
            )));
        }
    }
    if arms
        .values
        .iter()
        .filter(|(variant, expression)| variant == "PageBids" && exact_page_bids_arm(expression))
        .count()
        != 1
    {
        return Err(invalid(format!(
            "{adapter} named handler behavior differs for PageBids"
        )));
    }
    if adapter == "fastly" {
        validate_fastly_run_named_specials(function)?;
    }
    if adapter == "axum"
        && arms
            .values
            .iter()
            .filter(|(variant, expression)| {
                variant == "AdminNotSupported"
                    && matches!(strip_parens(expression), Expr::Block(block)
                        if exact_not_implemented_response(&block.block, "resp", true, "axum"))
            })
            .count()
            != 1
    {
        return Err(invalid("axum unsupported handler response differs"));
    }
    Ok(())
}

fn validate_fastly_early_named_dispatch(file: &syn::File) -> Result<(), Report<RouteError>> {
    let execute = exact_top_function(file, "execute_named")?;
    let batch = execute
        .block
        .stmts
        .iter()
        .filter_map(statement_if)
        .filter(|branch| exact_matches_macro(&branch.cond, "handler,NamedRouteHandler::BatchSync"))
        .collect::<Vec<_>>();
    let [batch] = batch.as_slice() else {
        return Err(invalid(
            "Fastly BatchSync early dispatch is missing or ambiguous",
        ));
    };
    if batch.else_branch.is_some()
        || !matches!(batch.then_branch.stmts.as_slice(), [Stmt::Expr(Expr::Return(value), _)]
            if matches!(value.expr.as_deref().map(strip_parens), Some(Expr::Call(ok))
                if call_named_route(ok, "Ok")
                    && ok.args.len() == 1
                    && matches!(ok.args.first().map(strip_parens), Some(Expr::Call(run))
                        if call_named_route(run, "run_batch_sync")
                            && run.args.len() == 3
                            && exact_reference(run.args.first(), "state")
                            && exact_reference(run.args.iter().nth(1), "services")
                            && run.args.iter().nth(2).is_some_and(|argument| is_ident(argument, "req")))))
    {
        return Err(invalid("Fastly BatchSync early dispatch behavior differs"));
    }

    let admin = execute
        .block
        .stmts
        .iter()
        .filter_map(statement_if)
        .filter(|branch| {
            exact_matches_macro(
                &branch.cond,
                "handler,NamedRouteHandler::AdminEcLookup|NamedRouteHandler::AdminEidsLookup",
            )
        })
        .collect::<Vec<_>>();
    let [admin] = admin.as_slice() else {
        return Err(invalid(
            "Fastly admin diagnostic early dispatch is missing or ambiguous",
        ));
    };
    if admin.else_branch.is_some()
        || admin.then_branch.stmts.len() != 2
        || !matches!(&admin.then_branch.stmts[0], Stmt::Local(local)
            if matches!(&local.pat, Pat::Ident(binding)
                if binding.ident == "response"
                    && binding.mutability.is_none()
                    && binding.subpat.is_none())
                && exact_fastly_admin_response_initializer(local))
        || exact_call_occurrences(&admin.then_branch, &["handle_admin_ec_lookup"]) != 1
        || exact_call_occurrences(&admin.then_branch, &["handle_admin_eids_lookup"]) != 1
        || !exact_fastly_admin_calls(&admin.then_branch)
        || !matches!(admin.then_branch.stmts.last(), Some(Stmt::Expr(Expr::Return(value), _))
            if matches!(value.expr.as_deref().map(strip_parens), Some(Expr::Call(ok))
                if call_named_route(ok, "Ok")
                    && ok.args.len() == 1
                    && ok.args.first().is_some_and(|argument| is_ident(argument, "response"))))
    {
        return Err(invalid(
            "Fastly admin diagnostic early dispatch behavior differs",
        ));
    }
    Ok(())
}

fn exact_fastly_admin_calls(block: &Block) -> bool {
    struct AdminCalls {
        ec: usize,
        eids: usize,
    }
    impl<'ast> Visit<'ast> for AdminCalls {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if exact_route_call_path(call, &["handle_admin_ec_lookup"]) {
                self.ec += usize::from(
                    call.args.len() == 3
                        && call.args.first().is_some_and(|argument| {
                            exact_zero_arg_method(argument, "as_ref", |receiver| {
                                is_ident(receiver, "kv")
                            })
                        })
                        && exact_reference(call.args.iter().nth(1), "registry")
                        && exact_reference(call.args.iter().nth(2), "req"),
                );
            } else if exact_route_call_path(call, &["handle_admin_eids_lookup"]) {
                self.eids += usize::from(
                    call.args.len() == 2
                        && exact_reference(call.args.first(), "registry")
                        && exact_reference(call.args.iter().nth(1), "req"),
                );
            }
            visit::visit_expr_call(self, call);
        }
    }
    let mut calls = AdminCalls { ec: 0, eids: 0 };
    calls.visit_block(block);
    calls.ec == 1 && calls.eids == 1
}

fn exact_fastly_admin_response_initializer(local: &syn::Local) -> bool {
    let Some(init) = &local.init else {
        return false;
    };
    let Expr::MethodCall(unwrap) = strip_parens(&init.expr) else {
        return false;
    };
    let exact_error = unwrap.method == "unwrap_or_else"
        && unwrap.args.len() == 1
        && matches!(unwrap.args.first().map(strip_parens), Some(Expr::Closure(closure))
            if matches!(closure.inputs.iter().collect::<Vec<_>>().as_slice(),
                [Pat::Ident(binding)] if binding.ident == "error" && binding.subpat.is_none())
                && matches!(strip_parens(&closure.body), Expr::Call(call)
                    if exact_route_call_path(call, &["http_error"])
                        && call.args.len() == 1
                        && exact_reference(call.args.first(), "error")));
    let Expr::MethodCall(and_then) = strip_parens(&unwrap.receiver) else {
        return false;
    };
    let exact_dispatch = and_then.method == "and_then"
        && and_then.args.len() == 1
        && matches!(and_then.args.first().map(strip_parens), Some(Expr::Closure(closure))
            if matches!(closure.inputs.iter().collect::<Vec<_>>().as_slice(),
                [Pat::Ident(binding)] if binding.ident == "registry" && binding.subpat.is_none())
                && matches!(strip_parens(&closure.body), Expr::Match(dispatch)
                    if is_ident(&dispatch.expr, "handler")
                        && exact_fastly_admin_match_arms(&dispatch.arms)));
    let exact_registry = matches!(strip_parens(&and_then.receiver), Expr::Call(call)
        if exact_route_call_path(call, &["PartnerRegistry", "from_config"])
            && call.args.len() == 1
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none()
                    && exact_field_chain(&reference.expr, "state", &["settings", "ec", "partners"])));
    exact_error && exact_dispatch && exact_registry
}

fn exact_fastly_admin_match_arms(arms: &[syn::Arm]) -> bool {
    let [ec, eids, fallback] = arms else {
        return false;
    };
    let exact_ec = exact_named_variant_pattern(&ec.pat, "AdminEcLookup")
        && ec.guard.is_none()
        && matches!(strip_parens(&ec.body), Expr::Block(block)
            if matches!(block.block.stmts.as_slice(), [Stmt::Local(kv), Stmt::Expr(Expr::Call(call), None)]
                if matches!(&kv.pat, Pat::Ident(binding)
                    if binding.ident == "kv"
                        && binding.mutability.is_none()
                        && binding.subpat.is_none())
                    && kv.init.as_ref().is_some_and(|init| {
                        matches!(strip_parens(&init.expr), Expr::Call(call)
                            if exact_route_call_path(call, &["crate", "maybe_identity_graph"])
                                && call.args.len() == 1
                                && exact_reference_field(call.args.first(), "state", "settings"))
                    })
                    && exact_route_call_path(call, &["handle_admin_ec_lookup"])
                    && call.args.len() == 3
                    && call.args.first().is_some_and(|argument| {
                        exact_zero_arg_method(argument, "as_ref", |receiver| is_ident(receiver, "kv"))
                    })
                    && exact_reference(call.args.iter().nth(1), "registry")
                    && exact_reference(call.args.iter().nth(2), "req")));
    let exact_eids = exact_named_variant_pattern(&eids.pat, "AdminEidsLookup")
        && eids.guard.is_none()
        && matches!(strip_parens(&eids.body), Expr::Call(call)
            if exact_route_call_path(call, &["handle_admin_eids_lookup"])
                && call.args.len() == 2
                && exact_reference(call.args.first(), "registry")
                && exact_reference(call.args.iter().nth(1), "req"));
    let exact_fallback = matches!(&fallback.pat, Pat::Wild(_))
        && fallback.guard.is_none()
        && matches!(strip_parens(&fallback.body), Expr::Macro(value)
        if value.mac.path.is_ident("unreachable")
            && syn::parse2::<syn::LitStr>(value.mac.tokens.clone()).is_ok_and(|message| {
                message.value() == "admin diagnostics should use early dispatch"
            }));
    exact_ec && exact_eids && exact_fallback
}

fn exact_field_chain(expression: &Expr, root: &str, fields: &[&str]) -> bool {
    let mut current = strip_parens(expression);
    for expected in fields.iter().rev() {
        let Expr::Field(field) = current else {
            return false;
        };
        if !matches!(&field.member, syn::Member::Named(name) if name == expected) {
            return false;
        }
        current = strip_parens(&field.base);
    }
    is_ident(current, root)
}

fn exact_reference(expression: Option<&Expr>, name: &str) -> bool {
    matches!(expression.map(strip_parens), Some(Expr::Reference(reference))
        if reference.mutability.is_none() && is_ident(&reference.expr, name))
}

fn exact_matches_macro(expression: &Expr, expected: &str) -> bool {
    let Expr::Macro(value) = strip_parens(expression) else {
        return false;
    };
    value.mac.path.is_ident("matches")
        && value
            .mac
            .tokens
            .to_string()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            == expected
}

fn exact_call_occurrences(block: &Block, path: &[&str]) -> usize {
    struct Calls<'a> {
        path: &'a [&'a str],
        count: usize,
    }
    impl<'ast> Visit<'ast> for Calls<'_> {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            self.count += usize::from(exact_route_call_path(call, self.path));
            visit::visit_expr_call(self, call);
        }
    }
    let mut calls = Calls { path, count: 0 };
    calls.visit_block(block);
    calls.count
}

fn validate_fastly_run_named_specials(function: &syn::ItemFn) -> Result<(), Report<RouteError>> {
    let [Stmt::Expr(Expr::Match(dispatch), None)] = function.block.stmts.as_slice() else {
        return Err(invalid(
            "fastly run_named_route must return one handler match",
        ));
    };
    let identify = dispatch
        .arms
        .iter()
        .filter(|arm| {
            exact_named_variant_pattern(&arm.pat, "Identify") && exact_identify_arm(&arm.body)
        })
        .count();
    let batch = dispatch
        .arms
        .iter()
        .filter(|arm| {
            exact_named_variant_pattern(&arm.pat, "BatchSync")
                && exact_unreachable_arm(
                    &arm.body,
                    "batch-sync should be handled by run_batch_sync",
                )
        })
        .count();
    let admin = dispatch
        .arms
        .iter()
        .filter(|arm| {
            exact_admin_variant_pattern(&arm.pat)
                && exact_unreachable_arm(
                    &arm.body,
                    "admin diagnostics should be handled before EC setup",
                )
        })
        .count();
    if identify != 1 || batch != 1 || admin != 1 {
        return Err(invalid(format!(
            "fastly special named handler behavior differs: identify={identify}, batch={batch}, admin={admin}"
        )));
    }
    Ok(())
}

fn exact_named_variant_pattern(pattern: &Pat, variant: &str) -> bool {
    matches!(pattern, Pat::Path(path)
        if path.qself.is_none()
            && path.path.leading_colon.is_none()
            && path.path.segments.len() == 2
            && path.path.segments[0].ident == "NamedRouteHandler"
            && path.path.segments[1].ident == variant
            && path.path.segments.iter().all(|segment| matches!(segment.arguments, syn::PathArguments::None)))
}

fn exact_admin_variant_pattern(pattern: &Pat) -> bool {
    let Pat::Or(alternatives) = pattern else {
        return false;
    };
    matches!(alternatives.cases.iter().collect::<Vec<_>>().as_slice(), [first, second]
        if exact_named_variant_pattern(first, "AdminEcLookup")
            && exact_named_variant_pattern(second, "AdminEidsLookup"))
}

fn exact_unreachable_arm(expression: &Expr, message: &str) -> bool {
    let Expr::Block(block) = strip_parens(expression) else {
        return false;
    };
    let [statement] = block.block.stmts.as_slice() else {
        return false;
    };
    let item = match statement {
        Stmt::Macro(statement) => &statement.mac,
        Stmt::Expr(Expr::Macro(expression), _) => &expression.mac,
        _ => return false,
    };
    item.path.is_ident("unreachable")
        && syn::parse2::<syn::LitStr>(item.tokens.clone())
            .is_ok_and(|value| value.value() == message)
}

fn exact_identify_arm(expression: &Expr) -> bool {
    let Expr::Block(block) = strip_parens(expression) else {
        return false;
    };
    let [Stmt::Expr(Expr::If(branch), None)] = block.block.stmts.as_slice() else {
        return false;
    };
    let condition = exact_equality(
        &branch.cond,
        |value| {
            matches!(strip_parens(value), Expr::MethodCall(call)
            if call.method == "method" && call.args.is_empty() && is_ident(&call.receiver, "req"))
        },
        |value| {
            matches!(strip_parens(value), Expr::Path(path)
            if path.qself.is_none()
                && path.path.leading_colon.is_none()
                && path.path.segments.len() == 2
                && path.path.segments[0].ident == "Method"
                && path.path.segments[1].ident == "OPTIONS")
        },
    );
    let preflight = matches!(branch.then_branch.stmts.as_slice(), [Stmt::Expr(Expr::Call(call), None)]
        if call_named_route(call, "cors_preflight_identify")
            && call.args.len() == 2
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none()
                    && matches!(strip_parens(&reference.expr), Expr::Field(settings)
                        if is_ident(&settings.base, "state")
                            && matches!(&settings.member, syn::Member::Named(member) if member == "settings")))
            && exact_reference(call.args.iter().nth(1), "req"));
    let real = branch.else_branch.as_ref().is_some_and(|(_, alternative)| {
        matches!(strip_parens(alternative), Expr::Block(body)
            if terminal_block_behavior_name(&body.block).as_deref() == Some("handle_identify")
                && terminal_behavior_call(alternative, "handle_identify")
                    .is_some_and(exact_fastly_identify_call))
    });
    condition && preflight && real
}

fn exact_page_bids_arm(expression: &Expr) -> bool {
    let Expr::Block(block) = strip_parens(expression) else {
        return false;
    };
    let Some(Stmt::Expr(Expr::If(preflight), _)) = block.block.stmts.first() else {
        return false;
    };
    let exact_condition = exact_equality(
        &preflight.cond,
        |value| {
            matches!(strip_parens(value), Expr::MethodCall(call)
            if call.method == "method" && call.args.is_empty() && is_ident(&call.receiver, "req"))
        },
        |value| exact_route_path(value, &["Method", "OPTIONS"]),
    );
    let exact_denial = matches!(preflight.then_branch.stmts.as_slice(), [statement]
        if match statement {
            Stmt::Expr(Expr::Return(value), _) => value.expr.as_deref(),
            Stmt::Expr(value, None) => Some(value),
            _ => None,
        }.is_some_and(|value| terminal_behavior_name(value).as_deref() == Some("page_bids_preflight_denied")));
    let handler_expression = if let Some((_, alternative)) = &preflight.else_branch {
        Some(alternative.as_ref())
    } else {
        block
            .block
            .stmts
            .last()
            .and_then(statement_terminal_expression)
    };
    let exact_real_handler = if preflight.else_branch.is_some() {
        handler_expression
            .and_then(terminal_behavior_name)
            .as_deref()
            == Some("handle_page_bids")
    } else {
        terminal_statements_behavior_name(&block.block.stmts[1..]).as_deref()
            == Some("handle_page_bids")
    };
    let exact_call = handler_expression
        .and_then(|expression| terminal_behavior_call(expression, "handle_page_bids"))
        .is_some_and(exact_page_bids_handler_call);
    exact_condition && exact_denial && exact_real_handler && exact_call
}

fn terminal_behavior_call<'a>(expression: &'a Expr, expected: &str) -> Option<&'a syn::ExprCall> {
    match strip_parens(expression) {
        Expr::Call(call) => {
            if exact_route_call_path(call, &[expected]) {
                Some(call)
            } else if (call_named_route(call, "Ok")
                || call_named_route(call, "Some")
                || call_named_route(call, "make_handler"))
                && (call.args.len() == 1
                    || (call.args.len() == 2 && call_named_route(call, "make_handler")))
            {
                terminal_behavior_call(call.args.last()?, expected)
            } else {
                None
            }
        }
        Expr::MethodCall(call)
            if matches!(call.method.to_string().as_str(), "unwrap_or_else" | "clone") =>
        {
            terminal_behavior_call(&call.receiver, expected)
        }
        Expr::Await(value) => terminal_behavior_call(&value.base, expected),
        Expr::Try(value) => terminal_behavior_call(&value.expr, expected),
        Expr::Closure(value) => terminal_behavior_call(&value.body, expected),
        Expr::Async(value) => value
            .block
            .stmts
            .last()
            .and_then(statement_terminal_expression)
            .and_then(|terminal| terminal_behavior_call(terminal, expected)),
        Expr::Block(value) => value
            .block
            .stmts
            .last()
            .and_then(statement_terminal_expression)
            .and_then(|terminal| terminal_behavior_call(terminal, expected)),
        _ => None,
    }
}

fn exact_named_handler_call(
    adapter: &str,
    variant: &str,
    behavior: &str,
    expression: &Expr,
) -> bool {
    let Some(call) = terminal_behavior_call(expression, behavior) else {
        return false;
    };
    match (adapter, variant) {
        (
            "fastly",
            "TrustedServerDiscovery" | "VerifySignature" | "RotateKey" | "DeactivateKey",
        )
        | (
            "fastly",
            "FirstPartyProxy" | "FirstPartyClick" | "FirstPartySign" | "FirstPartyProxyRebuild",
        ) => {
            call.args.len() == 3
                && exact_reference_field(call.args.first(), "state", "settings")
                && call
                    .args
                    .iter()
                    .nth(1)
                    .is_some_and(|value| is_ident(value, "services"))
                && call
                    .args
                    .iter()
                    .nth(2)
                    .is_some_and(|value| is_ident(value, "req"))
        }
        ("fastly", "LegacyAdminDenied") => call.args.is_empty(),
        ("fastly", "SetTester" | "ClearTester") => {
            call.args.len() == 1 && exact_reference_field(call.args.first(), "state", "settings")
        }
        ("fastly", "Auction") => {
            call.args.len() == 7
                && exact_reference_field(call.args.first(), "state", "settings")
                && exact_reference_field(call.args.iter().nth(1), "state", "orchestrator")
                && call.args.iter().nth(2).is_some_and(|argument| {
                    exact_zero_arg_method(argument, "as_ref", |receiver| {
                        exact_field_receiver(receiver, "ec", "kv_graph")
                    })
                })
                && call
                    .args
                    .iter()
                    .nth(3)
                    .is_some_and(|value| is_ident(value, "registry_ref"))
                && exact_reference_field(call.args.iter().nth(4), "ec", "ec_context")
                && exact_reference(call.args.iter().nth(5), "consent_services")
                && call
                    .args
                    .iter()
                    .nth(6)
                    .is_some_and(|value| is_ident(value, "req"))
        }
        ("axum", "TrustedServerDiscovery" | "VerifySignature")
        | (
            "axum",
            "FirstPartyProxy" | "FirstPartyClick" | "FirstPartySign" | "FirstPartyProxyRebuild",
        ) => {
            call.args.len() == 3
                && exact_reference_field(call.args.first(), "state", "settings")
                && exact_reference(call.args.iter().nth(1), "services")
                && call
                    .args
                    .iter()
                    .nth(2)
                    .is_some_and(|value| is_ident(value, "req"))
        }
        ("axum", "AdminEcNotSupported" | "LegacyAdminDenied") => call.args.is_empty(),
        ("axum", "AdminEidsLookup") => {
            call.args.len() == 2
                && exact_reference(call.args.first(), "partner_registry")
                && exact_reference(call.args.iter().nth(1), "req")
        }
        ("axum", "Auction") => {
            call.args.len() == 7
                && exact_reference_field(call.args.first(), "state", "settings")
                && exact_reference_field(call.args.iter().nth(1), "state", "orchestrator")
                && call
                    .args
                    .iter()
                    .nth(2)
                    .is_some_and(|value| exact_route_path(value, &["None"]))
                && call
                    .args
                    .iter()
                    .nth(3)
                    .is_some_and(|value| exact_route_path(value, &["None"]))
                && exact_reference(call.args.iter().nth(4), "ec_context")
                && exact_reference(call.args.iter().nth(5), "services")
                && call
                    .args
                    .iter()
                    .nth(6)
                    .is_some_and(|value| is_ident(value, "req"))
        }
        _ => false,
    }
}

fn exact_reference_field(expression: Option<&Expr>, root: &str, field: &str) -> bool {
    matches!(expression.map(strip_parens), Some(Expr::Reference(reference))
        if reference.mutability.is_none()
            && exact_field_receiver(&reference.expr, root, field))
}

fn exact_zero_arg_method(
    expression: &Expr,
    method: &str,
    receiver: impl FnOnce(&Expr) -> bool,
) -> bool {
    matches!(strip_parens(expression), Expr::MethodCall(call)
        if call.method == method && call.args.is_empty() && receiver(&call.receiver))
}

fn exact_fastly_identify_call(call: &syn::ExprCall) -> bool {
    call.args.len() == 5
        && exact_reference_field(call.args.first(), "state", "settings")
        && exact_reference(call.args.iter().nth(1), "kv")
        && exact_reference(call.args.iter().nth(2), "partner_registry")
        && exact_reference(call.args.iter().nth(3), "req")
        && exact_reference_field(call.args.iter().nth(4), "ec", "ec_context")
}

fn exact_page_bids_handler_call(call: &syn::ExprCall) -> bool {
    if call.args.len() != 6 {
        return false;
    }
    let fastly = exact_reference_field(call.args.first(), "state", "settings")
        && exact_reference(call.args.iter().nth(1), "consent_services")
        && call.args.iter().nth(2).is_some_and(|argument| {
            exact_zero_arg_method(argument, "as_ref", |receiver| {
                exact_field_receiver(receiver, "ec", "kv_graph")
            })
        })
        && call
            .args
            .iter()
            .nth(3)
            .is_some_and(|value| is_ident(value, "auction"))
        && exact_reference_field(call.args.iter().nth(4), "ec", "ec_context")
        && call
            .args
            .iter()
            .nth(5)
            .is_some_and(|value| is_ident(value, "req"));
    let axum = exact_reference_field(call.args.first(), "state", "settings")
        && exact_reference(call.args.iter().nth(1), "services")
        && call
            .args
            .iter()
            .nth(2)
            .is_some_and(|value| exact_route_path(value, &["None"]))
        && call
            .args
            .iter()
            .nth(3)
            .is_some_and(|value| is_ident(value, "auction"))
        && exact_reference(call.args.iter().nth(4), "ec_context")
        && call
            .args
            .iter()
            .nth(5)
            .is_some_and(|value| is_ident(value, "req"));
    fastly || axum
}

fn statement_terminal_expression(statement: &Stmt) -> Option<&Expr> {
    match statement {
        Stmt::Expr(Expr::Return(value), _) => value.expr.as_deref(),
        Stmt::Expr(value, None) => Some(value),
        _ => None,
    }
}

fn terminal_behavior_name(expression: &Expr) -> Option<String> {
    match strip_parens(expression) {
        Expr::Path(path) => unqualified_path_name(path, false),
        Expr::Call(call) => {
            let path = match call.func.as_ref() {
                Expr::Path(path) => path,
                _ => return None,
            };
            let wrapper = path.path.segments.len() == 1
                && matches!(
                    path.path.segments[0].ident.to_string().as_str(),
                    "Ok" | "Some"
                );
            let name = unqualified_path_name(path, wrapper)?;
            if matches!(name.as_str(), "Ok" | "Some" | "make_handler") {
                terminal_behavior_name(call.args.last()?)
            } else {
                Some(name)
            }
        }
        Expr::MethodCall(call)
            if matches!(call.method.to_string().as_str(), "unwrap_or_else" | "clone") =>
        {
            terminal_behavior_name(&call.receiver)
        }
        Expr::Await(value) => terminal_behavior_name(&value.base),
        Expr::Try(value) => terminal_behavior_name(&value.expr),
        Expr::Closure(value) => terminal_behavior_name(&value.body),
        Expr::Async(value) => terminal_block_behavior_name(&value.block),
        Expr::Block(value) => terminal_block_behavior_name(&value.block),
        _ => None,
    }
}

fn terminal_block_behavior_name(block: &Block) -> Option<String> {
    terminal_statements_behavior_name(&block.stmts)
}

fn terminal_statements_behavior_name(statements: &[Stmt]) -> Option<String> {
    let (terminal, preceding) = statements.split_last()?;
    let behavior = statement_terminal_expression(terminal).and_then(terminal_behavior_name)?;
    let preceding_is_closed = preceding.iter().all(|statement| match statement {
        Stmt::Local(local) => local_initializer_is_closed(local, behavior.as_str()),
        Stmt::Expr(Expr::If(branch), _) => {
            matches!(behavior.as_str(), "handle_auction" | "handle_page_bids")
                && exact_registered_prepare_request_guard(branch)
        }
        _ => false,
    });
    preceding_is_closed.then_some(behavior)
}

fn local_initializer_is_closed(local: &syn::Local, behavior: &str) -> bool {
    if !local.attrs.is_empty()
        || local
            .init
            .as_ref()
            .is_some_and(|init| init.diverge.is_some())
    {
        return false;
    }
    exact_registered_try_local(local, behavior)
        || exact_registered_clone_local(local)
        || exact_registered_services_local(local)
        || exact_registered_request_local(local, behavior)
        || exact_registered_ec_context_local(local)
        || exact_registered_registry_ref_local(local)
        || exact_registered_auction_local(local)
}

fn exact_registered_clone_local(local: &syn::Local) -> bool {
    exact_local_binding(local, "s", false)
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["Arc", "clone"])
                    && call.args.len() == 1
                    && (exact_reference(call.args.first(), "s")
                        || exact_reference(call.args.first(), "state")))
        })
}

fn exact_registered_services_local(local: &syn::Local) -> bool {
    exact_local_binding(local, "services", false)
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["build_runtime_services"])
                    && call.args.len() == 1
                    && exact_reference(call.args.first(), "ctx"))
        })
}

fn exact_registered_request_local(local: &syn::Local, behavior: &str) -> bool {
    let mutable = matches!(behavior, "handle_auction" | "handle_page_bids");
    exact_local_binding(local, "req", mutable)
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::MethodCall(call)
                if call.method == "into_request"
                    && call.args.is_empty()
                    && is_ident(&call.receiver, "ctx"))
        })
}

fn exact_registered_ec_context_local(local: &syn::Local) -> bool {
    exact_local_binding(local, "ec_context", false)
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["build_ec_context"])
                    && call.args.len() == 3
                    && (exact_reference(call.args.first(), "state")
                        || exact_reference_field(call.args.first(), "s", "settings"))
                    && exact_reference(call.args.iter().nth(1), "services")
                    && exact_reference(call.args.iter().nth(2), "req"))
        })
}

fn exact_registered_registry_ref_local(local: &syn::Local) -> bool {
    exact_local_binding(local, "registry_ref", false)
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::If(branch)
                if branch.else_branch.as_ref().is_some_and(|(_, alternative)| {
                    matches!(strip_parens(alternative), Expr::Block(block)
                        if matches!(block.block.stmts.as_slice(), [Stmt::Expr(Expr::Call(some), None)]
                            if exact_route_call_path(some, &["Some"])
                                && some.args.len() == 1
                                && exact_reference(some.args.first(), "partner_registry")))
                })
                    && matches!(strip_parens(&branch.cond), Expr::MethodCall(empty)
                        if empty.method == "is_empty"
                            && empty.args.is_empty()
                            && is_ident(&empty.receiver, "partner_registry"))
                    && matches!(branch.then_branch.stmts.as_slice(), [Stmt::Expr(none, None)]
                        if exact_route_path(none, &["None"])))
        })
}

fn exact_registered_auction_local(local: &syn::Local) -> bool {
    if !exact_local_binding(local, "auction", false) {
        return false;
    }
    let Some(init) = &local.init else {
        return false;
    };
    let Expr::Struct(auction) = strip_parens(&init.expr) else {
        return false;
    };
    if auction.qself.is_some()
        || !auction.path.is_ident("AuctionDispatch")
        || auction.rest.is_some()
        || auction.fields.len() != 3
    {
        return false;
    }
    let Some(orchestrator) = struct_field(auction, "orchestrator") else {
        return false;
    };
    let Some(slots) = struct_field(auction, "slots") else {
        return false;
    };
    let Some(registry) = struct_field(auction, "registry") else {
        return false;
    };
    let root = if exact_reference_field(Some(orchestrator), "state", "orchestrator") {
        "state"
    } else if exact_reference_field(Some(orchestrator), "s", "orchestrator") {
        "s"
    } else {
        return false;
    };
    exact_settings_slots_call(slots, root)
        && (is_ident(registry, "registry_ref") || exact_route_path(registry, &["None"]))
}

fn exact_settings_slots_call(expression: &Expr, root: &str) -> bool {
    matches!(strip_parens(expression), Expr::MethodCall(call)
        if call.method == "creative_opportunity_slots"
            && call.args.is_empty()
            && exact_field_receiver(&call.receiver, root, "settings"))
}

fn exact_local_binding(local: &syn::Local, name: &str, mutable: bool) -> bool {
    matches!(&local.pat, Pat::Ident(binding)
        if binding.ident == name
            && binding.mutability.is_some() == mutable
            && binding.subpat.is_none())
}

fn exact_registered_try_local(local: &syn::Local, behavior: &str) -> bool {
    let Pat::Ident(binding) = &local.pat else {
        return false;
    };
    if binding.mutability.is_some() || binding.subpat.is_some() {
        return false;
    }
    let Some(init) = &local.init else {
        return false;
    };
    let Expr::Try(attempt) = strip_parens(&init.expr) else {
        return false;
    };
    let Expr::Call(call) = strip_parens(&attempt.expr) else {
        return false;
    };
    match (behavior, binding.ident.to_string().as_str()) {
        ("handle_identify", "kv") => {
            exact_route_call_path(call, &["crate", "require_identity_graph"])
                && call.args.len() == 1
                && exact_reference_field(call.args.first(), "state", "settings")
        }
        ("handle_auction" | "handle_page_bids", "consent_services") => {
            exact_route_call_path(call, &["runtime_services_for_consent_route"])
                && call.args.len() == 2
                && exact_reference_field(call.args.first(), "state", "settings")
                && call
                    .args
                    .iter()
                    .nth(1)
                    .is_some_and(|argument| is_ident(argument, "services"))
        }
        (
            "handle_identify" | "handle_admin_eids_lookup" | "handle_auction" | "handle_page_bids",
            "partner_registry",
        ) => {
            exact_route_call_path(call, &["PartnerRegistry", "from_config"])
                && call.args.len() == 1
                && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none()
                    && ["state", "s"].iter().any(|root| {
                        exact_field_chain(
                            &reference.expr,
                            root,
                            &["settings", "ec", "partners"],
                        )
                    }))
        }
        _ => false,
    }
}

fn exact_registered_prepare_request_guard(branch: &syn::ExprIf) -> bool {
    let Expr::Let(condition) = strip_parens(&branch.cond) else {
        return false;
    };
    let Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
        return false;
    };
    let exact_pattern = pattern.path.is_ident("Err")
        && pattern.elems.len() == 1
        && matches!(pattern.elems.first(), Some(Pat::Ident(binding))
            if binding.ident == "error" && binding.subpat.is_none());
    let exact_call = matches!(strip_parens(&condition.expr), Expr::Call(call)
        if exact_route_call_path(
            call,
            &["trusted_server_core", "integrations", "gpt_diagnostics", "prepare_request"],
        )
            && call.args.len() == 2
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none()
                    && exact_field_receiver(&reference.expr, "s", "settings"))
            && matches!(call.args.iter().nth(1).map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_some() && is_ident(&reference.expr, "req")));
    exact_pattern
        && exact_call
        && branch.else_branch.is_none()
        && matches!(branch.then_branch.stmts.as_slice(), [Stmt::Expr(Expr::Return(value), _)]
            if matches!(value.expr.as_deref().map(strip_parens), Some(Expr::Call(ok))
                if call_named_route(ok, "Ok")
                    && ok.args.len() == 1
                    && matches!(ok.args.first().map(strip_parens), Some(Expr::Call(error))
                        if call_named_route(error, "http_error")
                            && error.args.len() == 1
                            && matches!(error.args.first().map(strip_parens), Some(Expr::Reference(reference))
                                if reference.mutability.is_none() && is_ident(&reference.expr, "error")))))
}

fn validate_unsupported_response(
    adapter: &str,
    file: &syn::File,
) -> Result<(), Report<RouteError>> {
    let function = exact_top_function(file, "admin_key_management_not_supported")?;
    if !exact_not_implemented_response(&function.block, "response", false, adapter) {
        return Err(invalid(format!(
            "{adapter} unsupported handler response differs"
        )));
    }
    Ok(())
}

fn exact_not_implemented_response(
    block: &Block,
    response: &str,
    wrapped: bool,
    adapter: &str,
) -> bool {
    let expected_message = match adapter {
        "axum" => {
            "Admin key management is not supported on the Axum dev server.\nUse the Fastly adapter (via Viceroy or deployed) to rotate or deactivate keys.\n"
        }
        "cloudflare" => {
            "Admin key management is not supported on Cloudflare Workers.\nUse the Fastly adapter (via Viceroy or deployed) to rotate or deactivate keys.\n"
        }
        "spin" => {
            "Admin key management is not supported on Fermyon Spin.\nUse the Fastly adapter (via Viceroy or deployed) to rotate or deactivate keys.\n"
        }
        _ => return false,
    };
    let [
        Stmt::Local(body),
        Stmt::Local(response_local),
        status,
        header,
        terminal,
    ] = block.stmts.as_slice()
    else {
        return false;
    };
    let exact_body = matches!(&body.pat, Pat::Ident(binding)
        if binding.ident == "body" && binding.mutability.is_none() && binding.subpat.is_none())
        && body.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["edgezero_core", "body", "Body", "from"])
                    && call.args.len() == 1
                    && call.args.first().and_then(literal_string_value).as_deref()
                        == Some(expected_message))
        });
    let exact_response = matches!(&response_local.pat, Pat::Ident(binding)
        if binding.ident == response
            && binding.mutability.is_some()
            && binding.subpat.is_none())
        && response_local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["Response", "new"])
                    && call.args.len() == 1
                    && call.args.first().is_some_and(|argument| is_ident(argument, "body")))
        });
    let exact_status = matches!(status, Stmt::Expr(Expr::Assign(assignment), _)
    if exact_response_status_assignment(
        assignment,
        response,
        &["StatusCode", "NOT_IMPLEMENTED"],
    ));
    let exact_header = matches!(header, Stmt::Expr(Expr::MethodCall(insert), _)
        if exact_content_type_insert(insert, response, "text/plain; charset=utf-8"));
    let exact_terminal = statement_terminal_expression(terminal).is_some_and(|expression| {
        if wrapped {
            matches!(strip_parens(expression), Expr::Call(call)
                if exact_route_call_path(call, &["Ok"])
                    && call.args.len() == 1
                    && call.args.first().is_some_and(|value| is_ident(value, response)))
        } else {
            is_ident(expression, response)
        }
    });
    exact_body && exact_response && exact_status && exact_header && exact_terminal
}

fn exact_response_status_assignment(
    assignment: &syn::ExprAssign,
    response: &str,
    status: &[&str],
) -> bool {
    matches!(strip_parens(&assignment.left), Expr::Unary(unary)
        if matches!(unary.op, syn::UnOp::Deref(_))
            && matches!(strip_parens(&unary.expr), Expr::MethodCall(call)
                if call.method == "status_mut"
                    && call.args.is_empty()
                    && is_ident(&call.receiver, response)))
        && exact_route_path(&assignment.right, status)
}

fn validate_guarded_response_bodies(
    publisher: &syn::File,
    admin: &syn::File,
    fastly: &syn::File,
    axum: &syn::File,
    cloudflare: &syn::File,
    spin: &syn::File,
) -> Result<(), Report<RouteError>> {
    validate_page_bids_preflight_body(publisher)?;
    validate_admin_ec_unsupported_body(admin)?;
    for (adapter, file) in [
        ("fastly", fastly),
        ("axum", axum),
        ("cloudflare", cloudflare),
        ("spin", spin),
    ] {
        let function = exact_top_function(file, "legacy_admin_alias_denied")?;
        if !exact_legacy_admin_denial(&function.block, adapter) {
            return Err(invalid(format!(
                "{adapter} legacy admin denial response differs"
            )));
        }
    }
    for (adapter, file) in [("cloudflare", cloudflare), ("spin", spin)] {
        let function = exact_top_function(file, "admin_ec_lookup_not_supported")?;
        if !matches!(function.block.stmts.as_slice(), [Stmt::Expr(Expr::Call(call), None)]
            if exact_route_call_path(call, &["core_admin_ec_lookup_not_supported"])
                && call.args.is_empty())
        {
            return Err(invalid(format!(
                "{adapter} portable admin EC response binding differs"
            )));
        }
    }
    Ok(())
}

fn validate_page_bids_preflight_body(file: &syn::File) -> Result<(), Report<RouteError>> {
    let function = exact_top_function(file, "page_bids_preflight_denied")?;
    let [
        Stmt::Local(response),
        status,
        privacy,
        Stmt::Expr(terminal, None),
    ] = function.block.stmts.as_slice()
    else {
        return Err(invalid("page-bids preflight denial body differs"));
    };
    let exact_response = matches!(&response.pat, Pat::Ident(binding)
        if binding.ident == "response"
            && binding.mutability.is_some()
            && binding.subpat.is_none())
        && response.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["Response", "new"])
                    && call.args.len() == 1
                    && matches!(call.args.first().map(strip_parens), Some(Expr::Call(body))
                        if exact_route_call_path(body, &["EdgeBody", "from"])
                            && body.args.len() == 1
                            && body.args.first().and_then(literal_string_value).as_deref()
                                == Some("Forbidden")))
        });
    let exact_status = matches!(status, Stmt::Expr(Expr::Assign(assignment), _)
    if exact_response_status_assignment(
        assignment,
        "response",
        &["StatusCode", "FORBIDDEN"],
    ));
    let exact_privacy = matches!(privacy, Stmt::Expr(Expr::Call(call), _)
        if exact_route_call_path(call, &["enforce_terminal_private_cache_privacy"])
            && call.args.len() == 1
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_some() && is_ident(&reference.expr, "response")));
    if exact_response && exact_status && exact_privacy && is_ident(terminal, "response") {
        Ok(())
    } else {
        Err(invalid("page-bids preflight denial body differs"))
    }
}

fn validate_admin_ec_unsupported_body(file: &syn::File) -> Result<(), Report<RouteError>> {
    let function = exact_top_function(file, "admin_ec_lookup_not_supported")?;
    let exact = matches!(function.block.stmts.as_slice(), [Stmt::Expr(Expr::Call(call), None)]
        if exact_route_call_path(call, &["json_error"])
            && call.args.len() == 2
            && call.args.first().is_some_and(|argument| {
                exact_route_path(argument, &["StatusCode", "NOT_IMPLEMENTED"])
            })
            && call.args.iter().nth(1).and_then(literal_string_value).as_deref()
                == Some("EC identity graph is not configured on this deployment"));
    if exact {
        Ok(())
    } else {
        Err(invalid("portable admin EC unsupported response differs"))
    }
}

fn exact_legacy_admin_denial(block: &Block, adapter: &str) -> bool {
    let [
        Stmt::Local(response),
        status,
        header,
        Stmt::Expr(terminal, None),
    ] = block.stmts.as_slice()
    else {
        return false;
    };
    let exact_response = matches!(&response.pat, Pat::Ident(binding)
        if binding.ident == "response"
            && binding.mutability.is_some()
            && binding.subpat.is_none())
        && response.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["Response", "new"])
                    && call.args.len() == 1
                    && matches!(call.args.first().map(strip_parens), Some(Expr::Call(body))
                        if exact_route_call_path(body, &["edgezero_core", "body", "Body", "from"])
                            && body.args.len() == 1
                            && body.args.first().and_then(literal_string_value).as_deref()
                                == Some("Not found\n")))
        });
    let status_path: &[&str] = if adapter == "cloudflare" {
        &["edgezero_core", "http", "StatusCode", "NOT_FOUND"]
    } else {
        &["StatusCode", "NOT_FOUND"]
    };
    let exact_status = matches!(status, Stmt::Expr(Expr::Assign(assignment), _)
        if exact_response_status_assignment(assignment, "response", status_path));
    let exact_header = matches!(header, Stmt::Expr(Expr::MethodCall(insert), _)
        if exact_content_type_insert(insert, "response", "text/plain; charset=utf-8"));
    exact_response && exact_status && exact_header && is_ident(terminal, "response")
}

fn validate_dynamic_tsjs_guard(adapter: &str, file: &syn::File) -> Result<(), Report<RouteError>> {
    let function = match adapter {
        "fastly" => exact_top_function(file, "uses_dynamic_tsjs_fallback")?,
        "axum" => exact_top_function(file, "dispatch_fallback")?,
        "cloudflare" | "spin" => direct_dispatch_function(file)?,
        _ => return Err(invalid("unknown adapter for TSJS AST receipt")),
    };
    if adapter == "fastly" {
        let Some(Stmt::Expr(condition, None)) = function.block.stmts.last() else {
            return Err(invalid("Fastly TSJS predicate must be the function result"));
        };
        if !exact_tsjs_condition(condition) {
            return Err(invalid("fastly dynamic TSJS GET AST receipt differs"));
        }
        return Ok(());
    }

    let cloudflare_guard = if adapter == "cloudflare" {
        exact_unshadowed_allow_tsjs(function)
    } else {
        false
    };
    let matches = function
        .block
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| statement_if(statement).map(|branch| (index, branch)))
        .filter(|(_, expression)| {
            (exact_tsjs_condition(&expression.cond)
                || (cloudflare_guard && exact_allow_tsjs_condition(&expression.cond)))
                && exact_tsjs_branch_result(&expression.then_branch, adapter == "axum")
        })
        .collect::<Vec<_>>();
    let exact_prior_flow = matches.first().is_some_and(|(index, _)| {
        prior_tsjs_flow_is_closed(adapter, &function.block.stmts[..*index])
    });
    if matches.len() != 1 || !exact_prior_flow {
        return Err(invalid(format!(
            "{adapter} dynamic TSJS condition/handler AST receipt differs"
        )));
    }
    Ok(())
}

fn prior_tsjs_flow_is_closed(adapter: &str, statements: &[Stmt]) -> bool {
    match adapter {
        "axum" => matches!(statements, [
            Stmt::Expr(Expr::If(admin), _),
            prepare,
            Stmt::Local(path),
            Stmt::Local(method),
        ] if exact_admin_fallback_guard(admin)
            && exact_axum_prepare_request_statement(prepare)
            && exact_request_path_binding(path, "to_string")
            && exact_request_method_binding(method)),
        "cloudflare" => matches!(statements, [
            Stmt::Local(services),
            Stmt::Local(request),
            Stmt::Expr(Expr::If(admin), _),
            Stmt::Expr(Expr::If(prepare), _),
            Stmt::Local(path),
            Stmt::Local(method),
            Stmt::Local(allow),
        ] if exact_services_binding(services, "build_per_request_services")
            && exact_request_binding(request)
            && exact_admin_fallback_guard(admin)
            && exact_prepare_request_guard(prepare)
            && exact_request_path_binding(path, "to_owned")
            && exact_request_method_binding(method)
            && exact_allow_tsjs_binding(allow)),
        "spin" => matches!(statements, [
            Stmt::Local(services),
            Stmt::Local(request),
            Stmt::Expr(Expr::If(admin), _),
            Stmt::Expr(Expr::If(prepare), _),
            Stmt::Local(path),
            Stmt::Local(method),
        ] if exact_services_binding(services, "build_runtime_services")
            && exact_request_binding(request)
            && exact_admin_fallback_guard(admin)
            && exact_prepare_request_guard(prepare)
            && exact_request_path_binding(path, "to_owned")
            && exact_request_method_binding(method)),
        _ => false,
    }
}

fn exact_axum_prepare_request_statement(statement: &Stmt) -> bool {
    matches!(statement, Stmt::Expr(Expr::Try(attempt), Some(_))
        if matches!(strip_parens(&attempt.expr), Expr::Call(call)
            if exact_route_call_path(
                call,
                &["trusted_server_core", "integrations", "gpt_diagnostics", "prepare_request"],
            )
                && call.args.len() == 2
                && exact_reference_field(call.args.first(), "state", "settings")
                && matches!(call.args.iter().nth(1).map(strip_parens), Some(Expr::Reference(reference))
                    if reference.mutability.is_some() && is_ident(&reference.expr, "req"))))
}

fn exact_services_binding(local: &syn::Local, function: &str) -> bool {
    matches!(&local.pat, Pat::Ident(binding)
        if binding.ident == "services"
            && binding.mutability.is_none()
            && binding.subpat.is_none())
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &[function])
                    && call.args.len() == 1
                    && exact_reference(call.args.first(), "ctx"))
        })
}

fn exact_request_binding(local: &syn::Local) -> bool {
    matches!(&local.pat, Pat::Ident(binding)
        if binding.ident == "req"
            && binding.mutability.is_some()
            && binding.subpat.is_none())
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::MethodCall(call)
                if call.method == "into_request"
                    && call.args.is_empty()
                    && is_ident(&call.receiver, "ctx"))
        })
}

fn exact_request_path_binding(local: &syn::Local, conversion: &str) -> bool {
    matches!(&local.pat, Pat::Ident(binding)
        if binding.ident == "path"
            && binding.mutability.is_none()
            && binding.subpat.is_none())
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::MethodCall(convert)
                if convert.method == conversion
                    && convert.args.is_empty()
                    && matches!(strip_parens(&convert.receiver), Expr::MethodCall(path)
                        if path.method == "path"
                            && path.args.is_empty()
                            && matches!(strip_parens(&path.receiver), Expr::MethodCall(uri)
                                if uri.method == "uri"
                                    && uri.args.is_empty()
                                    && is_ident(&uri.receiver, "req"))))
        })
}

fn exact_request_method_binding(local: &syn::Local) -> bool {
    matches!(&local.pat, Pat::Ident(binding)
        if binding.ident == "method"
            && binding.mutability.is_none()
            && binding.subpat.is_none())
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::MethodCall(clone)
                if clone.method == "clone"
                    && clone.args.is_empty()
                    && matches!(strip_parens(&clone.receiver), Expr::MethodCall(method)
                        if method.method == "method"
                            && method.args.is_empty()
                            && is_ident(&method.receiver, "req")))
        })
}

fn exact_allow_tsjs_binding(local: &syn::Local) -> bool {
    matches!(&local.pat, Pat::Ident(binding)
        if binding.ident == "allow_tsjs"
            && binding.mutability.is_none()
            && binding.subpat.is_none())
        && local.init.as_ref().is_some_and(|init| {
            exact_equality(
                &init.expr,
                |value| is_ident(value, "method"),
                |value| exact_route_path(value, &["Method", "GET"]),
            )
        })
}

fn exact_admin_fallback_guard(branch: &syn::ExprIf) -> bool {
    let Expr::Let(condition) = strip_parens(&branch.cond) else {
        return false;
    };
    let Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
        return false;
    };
    let exact_pattern = pattern.path.is_ident("Some")
        && pattern.elems.len() == 1
        && matches!(pattern.elems.first(), Some(Pat::Ident(binding))
            if binding.ident == "response" && binding.subpat.is_none());
    let exact_call = matches!(strip_parens(&condition.expr), Expr::Call(call)
        if call_named_route(call, "deny_admin_diagnostic_fallback")
            && call.args.len() == 1
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none() && is_ident(&reference.expr, "req")));
    exact_pattern
        && exact_call
        && branch.else_branch.is_none()
        && matches!(branch.then_branch.stmts.as_slice(), [Stmt::Expr(Expr::Return(value), _)]
        if value.expr.as_deref().is_some_and(|expression| {
            is_ident(expression, "response")
                || matches!(strip_parens(expression), Expr::Call(ok)
                    if call_named_route(ok, "Ok")
                        && ok.args.len() == 1
                        && ok.args.first().is_some_and(|argument| is_ident(argument, "response")))
        }))
}

fn exact_prepare_request_guard(branch: &syn::ExprIf) -> bool {
    let Expr::Let(condition) = strip_parens(&branch.cond) else {
        return false;
    };
    let Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
        return false;
    };
    let exact_pattern = pattern.path.is_ident("Err")
        && pattern.elems.len() == 1
        && matches!(pattern.elems.first(), Some(Pat::Ident(binding))
            if binding.ident == "error" && binding.subpat.is_none());
    let exact_call = matches!(strip_parens(&condition.expr), Expr::Call(call)
        if exact_route_call_path(
            call,
            &["trusted_server_core", "integrations", "gpt_diagnostics", "prepare_request"],
        )
            && call.args.len() == 2
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none()
                    && matches!(strip_parens(&reference.expr), Expr::Field(settings)
                        if is_ident(&settings.base, "state")
                            && matches!(&settings.member, syn::Member::Named(member) if member == "settings")))
            && matches!(call.args.iter().nth(1).map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_some() && is_ident(&reference.expr, "req")));
    exact_pattern
        && exact_call
        && branch.else_branch.is_none()
        && matches!(branch.then_branch.stmts.as_slice(), [Stmt::Expr(Expr::Return(value), _)]
            if matches!(value.expr.as_deref().map(strip_parens), Some(Expr::Call(ok))
                if call_named_route(ok, "Ok")
                    && ok.args.len() == 1
                    && matches!(ok.args.first().map(strip_parens), Some(Expr::Call(error))
                        if call_named_route(error, "http_error")
                            && error.args.len() == 1
                            && matches!(error.args.first().map(strip_parens), Some(Expr::Reference(reference))
                                if reference.mutability.is_none() && is_ident(&reference.expr, "error")))))
}

fn exact_route_call_path(call: &syn::ExprCall, expected: &[&str]) -> bool {
    matches!(call.func.as_ref(), Expr::Path(path)
    if path.qself.is_none()
        && path.path.leading_colon.is_none()
        && path.path.segments.len() == expected.len()
        && path.path.segments.iter().zip(expected).all(|(segment, name)| {
            segment.ident == *name && matches!(segment.arguments, syn::PathArguments::None)
        }))
}

fn direct_dispatch_function(file: &syn::File) -> Result<&syn::ItemFn, Report<RouteError>> {
    let build_router = exact_top_function(file, "build_router")?;
    let [Stmt::Expr(Expr::Block(body), None)] = build_router.block.stmts.as_slice() else {
        return Err(invalid("build_router must return one direct lexical block"));
    };
    let direct = body
        .block
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Item(Item::Fn(function)) if function.sig.ident == "dispatch" => Some(function),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [dispatch] = direct.as_slice() else {
        return Err(invalid(
            "fallback dispatch must have one direct lexical definition",
        ));
    };

    struct DispatchDefinitions<'a> {
        functions: Vec<&'a syn::ItemFn>,
    }
    impl<'ast> Visit<'ast> for DispatchDefinitions<'ast> {
        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            if function.sig.ident == "dispatch" {
                self.functions.push(function);
            }
            visit::visit_item_fn(self, function);
        }
    }
    let mut definitions = DispatchDefinitions {
        functions: Vec::new(),
    };
    definitions.visit_file(file);
    let mut production = Vec::new();
    for function in definitions.functions {
        if route_item_is_production(&function.attrs, "fallback dispatch")? {
            production.push(function);
        }
    }
    if production.len() != 1 {
        return Err(invalid(
            "fallback dispatch must have one production lexical definition",
        ));
    }
    Ok(*dispatch)
}

fn validate_nested_dispatch_binding(
    adapter: &str,
    file: &syn::File,
) -> Result<(), Report<RouteError>> {
    let _dispatch = direct_dispatch_function(file)?;
    let build_router = exact_top_function(file, "build_router")?;
    let [Stmt::Expr(Expr::Block(body), None)] = build_router.block.stmts.as_slice() else {
        return Err(invalid(format!(
            "{adapter} build_router must return one direct lexical block"
        )));
    };
    let fallbacks = body
        .block
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Local(local)
                if matches!(&local.pat, Pat::Ident(binding) if binding.ident == "fallback") =>
            {
                local.init.as_ref().map(|init| init.expr.as_ref())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let [fallback] = fallbacks.as_slice() else {
        return Err(invalid(format!(
            "{adapter} fallback must have one direct lexical binding"
        )));
    };
    if !terminal_exact_dispatch_call(fallback) {
        return Err(invalid(format!(
            "{adapter} fallback does not call its direct lexical dispatch"
        )));
    }
    Ok(())
}

fn terminal_exact_dispatch_call(expression: &Expr) -> bool {
    match strip_parens(expression) {
        Expr::Closure(closure) => terminal_exact_dispatch_call(&closure.body),
        Expr::Block(block) => block
            .block
            .stmts
            .last()
            .and_then(statement_terminal_expression)
            .is_some_and(terminal_exact_dispatch_call),
        Expr::Call(call) => {
            call_named_route(call, "dispatch")
                && call.args.len() == 2
                && call
                    .args
                    .first()
                    .is_some_and(|argument| is_ident(argument, "s"))
                && call
                    .args
                    .iter()
                    .nth(1)
                    .is_some_and(|argument| is_ident(argument, "ctx"))
        }
        _ => false,
    }
}

fn exact_unshadowed_allow_tsjs(function: &syn::ItemFn) -> bool {
    struct Bindings {
        valid: usize,
        total: usize,
        assignments: usize,
    }
    impl<'ast> Visit<'ast> for Bindings {
        fn visit_local(&mut self, local: &'ast syn::Local) {
            if matches!(&local.pat, Pat::Ident(binding) if binding.ident == "allow_tsjs") {
                self.total += 1;
                self.valid += usize::from(
                    local.attrs.is_empty()
                        && matches!(&local.pat, Pat::Ident(binding)
                            if binding.mutability.is_none() && binding.subpat.is_none())
                        && local
                            .init
                            .as_ref()
                            .is_some_and(|init| exact_method_get(&init.expr)),
                );
            }
            visit::visit_local(self, local);
        }

        fn visit_expr_assign(&mut self, assignment: &'ast syn::ExprAssign) {
            self.assignments += usize::from(is_ident(&assignment.left, "allow_tsjs"));
            visit::visit_expr_assign(self, assignment);
        }
    }
    let mut bindings = Bindings {
        valid: 0,
        total: 0,
        assignments: 0,
    };
    bindings.visit_block(&function.block);
    bindings.valid == 1 && bindings.total == 1 && bindings.assignments == 0
}

fn exact_top_function<'a>(
    file: &'a syn::File,
    name: &str,
) -> Result<&'a syn::ItemFn, Report<RouteError>> {
    let mut functions = Vec::new();
    for item in &file.items {
        let Item::Fn(function) = item else { continue };
        if function.sig.ident == name
            && route_item_is_production(&function.attrs, &format!("adapter function `{name}`"))?
        {
            functions.push(function);
        }
    }
    if functions.len() != 1 {
        return Err(invalid(format!(
            "adapter function `{name}` is missing or ambiguous"
        )));
    }
    Ok(functions[0])
}

fn exact_impl_method<'a>(
    file: &'a syn::File,
    owner: &str,
    name: &str,
) -> Result<&'a Block, Report<RouteError>> {
    let mut methods = Vec::new();
    for item in &file.items {
        let Item::Impl(item) = item else { continue };
        let syn::Type::Path(self_type) = item.self_ty.as_ref() else {
            continue;
        };
        if item.trait_.is_some() || !self_type.path.is_ident(owner) {
            continue;
        }
        if !route_item_is_production(&item.attrs, &format!("{owner} impl"))? {
            continue;
        }
        for member in &item.items {
            let syn::ImplItem::Fn(function) = member else {
                continue;
            };
            if function.sig.ident == name
                && route_item_is_production(&function.attrs, &format!("{owner}::{name}"))?
            {
                methods.push(&function.block);
            }
        }
    }
    let Some(method) = methods.first().copied() else {
        return Err(invalid(format!("missing {owner}::{name}")));
    };
    if methods.len() != 1 {
        return Err(invalid(format!("duplicate {owner}::{name}")));
    }
    Ok(method)
}

fn route_item_is_production(
    attributes: &[Attribute],
    label: &str,
) -> Result<bool, Report<RouteError>> {
    let mut cfg_test = false;
    for attribute in attributes {
        if attribute.path().is_ident("cfg_attr") {
            return Err(invalid(format!(
                "unsupported conditional attribute on {label}"
            )));
        }
        if !attribute.path().is_ident("cfg") {
            continue;
        }
        let syn::Meta::List(list) = &attribute.meta else {
            return Err(invalid(format!("invalid cfg on {label}")));
        };
        if list.tokens.to_string() == "test" && !cfg_test {
            cfg_test = true;
        } else {
            return Err(invalid(format!("unsupported production cfg on {label}")));
        }
    }
    Ok(!cfg_test)
}

fn statement_if(statement: &Stmt) -> Option<&syn::ExprIf> {
    match statement {
        Stmt::Expr(Expr::If(expression), _) => Some(expression),
        Stmt::Local(local) => local
            .init
            .as_ref()
            .and_then(|init| match init.expr.as_ref() {
                Expr::If(expression) => Some(expression),
                _ => None,
            }),
        _ => None,
    }
}

fn exact_get_path_condition(expression: &Expr, path: &str) -> bool {
    let Expr::Binary(and) = strip_parens(expression) else {
        return false;
    };
    if !matches!(and.op, syn::BinOp::And(_)) {
        return false;
    }
    (exact_request_method_get(&and.left) && exact_request_path(&and.right, path))
        || (exact_request_path(&and.left, path) && exact_request_method_get(&and.right))
}

fn exact_tsjs_condition(expression: &Expr) -> bool {
    let Expr::Binary(and) = strip_parens(expression) else {
        return false;
    };
    if !matches!(and.op, syn::BinOp::And(_)) {
        return false;
    }
    (exact_method_get(&and.left) && exact_tsjs_prefix(&and.right))
        || (exact_tsjs_prefix(&and.left) && exact_method_get(&and.right))
}

fn exact_allow_tsjs_condition(expression: &Expr) -> bool {
    let Expr::Binary(and) = strip_parens(expression) else {
        return false;
    };
    if !matches!(and.op, syn::BinOp::And(_)) {
        return false;
    }
    (is_ident(&and.left, "allow_tsjs") && exact_tsjs_prefix(&and.right))
        || (exact_tsjs_prefix(&and.left) && is_ident(&and.right, "allow_tsjs"))
}

fn exact_request_method_get(expression: &Expr) -> bool {
    exact_equality(
        expression,
        |value| {
            matches!(strip_parens(value), Expr::MethodCall(call)
            if call.method == "get_method" && call.args.is_empty() && is_ident(&call.receiver, "req"))
        },
        expression_is_get,
    )
}

fn exact_method_get(expression: &Expr) -> bool {
    exact_equality(
        expression,
        |value| {
            matches!(strip_parens(value), Expr::Path(path) if path.path.is_ident("method"))
                || matches!(strip_parens(value), Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Deref(_)) && is_ident(&unary.expr, "method"))
        },
        expression_is_get,
    )
}

fn exact_request_path(expression: &Expr, expected: &str) -> bool {
    exact_equality(
        expression,
        |value| {
            matches!(strip_parens(value), Expr::MethodCall(call)
            if call.method == "get_path" && call.args.is_empty() && is_ident(&call.receiver, "req"))
        },
        |value| literal_string_value(strip_parens(value)).as_deref() == Some(expected),
    )
}

fn exact_equality(
    expression: &Expr,
    left: impl Fn(&Expr) -> bool,
    right: impl Fn(&Expr) -> bool,
) -> bool {
    let Expr::Binary(binary) = strip_parens(expression) else {
        return false;
    };
    matches!(binary.op, syn::BinOp::Eq(_))
        && ((left(&binary.left) && right(&binary.right))
            || (right(&binary.left) && left(&binary.right)))
}

fn exact_tsjs_prefix(expression: &Expr) -> bool {
    matches!(strip_parens(expression), Expr::MethodCall(call)
        if call.method == "starts_with"
            && is_ident(&call.receiver, "path")
            && call.args.len() == 1
            && call.args.first().and_then(literal_string_value).as_deref() == Some("/static/tsjs="))
}

fn strip_parens(expression: &Expr) -> &Expr {
    match expression {
        Expr::Paren(paren) => strip_parens(&paren.expr),
        Expr::Group(group) => strip_parens(&group.expr),
        _ => expression,
    }
}

fn exact_tsjs_branch_result(block: &Block, returned: bool) -> bool {
    let [statement] = block.stmts.as_slice() else {
        return false;
    };
    let expression = match (returned, statement) {
        (true, Stmt::Expr(Expr::Return(value), _)) => value.expr.as_deref(),
        (false, Stmt::Expr(expression, None)) => Some(expression),
        _ => None,
    };
    expression.is_some_and(|expression| {
        expression_root_call_name(expression) == Some("handle_tsjs_dynamic")
    })
}

fn expression_root_call_name(expression: &Expr) -> Option<&str> {
    match strip_parens(expression) {
        Expr::Call(call) => match call.func.as_ref() {
            Expr::Path(path) => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .and_then(|name| (name == "handle_tsjs_dynamic").then_some("handle_tsjs_dynamic")),
            _ => None,
        },
        Expr::Await(value) => expression_root_call_name(&value.base),
        Expr::Try(value) => expression_root_call_name(&value.expr),
        _ => None,
    }
}

fn block_returns_health_200_ok(block: &Block) -> bool {
    let [Stmt::Expr(Expr::Return(value), _)] = block.stmts.as_slice() else {
        return false;
    };
    let Some(Expr::Call(some)) = value.expr.as_deref().map(strip_parens) else {
        return false;
    };
    if !call_named_route(some, "Some") || some.args.len() != 1 {
        return false;
    }
    matches!(some.args.first().map(strip_parens), Some(Expr::MethodCall(body))
        if body.method == "with_body_text_plain"
            && body.args.len() == 1
            && body.args.first().and_then(literal_string_value).as_deref() == Some("ok")
            && matches!(strip_parens(&body.receiver), Expr::Call(status)
                            if exact_route_call_path(status, &["FastlyResponse", "from_status"])
                    && status.args.len() == 1
                    && matches!(status.args.first().map(strip_parens), Some(Expr::Lit(literal))
                        if matches!(&literal.lit, Lit::Int(number) if number.base10_digits() == "200"))))
}

fn is_review_date(value: &str) -> bool {
    if !(value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit()))
    {
        return false;
    }
    let year = value[0..4].parse::<u32>().unwrap_or(0);
    let month = value[5..7].parse::<u32>().unwrap_or(0);
    let day = value[8..10].parse::<u32>().unwrap_or(0);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let maximum = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year > 0 && (1..=maximum).contains(&day)
}

fn read_utf8(repository: &Repository, path: &str) -> Result<String, Report<RouteError>> {
    let path = NormalizedRelativePath::new(std::path::Path::new(path))
        .map_err(|error| invalid(format!("invalid route source path: {error:?}")))?;
    let bytes = repository
        .read_tracked_bounded(&path, MAX_ROUTE_INPUT_BYTES)
        .map_err(|error| invalid(format!("cannot read route source: {error:?}")))?;
    String::from_utf8(bytes).map_err(|error| invalid(format!("route source is not UTF-8: {error}")))
}

/// Require exact route-set equality and identify a changed semantic axis.
///
/// # Errors
///
/// Returns an error for every missing, extra, or altered route record.
pub fn validate_routes(
    expected: &BTreeSet<RouteRecord>,
    observed: &BTreeSet<RouteRecord>,
) -> Result<(), Report<RouteError>> {
    if expected == observed {
        return Ok(());
    }
    for expected_record in expected {
        let candidates = observed
            .iter()
            .filter(|record| {
                record.adapter == expected_record.adapter && record.path == expected_record.path
            })
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let observed_record = candidates[0];
            if expected_record == observed_record {
                continue;
            }
            let axis = if expected_record.methods != observed_record.methods {
                "method"
            } else if expected_record.shape != observed_record.shape {
                "shape"
            } else if expected_record.predicate != observed_record.predicate {
                "predicate"
            } else if expected_record.status != observed_record.status {
                "status"
            } else if expected_record.startup_router != observed_record.startup_router {
                "startup-router"
            } else {
                "route"
            };
            return Err(Report::new(RouteError::Drift { axis }));
        }
    }
    Err(Report::new(RouteError::Drift { axis: "route" }))
}

/// Extract the private Fastly, Axum, or Spin named-route collection.
///
/// # Errors
///
/// Returns an error if the named collection is absent or leaves the closed
/// struct/tuple, path-constant, and method-array grammar.
pub fn extract_named_routes(
    adapter: &str,
    source: &str,
) -> Result<BTreeSet<RouteRecord>, Report<RouteError>> {
    extract_named_routes_with_constants(adapter, source, &BTreeMap::new())
}

fn extract_named_routes_with_constants(
    adapter: &str,
    source: &str,
    authoritative_paths: &BTreeMap<String, String>,
) -> Result<BTreeSet<RouteRecord>, Report<RouteError>> {
    ensure_route_input_bound(&format!("{adapter} route source"), source)?;
    if !matches!(adapter, "fastly" | "axum" | "spin") {
        return Err(invalid(format!(
            "unsupported named-route adapter: {adapter}"
        )));
    }
    let file = syn::parse_file(source)
        .map_err(|error| invalid(format!("invalid {adapter} Rust: {error}")))?;
    let strings = string_constants(&file.items).with_authoritative(authoritative_paths);
    let methods = method_array_constants(&file.items)?;
    let array = if adapter == "fastly" {
        let mut arrays = Vec::new();
        for item in &file.items {
            let Item::Const(item) = item else { continue };
            if item.ident == "NAMED_ROUTES"
                && route_item_is_production(&item.attrs, "Fastly NAMED_ROUTES")?
            {
                arrays.push(
                    expression_array(&item.expr)
                        .ok_or_else(|| invalid("unsupported Fastly NAMED_ROUTES expression"))?,
                );
            }
        }
        if arrays.len() > 1 {
            return Err(invalid("duplicate Fastly NAMED_ROUTES collection"));
        }
        arrays.first().copied()
    } else {
        let symbol = if adapter == "axum" {
            "named_routes"
        } else {
            "named_fallback_paths"
        };
        let function = exact_top_function(&file, symbol)?;
        function
            .block
            .stmts
            .last()
            .and_then(|statement| match statement {
                Stmt::Expr(expression, _) => expression_array(expression),
                _ => None,
            })
    }
    .ok_or_else(|| invalid(format!("missing {adapter} named route collection")))?;

    let mut records = RouteAccumulator::default();
    for entry in &array.elems {
        let (path_expression, method_expression, handler_expression) = if adapter == "spin" {
            let Expr::Tuple(tuple) = entry else {
                return Err(invalid("unsupported Spin named route row"));
            };
            if tuple.elems.len() != 2 {
                return Err(invalid("Spin named route rows require path and methods"));
            }
            (&tuple.elems[0], &tuple.elems[1], None)
        } else {
            let Expr::Struct(row) = entry else {
                return Err(invalid(format!("unsupported {adapter} named route row")));
            };
            let path = struct_field(row, "path")
                .ok_or_else(|| invalid(format!("{adapter} route row is missing path")))?;
            let methods = struct_field(row, "primary_methods").ok_or_else(|| {
                invalid(format!("{adapter} route row is missing primary_methods"))
            })?;
            let handler = struct_field(row, "handler")
                .ok_or_else(|| invalid(format!("{adapter} route row is missing handler")))?;
            if row.rest.is_some() || row.fields.len() != 3 {
                return Err(invalid(format!("unsupported {adapter} named route fields")));
            }
            (path, methods, Some(handler))
        };
        let path = resolve_static_path(path_expression, &strings)?;
        if let Some(handler) = handler_expression {
            validate_named_handler(adapter, path_expression, &path, handler)?;
        }
        let route_methods = resolve_method_array(method_expression, &methods)?;
        let shape = if path.contains('{') {
            RouteShape::Template
        } else {
            RouteShape::Literal
        };
        for method in route_methods {
            let (predicate, status) = named_semantics(adapter, &path, &method);
            records.insert(&RouteRecord::new(
                adapter,
                &path,
                [&method],
                shape,
                predicate,
                status,
                false,
            ))?;
        }
    }
    Ok(records.finish())
}

fn validate_named_handler(
    adapter: &str,
    path_expression: &Expr,
    path: &str,
    expression: &Expr,
) -> Result<(), Report<RouteError>> {
    let path_symbol = match strip_parens(path_expression) {
        Expr::Path(value) => value.path.get_ident().map(ToString::to_string),
        _ => None,
    };
    let expected = match (adapter, path, path_symbol.as_deref()) {
        (_, _, Some("PAGE_BIDS_PATH" | "PAGE_BIDS_LEGACY_PATH")) => "PageBids",
        (_, "/.well-known/trusted-server.json", _) => "TrustedServerDiscovery",
        (_, "/verify-signature", _) => "VerifySignature",
        ("fastly", "/_ts/admin/keys/rotate", _) => "RotateKey",
        ("fastly", "/_ts/admin/keys/deactivate", _) => "DeactivateKey",
        ("fastly", "/_ts/admin/ec" | "/_ts/admin/ec/{id}", _) => "AdminEcLookup",
        ("axum", "/_ts/admin/keys/rotate" | "/_ts/admin/keys/deactivate", _) => "AdminNotSupported",
        ("axum", "/_ts/admin/ec" | "/_ts/admin/ec/{id}", _) => "AdminEcNotSupported",
        (_, "/_ts/admin/eids", _) => "AdminEidsLookup",
        (_, "/admin/keys/rotate" | "/admin/keys/deactivate", _) => "LegacyAdminDenied",
        ("fastly", "/_ts/api/v1/batch-sync", _) => "BatchSync",
        ("fastly", "/_ts/api/v1/identify", _) => "Identify",
        ("fastly", "/_ts/set-tester", _) => "SetTester",
        ("fastly", "/_ts/clear-tester", _) => "ClearTester",
        (_, "/auction", _) => "Auction",
        (_, "/_ts/page-bids" | "/__ts/page-bids", _) => "PageBids",
        (_, "/first-party/proxy", _) => "FirstPartyProxy",
        (_, "/first-party/click", _) => "FirstPartyClick",
        (_, "/first-party/sign", _) => "FirstPartySign",
        (_, "/first-party/proxy-rebuild", _) => "FirstPartyProxyRebuild",
        _ => {
            return Err(invalid(format!(
                "unknown {adapter} named route path: {path}"
            )));
        }
    };
    if !matches!(strip_parens(expression), Expr::Path(value)
        if value.path.segments.len() == 2
            && value.path.segments[0].ident == "NamedRouteHandler"
            && value.path.segments[1].ident == expected)
    {
        return Err(invalid(format!(
            "{adapter} named route handler differs for {path}"
        )));
    }
    Ok(())
}

fn expression_array(expression: &Expr) -> Option<&syn::ExprArray> {
    match expression {
        Expr::Array(array) => Some(array),
        Expr::Reference(reference) => expression_array(&reference.expr),
        Expr::Paren(paren) => expression_array(&paren.expr),
        _ => None,
    }
}

fn struct_field<'a>(row: &'a syn::ExprStruct, name: &str) -> Option<&'a Expr> {
    row.fields.iter().find_map(|field| {
        matches!(&field.member, syn::Member::Named(ident) if ident == name).then_some(&field.expr)
    })
}

fn resolve_static_path(
    expression: &Expr,
    constants: &RouteStringConstants,
) -> Result<String, Report<RouteError>> {
    match expression {
        Expr::Lit(literal) => match &literal.lit {
            Lit::Str(value) => Ok(value.value()),
            _ => Err(invalid("named route path must be a string")),
        },
        Expr::Path(path) => {
            let name = path
                .path
                .get_ident()
                .map(ToString::to_string)
                .ok_or_else(|| invalid("unsupported named route path"))?;
            constants.resolve(&name)
        }
        _ => Err(invalid("unsupported named route path expression")),
    }
}

fn resolve_method_array(
    expression: &Expr,
    constants: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>, Report<RouteError>> {
    if let Some(array) = expression_array(expression) {
        return parse_method_array(array);
    }
    let Expr::Path(path) = expression else {
        return Err(invalid("unsupported named route method expression"));
    };
    let name = path
        .path
        .get_ident()
        .map(ToString::to_string)
        .ok_or_else(|| invalid("unsupported named route method constant"))?;
    constants
        .get(&name)
        .cloned()
        .ok_or_else(|| invalid(format!("unknown named route method constant: {name}")))
}

fn method_array_constants(
    items: &[Item],
) -> Result<BTreeMap<String, Vec<String>>, Report<RouteError>> {
    let mut constants = BTreeMap::new();
    for item in items {
        let (name, expression, attributes) = match item {
            Item::Const(item) => (item.ident.to_string(), &*item.expr, item.attrs.as_slice()),
            Item::Static(item) => (item.ident.to_string(), &*item.expr, item.attrs.as_slice()),
            _ => continue,
        };
        if let Some(array) = expression_array(expression)
            && (array.elems.is_empty()
                || matches!(array.elems.first(), Some(Expr::Path(path)) if path.path.segments.first().is_some_and(|segment| segment.ident == "Method")))
        {
            if !route_item_is_production(attributes, &format!("method array `{name}`"))? {
                continue;
            }
            if constants
                .insert(name.clone(), parse_method_array(array)?)
                .is_some()
            {
                return Err(invalid(format!("duplicate method array: {name}")));
            }
        }
    }
    Ok(constants)
}

fn parse_method_array(array: &syn::ExprArray) -> Result<Vec<String>, Report<RouteError>> {
    let methods = array
        .elems
        .iter()
        .map(|expression| {
            let Expr::Path(path) = expression else {
                return Err(invalid("named route method array contains a non-path"));
            };
            let segments = path.path.segments.iter().collect::<Vec<_>>();
            if segments.len() != 2 || segments[0].ident != "Method" {
                return Err(invalid("named route method is not Method::<VERB>"));
            }
            Ok(segments[1].ident.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let count = methods.len();
    if methods.iter().collect::<BTreeSet<_>>().len() != count {
        return Err(invalid("duplicate named route method"));
    }
    Ok(methods)
}

fn named_semantics(adapter: &str, path: &str, method: &str) -> (&'static str, RouteStatus) {
    if path.starts_with("/admin/keys/") || (path.ends_with("page-bids") && method == "OPTIONS") {
        ("always", RouteStatus::Guarded)
    } else if adapter != "fastly"
        && (path.starts_with("/_ts/admin/keys/") || path.starts_with("/_ts/admin/ec"))
    {
        ("always", RouteStatus::Unsupported)
    } else if path == "/_ts/set-tester" || path == "/_ts/clear-tester" {
        ("settings.tester_cookie.enabled", RouteStatus::Real)
    } else {
        ("always", RouteStatus::Real)
    }
}

/// Extract Cloudflare route registrations from the supported router grammar.
///
/// # Errors
///
/// Returns an error for invalid Rust, a missing builder, or an unsupported
/// route-affecting construct.
pub fn extract_cloudflare_routes(
    source: &str,
) -> Result<BTreeSet<RouteRecord>, Report<RouteError>> {
    extract_cloudflare_routes_with_constants(source, &BTreeMap::new())
}

fn extract_cloudflare_routes_with_constants(
    source: &str,
    authoritative_paths: &BTreeMap<String, String>,
) -> Result<BTreeSet<RouteRecord>, Report<RouteError>> {
    ensure_route_input_bound("Cloudflare route source", source)?;
    let file = syn::parse_file(source)
        .map_err(|error| invalid(format!("invalid Cloudflare Rust: {error}")))?;
    let strings = string_constants(&file.items).with_authoritative(authoritative_paths);
    let method_lists = method_list_functions(&file.items)?;
    let build_router = &exact_top_function(&file, "build_router")?.block;
    if cloudflare_builder_occurrences(build_router) != 1 {
        return Err(invalid(
            "Cloudflare build_router must contain exactly one raw RouterService::builder call",
        ));
    }
    let mut parser = CloudflareParser {
        strings: &strings,
        method_lists: &method_lists,
        values: BTreeMap::new(),
        routes: RouteAccumulator::default(),
        authority: None,
    };
    parser.parse_block(build_router)?;
    let authority = parser
        .authority
        .as_deref()
        .ok_or_else(|| invalid("Cloudflare build_router has no builder authority"))?;
    if !block_returns_build(build_router, authority) {
        return Err(invalid(
            "Cloudflare build_router must return its builder authority",
        ));
    }
    if parser.routes.identities.is_empty() {
        return Err(invalid(
            "Cloudflare build_router contains no recognized routes",
        ));
    }
    Ok(parser.routes.finish())
}

struct CloudflareParser<'a> {
    strings: &'a RouteStringConstants,
    method_lists: &'a BTreeMap<String, Vec<String>>,
    values: BTreeMap<String, String>,
    routes: RouteAccumulator,
    authority: Option<String>,
}

impl CloudflareParser<'_> {
    fn parse_block(&mut self, block: &Block) -> Result<(), Report<RouteError>> {
        for statement in &block.stmts {
            match statement {
                Stmt::Local(local) => {
                    let Some(init) = &local.init else { continue };
                    let binding = match &local.pat {
                        Pat::Ident(binding) => Some(binding.ident.to_string()),
                        _ => None,
                    };
                    if binding.as_deref().is_some_and(|binding| {
                        self.authority
                            .as_deref()
                            .is_some_and(|authority| authority == binding)
                    }) {
                        return Err(invalid("Cloudflare router authority cannot be shadowed"));
                    }
                    if is_router_builder_chain(&init.expr) {
                        let Pat::Ident(ident) = &local.pat else {
                            return Err(invalid(
                                "Cloudflare builder authority must use an identifier binding",
                            ));
                        };
                        if self.authority.replace(ident.ident.to_string()).is_some() {
                            return Err(invalid("multiple Cloudflare router builders"));
                        }
                        self.parse_router_chain(&init.expr)?;
                    } else if expression_builder_count(&init.expr) != 0 {
                        return Err(invalid("unsupported hidden Cloudflare router builder"));
                    } else if matches!(strip_parens(&init.expr), Expr::Macro(_)) {
                        return Err(invalid("unsupported Cloudflare macro local initializer"));
                    } else if expression_route_method_count(&init.expr) != 0 {
                        return Err(invalid(
                            "unsupported competing Cloudflare route initializer",
                        ));
                    } else if self
                        .authority
                        .as_deref()
                        .is_some_and(|authority| expression_references_ident(&init.expr, authority))
                    {
                        return Err(invalid(
                            "unsupported Cloudflare route-affecting local initializer",
                        ));
                    }
                }
                Stmt::Item(Item::Fn(function)) if function.sig.ident == "dispatch" => {
                    if block_builder_count(&function.block) != 0 {
                        return Err(invalid("unsupported hidden Cloudflare router builder"));
                    }
                }
                Stmt::Item(_) => {
                    return Err(invalid("unsupported Cloudflare nested item"));
                }
                Stmt::Macro(_) => {
                    return Err(invalid("unsupported Cloudflare statement macro"));
                }
                Stmt::Expr(expression, _) => self.parse_expression(expression)?,
            }
        }
        Ok(())
    }

    fn parse_expression(&mut self, expression: &Expr) -> Result<(), Report<RouteError>> {
        let authority = self.authority.as_deref().unwrap_or("").to_owned();
        match expression {
            Expr::MethodCall(call)
                if call.method == "build"
                    && self.authority.is_none()
                    && is_router_builder_chain(&call.receiver) =>
            {
                self.authority = Some("<returned-chain>".to_owned());
                self.parse_router_chain(expression)
            }
            Expr::Assign(assign) if is_ident(&assign.left, &authority) => {
                self.parse_router_chain(&assign.right)
            }
            Expr::Assign(assign) if expression_route_method_count(&assign.right) != 0 => Err(
                invalid("Cloudflare route assignment does not update the returned authority"),
            ),
            Expr::ForLoop(loop_expression) => self.parse_loop(loop_expression),
            Expr::Block(block) => self.parse_block(&block.block),
            Expr::MethodCall(call)
                if call.method == "build" && is_ident(&call.receiver, &authority) =>
            {
                Ok(())
            }
            Expr::Call(call) => Err(invalid(format!(
                "unsupported Cloudflare builder call: {}",
                compact_expr(&call.func)
            ))),
            Expr::Macro(_) => Err(invalid("unsupported Cloudflare expression macro")),
            Expr::If(_)
            | Expr::Match(_)
            | Expr::While(_)
            | Expr::Loop(_)
            | Expr::Return(_)
            | Expr::Break(_)
            | Expr::Continue(_) => Err(invalid(
                "unsupported Cloudflare control flow in build_router",
            )),
            _ if !authority.is_empty() && expression_references_ident(expression, &authority) => {
                Err(invalid(
                    "unsupported Cloudflare expression that references the router",
                ))
            }
            _ => Ok(()),
        }
    }

    fn parse_loop(&mut self, expression: &syn::ExprForLoop) -> Result<(), Report<RouteError>> {
        let Pat::Ident(binding) = &*expression.pat else {
            return Err(invalid("unsupported Cloudflare loop binding"));
        };
        let name = binding.ident.to_string();
        let values = self.resolve_loop_values(&expression.expr)?;
        let previous = self.values.get(&name).cloned();
        for value in values {
            self.values.insert(name.clone(), value);
            self.parse_block(&expression.body)?;
        }
        if let Some(previous) = previous {
            self.values.insert(name, previous);
        } else {
            self.values.remove(&name);
        }
        Ok(())
    }

    fn resolve_loop_values(&self, expression: &Expr) -> Result<Vec<String>, Report<RouteError>> {
        match expression {
            Expr::Array(array) => array
                .elems
                .iter()
                .map(|value| self.resolve_path(value))
                .collect(),
            Expr::Call(call) => {
                let Expr::Path(path) = &*call.func else {
                    return Err(invalid("unsupported Cloudflare loop iterator"));
                };
                let name = path
                    .path
                    .get_ident()
                    .map(ToString::to_string)
                    .ok_or_else(|| invalid("unsupported Cloudflare loop function"))?;
                self.method_lists
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| invalid(format!("unsupported Cloudflare loop function: {name}")))
            }
            _ => Err(invalid("unsupported Cloudflare loop iterator")),
        }
    }

    fn parse_router_chain(&mut self, expression: &Expr) -> Result<(), Report<RouteError>> {
        if let Expr::Call(call) = expression
            && exact_route_call_path(call, &["RouterService", "builder"])
            && call.args.is_empty()
        {
            return Ok(());
        }
        let Expr::MethodCall(call) = expression else {
            if self
                .authority
                .as_deref()
                .is_some_and(|authority| is_ident(expression, authority))
            {
                return Ok(());
            }
            return Err(invalid("unsupported Cloudflare router assignment"));
        };
        if self
            .authority
            .as_deref()
            .is_none_or(|authority| !is_ident(&call.receiver, authority))
        {
            self.parse_router_chain(&call.receiver)?;
        }
        let method = call.method.to_string();
        match method.as_str() {
            "middleware" => Ok(()),
            "get" | "post" | "head" | "options" | "put" | "patch" | "delete" => {
                let path = call
                    .args
                    .first()
                    .ok_or_else(|| invalid("Cloudflare route helper is missing its path"))?;
                if call.args.len() != 2 {
                    return Err(invalid("Cloudflare route helper has unsupported arguments"));
                }
                let path = self.resolve_path(path)?;
                let method = method.to_ascii_uppercase();
                validate_cloudflare_handler(
                    &path,
                    &method,
                    call.args
                        .iter()
                        .nth(1)
                        .expect("route helper should have a handler"),
                )?;
                self.add(&path, method)?;
                Ok(())
            }
            "route" => {
                let mut args = call.args.iter();
                let path = args
                    .next()
                    .ok_or_else(|| invalid("Cloudflare route call is missing its path"))?;
                let method = args
                    .next()
                    .ok_or_else(|| invalid("Cloudflare route call is missing its method"))?;
                if call.args.len() != 3 {
                    return Err(invalid("Cloudflare route call has unsupported arguments"));
                }
                let path = self.resolve_path(path)?;
                let method = self.resolve_method(method)?;
                validate_cloudflare_handler(
                    &path,
                    &method,
                    call.args
                        .iter()
                        .nth(2)
                        .expect("route call should have a handler"),
                )?;
                self.add(&path, method)?;
                Ok(())
            }
            "build" | "builder" => Ok(()),
            _ => Err(invalid(format!(
                "unsupported Cloudflare router method: {method}"
            ))),
        }
    }

    fn resolve_path(&self, expression: &Expr) -> Result<String, Report<RouteError>> {
        match expression {
            Expr::Lit(literal) => match &literal.lit {
                Lit::Str(value) => Ok(value.value()),
                _ => Err(invalid("Cloudflare route path must be a string")),
            },
            Expr::Path(path) => {
                let name = path
                    .path
                    .get_ident()
                    .map(ToString::to_string)
                    .ok_or_else(|| invalid("unsupported Cloudflare route path"))?;
                if let Some(value) = self.values.get(&name) {
                    Ok(value.clone())
                } else {
                    self.strings.resolve(&name)
                }
            }
            _ => Err(invalid("unsupported Cloudflare route path expression")),
        }
    }

    fn resolve_method(&self, expression: &Expr) -> Result<String, Report<RouteError>> {
        match expression {
            Expr::Path(path) => {
                let segments = path.path.segments.iter().collect::<Vec<_>>();
                if segments.len() == 2 && segments[0].ident == "Method" {
                    return Ok(segments[1].ident.to_string());
                }
                let name = path
                    .path
                    .get_ident()
                    .map(ToString::to_string)
                    .ok_or_else(|| invalid("unsupported Cloudflare route method"))?;
                self.values
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| invalid(format!("unknown Cloudflare route method: {name}")))
            }
            Expr::MethodCall(call) if call.method == "clone" => self.resolve_method(&call.receiver),
            _ => Err(invalid("unsupported Cloudflare route method expression")),
        }
    }

    fn add(&mut self, path: &str, method: String) -> Result<(), Report<RouteError>> {
        let shape = if path.contains('{') {
            RouteShape::Template
        } else {
            RouteShape::Literal
        };
        let (predicate, status) = cloudflare_semantics(path, &method);
        self.routes.insert(&RouteRecord::new(
            "cloudflare",
            path,
            [method],
            shape,
            predicate,
            status,
            false,
        ))
    }
}

fn is_router_builder_chain(expression: &Expr) -> bool {
    match expression {
        Expr::Call(call) => {
            exact_route_call_path(call, &["RouterService", "builder"]) && call.args.is_empty()
        }
        Expr::MethodCall(call) => is_router_builder_chain(&call.receiver),
        Expr::Paren(paren) => is_router_builder_chain(&paren.expr),
        _ => false,
    }
}

fn block_returns_build(block: &Block, authority: &str) -> bool {
    let Some(Stmt::Expr(expression, None)) = block.stmts.last() else {
        return false;
    };
    match expression {
        Expr::Block(block) => block_returns_build(&block.block, authority),
        Expr::MethodCall(call) => {
            call.method == "build"
                && call.args.is_empty()
                && (is_ident(&call.receiver, authority)
                    || (authority == "<returned-chain>" && is_router_builder_chain(&call.receiver)))
        }
        _ => false,
    }
}

fn expression_builder_count(expression: &Expr) -> usize {
    struct Counter(usize);
    impl<'ast> Visit<'ast> for Counter {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if exact_route_call_path(call, &["RouterService", "builder"]) {
                self.0 += 1;
            }
            visit::visit_expr_call(self, call);
        }
    }
    let mut counter = Counter(0);
    counter.visit_expr(expression);
    counter.0
}

fn expression_route_method_count(expression: &Expr) -> usize {
    struct Counter(usize);
    impl<'ast> Visit<'ast> for Counter {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            self.0 += usize::from(route_method_call(call));
            visit::visit_expr_method_call(self, call);
        }
    }
    let mut counter = Counter(0);
    counter.visit_expr(expression);
    counter.0
}

fn block_builder_count(block: &Block) -> usize {
    struct Counter(usize);
    impl<'ast> Visit<'ast> for Counter {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if exact_route_call_path(call, &["RouterService", "builder"]) {
                self.0 += 1;
            }
            visit::visit_expr_call(self, call);
        }

        fn visit_macro(&mut self, item: &'ast syn::Macro) {
            let tokens = item.tokens.to_string();
            if tokens.contains("RouterService") && tokens.contains("builder") {
                self.0 += 1;
            }
            visit::visit_macro(self, item);
        }
    }
    let mut counter = Counter(0);
    counter.visit_block(block);
    counter.0
}

fn cloudflare_builder_occurrences(block: &Block) -> usize {
    block_builder_count(block)
}

fn validate_cloudflare_handler(
    path: &str,
    method: &str,
    expression: &Expr,
) -> Result<(), Report<RouteError>> {
    let expected: &[&str] = match (path, method) {
        ("/.well-known/trusted-server.json", "GET") => {
            &["handle_trusted_server_discovery", "discovery"]
        }
        ("/verify-signature", "POST") => &["handle_verify_signature", "verify"],
        ("/_ts/admin/keys/rotate" | "/_ts/admin/keys/deactivate", "POST") => &[
            "admin_key_management_not_supported",
            "admin_not_supported_handler",
        ],
        ("/_ts/admin/ec" | "/_ts/admin/ec/{id}", "GET") => &[
            "admin_ec_lookup_not_supported",
            "admin_ec_not_supported_handler",
        ],
        ("/_ts/admin/eids", "GET") => &["handle_admin_eids_lookup", "admin_eids_handler"],
        ("/auction", "POST") => &["handle_auction", "auction"],
        ("/_ts/page-bids" | "/__ts/page-bids", "GET") => {
            &["handle_page_bids", "page_bids", "page_bids_handler"]
        }
        ("/_ts/page-bids" | "/__ts/page-bids", "OPTIONS") => &[
            "page_bids_preflight_denied",
            "page_bids_preflight",
            "page_bids_options_handler",
        ],
        ("/first-party/proxy", "GET") => &["handle_first_party_proxy", "fp_proxy_handler"],
        ("/first-party/click", "GET") => &["handle_first_party_click", "fp_click_handler"],
        ("/first-party/sign", "GET" | "POST") => &[
            "handle_first_party_proxy_sign",
            "fp_sign_handler",
            "fp_sign_post_handler",
        ],
        ("/first-party/proxy-rebuild", "GET" | "POST") => &[
            "handle_first_party_proxy_rebuild",
            "fp_rebuild_handler",
            "fp_rebuild_post_handler",
        ],
        ("/admin/keys/rotate" | "/admin/keys/deactivate", _) => {
            &["legacy_admin_alias_denied", "legacy_admin_deny"]
        }
        ("/" | "/{*rest}", _) => &["dispatch", "fallback"],
        _ => return Ok(()),
    };
    let Some(actual) = terminal_handler_authority(expression) else {
        return Err(invalid(format!(
            "Cloudflare handler is not bound for {method} {path}"
        )));
    };
    if !expected.contains(&actual.as_str()) {
        return Err(invalid(format!(
            "Cloudflare handler differs for {method} {path}"
        )));
    }
    if actual == expected[0]
        && !exact_portable_handler_expression("Cloudflare", expected[0], expression)
    {
        return Err(invalid(format!(
            "Cloudflare handler arguments differ for {method} {path}"
        )));
    }
    Ok(())
}

fn terminal_handler_authority(expression: &Expr) -> Option<String> {
    match strip_parens(expression) {
        Expr::Path(path) => unqualified_path_name(path, false),
        Expr::MethodCall(call) if call.method == "clone" && call.args.is_empty() => {
            terminal_handler_authority(&call.receiver)
        }
        Expr::Call(call) => {
            let path = match call.func.as_ref() {
                Expr::Path(path) => path,
                _ => return None,
            };
            let wrapper = path.path.segments.len() == 1
                && matches!(
                    path.path.segments[0].ident.to_string().as_str(),
                    "Ok" | "Some"
                );
            let name = unqualified_path_name(path, wrapper)?;
            if name == "make_handler" || name == "Ok" {
                terminal_handler_authority(call.args.last()?)
            } else {
                Some(name)
            }
        }
        Expr::Closure(closure) => terminal_handler_authority(&closure.body),
        Expr::Async(value) => terminal_block_authority(&value.block),
        Expr::Block(value) => terminal_block_authority(&value.block),
        Expr::Await(value) => terminal_handler_authority(&value.base),
        Expr::Try(value) => terminal_handler_authority(&value.expr),
        _ => None,
    }
}

fn unqualified_path_name(path: &syn::ExprPath, allow_arguments: bool) -> Option<String> {
    if path.qself.is_some()
        || path.path.leading_colon.is_some()
        || path.path.segments.len() != 1
        || (!allow_arguments
            && !matches!(path.path.segments[0].arguments, syn::PathArguments::None))
    {
        return None;
    }
    Some(path.path.segments[0].ident.to_string())
}

fn terminal_block_authority(block: &Block) -> Option<String> {
    let Stmt::Expr(expression, None) = block.stmts.last()? else {
        return None;
    };
    terminal_handler_authority(expression)
}

fn validate_spin_named_handlers(
    file: &syn::File,
    authoritative_paths: &BTreeMap<String, String>,
) -> Result<(), Report<RouteError>> {
    struct SpinRoutes<'a> {
        strings: &'a RouteStringConstants,
        page_bids_path: &'a str,
        page_bids_legacy_path: &'a str,
        seen: BTreeSet<(String, String)>,
        error: Option<Report<RouteError>>,
    }
    impl<'ast> Visit<'ast> for SpinRoutes<'_> {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if self.error.is_some() {
                return;
            }
            let method = call.method.to_string();
            if method == "route"
                && let Some(path_expression) = call.args.first()
                && let Ok(path) = resolve_static_path(path_expression, self.strings)
                && matches!(
                    path.as_str(),
                    "/admin/keys/rotate" | "/admin/keys/deactivate"
                )
                && call
                    .args
                    .iter()
                    .nth(2)
                    .and_then(terminal_handler_authority)
                    .as_deref()
                    != Some("legacy_admin_deny")
            {
                self.error = Some(invalid(format!(
                    "Spin legacy admin handler differs for {path}"
                )));
                return;
            }
            let verb = match method.as_str() {
                "get" | "post" => Some(method.to_ascii_uppercase()),
                "route" => call
                    .args
                    .iter()
                    .nth(1)
                    .and_then(|value| match strip_parens(value) {
                        Expr::Path(path) => path
                            .path
                            .segments
                            .last()
                            .map(|segment| segment.ident.to_string()),
                        _ => None,
                    }),
                _ => None,
            };
            if let Some(verb) = verb
                && let Some(path_expression) = call.args.first()
                && let Ok(path) = resolve_static_path(path_expression, self.strings)
                && spin_expected_handler(
                    &path,
                    &verb,
                    self.page_bids_path,
                    self.page_bids_legacy_path,
                )
                .is_some()
            {
                let handler_index = if method == "route" { 2 } else { 1 };
                let valid = call.args.iter().nth(handler_index).is_some_and(|handler| {
                    terminal_handler_authority(handler).is_some_and(|actual| {
                        spin_expected_handler(
                            &path,
                            &verb,
                            self.page_bids_path,
                            self.page_bids_legacy_path,
                        )
                        .is_some_and(|expected| expected.contains(&actual.as_str()))
                    })
                });
                if !valid || !self.seen.insert((path.clone(), verb.clone())) {
                    self.error = Some(invalid(format!("Spin handler differs for {verb} {path}")));
                    return;
                }
            }
            visit::visit_expr_method_call(self, call);
        }
    }
    let strings = string_constants(&file.items).with_authoritative(authoritative_paths);
    let page_bids_path = strings.resolve("PAGE_BIDS_PATH")?;
    let page_bids_legacy_path = strings.resolve("PAGE_BIDS_LEGACY_PATH")?;
    let build_router = exact_top_function(file, "build_router")?;
    validate_spin_discovery_handler(build_router)?;
    validate_local_handler_behaviors(
        &build_router.block,
        "Spin",
        &[
            ("discovery_handler", "handle_trusted_server_discovery"),
            ("verify_handler", "handle_verify_signature"),
            (
                "admin_not_supported_handler",
                "admin_key_management_not_supported",
            ),
            (
                "admin_ec_not_supported_handler",
                "admin_ec_lookup_not_supported",
            ),
            ("admin_eids_handler", "handle_admin_eids_lookup"),
            ("auction_handler", "handle_auction"),
            ("page_bids_handler", "handle_page_bids"),
            ("page_bids_options_handler", "page_bids_preflight_denied"),
            ("fp_proxy_handler", "handle_first_party_proxy"),
            ("fp_click_handler", "handle_first_party_click"),
            ("fp_sign_handler", "handle_first_party_proxy_sign"),
            ("fp_sign_post_handler", "handle_first_party_proxy_sign"),
            ("fp_rebuild_handler", "handle_first_party_proxy_rebuild"),
            (
                "fp_rebuild_post_handler",
                "handle_first_party_proxy_rebuild",
            ),
            ("legacy_admin_deny", "legacy_admin_alias_denied"),
            ("fallback", "dispatch"),
        ],
        &[],
    )?;
    let mut routes = SpinRoutes {
        strings: &strings,
        page_bids_path: &page_bids_path,
        page_bids_legacy_path: &page_bids_legacy_path,
        seen: BTreeSet::new(),
        error: None,
    };
    routes.visit_block(&build_router.block);
    if let Some(error) = routes.error {
        return Err(error);
    }
    let mut expected = [
        ("/.well-known/trusted-server.json", "GET"),
        ("/verify-signature", "POST"),
        ("/_ts/admin/keys/rotate", "POST"),
        ("/_ts/admin/keys/deactivate", "POST"),
        ("/_ts/admin/ec", "GET"),
        ("/_ts/admin/ec/{id}", "GET"),
        ("/_ts/admin/eids", "GET"),
        ("/auction", "POST"),
        ("/first-party/proxy", "GET"),
        ("/first-party/click", "GET"),
        ("/first-party/sign", "GET"),
        ("/first-party/sign", "POST"),
        ("/first-party/proxy-rebuild", "GET"),
        ("/first-party/proxy-rebuild", "POST"),
    ]
    .into_iter()
    .map(|(path, method)| (path.to_owned(), method.to_owned()))
    .collect::<BTreeSet<_>>();
    for path in [&page_bids_path, &page_bids_legacy_path] {
        for method in ["GET", "OPTIONS"] {
            expected.insert((path.clone(), method.to_owned()));
        }
    }
    if routes.seen != expected {
        return Err(invalid("Spin named route handler coverage differs"));
    }
    Ok(())
}

fn validate_spin_discovery_handler(build_router: &syn::ItemFn) -> Result<(), Report<RouteError>> {
    let [Stmt::Expr(Expr::Block(active), None)] = build_router.block.stmts.as_slice() else {
        return Err(invalid("Spin discovery handler lexical scope differs"));
    };
    let handlers = active
        .block
        .stmts
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Local(local)
                if matches!(&local.pat, Pat::Ident(binding)
                    if binding.ident == "discovery_handler" && binding.subpat.is_none()) =>
            {
                local.init.as_ref().map(|init| &*init.expr)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let [handler] = handlers.as_slice() else {
        return Err(invalid("Spin discovery handler ownership differs"));
    };
    let Expr::Closure(outer) = strip_parens(handler) else {
        return Err(invalid("Spin discovery handler shape differs"));
    };
    let Expr::Block(body) = strip_parens(&outer.body) else {
        return Err(invalid("Spin discovery handler shape differs"));
    };
    let [Stmt::Local(clone), Stmt::Expr(Expr::Async(future), None)] = body.block.stmts.as_slice()
    else {
        return Err(invalid("Spin discovery handler must have one live future"));
    };
    let exact_clone = matches!(&clone.pat, Pat::Ident(binding)
        if binding.ident == "s" && binding.subpat.is_none())
        && clone.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if exact_route_call_path(call, &["Arc", "clone"])
                    && call.args.len() == 1
                    && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                        if reference.mutability.is_none() && is_ident(&reference.expr, "s")))
        });
    let [
        Stmt::Local(services),
        Stmt::Local(request),
        Stmt::Expr(Expr::Call(ok), None),
    ] = future.block.stmts.as_slice()
    else {
        return Err(invalid(
            "Spin discovery handler must have one terminal response",
        ));
    };
    let exact_services = matches!(&services.pat, Pat::Ident(binding)
        if binding.ident == "services" && binding.subpat.is_none())
        && services.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::Call(call)
                if call_named_route(call, "build_runtime_services")
                    && call.args.len() == 1
                    && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                        if reference.mutability.is_none() && is_ident(&reference.expr, "ctx")))
        });
    let exact_request = matches!(&request.pat, Pat::Ident(binding)
        if binding.ident == "req" && binding.subpat.is_none())
        && request.init.as_ref().is_some_and(|init| {
            matches!(strip_parens(&init.expr), Expr::MethodCall(call)
                if call.method == "into_request" && call.args.is_empty()
                    && is_ident(&call.receiver, "ctx"))
        });
    let exact_response = call_named_route(ok, "Ok")
        && ok.args.len() == 1
        && ok.args.first().is_some_and(|argument| {
            matches!(strip_parens(argument), Expr::MethodCall(unwrap)
                if unwrap.method == "unwrap_or_else" && unwrap.args.len() == 1
                    && matches!(strip_parens(&unwrap.receiver), Expr::Call(call)
                        if call_named_route(call, "handle_trusted_server_discovery")
                            && call.args.len() == 3
                            && call.args.first().is_some_and(|value| {
                                matches!(strip_parens(value), Expr::Reference(reference)
                                    if reference.mutability.is_none()
                                        && exact_field_receiver(&reference.expr, "s", "settings"))
                            })
                            && call.args.iter().nth(1).is_some_and(|value| {
                                matches!(strip_parens(value), Expr::Reference(reference)
                                    if reference.mutability.is_none()
                                        && is_ident(&reference.expr, "services"))
                            })
                            && call.args.iter().nth(2).is_some_and(|value| is_ident(value, "req"))))
        });
    if !exact_clone || !exact_services || !exact_request || !exact_response {
        return Err(invalid("Spin discovery handler call differs"));
    }
    Ok(())
}

fn validate_cloudflare_local_handlers(file: &syn::File) -> Result<(), Report<RouteError>> {
    let build_router = exact_top_function(file, "build_router")?;
    let optional = [
        ("discovery", "handle_trusted_server_discovery"),
        ("verify", "handle_verify_signature"),
        (
            "admin_not_supported_handler",
            "admin_key_management_not_supported",
        ),
        (
            "admin_ec_not_supported_handler",
            "admin_ec_lookup_not_supported",
        ),
        ("admin_eids_handler", "handle_admin_eids_lookup"),
        ("auction", "handle_auction"),
        ("page_bids_handler", "handle_page_bids"),
        ("page_bids_options_handler", "page_bids_preflight_denied"),
        ("fp_proxy_handler", "handle_first_party_proxy"),
        ("fp_click_handler", "handle_first_party_click"),
        ("fp_sign_handler", "handle_first_party_proxy_sign"),
        ("fp_sign_post_handler", "handle_first_party_proxy_sign"),
        ("fp_rebuild_handler", "handle_first_party_proxy_rebuild"),
        (
            "fp_rebuild_post_handler",
            "handle_first_party_proxy_rebuild",
        ),
    ];
    let registered = cloudflare_registered_handler_aliases(&build_router.block, &optional);
    let optional = optional
        .into_iter()
        .filter(|(name, _)| registered.contains(*name))
        .collect::<Vec<_>>();
    validate_local_handler_behaviors(
        &build_router.block,
        "Cloudflare",
        &[
            ("page_bids", "handle_page_bids"),
            ("page_bids_preflight", "page_bids_preflight_denied"),
            ("legacy_admin_deny", "legacy_admin_alias_denied"),
            ("fallback", "dispatch"),
        ],
        &optional,
    )
}

fn cloudflare_registered_handler_aliases(
    block: &Block,
    candidates: &[(&str, &str)],
) -> BTreeSet<String> {
    struct Registrations<'a> {
        candidates: &'a [(&'a str, &'a str)],
        aliases: BTreeSet<String>,
    }
    impl<'ast> Visit<'ast> for Registrations<'_> {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let index = match call.method.to_string().as_str() {
                "get" | "post" if call.args.len() == 2 => Some(1),
                "route" if call.args.len() == 3 => Some(2),
                _ => None,
            };
            if let Some(alias) = index
                .and_then(|index| call.args.iter().nth(index))
                .and_then(terminal_handler_authority)
                && self.candidates.iter().any(|(name, _)| *name == alias)
            {
                self.aliases.insert(alias);
            }
            visit::visit_expr_method_call(self, call);
        }
    }
    let mut registrations = Registrations {
        candidates,
        aliases: BTreeSet::new(),
    };
    registrations.visit_block(block);
    registrations.aliases
}

fn validate_cloudflare_discovery_handler(file: &syn::File) -> Result<(), Report<RouteError>> {
    struct Discovery<'a>(Vec<&'a Expr>);
    impl<'ast> Visit<'ast> for Discovery<'ast> {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if call.method == "get"
                && call.args.len() == 2
                && call.args.first().and_then(literal_string_value).as_deref()
                    == Some("/.well-known/trusted-server.json")
                && let Some(handler) = call.args.iter().nth(1)
            {
                self.0.push(handler);
            }
            visit::visit_expr_method_call(self, call);
        }
    }
    let build_router = exact_top_function(file, "build_router")?;
    let mut discovery = Discovery(Vec::new());
    discovery.visit_block(&build_router.block);
    let [handler] = discovery.0.as_slice() else {
        return Err(invalid(
            "Cloudflare discovery must have one live registration",
        ));
    };
    let Expr::Call(make) = strip_parens(handler) else {
        return Err(invalid("Cloudflare discovery handler shape differs"));
    };
    if !call_named_route(make, "make_handler") || make.args.len() != 2 {
        return Err(invalid("Cloudflare discovery handler shape differs"));
    }
    let exact_state = make.args.first().is_some_and(|value| {
        matches!(strip_parens(value), Expr::Call(clone)
            if exact_route_call_path(clone, &["Arc", "clone"])
                && clone.args.len() == 1
                && matches!(clone.args.first().map(strip_parens), Some(Expr::Reference(reference))
                    if reference.mutability.is_none() && is_ident(&reference.expr, "state")))
    });
    let exact_closure = make.args.iter().nth(1).is_some_and(|value| {
        let Expr::Closure(closure) = strip_parens(value) else {
            return false;
        };
        let inputs = closure
            .inputs
            .iter()
            .map(|pattern| match pattern {
                Pat::Ident(binding) if binding.subpat.is_none() => Some(binding.ident.to_string()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>();
        let Expr::Async(future) = strip_parens(&closure.body) else {
            return false;
        };
        let [Stmt::Expr(Expr::Call(call), None)] = future.block.stmts.as_slice() else {
            return false;
        };
        inputs.as_deref() == Some(&["s".to_owned(), "services".to_owned(), "req".to_owned()])
            && call_named_route(call, "handle_trusted_server_discovery")
            && call.args.len() == 3
            && call.args.first().is_some_and(|argument| {
                matches!(strip_parens(argument), Expr::Reference(reference)
                    if reference.mutability.is_none()
                        && exact_field_receiver(&reference.expr, "s", "settings"))
            })
            && call.args.iter().nth(1).is_some_and(|argument| {
                matches!(strip_parens(argument), Expr::Reference(reference)
                    if reference.mutability.is_none() && is_ident(&reference.expr, "services"))
            })
            && call
                .args
                .iter()
                .nth(2)
                .is_some_and(|argument| is_ident(argument, "req"))
    });
    if !exact_state || !exact_closure {
        return Err(invalid("Cloudflare discovery handler call differs"));
    }
    Ok(())
}

fn exact_local_clone_target(expression: &Expr) -> Option<String> {
    let Expr::MethodCall(clone) = strip_parens(expression) else {
        return None;
    };
    (clone.method == "clone" && clone.args.is_empty())
        .then(|| match strip_parens(&clone.receiver) {
            Expr::Path(path) => unqualified_path_name(path, false),
            _ => None,
        })
        .flatten()
}

fn exact_portable_handler_expression(adapter: &str, behavior: &str, expression: &Expr) -> bool {
    terminal_behavior_name(expression).as_deref() == Some(behavior)
        && terminal_behavior_call(expression, behavior)
            .is_some_and(|call| exact_portable_handler_call(adapter, behavior, call))
        && match adapter {
            "Cloudflare" => exact_cloudflare_handler_wrapper(behavior, expression),
            "Spin" => exact_spin_handler_wrapper(behavior, expression),
            _ => false,
        }
}

fn exact_portable_handler_call(adapter: &str, behavior: &str, call: &syn::ExprCall) -> bool {
    match behavior {
        "handle_trusted_server_discovery"
        | "handle_verify_signature"
        | "handle_first_party_proxy"
        | "handle_first_party_click"
        | "handle_first_party_proxy_sign"
        | "handle_first_party_proxy_rebuild" => {
            call.args.len() == 3
                && exact_reference_field(call.args.first(), "s", "settings")
                && exact_reference(call.args.iter().nth(1), "services")
                && call
                    .args
                    .iter()
                    .nth(2)
                    .is_some_and(|argument| is_ident(argument, "req"))
        }
        "admin_key_management_not_supported"
        | "admin_ec_lookup_not_supported"
        | "page_bids_preflight_denied"
        | "legacy_admin_alias_denied" => call.args.is_empty(),
        "handle_admin_eids_lookup" => {
            let registry = if adapter == "Cloudflare" {
                "partner_registry"
            } else {
                "registry"
            };
            call.args.len() == 2
                && exact_reference(call.args.first(), registry)
                && exact_reference(call.args.iter().nth(1), "req")
        }
        "handle_auction" => {
            call.args.len() == 7
                && exact_reference_field(call.args.first(), "s", "settings")
                && exact_reference_field(call.args.iter().nth(1), "s", "orchestrator")
                && call
                    .args
                    .iter()
                    .nth(2)
                    .is_some_and(|argument| exact_route_path(argument, &["None"]))
                && call
                    .args
                    .iter()
                    .nth(3)
                    .is_some_and(|argument| exact_route_path(argument, &["None"]))
                && exact_reference(call.args.iter().nth(4), "ec_context")
                && exact_reference(call.args.iter().nth(5), "services")
                && call
                    .args
                    .iter()
                    .nth(6)
                    .is_some_and(|argument| is_ident(argument, "req"))
        }
        "handle_page_bids" => {
            call.args.len() == 6
                && exact_reference_field(call.args.first(), "s", "settings")
                && exact_reference(call.args.iter().nth(1), "services")
                && call
                    .args
                    .iter()
                    .nth(2)
                    .is_some_and(|argument| exact_route_path(argument, &["None"]))
                && call
                    .args
                    .iter()
                    .nth(3)
                    .is_some_and(|argument| is_ident(argument, "auction"))
                && exact_reference(call.args.iter().nth(4), "ec_context")
                && call
                    .args
                    .iter()
                    .nth(5)
                    .is_some_and(|argument| is_ident(argument, "req"))
        }
        "dispatch" => {
            call.args.len() == 2
                && call
                    .args
                    .first()
                    .is_some_and(|argument| is_ident(argument, "s"))
                && call
                    .args
                    .iter()
                    .nth(1)
                    .is_some_and(|argument| is_ident(argument, "ctx"))
        }
        _ => false,
    }
}

fn exact_cloudflare_handler_wrapper(behavior: &str, expression: &Expr) -> bool {
    if behavior == "dispatch" {
        return true;
    }
    match strip_parens(expression) {
        Expr::Call(make) => {
            if !exact_route_call_path(make, &["make_handler"]) || make.args.len() != 2 {
                return false;
            }
            let exact_state = matches!(make.args.first().map(strip_parens), Some(Expr::Call(clone))
                if exact_route_call_path(clone, &["Arc", "clone"])
                    && clone.args.len() == 1
                    && exact_reference(clone.args.first(), "state"));
            let Some(Expr::Closure(handler)) = make.args.iter().nth(1).map(strip_parens) else {
                return false;
            };
            let expected_inputs: &[&str] = match behavior {
                "handle_admin_eids_lookup" => &["s", "_services", "req"],
                "page_bids_preflight_denied" | "legacy_admin_alias_denied" => {
                    &["_s", "_services", "_req"]
                }
                _ => &["s", "services", "req"],
            };
            exact_state
                && exact_untyped_closure_inputs(handler, expected_inputs)
                && matches!(strip_parens(&handler.body), Expr::Async(_))
        }
        Expr::Closure(handler)
            if matches!(
                behavior,
                "admin_key_management_not_supported" | "admin_ec_lookup_not_supported"
            ) =>
        {
            exact_typed_closure_input(handler, "_ctx")
                && matches!(strip_parens(&handler.body), Expr::Async(_))
        }
        _ => false,
    }
}

fn exact_spin_handler_wrapper(behavior: &str, expression: &Expr) -> bool {
    let Expr::Closure(handler) = strip_parens(expression) else {
        return false;
    };
    let input = if matches!(
        behavior,
        "admin_key_management_not_supported"
            | "admin_ec_lookup_not_supported"
            | "page_bids_preflight_denied"
            | "legacy_admin_alias_denied"
    ) {
        "_ctx"
    } else {
        "ctx"
    };
    if !exact_typed_closure_input(handler, input) {
        return false;
    }
    if input == "_ctx" {
        return matches!(strip_parens(&handler.body), Expr::Async(_));
    }
    let Expr::Block(body) = strip_parens(&handler.body) else {
        return false;
    };
    matches!(body.block.stmts.as_slice(), [Stmt::Local(clone), Stmt::Expr(terminal, None)]
    if exact_registered_clone_local(clone)
        && if behavior == "dispatch" {
            terminal_behavior_name(terminal).as_deref() == Some("dispatch")
        } else {
            matches!(terminal, Expr::Async(_))
        })
}

fn exact_untyped_closure_inputs(closure: &syn::ExprClosure, expected: &[&str]) -> bool {
    closure.inputs.len() == expected.len()
        && closure.inputs.iter().zip(expected).all(|(pattern, name)| {
            matches!(pattern, Pat::Ident(binding)
                if binding.ident == *name && binding.subpat.is_none())
        })
}

fn exact_typed_closure_input(closure: &syn::ExprClosure, expected: &str) -> bool {
    matches!(closure.inputs.iter().collect::<Vec<_>>().as_slice(), [Pat::Type(typed)]
        if matches!(typed.pat.as_ref(), Pat::Ident(binding)
            if binding.ident == expected && binding.subpat.is_none())
            && exact_route_path_type(&typed.ty, &["RequestContext"]))
}

fn exact_route_path_type(ty: &syn::Type, expected: &[&str]) -> bool {
    matches!(ty, syn::Type::Path(path)
    if path.qself.is_none()
        && path.path.leading_colon.is_none()
        && path.path.segments.len() == expected.len()
        && path.path.segments.iter().zip(expected).all(|(segment, name)| {
            segment.ident == *name
                && matches!(segment.arguments, syn::PathArguments::None)
        }))
}

fn exact_spin_admin_eids_handler(expression: &Expr) -> bool {
    let Expr::Closure(handler) = strip_parens(expression) else {
        return false;
    };
    let Expr::Block(body) = strip_parens(&handler.body) else {
        return false;
    };
    let [Stmt::Local(clone), Stmt::Expr(Expr::Async(future), None)] = body.block.stmts.as_slice()
    else {
        return false;
    };
    let [
        Stmt::Local(request),
        Stmt::Local(result),
        Stmt::Expr(Expr::Call(ok), None),
    ] = future.block.stmts.as_slice()
    else {
        return false;
    };
    exact_typed_closure_input(handler, "ctx")
        && exact_registered_clone_local(clone)
        && exact_registered_request_local(request, "handle_admin_eids_lookup")
        && exact_spin_admin_eids_result(result)
        && exact_result_http_error(ok)
}

fn exact_spin_admin_eids_result(local: &syn::Local) -> bool {
    let Some(init) = &local.init else {
        return false;
    };
    let Expr::MethodCall(and_then) = strip_parens(&init.expr) else {
        return false;
    };
    let exact_registry = matches!(strip_parens(&and_then.receiver), Expr::Call(call)
        if exact_route_call_path(call, &["PartnerRegistry", "from_config"])
            && call.args.len() == 1
            && matches!(call.args.first().map(strip_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none()
                    && exact_field_chain(&reference.expr, "s", &["settings", "ec", "partners"])));
    let exact_handler = and_then.method == "and_then"
        && and_then.args.len() == 1
        && matches!(and_then.args.first().map(strip_parens), Some(Expr::Closure(handler))
        if exact_untyped_closure_inputs(handler, &["registry"])
            && terminal_behavior_call(&handler.body, "handle_admin_eids_lookup")
                .is_some_and(|call| exact_portable_handler_call(
                    "Spin",
                    "handle_admin_eids_lookup",
                    call,
                )));
    exact_local_binding(local, "result", false) && exact_registry && exact_handler
}

fn exact_result_http_error(ok: &syn::ExprCall) -> bool {
    call_named_route(ok, "Ok")
        && ok.args.len() == 1
        && matches!(ok.args.first().map(strip_parens), Some(Expr::MethodCall(unwrap))
            if unwrap.method == "unwrap_or_else"
                && unwrap.args.len() == 1
                && is_ident(&unwrap.receiver, "result")
                && matches!(unwrap.args.first().map(strip_parens), Some(Expr::Closure(error))
                    if exact_untyped_closure_inputs(error, &["e"])
                        && matches!(strip_parens(&error.body), Expr::Call(call)
                            if exact_route_call_path(call, &["http_error"])
                                && call.args.len() == 1
                                && exact_reference(call.args.first(), "e"))))
}

fn validate_local_handler_behaviors(
    block: &Block,
    adapter: &str,
    expected: &[(&str, &str)],
    optional: &[(&str, &str)],
) -> Result<(), Report<RouteError>> {
    struct Locals<'a> {
        values: BTreeMap<String, Vec<&'a Expr>>,
    }
    impl<'ast> Visit<'ast> for Locals<'ast> {
        fn visit_local(&mut self, local: &'ast syn::Local) {
            if let Pat::Ident(binding) = &local.pat
                && let Some(init) = &local.init
            {
                self.values
                    .entry(binding.ident.to_string())
                    .or_default()
                    .push(&init.expr);
            }
            visit::visit_local(self, local);
        }
    }
    fn exact_behavior(
        adapter: &str,
        name: &str,
        behavior: &str,
        locals: &BTreeMap<String, Vec<&Expr>>,
        depth: usize,
    ) -> bool {
        if depth > 2 {
            return false;
        }
        let Some(expressions) = locals.get(name) else {
            return false;
        };
        let [expression] = expressions.as_slice() else {
            return false;
        };
        if adapter == "Spin"
            && behavior == "handle_admin_eids_lookup"
            && exact_spin_admin_eids_handler(expression)
        {
            return true;
        }
        if terminal_behavior_name(expression).as_deref() == Some(behavior) {
            return exact_portable_handler_expression(adapter, behavior, expression);
        }
        exact_local_clone_target(expression)
            .is_some_and(|target| exact_behavior(adapter, &target, behavior, locals, depth + 1))
    }
    let mut locals = Locals {
        values: BTreeMap::new(),
    };
    let audited_behaviors = expected
        .iter()
        .chain(optional)
        .filter_map(|(_, behavior)| (*behavior != "dispatch").then_some(*behavior))
        .collect::<Vec<_>>();
    if block_shadows_route_authority(block, &audited_behaviors) {
        return Err(invalid(format!(
            "{adapter} local handler authority is lexically shadowed"
        )));
    }
    locals.visit_block(block);
    for (name, behavior) in expected {
        if !exact_behavior(adapter, name, behavior, &locals.values, 0) {
            return Err(invalid(format!(
                "{adapter} local handler behavior differs for {name}"
            )));
        }
    }
    for (name, behavior) in optional {
        if locals.values.contains_key(*name)
            && !exact_behavior(adapter, name, behavior, &locals.values, 0)
        {
            return Err(invalid(format!(
                "{adapter} local handler behavior differs for {name}"
            )));
        }
    }
    Ok(())
}

fn spin_expected_handler(
    path: &str,
    method: &str,
    page_bids_path: &str,
    page_bids_legacy_path: &str,
) -> Option<&'static [&'static str]> {
    if path == page_bids_path || path == page_bids_legacy_path {
        return match method {
            "GET" => Some(&["page_bids_handler"]),
            "OPTIONS" => Some(&["page_bids_options_handler"]),
            _ => None,
        };
    }
    match (path, method) {
        ("/.well-known/trusted-server.json", "GET") => Some(&["discovery_handler"]),
        ("/verify-signature", "POST") => Some(&["verify_handler"]),
        ("/_ts/admin/keys/rotate" | "/_ts/admin/keys/deactivate", "POST") => {
            Some(&["admin_not_supported_handler"])
        }
        ("/_ts/admin/ec" | "/_ts/admin/ec/{id}", "GET") => {
            Some(&["admin_ec_not_supported_handler"])
        }
        ("/_ts/admin/eids", "GET") => Some(&["admin_eids_handler"]),
        ("/auction", "POST") => Some(&["auction_handler"]),
        ("/first-party/proxy", "GET") => Some(&["fp_proxy_handler"]),
        ("/first-party/click", "GET") => Some(&["fp_click_handler"]),
        ("/first-party/sign", "GET") => Some(&["fp_sign_handler"]),
        ("/first-party/sign", "POST") => Some(&["fp_sign_post_handler"]),
        ("/first-party/proxy-rebuild", "GET") => Some(&["fp_rebuild_handler"]),
        ("/first-party/proxy-rebuild", "POST") => Some(&["fp_rebuild_post_handler"]),
        _ => None,
    }
}

fn expression_references_ident(expression: &Expr, name: &str) -> bool {
    struct IdentFinder<'a> {
        name: &'a str,
        found: bool,
    }

    impl<'ast> Visit<'ast> for IdentFinder<'_> {
        fn visit_ident(&mut self, ident: &'ast syn::Ident) {
            self.found |= ident == self.name;
            if !self.found {
                visit::visit_ident(self, ident);
            }
        }
    }

    let mut finder = IdentFinder { name, found: false };
    finder.visit_expr(expression);
    finder.found
}

fn cloudflare_semantics(path: &str, method: &str) -> (&'static str, RouteStatus) {
    if path == "/" || path == "/{*rest}" {
        ("publisher_fallback", RouteStatus::PublisherFallback)
    } else if path == "/admin/keys/rotate"
        || path == "/admin/keys/deactivate"
        || ((path == "/_ts/page-bids" || path == "/__ts/page-bids") && method == "OPTIONS")
    {
        ("always", RouteStatus::Guarded)
    } else if path.starts_with("/_ts/admin/keys/") || path.starts_with("/_ts/admin/ec") {
        ("always", RouteStatus::Unsupported)
    } else {
        ("always", RouteStatus::Real)
    }
}

#[derive(Clone, Copy)]
enum RouteConstantOwnership {
    Production,
    Test,
    Invalid,
}

struct RouteStringConstants {
    definitions: BTreeMap<String, Vec<(RouteConstantOwnership, String)>>,
    authoritative: BTreeMap<String, String>,
}

impl RouteStringConstants {
    fn with_authoritative(mut self, authoritative: &BTreeMap<String, String>) -> Self {
        self.authoritative.clone_from(authoritative);
        self
    }

    fn resolve(&self, name: &str) -> Result<String, Report<RouteError>> {
        let Some(definitions) = self.definitions.get(name) else {
            return self
                .authoritative
                .get(name)
                .cloned()
                .ok_or_else(|| invalid(format!("unknown named route path: {name}")));
        };
        if definitions
            .iter()
            .any(|(ownership, _)| matches!(ownership, RouteConstantOwnership::Invalid))
        {
            return Err(invalid(format!(
                "unsupported conditional route constant: {name}"
            )));
        }
        let production = definitions
            .iter()
            .filter_map(|(ownership, value)| {
                matches!(ownership, RouteConstantOwnership::Production).then_some(value)
            })
            .collect::<Vec<_>>();
        let [value] = production.as_slice() else {
            return Err(invalid(format!(
                "route constant must have exactly one production definition: {name}"
            )));
        };
        Ok((*value).clone())
    }
}

fn string_constants(items: &[Item]) -> RouteStringConstants {
    let mut definitions = BTreeMap::<String, Vec<(RouteConstantOwnership, String)>>::new();
    for item in items {
        let Item::Const(item) = item else { continue };
        if let Expr::Lit(literal) = &*item.expr
            && let Lit::Str(value) = &literal.lit
        {
            let mut cfg_test = false;
            let mut invalid_cfg = false;
            for attribute in &item.attrs {
                if attribute.path().is_ident("cfg_attr") {
                    invalid_cfg = true;
                } else if attribute.path().is_ident("cfg") {
                    let exact_test = matches!(&attribute.meta, syn::Meta::List(list)
                        if list.tokens.to_string() == "test");
                    if exact_test && !cfg_test {
                        cfg_test = true;
                    } else {
                        invalid_cfg = true;
                    }
                }
            }
            let ownership = if invalid_cfg {
                RouteConstantOwnership::Invalid
            } else if cfg_test {
                RouteConstantOwnership::Test
            } else {
                RouteConstantOwnership::Production
            };
            definitions
                .entry(item.ident.to_string())
                .or_default()
                .push((ownership, value.value()));
        }
    }
    RouteStringConstants {
        definitions,
        authoritative: BTreeMap::new(),
    }
}

fn publisher_route_constants(source: &str) -> Result<BTreeMap<String, String>, Report<RouteError>> {
    let file = parse_route_source("core publisher", source)?;
    let constants = string_constants(&file.items);
    ["PAGE_BIDS_PATH", "PAGE_BIDS_LEGACY_PATH"]
        .into_iter()
        .map(|name| {
            constants
                .resolve(name)
                .map(|value| (name.to_owned(), value))
        })
        .collect()
}

fn method_list_functions(
    items: &[Item],
) -> Result<BTreeMap<String, Vec<String>>, Report<RouteError>> {
    let mut lists = BTreeMap::new();
    for item in items {
        let Item::Fn(function) = item else { continue };
        let Some(Stmt::Expr(Expr::Array(array), _)) = function.block.stmts.last() else {
            continue;
        };
        let mut methods = Vec::new();
        let mut all_methods = true;
        for expression in &array.elems {
            let Expr::Path(path) = expression else {
                all_methods = false;
                break;
            };
            let segments = path.path.segments.iter().collect::<Vec<_>>();
            if segments.len() != 2 || segments[0].ident != "Method" {
                all_methods = false;
                break;
            }
            methods.push(segments[1].ident.to_string());
        }
        if all_methods {
            let name = function.sig.ident.to_string();
            if !route_item_is_production(&function.attrs, &format!("method function `{name}`"))? {
                continue;
            }
            if lists.insert(name.clone(), methods).is_some() {
                return Err(invalid(format!("duplicate method function: {name}")));
            }
        }
    }
    Ok(lists)
}

fn is_ident(expression: &Expr, expected: &str) -> bool {
    matches!(expression, Expr::Path(path) if path.path.is_ident(expected))
}

fn compact_expr(expression: &Expr) -> String {
    match expression {
        Expr::Path(path) => path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        _ => "expression".to_owned(),
    }
}

fn invalid(detail: impl Into<String>) -> Report<RouteError> {
    Report::new(RouteError::Invalid {
        detail: detail.into(),
    })
}

fn ensure_route_input_bound(label: &str, source: &str) -> Result<(), Report<RouteError>> {
    if source.len() > MAX_ROUTE_INPUT_BYTES {
        Err(invalid(format!("{label} exceeds the 4 MiB input limit")))
    } else {
        Ok(())
    }
}

fn unique_route_values(
    values: Vec<String>,
    axis: &'static str,
) -> Result<BTreeSet<String>, Report<RouteError>> {
    if values.len() > MAX_ROUTE_ENTRIES {
        return Err(invalid(format!("route {axis} cardinality limit exceeded")));
    }
    if values
        .iter()
        .any(|value| value.len() > MAX_ROUTE_STRING_BYTES)
    {
        return Err(invalid(format!(
            "route {axis} contains an oversized string"
        )));
    }
    let count = values.len();
    let unique = values.into_iter().collect::<BTreeSet<_>>();
    if unique.len() != count {
        Err(invalid(format!("duplicate route {axis}")))
    } else {
        Ok(unique)
    }
}
