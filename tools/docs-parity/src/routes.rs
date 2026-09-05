//! Closed adapter-route records and Cloudflare builder extraction.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde::Deserialize;
use syn::visit::{self, Visit};
use syn::{Block, Expr, Item, Lit, Pat, Stmt};

use crate::repository::{NormalizedRelativePath, Repository};

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
    adapters: BTreeSet<String>,
    path: String,
    methods: BTreeSet<String>,
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
        let manifest = toml::from_str::<RawRouteManifest>(source)
            .map_err(|error| invalid(format!("malformed route manifest: {error}")))?;
        if manifest.version != 1 {
            return Err(invalid("route manifest version must equal 1"));
        }
        if !manifest.reviewed {
            return Err(invalid("route manifest reviewed must be true"));
        }
        let mut routes = BTreeSet::new();
        for row in manifest.routes {
            if row.adapters.is_empty() {
                return Err(invalid("route row adapters must not be empty"));
            }
            if row.methods.is_empty() {
                return Err(invalid("route row methods must not be empty"));
            }
            for adapter in row.adapters {
                if !matches!(adapter.as_str(), "fastly" | "axum" | "cloudflare" | "spin") {
                    return Err(invalid(format!("unknown route adapter: {adapter}")));
                }
                let record = RouteRecord::new(
                    &adapter,
                    &row.path,
                    &row.methods,
                    row.shape,
                    &row.predicate,
                    row.status,
                    row.startup_router,
                );
                if !routes.insert(record) {
                    return Err(invalid("duplicate expanded route record"));
                }
            }
        }
        Ok(Self { routes })
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
        let manifest = toml::from_str::<RawAdapterSupportManifest>(source)
            .map_err(|error| invalid(format!("malformed adapter support manifest: {error}")))?;
        if manifest.version != 1 || !manifest.reviewed {
            return Err(invalid(
                "adapter support manifest requires version 1 and reviewed=true",
            ));
        }
        let mut ids = BTreeSet::new();
        let adapters = manifest
            .adapters
            .into_iter()
            .map(|record| {
                if !ids.insert(record.id.clone()) {
                    return Err(invalid(format!(
                        "duplicate adapter support row: {}",
                        record.id
                    )));
                }
                if record.owner.trim().is_empty()
                    || !is_review_date(&record.reviewed_at)
                    || !matches!(record.release_status.as_str(), "production" | "development")
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
            )
        })
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        ("axum", "real", 500, false, "multiple"),
        ("cloudflare", "absent", 500, false, "single"),
        ("fastly", "pre_router", 500, true, "multiple"),
        ("spin", "real", 503, true, "single"),
    ]);
    if facts != expected {
        return Err(Report::new(RouteError::Drift {
            axis: "adapter-support",
        }));
    }
    Ok(())
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
    let mut routes = BTreeSet::new();
    routes.extend(extract_named_routes("fastly", sources.fastly_app)?);
    routes.extend(extract_named_routes("axum", sources.axum_app)?);
    routes.extend(extract_named_routes("spin", sources.spin_app)?);
    routes.extend(extract_cloudflare_routes(sources.cloudflare_app)?);

    require_source(sources.fastly_entrypoint, "req.get_path() == \"/health\"")?;
    require_source(
        sources.fastly_entrypoint,
        "req.get_path() == \"/_ts/debug/ja4\"",
    )?;
    require_source(sources.fastly_app, "uses_dynamic_tsjs_fallback")?;
    require_source(sources.fastly_app, "asset_route_for_path")?;
    require_source(sources.axum_app, "path.starts_with(\"/static/tsjs=\")")?;
    require_source(
        sources.cloudflare_app,
        "path.starts_with(\"/static/tsjs=\")",
    )?;
    require_source(sources.spin_app, "path.starts_with(\"/static/tsjs=\")")?;

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
        );
        add_record(
            &mut routes,
            adapter,
            "/{*rest}",
            &fallback_methods,
            RouteShape::Template,
            "publisher_fallback",
            RouteStatus::PublisherFallback,
            false,
        );
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
        );
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
    );
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
        );
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
    );
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
            );
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
        );
    }
    Ok(routes)
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
    let observed = extract_repository_routes(&RouteSources {
        fastly_app: &fastly_app,
        fastly_entrypoint: &fastly_entrypoint,
        axum_app: &axum_app,
        cloudflare_app: &cloudflare_app,
        spin_app: &spin_app,
    })?;
    validate_routes(route_manifest.routes(), &observed)?;
    validate_adapter_support(&support_manifest)
}

#[allow(clippy::too_many_arguments)]
fn add_record(
    routes: &mut BTreeSet<RouteRecord>,
    adapter: &str,
    path: &str,
    methods: &[&str],
    shape: RouteShape,
    predicate: &str,
    status: RouteStatus,
    startup_router: bool,
) {
    routes.insert(RouteRecord::new(
        adapter,
        path,
        methods,
        shape,
        predicate,
        status,
        startup_router,
    ));
}

