//! Closed integration inventory records and exact parity comparison.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde::Deserialize;
use syn::visit::{self, Visit as _};
use syn::{Expr, ExprCall, ExprLit, ImplItem, Item, Lit, Member};

use crate::repository::{NormalizedRelativePath, Repository};

/// Integration inventory validation failure.
#[derive(Debug, derive_more::Display)]
pub enum IntegrationError {
    /// The checked manifest cannot be parsed or violates its closed schema.
    #[display("invalid integration manifest: {detail}")]
    InvalidManifest {
        /// Stable diagnostic detail.
        detail: String,
    },
    /// One observed source or behavior set differs from the reviewed record.
    #[display("integration {axis} inventory differs")]
    InventoryDrift {
        /// Name of the mismatched inventory axis.
        axis: &'static str,
    },
}

impl core::error::Error for IntegrationError {}

/// Browser loading behavior for one integration module.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum LoadingMode {
    /// Included in the synchronous aggregate bundle.
    Bundled,
    /// Served as an independent deferred script.
    Deferred,
    /// Served by a dedicated standalone-tag decision path.
    Standalone,
}

/// One exact integration behavior observation under a named predicate.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRecord {
    /// Stable integration identifier.
    pub id: String,
    /// Exact configuration predicate for this observation.
    pub predicate: String,
    /// Observed method-and-route entries.
    #[serde(default)]
    pub proxy_routes: BTreeSet<String>,
    /// Observed attribute rewriter identities.
    #[serde(default)]
    pub attribute_rewriters: BTreeSet<String>,
    /// Observed script rewriter selectors.
    #[serde(default)]
    pub script_rewriters: BTreeSet<String>,
    /// Observed head injector identities.
    #[serde(default)]
    pub head_injectors: BTreeSet<String>,
    /// Observed HTML post-processor identities.
    #[serde(default)]
    pub post_processors: BTreeSet<String>,
    /// Observed request filter identities.
    #[serde(default)]
    pub request_filters: BTreeSet<String>,
    /// Observed auction-provider identities for mediator-only integrations.
    #[serde(default)]
    pub providers: BTreeSet<String>,
    /// Exact JS loading disposition.
    pub js_mode: String,
}

impl CapabilityRecord {
    /// Construct a capability record from exact iterable observations.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new<P, A, S, H, O, F, V>(
        id: &str,
        predicate: &str,
        proxy_routes: P,
        attribute_rewriters: A,
        script_rewriters: S,
        head_injectors: H,
        post_processors: O,
        request_filters: F,
        providers: V,
        js_mode: &str,
    ) -> Self
    where
        P: IntoIterator,
        P::Item: AsRef<str>,
        A: IntoIterator,
        A::Item: AsRef<str>,
        S: IntoIterator,
        S::Item: AsRef<str>,
        H: IntoIterator,
        H::Item: AsRef<str>,
        O: IntoIterator,
        O::Item: AsRef<str>,
        F: IntoIterator,
        F::Item: AsRef<str>,
        V: IntoIterator,
        V::Item: AsRef<str>,
    {
        Self {
            id: id.to_owned(),
            predicate: predicate.to_owned(),
            proxy_routes: strings(proxy_routes),
            attribute_rewriters: strings(attribute_rewriters),
            script_rewriters: strings(script_rewriters),
            head_injectors: strings(head_injectors),
            post_processors: strings(post_processors),
            request_filters: strings(request_filters),
            providers: strings(providers),
            js_mode: js_mode.to_owned(),
        }
    }
}

/// Complete checked integration inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrationInventory {
    /// IDs accepted by deploy validation.
    pub deploy_ids: BTreeSet<String>,
    /// IDs registered through settings-only builders.
    pub builder_ids: BTreeSet<String>,
    /// IDs registered from a compiled auction plan.
    pub plan_registration_ids: BTreeSet<String>,
    /// Compile-time provider profile IDs.
    pub profile_ids: BTreeSet<String>,
    /// Integration-like mediator IDs outside the integration registry.
    pub mediator_ids: BTreeSet<String>,
    /// Browser integration source module IDs.
    pub js_source_module_ids: BTreeSet<String>,
    /// Emitted JS bundle IDs, including core.
    pub js_bundle_ids: BTreeSet<String>,
    /// Loading disposition for each browser-served integration.
    pub loading_modes: BTreeMap<String, LoadingMode>,
    /// Exact behavior observations across the predicate matrix.
    pub capabilities: BTreeSet<CapabilityRecord>,
    /// Manually owned integration release/operational status records.
    pub operational: BTreeSet<OperationalRecord>,
}