fn require_source(source: &str, exact: &str) -> Result<(), Report<RouteError>> {
    if source.contains(exact) {
        Ok(())
    } else {
        Err(invalid(format!(
            "missing required route source shape: {exact}"
        )))
    }
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
        .read_tracked(&path)
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
    if !matches!(adapter, "fastly" | "axum" | "spin") {
        return Err(invalid(format!(
            "unsupported named-route adapter: {adapter}"
        )));
    }
    let file = syn::parse_file(source)
        .map_err(|error| invalid(format!("invalid {adapter} Rust: {error}")))?;
    let strings = string_constants(&file.items);
    let methods = method_array_constants(&file.items)?;
    let array = if adapter == "fastly" {
        file.items.iter().find_map(|item| match item {
            Item::Const(item) if item.ident == "NAMED_ROUTES" => expression_array(&item.expr),
            _ => None,
        })
    } else {
        let symbol = if adapter == "axum" {
            "named_routes"
        } else {
            "named_fallback_paths"
        };
        file.items.iter().find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == symbol => function
                .block
                .stmts
                .last()
                .and_then(|statement| match statement {
                    Stmt::Expr(expression, _) => expression_array(expression),
                    _ => None,
                }),
            _ => None,
        })
    }
    .ok_or_else(|| invalid(format!("missing {adapter} named route collection")))?;

    let mut records = BTreeMap::new();
    for entry in &array.elems {
        let (path_expression, method_expression) = if adapter == "spin" {
            let Expr::Tuple(tuple) = entry else {
                return Err(invalid("unsupported Spin named route row"));
            };
            if tuple.elems.len() != 2 {
                return Err(invalid("Spin named route rows require path and methods"));
            }
            (&tuple.elems[0], &tuple.elems[1])
        } else {
            let Expr::Struct(row) = entry else {
                return Err(invalid(format!("unsupported {adapter} named route row")));
            };
            let path = struct_field(row, "path")
                .ok_or_else(|| invalid(format!("{adapter} route row is missing path")))?;
            let methods = struct_field(row, "primary_methods").ok_or_else(|| {
                invalid(format!("{adapter} route row is missing primary_methods"))
            })?;
            (path, methods)
        };
        let path = resolve_static_path(path_expression, &strings)?;
        let route_methods = resolve_method_array(method_expression, &methods)?;
        let shape = if path.contains('{') {
            RouteShape::Template
        } else {
            RouteShape::Literal
        };
        for method in route_methods {
            let (predicate, status) = named_semantics(adapter, &path, &method);
            let key = (path.clone(), shape, predicate.to_owned(), status, false);
            records
                .entry(key)
                .and_modify(|record: &mut RouteRecord| {
                    record.methods.insert(method.clone());
                })
                .or_insert_with(|| {
                    RouteRecord::new(adapter, &path, [&method], shape, predicate, status, false)
                });
        }
    }
    Ok(records.into_values().collect())
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
    constants: &BTreeMap<String, String>,
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
            constants
                .get(&name)
                .cloned()
                .ok_or_else(|| invalid(format!("unknown named route path: {name}")))
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
        let (name, expression) = match item {
            Item::Const(item) => (item.ident.to_string(), &*item.expr),
            Item::Static(item) => (item.ident.to_string(), &*item.expr),
            _ => continue,
        };
        if let Some(array) = expression_array(expression)
            && (array.elems.is_empty()
                || matches!(array.elems.first(), Some(Expr::Path(path)) if path.path.segments.first().is_some_and(|segment| segment.ident == "Method")))
        {
            constants.insert(name, parse_method_array(array)?);
        }
    }
    Ok(constants)
}

fn parse_method_array(array: &syn::ExprArray) -> Result<Vec<String>, Report<RouteError>> {
    array
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
        .collect()
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
    let file = syn::parse_file(source)
        .map_err(|error| invalid(format!("invalid Cloudflare Rust: {error}")))?;
    let strings = string_constants(&file.items);
    let method_lists = method_list_functions(&file.items);
    let build_router = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "build_router" => Some(&function.block),
            _ => None,
        })
        .ok_or_else(|| invalid("missing Cloudflare build_router"))?;
    let mut parser = CloudflareParser {
        strings: &strings,
        method_lists: &method_lists,
        values: BTreeMap::new(),
        routes: BTreeMap::new(),
    };
    parser.parse_block(build_router)?;
    if parser.routes.is_empty() {
        return Err(invalid(
            "Cloudflare build_router contains no recognized routes",
        ));
    }
    Ok(parser.routes.into_values().collect())
}

type RouteKey = (String, RouteShape, String, RouteStatus, bool);

struct CloudflareParser<'a> {
    strings: &'a BTreeMap<String, String>,
    method_lists: &'a BTreeMap<String, Vec<String>>,
    values: BTreeMap<String, String>,
    routes: BTreeMap<RouteKey, RouteRecord>,
}

impl CloudflareParser<'_> {
    fn parse_block(&mut self, block: &Block) -> Result<(), Report<RouteError>> {
        for statement in &block.stmts {
            match statement {
                Stmt::Local(local) => {
                    let Some(init) = &local.init else { continue };
                    if matches!(&local.pat, Pat::Ident(ident) if ident.ident == "router") {
                        self.parse_router_chain(&init.expr)?;
                    } else if expression_references_ident(&init.expr, "router") {
                        return Err(invalid(
                            "unsupported Cloudflare route-affecting local initializer",
                        ));
                    }
                }
                Stmt::Item(_) => {}
                Stmt::Macro(statement)
                    if token_stream_contains_ident(&statement.mac.tokens, "router") =>
                {
                    return Err(invalid(
                        "unsupported Cloudflare route-affecting statement macro",
                    ));
                }
                Stmt::Macro(_) => {}
                Stmt::Expr(expression, _) => self.parse_expression(expression)?,
            }
        }
        Ok(())
    }

    fn parse_expression(&mut self, expression: &Expr) -> Result<(), Report<RouteError>> {
        match expression {
            Expr::Assign(assign) if is_ident(&assign.left, "router") => {
                self.parse_router_chain(&assign.right)
            }
            Expr::ForLoop(loop_expression) => self.parse_loop(loop_expression),
            Expr::Block(block) => self.parse_block(&block.block),
            Expr::MethodCall(call)
                if call.method == "build" && is_ident(&call.receiver, "router") =>
            {
                Ok(())
            }
            Expr::Call(call) => Err(invalid(format!(
                "unsupported Cloudflare builder call: {}",
                compact_expr(&call.func)
            ))),
            Expr::If(_) | Expr::Match(_) | Expr::While(_) | Expr::Loop(_) => Err(invalid(
                "unsupported Cloudflare control flow in build_router",
            )),
            _ if expression_references_ident(expression, "router") => Err(invalid(
                "unsupported Cloudflare expression that references the router",
            )),
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
            && let Expr::Path(path) = &*call.func
            && path.path.segments.len() == 2
            && path.path.segments[0].ident == "RouterService"
            && path.path.segments[1].ident == "builder"
            && call.args.is_empty()
        {
            return Ok(());
        }
        let Expr::MethodCall(call) = expression else {
            if is_ident(expression, "router") {
                return Ok(());
            }
            return Err(invalid("unsupported Cloudflare router assignment"));
        };
        if !is_ident(&call.receiver, "router") {
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
                self.add(&self.resolve_path(path)?, method.to_ascii_uppercase());
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
                self.add(&self.resolve_path(path)?, self.resolve_method(method)?);
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
                self.values
                    .get(&name)
                    .or_else(|| self.strings.get(&name))
                    .cloned()
                    .ok_or_else(|| invalid(format!("unknown Cloudflare route path: {name}")))
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

    fn add(&mut self, path: &str, method: String) {
        let shape = if path.contains('{') {
            RouteShape::Template
        } else {
            RouteShape::Literal
        };
        let (predicate, status) = cloudflare_semantics(path, &method);
        let key = (path.to_owned(), shape, predicate.to_owned(), status, false);
        self.routes
            .entry(key)
            .and_modify(|record| {
                record.methods.insert(method.clone());
            })
            .or_insert_with(|| {
                RouteRecord::new(
                    "cloudflare",
                    path,
                    [method],
                    shape,
                    predicate,
                    status,
                    false,
                )
            });
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

fn token_stream_contains_ident(tokens: &impl ToString, name: &str) -> bool {
    tokens
        .to_string()
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|word| word == name)
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

fn string_constants(items: &[Item]) -> BTreeMap<String, String> {
    let mut constants = BTreeMap::from([
        ("PAGE_BIDS_PATH".to_owned(), "/_ts/page-bids".to_owned()),
        (
            "PAGE_BIDS_LEGACY_PATH".to_owned(),
            "/__ts/page-bids".to_owned(),
        ),
    ]);
    for item in items {
        let Item::Const(item) = item else { continue };
        if let Expr::Lit(literal) = &*item.expr
            && let Lit::Str(value) = &literal.lit
        {
            constants.insert(item.ident.to_string(), value.value());
        }
    }
    constants
}

fn method_list_functions(items: &[Item]) -> BTreeMap<String, Vec<String>> {
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
            lists.insert(function.sig.ident.to_string(), methods);
        }
    }
    lists
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