/// One manually reviewed integration release/operational status.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct OperationalRecord {
    /// Stable integration identifier.
    pub id: String,
    /// Manual maturity label; never inferred from source registration.
    pub status: String,
    /// Responsible reviewer.
    pub owner: String,
    /// Review date in `YYYY-MM-DD` form.
    pub reviewed_at: String,
}

/// Production source documents that define the static integration inventory.
pub struct InventorySources<'a> {
    /// Deploy validation function source.
    pub deploy_validation: &'a str,
    /// Settings-only integration builder registry source.
    pub builders: &'a str,
    /// Plan-backed integration registry source.
    pub plan_registrations: &'a str,
    /// Provider profile registry source.
    pub profiles: &'a str,
    /// Auction mediator registration source.
    pub mediator: &'a str,
    /// Complete tracked repository path list.
    pub tracked_paths: &'a [&'a str],
}

/// Static inventory extracted from its authoritative production sources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceInventory {
    /// IDs exercised by deploy validation.
    pub deploy_ids: BTreeSet<String>,
    /// IDs in the settings-only builder table.
    pub builder_ids: BTreeSet<String>,
    /// IDs registered from an auction plan.
    pub plan_registration_ids: BTreeSet<String>,
    /// IDs in the provider profile registry.
    pub profile_ids: BTreeSet<String>,
    /// IDs registered as auction mediators outside the integration registry.
    pub mediator_ids: BTreeSet<String>,
    /// IDs with an integration `index.ts` source module.
    pub js_source_module_ids: BTreeSet<String>,
    /// IDs emitted by the JS bundle build, including core.
    pub js_bundle_ids: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    reviewed: bool,
    deploy_ids: BTreeSet<String>,
    builder_ids: BTreeSet<String>,
    plan_registration_ids: BTreeSet<String>,
    profile_ids: BTreeSet<String>,
    mediator_ids: BTreeSet<String>,
    js_source_module_ids: BTreeSet<String>,
    js_bundle_ids: BTreeSet<String>,
    #[serde(default)]
    loading_modes: Vec<LoadingRecord>,
    #[serde(default)]
    capabilities: Vec<CapabilityRecord>,
    #[serde(default)]
    operational: Vec<OperationalRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadingRecord {
    id: String,
    mode: LoadingMode,
}

impl IntegrationInventory {
    /// Parse a reviewed closed-schema inventory.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed TOML, unknown fields, an unsupported
    /// version, missing review attestation, or duplicate keyed rows.
    pub fn parse(source: &str) -> Result<Self, Report<IntegrationError>> {
        let manifest = toml::from_str::<Manifest>(source).map_err(|error| {
            invalid_manifest(format!("malformed TOML or unknown field: {error}"))
        })?;
        if manifest.version != 1 {
            return Err(invalid_manifest("version must equal 1"));
        }
        if !manifest.reviewed {
            return Err(invalid_manifest("reviewed must be true"));
        }
        let mut loading_modes = BTreeMap::new();
        for record in manifest.loading_modes {
            if loading_modes
                .insert(record.id.clone(), record.mode)
                .is_some()
            {
                return Err(invalid_manifest(format!(
                    "duplicate loading mode row: {}",
                    record.id
                )));
            }
        }
        let capability_count = manifest.capabilities.len();
        let capabilities = manifest.capabilities.into_iter().collect::<BTreeSet<_>>();
        if capabilities.len() != capability_count {
            return Err(invalid_manifest("duplicate capability row"));
        }
        let operational_count = manifest.operational.len();
        let operational = manifest.operational.into_iter().collect::<BTreeSet<_>>();
        if operational.len() != operational_count {
            return Err(invalid_manifest("duplicate operational row"));
        }
        let mut operational_ids = BTreeSet::new();
        for record in &operational {
            if !operational_ids.insert(&record.id) {
                return Err(invalid_manifest(format!(
                    "duplicate operational integration ID: {}",
                    record.id
                )));
            }
            if !matches!(record.status.as_str(), "production" | "development")
                || record.owner.trim().is_empty()
                || !is_review_date(&record.reviewed_at)
            {
                return Err(invalid_manifest(format!(
                    "invalid manual operational record: {}",
                    record.id
                )));
            }
        }
        Ok(Self {
            deploy_ids: manifest.deploy_ids,
            builder_ids: manifest.builder_ids,
            plan_registration_ids: manifest.plan_registration_ids,
            profile_ids: manifest.profile_ids,
            mediator_ids: manifest.mediator_ids,
            js_source_module_ids: manifest.js_source_module_ids,
            js_bundle_ids: manifest.js_bundle_ids,
            loading_modes,
            capabilities,
            operational,
        })
    }
}

/// Extract the static integration inventory from closed production-source shapes.
///
/// # Errors
///
/// Returns an error when Rust syntax is invalid, an authoritative symbol is
/// missing, or a registry contains an unsupported expression shape.
pub fn extract_source_inventory(
    sources: &InventorySources<'_>,
) -> Result<SourceInventory, Report<IntegrationError>> {
    let deploy_file = parse_source("deploy validation", sources.deploy_validation)?;
    let builders_file = parse_source("builder", sources.builders)?;
    let plan_file = parse_source("plan registration", sources.plan_registrations)?;
    let profiles_file = parse_source("profile", sources.profiles)?;
    let mediator_file = parse_source("mediator", sources.mediator)?;

    let deploy = find_function_block(&deploy_file.items, "validate_enabled_integrations")
        .ok_or_else(|| invalid_manifest("missing deploy validation function"))?;
    let mut deploy_visitor = DeployVisitor::default();
    deploy_visitor.visit_block(deploy);
    if let Some(detail) = deploy_visitor.error {
        return Err(invalid_manifest(detail));
    }

    let builder_ids = extract_builder_ids(&builders_file.items)?;
    let plan = find_function_block(&plan_file.items, "with_plan")
        .ok_or_else(|| invalid_manifest("missing plan registration function"))?;
    let mut plan_visitor = PlanRegistrationVisitor::default();
    plan_visitor.visit_block(plan);

    let profile_ids = extract_profile_ids(&profiles_file.items)?;
    let mediator = find_function_block(&mediator_file.items, "build_orchestrator_with_plan")
        .ok_or_else(|| invalid_manifest("missing mediator registration function"))?;
    let mut mediator_visitor = MediatorVisitor::default();
    mediator_visitor.visit_block(mediator);

    let js_source_module_ids = sources
        .tracked_paths
        .iter()
        .filter_map(|path| js_module_id(path))
        .collect::<BTreeSet<_>>();
    let mut js_bundle_ids = js_source_module_ids.clone();
    js_bundle_ids.insert("core".to_owned());

    Ok(SourceInventory {
        deploy_ids: deploy_visitor.ids,
        builder_ids,
        plan_registration_ids: plan_visitor.ids,
        profile_ids,
        mediator_ids: mediator_visitor.ids,
        js_source_module_ids,
        js_bundle_ids,
    })
}

/// Check the reviewed integration inventory against authoritative repository
/// sources. Behavioral capability receipts remain compiled in core tests.
///
/// # Errors
///
/// Returns an error for repository access, unsupported source grammar, static
/// inventory drift, or an incoherent capability/loading/ownership domain.
pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<IntegrationError>> {
    let expected = IntegrationInventory::parse(&read_utf8(
        repository,
        "tools/docs-parity/manifests/integrations.toml",
    )?)?;
    let tracked = repository
        .tracked_paths()
        .map_err(|error| invalid_manifest(format!("cannot list tracked paths: {error:?}")))?;
    let tracked_strings = tracked
        .iter()
        .map(|path| {
            path.as_utf8()
                .map(str::to_owned)
                .map_err(|error| invalid_manifest(format!("non-UTF-8 tracked path: {error:?}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tracked_refs = tracked_strings
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let deploy = read_utf8(repository, "crates/trusted-server-core/src/config.rs")?;
    let builders = read_utf8(
        repository,
        "crates/trusted-server-core/src/integrations/mod.rs",
    )?;
    let plan = read_utf8(
        repository,
        "crates/trusted-server-core/src/integrations/registry.rs",
    )?;
    let profiles = read_utf8(
        repository,
        "crates/trusted-server-core/src/auction/profile.rs",
    )?;
    let mediator = read_utf8(repository, "crates/trusted-server-core/src/auction/mod.rs")?;
    let source = extract_source_inventory(&InventorySources {
        deploy_validation: &deploy,
        builders: &builders,
        plan_registrations: &plan,
        profiles: &profiles,
        mediator: &mediator,
        tracked_paths: &tracked_refs,
    })?;
    let observed = IntegrationInventory {
        deploy_ids: source.deploy_ids,
        builder_ids: source.builder_ids,
        plan_registration_ids: source.plan_registration_ids,
        profile_ids: source.profile_ids,
        mediator_ids: source.mediator_ids,
        js_source_module_ids: source.js_source_module_ids,
        js_bundle_ids: source.js_bundle_ids,
        loading_modes: expected.loading_modes.clone(),
        capabilities: expected.capabilities.clone(),
        operational: expected.operational.clone(),
    };
    validate_inventory(&expected, &observed)?;
    validate_domains(&expected)
}

fn validate_domains(inventory: &IntegrationInventory) -> Result<(), Report<IntegrationError>> {
    let loading_ids = inventory
        .loading_modes
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if loading_ids != inventory.js_source_module_ids {
        return Err(Report::new(IntegrationError::InventoryDrift {
            axis: "loading domain",
        }));
    }
    let capability_ids = inventory
        .capabilities
        .iter()
        .map(|record| record.id.clone())
        .collect::<BTreeSet<_>>();
    let mut expected_capability_ids = inventory.deploy_ids.clone();
    expected_capability_ids.insert("creative".to_owned());
    if capability_ids != expected_capability_ids {
        return Err(Report::new(IntegrationError::InventoryDrift {
            axis: "capability domain",
        }));
    }
    let operational_ids = inventory
        .operational
        .iter()
        .map(|record| record.id.clone())
        .collect::<BTreeSet<_>>();
    if operational_ids != capability_ids {
        return Err(Report::new(IntegrationError::InventoryDrift {
            axis: "operational domain",
        }));
    }
    for capability in &inventory.capabilities {
        let expected = match inventory.loading_modes.get(&capability.id) {
            Some(LoadingMode::Bundled) => "bundled",
            Some(LoadingMode::Deferred) => "deferred",
            Some(LoadingMode::Standalone) => "standalone",
            None => "none",
        };
        if capability.js_mode != expected {
            return Err(Report::new(IntegrationError::InventoryDrift {
                axis: "capability loading",
            }));
        }
    }
    Ok(())
}

fn read_utf8(repository: &Repository, path: &str) -> Result<String, Report<IntegrationError>> {
    let path = NormalizedRelativePath::new(std::path::Path::new(path))
        .map_err(|error| invalid_manifest(format!("invalid inventory path: {error:?}")))?;
    let bytes = repository
        .read_tracked(&path)
        .map_err(|error| invalid_manifest(format!("cannot read inventory source: {error:?}")))?;
    String::from_utf8(bytes)
        .map_err(|error| invalid_manifest(format!("inventory source is not UTF-8: {error}")))
}

fn parse_source(label: &str, source: &str) -> Result<syn::File, Report<IntegrationError>> {
    syn::parse_file(source)
        .map_err(|error| invalid_manifest(format!("invalid {label} Rust source: {error}")))
}

fn find_function_block<'a>(items: &'a [Item], name: &str) -> Option<&'a syn::Block> {
    for item in items {
        match item {
            Item::Fn(function) if function.sig.ident == name => return Some(&function.block),
            Item::Impl(item_impl) => {
                for item in &item_impl.items {
                    if let ImplItem::Fn(function) = item
                        && function.sig.ident == name
                    {
                        return Some(&function.block);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Default)]
struct DeployVisitor {
    ids: BTreeSet<String>,
    error: Option<String>,
}

impl<'ast> syn::visit::Visit<'ast> for DeployVisitor {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        let Expr::Path(function) = call.func.as_ref() else {
            visit::visit_expr_call(self, call);
            return;
        };
        let Some(name) = function
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            visit::visit_expr_call(self, call);
            return;
        };
        if name == "validate_prebid" {
            self.ids.insert("prebid".to_owned());
        } else if name == "validate_integration" {
            match call.args.iter().nth(1).and_then(literal_string) {
                Some(id) => {
                    self.ids.insert(id);
                }
                None => {
                    self.error = Some("deploy validator ID must be a string literal".to_owned())
                }
            }
        } else if name.starts_with("validate_")
            && !matches!(
                name.as_str(),
                "validate_config_for_startup" | "validate_config_for_deploy" | "validate_datadome"
            )
        {
            self.error = Some(format!("unsupported deploy validator call: {name}"));
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "integration_config" {
            match call.args.first().and_then(literal_string) {
                Some(id) => {
                    self.ids.insert(id);
                }
                None => {
                    self.error =
                        Some("deploy integration_config ID must be a string literal".to_owned());
                }
            }
        }
        visit::visit_expr_method_call(self, call);
    }
}

#[derive(Default)]
struct PlanRegistrationVisitor {
    ids: BTreeSet<String>,
}

impl<'ast> syn::visit::Visit<'ast> for PlanRegistrationVisitor {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let segments = function.path.segments.iter().collect::<Vec<_>>();
            if segments
                .last()
                .is_some_and(|segment| segment.ident == "register_for_plan")
                && let Some(module) = segments.iter().rev().nth(1)
            {
                self.ids.insert(module.ident.to_string());
            }
        }
        visit::visit_expr_call(self, call);
    }
}

#[derive(Default)]
struct MediatorVisitor {
    ids: BTreeSet<String>,
}

impl<'ast> syn::visit::Visit<'ast> for MediatorVisitor {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let segments = function.path.segments.iter().collect::<Vec<_>>();
            if segments
                .last()
                .is_some_and(|segment| segment.ident == "register_providers")
                && let Some(module) = segments.iter().rev().nth(1)
            {
                self.ids.insert(module.ident.to_string());
            }
        }
        visit::visit_expr_call(self, call);
    }
}

fn extract_builder_ids(items: &[Item]) -> Result<BTreeSet<String>, Report<IntegrationError>> {
    let block = find_function_block(items, "builders")
        .ok_or_else(|| invalid_manifest("missing builder registry function"))?;
    let Some(syn::Stmt::Expr(expression, None)) = block.stmts.last() else {
        return Err(invalid_manifest(
            "builder registry must end with an array expression",
        ));
    };
    let Expr::Reference(reference) = expression else {
        return Err(invalid_manifest(
            "builder registry must return a borrowed array",
        ));
    };
    let Expr::Array(array) = reference.expr.as_ref() else {
        return Err(invalid_manifest(
            "builder registry must return a borrowed array",
        ));
    };
    let mut ids = BTreeSet::new();
    for expression in &array.elems {
        let Expr::Struct(record) = expression else {
            return Err(invalid_manifest("unknown builder registry expression"));
        };
        if record
            .path
            .segments
            .last()
            .is_none_or(|segment| segment.ident != "IntegrationBuilder")
            || record.rest.is_some()
            || record.fields.len() != 2
        {
            return Err(invalid_manifest("unknown builder record shape"));
        }
        let Some(id) = record
            .fields
            .iter()
            .find(|field| member_is(&field.member, "id"))
            .and_then(|field| literal_string(&field.expr))
        else {
            return Err(invalid_manifest("builder ID must be a string literal"));
        };
        if !ids.insert(id.clone()) {
            return Err(invalid_manifest(format!("duplicate builder ID: {id}")));
        }
    }
    Ok(ids)
}

fn extract_profile_ids(items: &[Item]) -> Result<BTreeSet<String>, Report<IntegrationError>> {
    let constants = items
        .iter()
        .filter_map(|item| {
            let Item::Const(constant) = item else {
                return None;
            };
            literal_string(&constant.expr).map(|value| (constant.ident.to_string(), value))
        })
        .collect::<BTreeMap<_, _>>();
    let registration = items.iter().find_map(|item| {
        let Item::Const(constant) = item else {
            return None;
        };
        (constant.ident == "PROFILE_REGISTRATIONS").then_some(constant)
    });
    let Some(registration) = registration else {
        return Err(invalid_manifest("missing profile registry constant"));
    };
    let Expr::Array(array) = registration.expr.as_ref() else {
        return Err(invalid_manifest("profile registry must be an array"));
    };
    let mut ids = BTreeSet::new();
    for expression in &array.elems {
        let Expr::Struct(record) = expression else {
            return Err(invalid_manifest("unknown profile registry expression"));
        };
        let Some(id_expression) = record
            .fields
            .iter()
            .find(|field| member_is(&field.member, "id"))
            .map(|field| &field.expr)
        else {
            return Err(invalid_manifest("profile record is missing id"));
        };
        let id = literal_string(id_expression).or_else(|| {
            let Expr::Path(path) = id_expression else {
                return None;
            };
            path.path
                .get_ident()
                .and_then(|identifier| constants.get(&identifier.to_string()).cloned())
        });
        let Some(id) = id else {
            return Err(invalid_manifest(
                "profile ID must resolve to a string constant",
            ));
        };
        if !ids.insert(id.clone()) {
            return Err(invalid_manifest(format!("duplicate profile ID: {id}")));
        }
    }
    Ok(ids)
}

fn literal_string(expression: &Expr) -> Option<String> {
    let Expr::Lit(ExprLit {
        lit: Lit::Str(value),
        ..
    }) = expression
    else {
        return None;
    };
    Some(value.value())
}

fn member_is(member: &Member, expected: &str) -> bool {
    matches!(member, Member::Named(identifier) if identifier == expected)
}

fn js_module_id(path: &str) -> Option<String> {
    const PREFIX: &str = "crates/trusted-server-js/lib/src/integrations/";
    let remainder = path.strip_prefix(PREFIX)?;
    let id = remainder.strip_suffix("/index.ts")?;
    (!id.is_empty() && !id.contains('/')).then(|| id.to_owned())
}

/// Require exact equality on every integration inventory axis.
///
/// # Errors
///
/// Returns the first stable axis whose observed set differs from the checked
/// inventory.
pub fn validate_inventory(
    expected: &IntegrationInventory,
    observed: &IntegrationInventory,
) -> Result<(), Report<IntegrationError>> {
    compare("deploy", &expected.deploy_ids, &observed.deploy_ids)?;
    compare("builder", &expected.builder_ids, &observed.builder_ids)?;
    compare(
        "plan",
        &expected.plan_registration_ids,
        &observed.plan_registration_ids,
    )?;
    compare("profile", &expected.profile_ids, &observed.profile_ids)?;
    compare("mediator", &expected.mediator_ids, &observed.mediator_ids)?;
    compare(
        "js source",
        &expected.js_source_module_ids,
        &observed.js_source_module_ids,
    )?;
    compare(
        "js bundle",
        &expected.js_bundle_ids,
        &observed.js_bundle_ids,
    )?;
    compare("loading", &expected.loading_modes, &observed.loading_modes)?;
    compare("capability", &expected.capabilities, &observed.capabilities)?;
    compare("operational", &expected.operational, &observed.operational)
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
    valid_calendar_date(year, month, day)
}

fn valid_calendar_date(year: u32, month: u32, day: u32) -> bool {
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

fn compare<T: PartialEq>(
    axis: &'static str,
    expected: &T,
    observed: &T,
) -> Result<(), Report<IntegrationError>> {
    if expected == observed {
        Ok(())
    } else {
        Err(Report::new(IntegrationError::InventoryDrift { axis }))
    }
}

fn strings<I>(values: I) -> BTreeSet<String>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    values
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect()
}

fn invalid_manifest(detail: impl Into<String>) -> Report<IntegrationError> {
    Report::new(IntegrationError::InvalidManifest {
        detail: detail.into(),
    })
}
