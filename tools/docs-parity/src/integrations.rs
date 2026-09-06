//! Closed integration inventory records and exact parity comparison.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde::Deserialize;
use serde::de::Error as _;
use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit as _};
use syn::{Attribute, Expr, ExprCall, ExprLit, ImplItem, Item, Lit, Member, Token, Type};

use crate::repository::{NormalizedRelativePath, Repository};

const MAX_INVENTORY_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_INVENTORY_ENTRIES: usize = 4096;
const MAX_INVENTORY_STRING_BYTES: usize = 16 * 1024;

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
    #[serde(default, deserialize_with = "deserialize_unique_proxy_routes")]
    pub proxy_routes: BTreeSet<String>,
    /// Observed attribute rewriter identities.
    #[serde(default, deserialize_with = "deserialize_unique_attribute_rewriters")]
    pub attribute_rewriters: BTreeSet<String>,
    /// Observed script rewriter selectors.
    #[serde(default, deserialize_with = "deserialize_unique_script_rewriters")]
    pub script_rewriters: BTreeSet<String>,
    /// Observed head injector identities.
    #[serde(default, deserialize_with = "deserialize_unique_head_injectors")]
    pub head_injectors: BTreeSet<String>,
    /// Observed HTML post-processor identities.
    #[serde(default, deserialize_with = "deserialize_unique_post_processors")]
    pub post_processors: BTreeSet<String>,
    /// Observed request filter identities.
    #[serde(default, deserialize_with = "deserialize_unique_request_filters")]
    pub request_filters: BTreeSet<String>,
    /// Observed auction-provider identities for mediator-only integrations.
    #[serde(default, deserialize_with = "deserialize_unique_providers")]
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
    /// Configuration entrypoints that invoke deploy and runtime validation.
    pub validation_entrypoints: &'a str,
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
    deploy_ids: Vec<String>,
    builder_ids: Vec<String>,
    plan_registration_ids: Vec<String>,
    profile_ids: Vec<String>,
    mediator_ids: Vec<String>,
    js_source_module_ids: Vec<String>,
    js_bundle_ids: Vec<String>,
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

macro_rules! unique_set_deserializer {
    ($function:ident, $axis:literal) => {
        fn $function<'de, D>(deserializer: D) -> Result<BTreeSet<String>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let values = Vec::<String>::deserialize(deserializer)?;
            if values.len() > MAX_INVENTORY_ENTRIES {
                return Err(D::Error::custom(concat!(
                    "capability ",
                    $axis,
                    " exceeds cardinality limit"
                )));
            }
            if values
                .iter()
                .any(|value| value.len() > MAX_INVENTORY_STRING_BYTES)
            {
                return Err(D::Error::custom(concat!(
                    "capability ",
                    $axis,
                    " contains an oversized string"
                )));
            }
            let count = values.len();
            let unique = values.into_iter().collect::<BTreeSet<_>>();
            if unique.len() != count {
                return Err(D::Error::custom(concat!("duplicate capability ", $axis)));
            }
            Ok(unique)
        }
    };
}

unique_set_deserializer!(deserialize_unique_proxy_routes, "proxy_routes");
unique_set_deserializer!(
    deserialize_unique_attribute_rewriters,
    "attribute_rewriters"
);
unique_set_deserializer!(deserialize_unique_script_rewriters, "script_rewriters");
unique_set_deserializer!(deserialize_unique_head_injectors, "head_injectors");
unique_set_deserializer!(deserialize_unique_post_processors, "post_processors");
unique_set_deserializer!(deserialize_unique_request_filters, "request_filters");
unique_set_deserializer!(deserialize_unique_providers, "providers");

impl IntegrationInventory {
    /// Parse a reviewed closed-schema inventory.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed TOML, unknown fields, an unsupported
    /// version, missing review attestation, or duplicate keyed rows.
    pub fn parse(source: &str) -> Result<Self, Report<IntegrationError>> {
        ensure_input_bound("integration manifest", source)?;
        let manifest = toml::from_str::<Manifest>(source).map_err(|error| {
            invalid_manifest(format!("malformed TOML or unknown field: {error}"))
        })?;
        if manifest.version != 1 {
            return Err(invalid_manifest("version must equal 1"));
        }
        if !manifest.reviewed {
            return Err(invalid_manifest("reviewed must be true"));
        }
        if manifest.loading_modes.len() > MAX_INVENTORY_ENTRIES
            || manifest.capabilities.len() > MAX_INVENTORY_ENTRIES
            || manifest.operational.len() > MAX_INVENTORY_ENTRIES
        {
            return Err(invalid_manifest("manifest row cardinality limit exceeded"));
        }
        let deploy_ids = unique_values(manifest.deploy_ids, "deploy_ids")?;
        let builder_ids = unique_values(manifest.builder_ids, "builder_ids")?;
        let plan_registration_ids =
            unique_values(manifest.plan_registration_ids, "plan_registration_ids")?;
        let profile_ids = unique_values(manifest.profile_ids, "profile_ids")?;
        let mediator_ids = unique_values(manifest.mediator_ids, "mediator_ids")?;
        let js_source_module_ids =
            unique_values(manifest.js_source_module_ids, "js_source_module_ids")?;
        let js_bundle_ids = unique_values(manifest.js_bundle_ids, "js_bundle_ids")?;
        let mut loading_modes = BTreeMap::new();
        for record in manifest.loading_modes {
            ensure_manifest_strings("loading mode", [&record.id])?;
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
        let mut capability_keys = BTreeSet::new();
        let mut capabilities = BTreeSet::new();
        for record in manifest.capabilities {
            ensure_manifest_strings(
                "capability",
                [&record.id, &record.predicate, &record.js_mode],
            )?;
            if !capability_keys.insert((record.id.clone(), record.predicate.clone())) {
                return Err(invalid_manifest(format!(
                    "duplicate capability key: {}/{}",
                    record.id, record.predicate
                )));
            }
            if !capabilities.insert(record) {
                return Err(invalid_manifest("duplicate capability row"));
            }
        }
        let operational_count = manifest.operational.len();
        let operational = manifest.operational.into_iter().collect::<BTreeSet<_>>();
        if operational.len() != operational_count {
            return Err(invalid_manifest("duplicate operational row"));
        }
        let mut operational_ids = BTreeSet::new();
        for record in &operational {
            ensure_manifest_strings(
                "operational",
                [
                    &record.id,
                    &record.status,
                    &record.owner,
                    &record.reviewed_at,
                ],
            )?;
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
            deploy_ids,
            builder_ids,
            plan_registration_ids,
            profile_ids,
            mediator_ids,
            js_source_module_ids,
            js_bundle_ids,
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
    let entrypoint_file = parse_source("validation entrypoints", sources.validation_entrypoints)?;
    let deploy_file = parse_source("deploy validation", sources.deploy_validation)?;
    let builders_file = parse_source("builder", sources.builders)?;
    let plan_file = parse_source("plan registration", sources.plan_registrations)?;
    let profiles_file = parse_source("profile", sources.profiles)?;
    let mediator_file = parse_source("mediator", sources.mediator)?;

    validate_validation_entrypoints(&entrypoint_file.items)?;

    let deploy = exact_top_level_function(
        &deploy_file.items,
        "validate_enabled_integrations",
        "deploy validation",
    )?;
    validate_closed_registration_grammar(deploy, "deploy", &[])?;
    validate_deploy_helper_bodies(&deploy_file.items)?;
    let mut deploy_visitor = DeployVisitor::default();
    deploy_visitor.visit_block(deploy);
    if let Some(detail) = deploy_visitor.error {
        return Err(invalid_manifest(detail));
    }

    let builder_ids = extract_builder_ids(&builders_file.items)?;
    let plan = exact_impl_function(
        &plan_file.items,
        "IntegrationRegistry",
        "with_plan",
        "plan registration",
    )?;
    let mut plan_visitor = PlanRegistrationVisitor::default();
    plan_visitor.visit_block(plan);
    if let Some(detail) = plan_visitor.error {
        return Err(invalid_manifest(detail));
    }
    validate_closed_registration_grammar(
        plan,
        "plan",
        &["debug_assert_eq", "format", "log::warn", "log::debug"],
    )?;

    let profile_ids = extract_profile_ids(&profiles_file.items)?;
    let mediator = exact_top_level_function(
        &mediator_file.items,
        "build_orchestrator_with_plan",
        "mediator registration",
    )?;
    let mut mediator_visitor = MediatorVisitor::default();
    mediator_visitor.visit_block(mediator);
    if let Some(detail) = mediator_visitor.error {
        return Err(invalid_manifest(detail));
    }
    validate_closed_registration_grammar(mediator, "mediator", &["log::info", "format"])?;

    let mut js_source_module_ids = BTreeSet::new();
    for id in sources
        .tracked_paths
        .iter()
        .filter_map(|path| js_module_id(path))
    {
        if !js_source_module_ids.insert(id.clone()) {
            return Err(invalid_manifest(format!(
                "duplicate JavaScript source module ID: {id}"
            )));
        }
    }
    let mut js_bundle_ids = js_source_module_ids.clone();
    if !js_bundle_ids.insert("core".to_owned()) {
        return Err(invalid_manifest("duplicate JavaScript bundle ID: core"));
    }

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

fn validate_deploy_helper_bodies(items: &[Item]) -> Result<(), Report<IntegrationError>> {
    let integration = items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "validate_integration" => Some(function),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !integration.is_empty()
        && (!matches!(integration.as_slice(), [function] if exact_validate_integration_body(function)))
    {
        return Err(invalid_manifest(
            "validate_integration helper differs from the authoritative body",
        ));
    }

    let prebid = items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "validate_prebid" => Some(function),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !prebid.is_empty()
        && (!matches!(prebid.as_slice(), [function] if exact_validate_prebid_body(function)))
    {
        return Err(invalid_manifest(
            "validate_prebid helper differs from the authoritative body",
        ));
    }
    Ok(())
}

fn exact_validate_integration_body(function: &syn::ItemFn) -> bool {
    let [syn::Stmt::Expr(Expr::MethodCall(map), None)] = function.block.stmts.as_slice() else {
        return false;
    };
    let Expr::MethodCall(config) = strip_expression_parens(&map.receiver) else {
        return false;
    };
    let exact_config = config.method == "integration_config"
        && is_ident_path(&config.receiver, "settings")
        && config.args.len() == 1
        && config
            .args
            .first()
            .is_some_and(|argument| is_ident_path(argument, "integration_id"))
        && config.turbofish.as_ref().is_some_and(|arguments| {
            matches!(arguments.args.iter().collect::<Vec<_>>().as_slice(),
                [syn::GenericArgument::Type(Type::Path(value))]
                    if value.qself.is_none() && value.path.is_ident("T"))
        });
    let exact_map = map.method == "map"
        && map.args.len() == 1
        && matches!(map.args.first().map(strip_expression_parens), Some(Expr::Closure(closure))
            if matches!(closure.inputs.iter().collect::<Vec<_>>().as_slice(),
                [syn::Pat::Ident(binding)] if binding.ident == "config" && binding.subpat.is_none())
                && matches!(strip_expression_parens(&closure.body), Expr::MethodCall(is_some)
                    if is_some.method == "is_some"
                        && is_some.args.is_empty()
                        && is_ident_path(&is_some.receiver, "config")));
    exact_config && exact_map
}

fn exact_validate_prebid_body(function: &syn::ItemFn) -> bool {
    let [
        syn::Stmt::Local(config),
        browser,
        syn::Stmt::Expr(Expr::Call(ownership), None),
    ] = function.block.stmts.as_slice()
    else {
        return false;
    };
    let exact_pattern = matches!(&config.pat, syn::Pat::TupleStruct(pattern)
        if pattern.path.is_ident("Some")
            && matches!(pattern.elems.iter().collect::<Vec<_>>().as_slice(),
                [syn::Pat::Ident(binding)] if binding.ident == "config" && binding.subpat.is_none()));
    let exact_initializer = config.init.as_ref().is_some_and(|init| {
        let Expr::Try(attempt) = strip_expression_parens(&init.expr) else {
            return false;
        };
        let Expr::MethodCall(call) = strip_expression_parens(&attempt.expr) else {
            return false;
        };
        let exact_call = call.method == "integration_config"
            && is_ident_path(&call.receiver, "settings")
            && call.args.len() == 1
            && call.args.first().and_then(literal_string).as_deref() == Some("prebid")
            && call.turbofish.as_ref().is_some_and(|arguments| {
                matches!(arguments.args.iter().collect::<Vec<_>>().as_slice(),
                    [syn::GenericArgument::Type(Type::Path(value))]
                        if value.qself.is_none()
                            && path_segments_equal(&value.path, &["prebid", "PrebidIntegrationConfig"]))
            });
        let exact_diverge = init.diverge.as_ref().is_some_and(|(_, expression)| {
            matches!(strip_expression_parens(expression), Expr::Block(block)
                if matches!(block.block.stmts.as_slice(), [syn::Stmt::Expr(Expr::Return(value), _)]
                    if matches!(value.expr.as_deref().map(strip_expression_parens), Some(Expr::Call(ok))
                        if matches!(ok.func.as_ref(), Expr::Path(path) if path.path.is_ident("Ok"))
                            && matches!(ok.args.iter().collect::<Vec<_>>().as_slice(),
                                [Expr::Tuple(tuple)] if tuple.elems.is_empty()))))
        });
        exact_call && exact_diverge
    });
    let exact_browser = matches!(browser, syn::Stmt::Expr(Expr::Try(attempt), Some(_))
        if matches!(strip_expression_parens(&attempt.expr), Expr::Call(call)
            if matches!(call.func.as_ref(), Expr::Path(path)
                if path.qself.is_none()
                    && path_segments_equal(&path.path, &["prebid", "validate_browser_config_for_startup"]))
                && call.args.len() == 2
                && matches!(call.args.first().map(strip_expression_parens), Some(Expr::Reference(reference))
                    if reference.mutability.is_none() && is_ident_path(&reference.expr, "config"))
                && matches!(call.args.iter().nth(1).map(strip_expression_parens), Some(Expr::Reference(reference))
                    if reference.mutability.is_none()
                        && matches!(strip_expression_parens(&reference.expr), Expr::Field(allowed)
                            if matches!(&allowed.member, syn::Member::Named(member) if member == "allowed_domains")
                                && matches!(strip_expression_parens(&allowed.base), Expr::Field(proxy)
                                    if is_ident_path(&proxy.base, "settings")
                                        && matches!(&proxy.member, syn::Member::Named(member) if member == "proxy"))))));
    let exact_ownership = matches!(ownership.func.as_ref(), Expr::Path(path)
        if path.qself.is_none()
            && path_segments_equal(&path.path, &["prebid", "validate_browser_bidder_ownership"]))
        && ownership.args.len() == 2
        && matches!(ownership.args.first().map(strip_expression_parens), Some(Expr::Reference(reference))
            if reference.mutability.is_none() && is_ident_path(&reference.expr, "config"))
        && ownership
            .args
            .iter()
            .nth(1)
            .is_some_and(|argument| is_ident_path(argument, "plan"));
    exact_pattern && exact_initializer && exact_browser && exact_ownership
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
        validation_entrypoints: &deploy,
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
    let mut expected_bundle_ids = inventory.js_source_module_ids.clone();
    expected_bundle_ids.insert("core".to_owned());
    if inventory.js_bundle_ids != expected_bundle_ids {
        return Err(Report::new(IntegrationError::InventoryDrift {
            axis: "js bundle domain",
        }));
    }
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
        .read_tracked_bounded(&path, MAX_INVENTORY_INPUT_BYTES)
        .map_err(|error| invalid_manifest(format!("cannot read inventory source: {error:?}")))?;
    String::from_utf8(bytes)
        .map_err(|error| invalid_manifest(format!("inventory source is not UTF-8: {error}")))
}

fn parse_source(label: &str, source: &str) -> Result<syn::File, Report<IntegrationError>> {
    ensure_input_bound(label, source)?;
    syn::parse_file(source)
        .map_err(|error| invalid_manifest(format!("invalid {label} Rust source: {error}")))
}

fn exact_top_level_function<'a>(
    items: &'a [Item],
    name: &str,
    label: &str,
) -> Result<&'a syn::Block, Report<IntegrationError>> {
    let mut found: Option<&syn::Block> = None;
    for item in items {
        let Item::Fn(function) = item else { continue };
        if function.sig.ident != name || !is_production_item(&function.attrs, label)? {
            continue;
        }
        if found.replace(function.block.as_ref()).is_some() {
            return Err(invalid_manifest(format!(
                "duplicate production {label} function"
            )));
        }
    }
    found.ok_or_else(|| invalid_manifest(format!("missing production {label} function")))
}

fn exact_impl_function<'a>(
    items: &'a [Item],
    owner: &str,
    name: &str,
    label: &str,
) -> Result<&'a syn::Block, Report<IntegrationError>> {
    let mut found: Option<&syn::Block> = None;
    for item in items {
        let Item::Impl(item_impl) = item else {
            continue;
        };
        let Type::Path(self_type) = item_impl.self_ty.as_ref() else {
            continue;
        };
        if item_impl.trait_.is_some()
            || self_type.path.segments.len() != 1
            || self_type.path.segments[0].ident != owner
            || !is_production_item(&item_impl.attrs, label)?
        {
            continue;
        }
        for item in &item_impl.items {
            let ImplItem::Fn(function) = item else {
                continue;
            };
            if function.sig.ident != name || !is_production_item(&function.attrs, label)? {
                continue;
            }
            if found.replace(&function.block).is_some() {
                return Err(invalid_manifest(format!(
                    "duplicate production {label} method"
                )));
            }
        }
    }
    found.ok_or_else(|| invalid_manifest(format!("missing production {label} method")))
}

fn is_production_item(
    attributes: &[Attribute],
    label: &str,
) -> Result<bool, Report<IntegrationError>> {
    let mut cfg_test = false;
    for attribute in attributes {
        if attribute.path().is_ident("cfg_attr") {
            return Err(invalid_manifest(format!(
                "unsupported conditional attribute on {label}"
            )));
        }
        if !attribute.path().is_ident("cfg") {
            continue;
        }
        let syn::Meta::List(list) = &attribute.meta else {
            return Err(invalid_manifest(format!("invalid cfg on {label}")));
        };
        if list.tokens.to_string() == "test" && !cfg_test {
            cfg_test = true;
        } else {
            return Err(invalid_manifest(format!(
                "unsupported production cfg on {label}"
            )));
        }
    }
    Ok(!cfg_test)
}

fn validate_validation_entrypoints(items: &[Item]) -> Result<(), Report<IntegrationError>> {
    exact_top_level_function(
        items,
        "validate_enabled_integrations",
        "validation entrypoints",
    )?;
    for (name, resolved_secrets) in [
        ("validate_settings_for_deploy", false),
        ("validate_settings_for_runtime", true),
    ] {
        let block = exact_top_level_function(items, name, name)?;
        validate_exact_validator_call(block, name, resolved_secrets)?;
    }
    Ok(())
}

fn validate_exact_validator_call(
    block: &syn::Block,
    owner: &str,
    resolved_secrets: bool,
) -> Result<(), Report<IntegrationError>> {
    #[derive(Default)]
    struct CallCount(usize);

    impl<'ast> syn::visit::Visit<'ast> for CallCount {
        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            if matches!(call.func.as_ref(), Expr::Path(path)
                if path.path.segments.last().is_some_and(|segment| segment.ident == "validate_enabled_integrations"))
            {
                self.0 += 1;
            }
            visit::visit_expr_call(self, call);
        }
    }

    let mut calls = CallCount::default();
    calls.visit_block(block);
    let direct = block
        .stmts
        .iter()
        .filter(|statement| exact_validator_call_statement(statement, resolved_secrets))
        .count();
    let direct_index = block
        .stmts
        .iter()
        .position(|statement| exact_validator_call_statement(statement, resolved_secrets));
    if calls.0 != 1
        || direct != 1
        || direct_index.is_none_or(|index| {
            !validator_call_prefix_is_linear(&block.stmts[..index])
                || block_shadows_validator_name(block)
        })
    {
        return Err(invalid_manifest(format!(
            "{owner} must contain exactly one direct validate_enabled_integrations call"
        )));
    }
    Ok(())
}

fn validator_call_prefix_is_linear(statements: &[syn::Stmt]) -> bool {
    #[derive(Default)]
    struct ControlFlow {
        invalid: bool,
    }

    impl<'ast> syn::visit::Visit<'ast> for ControlFlow {
        fn visit_expr(&mut self, expression: &'ast Expr) {
            if matches!(
                expression,
                Expr::If(_)
                    | Expr::Match(_)
                    | Expr::ForLoop(_)
                    | Expr::While(_)
                    | Expr::Loop(_)
                    | Expr::Return(_)
                    | Expr::Break(_)
                    | Expr::Continue(_)
                    | Expr::Macro(_)
            ) {
                self.invalid = true;
                return;
            }
            visit::visit_expr(self, expression);
        }
    }

    let mut flow = ControlFlow::default();
    for statement in statements {
        flow.visit_stmt(statement);
    }
    !flow.invalid
}

fn block_shadows_validator_name(block: &syn::Block) -> bool {
    #[derive(Default)]
    struct Shadows {
        found: bool,
    }

    impl<'ast> syn::visit::Visit<'ast> for Shadows {
        fn visit_item(&mut self, item: &'ast Item) {
            let shadows = match item {
                Item::Fn(value) => value.sig.ident == "validate_enabled_integrations",
                Item::Const(value) => value.ident == "validate_enabled_integrations",
                Item::Static(value) => value.ident == "validate_enabled_integrations",
                Item::Struct(value) => value.ident == "validate_enabled_integrations",
                Item::Use(value) => {
                    use_tree_binds_name(&value.tree, "validate_enabled_integrations")
                }
                _ => false,
            };
            if shadows {
                self.found = true;
                return;
            }
            visit::visit_item(self, item);
        }

        fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
            if pattern.ident == "validate_enabled_integrations" {
                self.found = true;
                return;
            }
            visit::visit_pat_ident(self, pattern);
        }
    }

    let mut shadows = Shadows::default();
    shadows.visit_block(block);
    shadows.found
}

fn use_tree_binds_name(tree: &syn::UseTree, name: &str) -> bool {
    match tree {
        syn::UseTree::Name(value) => value.ident == name,
        syn::UseTree::Rename(value) => value.rename == name,
        syn::UseTree::Path(value) => use_tree_binds_name(&value.tree, name),
        syn::UseTree::Group(value) => value
            .items
            .iter()
            .any(|item| use_tree_binds_name(item, name)),
        syn::UseTree::Glob(_) => true,
    }
}

fn exact_validator_call_statement(statement: &syn::Stmt, resolved_secrets: bool) -> bool {
    let syn::Stmt::Expr(Expr::Try(attempt), Some(_)) = statement else {
        return false;
    };
    let Expr::Call(call) = strip_expression_parens(&attempt.expr) else {
        return false;
    };
    matches!(call.func.as_ref(), Expr::Path(path)
        if path.qself.is_none()
            && path.path.is_ident("validate_enabled_integrations")
            && path.path.segments.iter().all(|segment| matches!(segment.arguments, syn::PathArguments::None)))
        && call.args.len() == 3
        && call
            .args
            .first()
            .is_some_and(|argument| is_ident_path(argument, "settings"))
        && matches!(call.args.iter().nth(1).map(strip_expression_parens), Some(Expr::Reference(reference))
            if reference.mutability.is_none() && is_ident_path(&reference.expr, "plan"))
        && matches!(call.args.iter().nth(2).map(strip_expression_parens), Some(Expr::Lit(ExprLit { lit: Lit::Bool(value), .. }))
            if value.value == resolved_secrets)
}

fn validate_closed_registration_grammar(
    block: &syn::Block,
    label: &'static str,
    allowed_macros: &[&str],
) -> Result<(), Report<IntegrationError>> {
    struct Guard<'a> {
        label: &'static str,
        allowed_macros: &'a [&'a str],
        error: Option<String>,
    }
    impl<'ast> syn::visit::Visit<'ast> for Guard<'_> {
        fn visit_attribute(&mut self, attribute: &'ast Attribute) {
            if attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr") {
                self.error = Some(format!(
                    "unsupported conditional statement in {} grammar",
                    self.label
                ));
            }
        }

        fn visit_macro(&mut self, item: &'ast syn::Macro) {
            let name = item
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            if !self.allowed_macros.contains(&name.as_str())
                || !safe_registration_macro(self.label, &name, item)
            {
                self.error = Some(format!(
                    "unsupported macro `{name}` in {} grammar",
                    self.label
                ));
            }
        }

        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            if let Expr::Path(path) = call.func.as_ref()
                && let Some(name) = path
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                && name.contains("register")
                && !matches!(
                    (self.label, name.as_str()),
                    ("plan", "register_for_plan") | ("mediator", "register_providers")
                )
            {
                self.error = Some(format!(
                    "unsupported indirect registration call `{name}` in {} grammar",
                    self.label
                ));
            }
            visit::visit_expr_call(self, call);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            let name = call.method.to_string();
            if name.contains("register") {
                self.error = Some(format!(
                    "unsupported registration method `{name}` in {} grammar",
                    self.label
                ));
            }
            visit::visit_expr_method_call(self, call);
        }
    }
    let mut guard = Guard {
        label,
        allowed_macros,
        error: None,
    };
    guard.visit_block(block);
    if let Some(detail) = guard.error {
        return Err(invalid_manifest(detail));
    }

    for statement in &block.stmts {
        let count = statement_registration_count(statement, label);
        if count == 0 {
            if !accepted_nonregistration_statement(statement, label, allowed_macros) {
                return Err(invalid_manifest(format!(
                    "unsupported {label}{} statement",
                    if label == "deploy" {
                        " validator"
                    } else {
                        " grammar"
                    }
                )));
            }
            continue;
        }
        let accepted = match (label, statement) {
            ("deploy", syn::Stmt::Expr(Expr::Try(expression), _)) => {
                matches!(expression.expr.as_ref(), Expr::Call(call) if deploy_direct_call(call))
            }
            ("deploy", syn::Stmt::Expr(Expr::If(expression), _)) => {
                deploy_datadome_branch_is_bound(expression)
            }
            ("plan", syn::Stmt::Expr(Expr::If(expression), _)) => {
                plan_registration_is_bound(expression)
            }
            ("mediator", syn::Stmt::Local(local)) => mediator_registration_is_bound(local),
            _ => false,
        };
        if count > 1 {
            return Err(invalid_manifest(format!("duplicate {label} registration")));
        }
        if !accepted {
            return Err(invalid_manifest(format!(
                "unsupported hidden registration in {label} grammar"
            )));
        }
    }
    if label == "plan" {
        validate_plan_production_dataflow(block)?;
    } else if label == "mediator" {
        validate_mediator_production_dataflow(block)?;
    }
    Ok(())
}

fn safe_registration_macro(label: &str, name: &str, item: &syn::Macro) -> bool {
    let parser = Punctuated::<Expr, Token![,]>::parse_terminated;
    let Ok(arguments) = parser.parse2(item.tokens.clone()) else {
        return false;
    };
    let arguments = arguments.iter().collect::<Vec<_>>();
    match (label, name, arguments.as_slice()) {
        ("plan", "debug_assert_eq", [left, right]) => {
            exact_field(left, "registration", "integration_id")
                && exact_field(right, "builder", "id")
        }
        ("plan", "format", [message, path])
            if literal_string(message).as_deref() == Some("{}/{{*rest}}") =>
        {
            exact_matchit_path_expression(path)
        }
        ("plan", "format", [message, method, path, error]) => {
            literal_string(message).as_deref()
                == Some("Integration route registration failed for {} {}: {:?}")
                && exact_field(method, "route", "method")
                && exact_field(path, "route", "path")
                && is_ident_path(error, "e")
        }
        ("plan", "log::warn", [message, method, path]) => {
            literal_string(message).as_deref() == Some("Unsupported HTTP method {} for route {}")
                && exact_field(method, "route", "method")
                && exact_field(path, "route", "path")
        }
        ("mediator", "log::info", [message]) => {
            literal_string(message).as_deref() == Some("Building plan-backed auction orchestrator")
        }
        ("mediator", "log::info", [message, count]) => {
            literal_string(message).as_deref()
                == Some("Auction orchestrator built with {} bidder providers")
                && matches!(strip_expression_parens(count), Expr::MethodCall(call)
                    if call.method == "provider_count" && call.args.is_empty() && is_ident_path(&call.receiver, "orchestrator"))
        }
        ("mediator", "format", [message]) => {
            literal_string(message).as_deref()
                == Some(
                    "auction mediator `{expected_id}` must reference a separately registered enabled integration with the exact same ID",
                )
        }
        _ => false,
    }
}

fn exact_field(expression: &Expr, base: &str, member: &str) -> bool {
    matches!(strip_expression_parens(expression), Expr::Field(field)
        if is_ident_path(&field.base, base)
            && matches!(&field.member, Member::Named(name) if name == member))
}

fn exact_matchit_path_expression(expression: &Expr) -> bool {
    let Expr::MethodCall(expect) = strip_expression_parens(expression) else {
        return false;
    };
    if expect.method != "expect"
        || expect.args.len() != 1
        || expect.args.first().and_then(literal_string).as_deref()
            != Some("path should end with '/*'")
    {
        return false;
    }
    matches!(strip_expression_parens(&expect.receiver), Expr::MethodCall(strip)
        if strip.method == "strip_suffix"
            && exact_field(&strip.receiver, "route", "path")
            && strip.args.len() == 1
            && strip.args.first().and_then(literal_string).as_deref() == Some("/*"))
}

fn deploy_datadome_branch_is_bound(expression: &syn::ExprIf) -> bool {
    let Expr::Let(condition) = strip_expression_parens(&expression.cond) else {
        return false;
    };
    let syn::Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
        return false;
    };
    if pattern
        .path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "Some")
        || pattern.elems.len() != 1
        || !matches!(pattern.elems.first(), Some(syn::Pat::Ident(binding)) if binding.ident == "config")
    {
        return false;
    }
    let Expr::Try(lookup) = strip_expression_parens(&condition.expr) else {
        return false;
    };
    let Expr::MethodCall(lookup) = strip_expression_parens(&lookup.expr) else {
        return false;
    };
    if lookup.method != "integration_config"
        || !is_ident_path(&lookup.receiver, "settings")
        || !exact_method_type_argument(lookup, &["DataDomeConfig"])
        || lookup.args.len() != 1
        || lookup.args.first().and_then(literal_string).as_deref() != Some("datadome")
        || expression.else_branch.is_some()
        || expression.then_branch.stmts.len() != 1
    {
        return false;
    }
    let Some(syn::Stmt::Expr(Expr::If(validation), _)) = expression.then_branch.stmts.first()
    else {
        return false;
    };
    is_ident_path(&validation.cond, "resolved_secrets")
        && validation.then_branch.stmts.len() == 1
        && validation
            .then_branch
            .stmts
            .first()
            .is_some_and(|statement| {
                exact_try_call_statement(statement, "validate_config_for_startup", "config")
            })
        && validation.else_branch.as_ref().is_some_and(|(_, branch)| {
            matches!(strip_expression_parens(branch), Expr::Block(block)
                if block.block.stmts.len() == 1
                    && block.block.stmts.first().is_some_and(|statement| exact_try_call_statement(statement, "validate_config_for_deploy", "config")))
        })
}

fn exact_try_call_statement(statement: &syn::Stmt, function: &str, argument: &str) -> bool {
    let syn::Stmt::Expr(Expr::Try(value), _) = statement else {
        return false;
    };
    let Expr::Call(call) = strip_expression_parens(&value.expr) else {
        return false;
    };
    let expected = match function {
        "validate_config_for_startup" => &[
            "crate",
            "integrations",
            "datadome",
            "DataDomeIntegration",
            "validate_config_for_startup",
        ][..],
        "validate_config_for_deploy" => &[
            "crate",
            "integrations",
            "datadome",
            "DataDomeIntegration",
            "validate_config_for_deploy",
        ][..],
        _ => return false,
    };
    matches!(call.func.as_ref(), Expr::Path(path)
        if path.qself.is_none() && path_segments_equal(&path.path, expected))
        && call.args.len() == 1
        && call
            .args
            .first()
            .is_some_and(|value| is_ident_path(value, argument))
}

fn validate_plan_production_dataflow(block: &syn::Block) -> Result<(), Report<IntegrationError>> {
    if block.stmts.is_empty() {
        return Ok(());
    }
    if block.stmts.len() < 5
        || !exact_named_local(
            &block.stmts[0],
            "inner",
            &["IntegrationRegistryInner", "default"],
        )
        || !exact_named_local(&block.stmts[1], "registrations", &["Vec", "new"])
    {
        return Err(invalid_manifest(
            "plan registration production dataflow differs from the closed shape",
        ));
    }
    let registration_end = block.stmts.len() - 3;
    if !block.stmts[2..registration_end].iter().all(|statement| {
        matches!(statement, syn::Stmt::Expr(Expr::If(value), _) if plan_registration_is_bound(value))
    }) || !matches!(&block.stmts[registration_end], syn::Stmt::Expr(Expr::ForLoop(value), _) if exact_builder_registration_loop(value))
        || !matches!(&block.stmts[registration_end + 1], syn::Stmt::Expr(Expr::ForLoop(value), _) if exact_registration_consumption_loop(value))
        || !exact_plan_return(&block.stmts[registration_end + 2])
    {
        return Err(invalid_manifest(
            "plan registration production dataflow differs from the closed shape",
        ));
    }
    Ok(())
}

fn exact_named_local(statement: &syn::Stmt, name: &str, initializer: &[&str]) -> bool {
    let syn::Stmt::Local(local) = statement else {
        return false;
    };
    local.attrs.is_empty()
        && matches!(&local.pat, syn::Pat::Ident(binding)
            if binding.ident == name && binding.by_ref.is_none() && binding.subpat.is_none())
        && local.init.as_ref().is_some_and(|init| {
            matches!(strip_expression_parens(&init.expr), Expr::Call(call)
                if matches!(call.func.as_ref(), Expr::Path(path)
                    if path.qself.is_none()
                        && path_segments_equal(&path.path, initializer)
                        && path.path.segments.iter().all(|segment| matches!(segment.arguments, syn::PathArguments::None)))
                    && call.args.is_empty())
        })
}

fn exact_builder_registration_loop(expression: &syn::ExprForLoop) -> bool {
    matches!(expression.pat.as_ref(), syn::Pat::Ident(binding) if binding.ident == "builder")
        && matches!(strip_expression_parens(&expression.expr), Expr::Call(call)
            if exact_plain_call(call, &["crate", "integrations", "builders"], &[]))
        && expression.body.stmts.len() == 1
        && matches!(expression.body.stmts.first(), Some(syn::Stmt::Expr(Expr::If(branch), _))
            if plan_builder_branch_is_bound(branch))
}

fn plan_builder_branch_is_bound(expression: &syn::ExprIf) -> bool {
    let Expr::Let(condition) = strip_expression_parens(&expression.cond) else {
        return false;
    };
    let syn::Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
        return false;
    };
    let binding_ok = pattern
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Some")
        && matches!(pattern.elems.first(), Some(syn::Pat::Ident(binding)) if binding.ident == "registration");
    let Expr::Try(call) = strip_expression_parens(&condition.expr) else {
        return false;
    };
    let Expr::Call(call) = strip_expression_parens(&call.expr) else {
        return false;
    };
    let builder_call = matches!(strip_expression_parens(&call.func), Expr::Field(field)
        if is_ident_path(&field.base, "builder") && matches!(&field.member, Member::Named(name) if name == "build"))
        && call.args.len() == 1
        && call
            .args
            .first()
            .is_some_and(|value| is_ident_path(value, "settings"));
    binding_ok
        && builder_call
        && expression.else_branch.is_none()
        && expression.then_branch.stmts.len() == 2
        && matches!(expression.then_branch.stmts.first(), Some(syn::Stmt::Macro(statement))
            if statement.mac.path.is_ident("debug_assert_eq") && !statement.mac.tokens.to_string().contains("register"))
        && expression
            .then_branch
            .stmts
            .get(1)
            .is_some_and(exact_registration_push)
}

fn exact_registration_push(statement: &syn::Stmt) -> bool {
    matches!(statement, syn::Stmt::Expr(Expr::MethodCall(call), _)
        if call.method == "push"
            && is_ident_path(&call.receiver, "registrations")
            && call.args.len() == 1
            && call.args.first().is_some_and(|value| is_ident_path(value, "registration")))
}

fn exact_registration_consumption_loop(expression: &syn::ExprForLoop) -> bool {
    if !matches!(expression.pat.as_ref(), syn::Pat::Ident(binding) if binding.ident == "registration")
        || !is_ident_path(&expression.expr, "registrations")
        || expression.body.stmts.len() != 8
    {
        return false;
    }
    exact_inner_push(
        &expression.body.stmts[0],
        "enabled_integration_ids",
        "integration_id",
    ) && exact_proxy_registration_loop(&expression.body.stmts[1])
        && exact_inner_extend(
            &expression.body.stmts[2],
            "html_rewriters",
            "attribute_rewriters",
        )
        && exact_inner_extend(
            &expression.body.stmts[3],
            "script_rewriters",
            "script_rewriters",
        )
        && exact_inner_extend(
            &expression.body.stmts[4],
            "html_post_processors",
            "html_post_processors",
        )
        && exact_inner_extend(
            &expression.body.stmts[5],
            "head_injectors",
            "head_injectors",
        )
        && exact_inner_extend(
            &expression.body.stmts[6],
            "request_filters",
            "request_filters",
        )
        && exact_js_disposition_branch(&expression.body.stmts[7])
}

fn exact_inner_push(statement: &syn::Stmt, target: &str, source: &str) -> bool {
    matches!(statement, syn::Stmt::Expr(Expr::MethodCall(call), _)
        if call.method == "push"
            && matches!(strip_expression_parens(&call.receiver), Expr::Field(field)
                if is_ident_path(&field.base, "inner") && matches!(&field.member, Member::Named(name) if name == target))
            && call.args.len() == 1
            && call.args.first().is_some_and(|argument| matches!(strip_expression_parens(argument), Expr::Field(field)
                if is_ident_path(&field.base, "registration") && matches!(&field.member, Member::Named(name) if name == source))))
}

fn exact_inner_extend(statement: &syn::Stmt, target: &str, source: &str) -> bool {
    matches!(statement, syn::Stmt::Expr(Expr::MethodCall(call), _)
        if call.method == "extend"
            && matches!(strip_expression_parens(&call.receiver), Expr::Field(field)
                if is_ident_path(&field.base, "inner") && matches!(&field.member, Member::Named(name) if name == target))
            && call.args.len() == 1
            && call.args.first().is_some_and(|argument| matches!(strip_expression_parens(argument), Expr::Field(field)
                if is_ident_path(&field.base, "registration") && matches!(&field.member, Member::Named(name) if name == source))))
}

fn exact_proxy_registration_loop(statement: &syn::Stmt) -> bool {
    let syn::Stmt::Expr(Expr::ForLoop(proxy), _) = statement else {
        return false;
    };
    if !matches!(proxy.pat.as_ref(), syn::Pat::Ident(binding) if binding.ident == "proxy")
        || !matches!(strip_expression_parens(&proxy.expr), Expr::Field(field)
            if is_ident_path(&field.base, "registration") && matches!(&field.member, Member::Named(name) if name == "proxies"))
        || proxy.body.stmts.len() != 1
    {
        return false;
    }
    let Some(syn::Stmt::Expr(Expr::ForLoop(routes), _)) = proxy.body.stmts.first() else {
        return false;
    };
    matches!(routes.pat.as_ref(), syn::Pat::Ident(binding) if binding.ident == "route")
        && matches!(strip_expression_parens(&routes.expr), Expr::MethodCall(call)
            if call.method == "routes" && call.args.is_empty() && is_ident_path(&call.receiver, "proxy"))
        && routes.body.stmts.len() == 5
        && exact_proxy_value_local(&routes.body.stmts[0])
        && exact_matchit_path_local(&routes.body.stmts[1])
        && exact_proxy_router_local(&routes.body.stmts[2])
        && exact_proxy_insert_branch(&routes.body.stmts[3])
        && exact_registered_route_push(&routes.body.stmts[4])
}

fn exact_local_initializer<'a>(statement: &'a syn::Stmt, name: &str) -> Option<&'a Expr> {
    let syn::Stmt::Local(local) = statement else {
        return None;
    };
    if local.attrs.is_empty()
        && matches!(&local.pat, syn::Pat::Ident(binding)
            if binding.ident == name && binding.subpat.is_none())
        && local
            .init
            .as_ref()
            .is_some_and(|init| init.diverge.is_none())
    {
        local.init.as_ref().map(|init| init.expr.as_ref())
    } else {
        None
    }
}

fn exact_proxy_value_local(statement: &syn::Stmt) -> bool {
    let Some(Expr::Tuple(value)) =
        exact_local_initializer(statement, "value").map(strip_expression_parens)
    else {
        return false;
    };
    value.elems.len() == 2
        && matches!(value.elems.first().map(strip_expression_parens), Some(Expr::MethodCall(call))
            if call.method == "clone" && call.args.is_empty() && is_ident_path(&call.receiver, "proxy"))
        && value
            .elems
            .iter()
            .nth(1)
            .is_some_and(|value| exact_field(value, "registration", "integration_id"))
}

fn exact_matchit_path_local(statement: &syn::Stmt) -> bool {
    let Some(Expr::If(branch)) =
        exact_local_initializer(statement, "matchit_path").map(strip_expression_parens)
    else {
        return false;
    };
    let exact_condition = matches!(strip_expression_parens(&branch.cond), Expr::MethodCall(call)
        if call.method == "ends_with"
            && exact_field(&call.receiver, "route", "path")
            && call.args.len() == 1
            && call.args.first().and_then(literal_string).as_deref() == Some("/*"));
    let exact_then = branch
        .then_branch
        .stmts
        .as_slice()
        .first()
        .filter(|_| branch.then_branch.stmts.len() == 1)
        .and_then(statement_macro)
        .is_some_and(|item| {
            item.path.is_ident("format") && safe_registration_macro("plan", "format", item)
        });
    let exact_else = branch.else_branch.as_ref().is_some_and(|(_, alternative)| {
        matches!(strip_expression_parens(alternative), Expr::Block(block)
            if matches!(block.block.stmts.as_slice(), [syn::Stmt::Expr(Expr::MethodCall(call), None)]
                if call.method == "clone"
                    && call.args.is_empty()
                    && exact_field(&call.receiver, "route", "path")))
    });
    exact_condition && exact_then && exact_else
}

fn exact_proxy_router_local(statement: &syn::Stmt) -> bool {
    let Some(Expr::Match(selection)) =
        exact_local_initializer(statement, "router").map(strip_expression_parens)
    else {
        return false;
    };
    if !exact_field(&selection.expr, "route", "method") || selection.arms.len() != 8 {
        return false;
    }
    let expected = [
        ("GET", "get_router"),
        ("POST", "post_router"),
        ("PUT", "put_router"),
        ("DELETE", "delete_router"),
        ("PATCH", "patch_router"),
        ("HEAD", "head_router"),
        ("OPTIONS", "options_router"),
    ];
    selection.arms[..7]
        .iter()
        .zip(expected)
        .all(|(arm, (method, router))| exact_router_arm(arm, method, router))
        && exact_unsupported_method_arm(&selection.arms[7])
}

fn statement_macro(statement: &syn::Stmt) -> Option<&syn::Macro> {
    match statement {
        syn::Stmt::Macro(statement) => Some(&statement.mac),
        syn::Stmt::Expr(Expr::Macro(expression), None) => Some(&expression.mac),
        _ => None,
    }
}

fn exact_router_arm(arm: &syn::Arm, method: &str, router: &str) -> bool {
    let exact_pattern = matches!(&arm.pat, syn::Pat::Path(pattern) if {
        let segments = pattern.path.segments.iter().collect::<Vec<_>>();
        segments.len() == 2 && segments[0].ident == "Method" && segments[1].ident == method
    });
    exact_pattern
        && arm.attrs.is_empty()
        && arm.guard.is_none()
        && matches!(strip_expression_parens(&arm.body), Expr::Reference(reference)
            if reference.mutability.is_some()
                && matches!(strip_expression_parens(&reference.expr), Expr::Field(field)
                    if is_ident_path(&field.base, "inner")
                        && matches!(&field.member, Member::Named(name) if name == router)))
}

fn exact_unsupported_method_arm(arm: &syn::Arm) -> bool {
    matches!(&arm.pat, syn::Pat::Wild(_))
        && arm.attrs.is_empty()
        && arm.guard.is_none()
        && matches!(strip_expression_parens(&arm.body), Expr::Block(block)
            if matches!(block.block.stmts.as_slice(),
                [syn::Stmt::Macro(statement), syn::Stmt::Expr(Expr::Continue(_), Some(_))]
                if statement.mac.path.segments.iter().map(|segment| segment.ident.to_string()).collect::<Vec<_>>().join("::") == "log::warn"
                    && safe_registration_macro("plan", "log::warn", &statement.mac)))
}

fn exact_proxy_insert_branch(statement: &syn::Stmt) -> bool {
    let syn::Stmt::Expr(Expr::If(branch), _) = statement else {
        return false;
    };
    let Expr::Let(condition) = strip_expression_parens(&branch.cond) else {
        return false;
    };
    let exact_pattern = matches!(condition.pat.as_ref(), syn::Pat::TupleStruct(pattern)
        if pattern.path.segments.last().is_some_and(|segment| segment.ident == "Err")
            && pattern.elems.len() == 1
            && matches!(pattern.elems.first(), Some(syn::Pat::Ident(binding)) if binding.ident == "e"));
    let exact_insert = matches!(strip_expression_parens(&condition.expr), Expr::MethodCall(call)
        if call.method == "insert"
            && is_ident_path(&call.receiver, "router")
            && call.args.len() == 2
            && matches!(call.args.first().map(strip_expression_parens), Some(Expr::Reference(reference))
                if reference.mutability.is_none() && is_ident_path(&reference.expr, "matchit_path"))
            && call.args.iter().nth(1).is_some_and(|value| is_ident_path(value, "value")));
    exact_pattern
        && exact_insert
        && branch.else_branch.is_none()
        && matches!(branch.then_branch.stmts.as_slice(), [statement]
            if exact_proxy_insert_error(statement))
}

fn exact_proxy_insert_error(statement: &syn::Stmt) -> bool {
    let syn::Stmt::Expr(Expr::Return(value), _) = statement else {
        return false;
    };
    let Some(Expr::Call(error)) = value.expr.as_deref().map(strip_expression_parens) else {
        return false;
    };
    if !call_named(error, "Err") || error.args.len() != 1 {
        return false;
    }
    let Some(Expr::Call(report)) = error.args.first().map(strip_expression_parens) else {
        return false;
    };
    if !matches!(report.func.as_ref(), Expr::Path(path) if path.path.segments.iter().map(|segment| segment.ident.to_string()).collect::<Vec<_>>().as_slice() == ["Report", "new"])
        || report.args.len() != 1
    {
        return false;
    }
    let Some(Expr::Struct(configuration)) = report.args.first().map(strip_expression_parens) else {
        return false;
    };
    configuration
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Configuration")
        && configuration.rest.is_none()
        && matches!(configuration.fields.iter().collect::<Vec<_>>().as_slice(), [field]
            if member_is(&field.member, "message")
                && matches!(strip_expression_parens(&field.expr), Expr::Macro(value)
                    if value.mac.path.is_ident("format")
                        && safe_registration_macro("plan", "format", &value.mac)))
}

fn exact_registered_route_push(statement: &syn::Stmt) -> bool {
    matches!(statement, syn::Stmt::Expr(Expr::MethodCall(call), _)
        if call.method == "push"
            && matches!(strip_expression_parens(&call.receiver), Expr::Field(field)
                if is_ident_path(&field.base, "inner")
                    && matches!(&field.member, Member::Named(name) if name == "routes"))
            && call.args.len() == 1
            && matches!(call.args.first().map(strip_expression_parens), Some(Expr::Tuple(tuple))
                if tuple.elems.len() == 2
                    && tuple.elems.first().is_some_and(|value| is_ident_path(value, "route"))
                    && tuple.elems.iter().nth(1).is_some_and(|value| exact_field(value, "registration", "integration_id"))))
}

fn exact_js_disposition_branch(statement: &syn::Stmt) -> bool {
    let syn::Stmt::Expr(Expr::If(disabled), _) = statement else {
        return false;
    };
    matches!(strip_expression_parens(&disabled.cond), Expr::Field(field)
        if is_ident_path(&field.base, "registration") && matches!(&field.member, Member::Named(name) if name == "js_disabled"))
        && disabled.then_branch.stmts.len() == 1
        && exact_inner_push(&disabled.then_branch.stmts[0], "disabled_js_ids", "integration_id")
        && disabled.else_branch.as_ref().is_some_and(|(_, branch)| {
            matches!(strip_expression_parens(branch), Expr::If(deferred)
                if matches!(strip_expression_parens(&deferred.cond), Expr::Field(field)
                    if is_ident_path(&field.base, "registration") && matches!(&field.member, Member::Named(name) if name == "js_deferred"))
                    && deferred.then_branch.stmts.len() == 1
                    && exact_inner_push(&deferred.then_branch.stmts[0], "deferred_js_ids", "integration_id")
                    && deferred.else_branch.is_none())
        })
}

fn exact_plan_return(statement: &syn::Stmt) -> bool {
    let syn::Stmt::Expr(Expr::Call(call), None) = statement else {
        return false;
    };
    if !exact_call_path(call, &["Ok"]) || call.args.len() != 1 {
        return false;
    }
    let Some(Expr::Struct(record)) = call.args.first() else {
        return false;
    };
    record.path.leading_colon.is_none()
        && path_segments_equal(&record.path, &["Self"])
        && record
            .path
            .segments
            .iter()
            .all(|segment| matches!(segment.arguments, syn::PathArguments::None))
        && record.rest.is_none()
        && record.fields.len() == 2
        && record.fields.iter().any(|field| {
            member_is(&field.member, "inner")
                && matches!(strip_expression_parens(&field.expr), Expr::Call(value)
                    if exact_call_path(value, &["Arc", "new"])
                        && value.args.len() == 1
                        && value.args.first().is_some_and(|argument| is_ident_path(argument, "inner")))
        })
        && record.fields.iter().any(|field| {
            member_is(&field.member, "plan")
                && matches!(strip_expression_parens(&field.expr), Expr::Call(value)
                    if exact_call_path(value, &["Some"])
                        && value.args.len() == 1
                        && value.args.first().is_some_and(|argument| is_ident_path(argument, "plan")))
        })
}

fn exact_call_path(call: &ExprCall, expected: &[&str]) -> bool {
    matches!(call.func.as_ref(), Expr::Path(path)
        if path.qself.is_none()
            && path_segments_equal(&path.path, expected)
            && path.path.segments.iter().all(|segment| matches!(segment.arguments, syn::PathArguments::None)))
}

fn plan_registration_is_bound(expression: &syn::ExprIf) -> bool {
    let Expr::Let(condition) = strip_expression_parens(&expression.cond) else {
        return false;
    };
    let syn::Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
        return false;
    };
    if pattern
        .path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "Some")
        || pattern.elems.len() != 1
    {
        return false;
    }
    let Some(syn::Pat::Ident(binding)) = pattern.elems.first() else {
        return false;
    };
    let Expr::Try(call) = strip_expression_parens(&condition.expr) else {
        return false;
    };
    let Expr::Call(call) = strip_expression_parens(&call.expr) else {
        return false;
    };
    let exact_registration = matches!(call.func.as_ref(), Expr::Path(path)
        if path.qself.is_none()
            && matches!(path.path.segments.iter().map(|segment| segment.ident.to_string()).collect::<Vec<_>>().as_slice(),
                [root, integrations, module, function]
                if root == "crate"
                    && integrations == "integrations"
                    && matches!(module.as_str(), "prebid" | "aps")
                    && function == "register_for_plan")
            && path.path.segments.iter().all(|segment| matches!(segment.arguments, syn::PathArguments::None)));
    if !exact_registration
        || call.args.len() != 2
        || call
            .args
            .first()
            .is_none_or(|argument| !is_ident_path(argument, "settings"))
        || call.args.iter().nth(1).is_none_or(|argument| {
            !matches!(strip_expression_parens(argument), Expr::Reference(reference)
                if reference.mutability.is_none() && is_ident_path(&reference.expr, "plan"))
        })
    {
        return false;
    }
    expression.then_branch.stmts.len() == 1
        && expression.then_branch.stmts.first().is_some_and(|statement| {
            matches!(statement, syn::Stmt::Expr(Expr::MethodCall(call), _)
                if call.method == "push"
                    && is_ident_path(&call.receiver, "registrations")
                    && call.args.len() == 1
                    && call.args.first().is_some_and(|argument| is_ident_path(argument, &binding.ident.to_string())))
        })
        && expression.else_branch.is_none()
}

fn mediator_registration_is_bound(local: &syn::Local) -> bool {
    if !matches!(&local.pat, syn::Pat::Ident(binding) if binding.ident == "mediator") {
        return false;
    }
    let Some(init) = &local.init else {
        return false;
    };
    let Expr::If(expression) = strip_expression_parens(&init.expr) else {
        return false;
    };
    let Expr::Let(condition) = strip_expression_parens(&expression.cond) else {
        return false;
    };
    let syn::Pat::TupleStruct(pattern) = condition.pat.as_ref() else {
        return false;
    };
    if pattern
        .path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "Some")
        || !matches!(pattern.elems.first(), Some(syn::Pat::Ident(binding)) if binding.ident == "expected_id" || binding.ident == "expected")
        || !matches!(strip_expression_parens(&condition.expr), Expr::MethodCall(call)
            if call.method == "mediator" && call.args.is_empty() && is_ident_path(&call.receiver, "plan"))
        || expression.then_branch.stmts.len() != 2
    {
        return false;
    }
    let provider = expression.then_branch.stmts.iter().find_map(|statement| {
        let syn::Stmt::Local(provider) = statement else {
            return None;
        };
        let syn::Pat::Ident(binding) = &provider.pat else {
            return None;
        };
        let init = provider.init.as_ref()?;
        exact_mediator_provider_initializer(&init.expr).then_some(&binding.ident)
    });
    let Some(provider) = provider else {
        return false;
    };
    let Some(syn::Stmt::Expr(Expr::Call(some), None)) = expression.then_branch.stmts.last() else {
        return false;
    };
    call_named(some, "Some")
        && some.args.len() == 1
        && some.args.first().is_some_and(|argument| matches!(strip_expression_parens(argument), Expr::Path(path) if path.path.is_ident(provider)))
        && expression.else_branch.as_ref().is_some_and(|(_, alternative)| {
            matches!(strip_expression_parens(alternative), Expr::Block(block)
                if matches!(block.block.stmts.as_slice(), [syn::Stmt::Expr(Expr::Path(path), None)]
                    if path.qself.is_none() && path.path.is_ident("None")))
        })
}

fn exact_mediator_provider_initializer(expression: &Expr) -> bool {
    let Expr::Try(required) = strip_expression_parens(expression) else {
        return false;
    };
    let Expr::MethodCall(required) = strip_expression_parens(&required.expr) else {
        return false;
    };
    if required.method != "ok_or_else" || required.args.len() != 1 {
        return false;
    }
    let Expr::Closure(error) = required
        .args
        .first()
        .map(strip_expression_parens)
        .unwrap_or(&required.receiver)
    else {
        return false;
    };
    if !exact_mediator_error_closure(error) {
        return false;
    }
    let Expr::MethodCall(find) = strip_expression_parens(&required.receiver) else {
        return false;
    };
    if find.method != "find" || find.args.len() != 1 {
        return false;
    }
    let Some(Expr::Closure(predicate)) = find.args.first().map(strip_expression_parens) else {
        return false;
    };
    if !matches!(predicate.inputs.first(), Some(syn::Pat::Ident(binding)) if binding.ident == "provider")
        || !matches!(strip_expression_parens(&predicate.body), Expr::Binary(equal)
            if matches!(equal.op, syn::BinOp::Eq(_))
                && matches!(strip_expression_parens(&equal.left), Expr::MethodCall(call)
                    if call.method == "provider_name" && call.args.is_empty() && is_ident_path(&call.receiver, "provider"))
                && is_ident_path(&equal.right, "expected_id"))
    {
        return false;
    }
    let Expr::MethodCall(iter) = strip_expression_parens(&find.receiver) else {
        return false;
    };
    if iter.method != "into_iter" || !iter.args.is_empty() {
        return false;
    }
    let Expr::Try(register) = strip_expression_parens(&iter.receiver) else {
        return false;
    };
    let Expr::Call(register) = strip_expression_parens(&register.expr) else {
        return false;
    };
    exact_plain_call(
        register,
        &[
            "crate",
            "integrations",
            "adserver_mock",
            "register_providers",
        ],
        &["settings"],
    )
}

fn exact_mediator_error_closure(closure: &syn::ExprClosure) -> bool {
    if !closure.attrs.is_empty()
        || closure.asyncness.is_some()
        || closure.capture.is_some()
        || closure.constness.is_some()
        || !closure.inputs.is_empty()
        || !matches!(closure.output, syn::ReturnType::Default)
    {
        return false;
    }
    let Expr::Block(block) = strip_expression_parens(&closure.body) else {
        return false;
    };
    let [syn::Stmt::Expr(Expr::Call(report), None)] = block.block.stmts.as_slice() else {
        return false;
    };
    if !exact_call_path(report, &["Report", "new"]) || report.args.len() != 1 {
        return false;
    }
    let Some(Expr::Struct(configuration)) = report.args.first().map(strip_expression_parens) else {
        return false;
    };
    configuration.path.leading_colon.is_none()
        && path_segments_equal(
            &configuration.path,
            &["TrustedServerError", "Configuration"],
        )
        && configuration
            .path
            .segments
            .iter()
            .all(|segment| matches!(segment.arguments, syn::PathArguments::None))
        && configuration.rest.is_none()
        && matches!(configuration.fields.iter().collect::<Vec<_>>().as_slice(), [field]
            if field.attrs.is_empty()
                && member_is(&field.member, "message")
                && matches!(strip_expression_parens(&field.expr), Expr::Macro(message)
                    if message.attrs.is_empty()
                        && message.mac.path.is_ident("format")
                        && safe_registration_macro("mediator", "format", &message.mac)))
}

fn validate_mediator_production_dataflow(
    block: &syn::Block,
) -> Result<(), Report<IntegrationError>> {
    if block.stmts.is_empty() {
        return Ok(());
    }
    let exact = block.stmts.len() == 5
        && matches!(&block.stmts[0], syn::Stmt::Macro(statement)
            if safe_registration_macro("mediator", "log::info", &statement.mac))
        && matches!(&block.stmts[1], syn::Stmt::Local(local) if mediator_registration_is_bound(local))
        && matches!(&block.stmts[2], syn::Stmt::Local(local)
            if matches!(&local.pat, syn::Pat::Ident(binding) if binding.ident == "orchestrator")
                && local.init.as_ref().is_some_and(|init| matches!(strip_expression_parens(&init.expr), Expr::Call(call)
                    if matches!(call.func.as_ref(), Expr::Path(path)
                        if path.qself.is_none()
                            && path_segments_equal(&path.path, &["AuctionOrchestrator", "from_plan"])
                            && path.path.segments.iter().all(|segment| matches!(segment.arguments, syn::PathArguments::None)))
                        && call.args.len() == 2
                        && call.args.first().is_some_and(|argument| is_ident_path(argument, "plan"))
                        && call.args.iter().nth(1).is_some_and(|argument| is_ident_path(argument, "mediator")))))
        && matches!(&block.stmts[3], syn::Stmt::Macro(statement)
            if safe_registration_macro("mediator", "log::info", &statement.mac))
        && matches!(&block.stmts[4], syn::Stmt::Expr(Expr::Call(call), None)
            if call_named(call, "Ok")
                && call.args.len() == 1
                && call.args.first().is_some_and(|argument| is_ident_path(argument, "orchestrator")));
    if !exact {
        return Err(invalid_manifest(
            "mediator registration production dataflow differs from the closed shape",
        ));
    }
    Ok(())
}

fn is_ident_path(expression: &Expr, expected: &str) -> bool {
    matches!(strip_expression_parens(expression), Expr::Path(path) if path.path.is_ident(expected))
}

fn strip_expression_parens(expression: &Expr) -> &Expr {
    match expression {
        Expr::Paren(paren) => strip_expression_parens(&paren.expr),
        Expr::Group(group) => strip_expression_parens(&group.expr),
        _ => expression,
    }
}

fn accepted_nonregistration_statement(
    statement: &syn::Stmt,
    label: &str,
    allowed_macros: &[&str],
) -> bool {
    match (label, statement) {
        ("deploy", syn::Stmt::Expr(Expr::Call(call), None)) => call_named(call, "Ok"),
        ("plan", syn::Stmt::Local(local)) => matches!(
            &local.pat,
            syn::Pat::Ident(binding) if binding.ident == "inner" || binding.ident == "registrations"
        ),
        ("plan", syn::Stmt::Expr(Expr::ForLoop(expression), _)) => {
            matches!(expression.expr.as_ref(), Expr::Path(path) if path.path.is_ident("registrations"))
                || matches!(expression.expr.as_ref(), Expr::Call(call) if call_named(call, "builders"))
        }
        ("plan", syn::Stmt::Expr(Expr::Call(call), None)) => call_named(call, "Ok"),
        ("mediator", syn::Stmt::Local(local)) => matches!(
            &local.pat,
            syn::Pat::Ident(binding) if binding.ident == "orchestrator"
        ),
        ("mediator", syn::Stmt::Expr(Expr::Call(call), None)) => call_named(call, "Ok"),
        (_, syn::Stmt::Macro(statement)) => {
            let name = statement
                .mac
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            allowed_macros.contains(&name.as_str())
        }
        _ => false,
    }
}

fn statement_registration_count(statement: &syn::Stmt, label: &str) -> usize {
    struct Finder<'a> {
        label: &'a str,
        count: usize,
    }
    impl<'ast> syn::visit::Visit<'ast> for Finder<'_> {
        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            if let Expr::Path(path) = call.func.as_ref()
                && let Some(name) = path
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
            {
                self.count += usize::from(match self.label {
                    "deploy" => name == "validate_prebid" || name == "validate_integration",
                    "plan" => name == "register_for_plan",
                    "mediator" => name == "register_providers",
                    _ => false,
                });
            }
            visit::visit_expr_call(self, call);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if self.label == "deploy" && call.method == "integration_config" {
                self.count += 1;
            }
            visit::visit_expr_method_call(self, call);
        }
    }
    let mut finder = Finder { label, count: 0 };
    finder.visit_stmt(statement);
    finder.count
}

fn deploy_direct_call(call: &ExprCall) -> bool {
    let Expr::Path(function) = call.func.as_ref() else {
        return false;
    };
    if function.qself.is_some() || function.path.segments.len() != 1 {
        return false;
    }
    let segment = &function.path.segments[0];
    if segment.ident == "validate_prebid" {
        return matches!(segment.arguments, syn::PathArguments::None)
            && call.args.len() == 2
            && call
                .args
                .first()
                .is_some_and(|argument| is_ident_path(argument, "settings"))
            && call
                .args
                .iter()
                .nth(1)
                .is_some_and(|argument| is_ident_path(argument, "plan"));
    }
    if segment.ident != "validate_integration" || call.args.len() != 2 {
        return false;
    }
    let Some(id) = call.args.iter().nth(1).and_then(literal_string) else {
        return false;
    };
    let expected_type = match id.as_str() {
        "aps" => "ApsConfig",
        "adserver_mock" => "AdServerMockConfig",
        "testlight" => "TestlightConfig",
        "nextjs" => "NextJsIntegrationConfig",
        "permutive" => "PermutiveConfig",
        "lockr" => "LockrConfig",
        "didomi" => "DidomiIntegrationConfig",
        "sourcepoint" => "SourcepointConfig",
        "osano" => "OsanoConfig",
        "google_tag_manager" => "GoogleTagManagerConfig",
        "gpt" => "GptConfig",
        "gpt_diagnostics" => "GptDiagnosticsConfig",
        _ => return false,
    };
    is_ident_path(
        call.args
            .first()
            .expect("validated deploy call should have settings"),
        "settings",
    ) && exact_path_type_argument(segment, &[expected_type])
}

fn exact_plain_call(call: &ExprCall, path: &[&str], arguments: &[&str]) -> bool {
    matches!(call.func.as_ref(), Expr::Path(function)
        if function.qself.is_none()
            && path_segments_equal(&function.path, path)
            && function.path.segments.iter().all(|segment| matches!(segment.arguments, syn::PathArguments::None)))
        && call.args.len() == arguments.len()
        && call
            .args
            .iter()
            .zip(arguments)
            .all(|(argument, expected)| is_ident_path(argument, expected))
}

fn exact_method_type_argument(call: &syn::ExprMethodCall, expected: &[&str]) -> bool {
    call.turbofish
        .as_ref()
        .is_some_and(|arguments| exact_generic_type_arguments(&arguments.args, expected))
}

fn exact_path_type_argument(segment: &syn::PathSegment, expected: &[&str]) -> bool {
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return false;
    };
    exact_generic_type_arguments(&arguments.args, expected)
}

fn exact_generic_type_arguments(
    arguments: &Punctuated<syn::GenericArgument, Token![,]>,
    expected: &[&str],
) -> bool {
    matches!(arguments.iter().collect::<Vec<_>>().as_slice(), [syn::GenericArgument::Type(Type::Path(value))]
        if value.qself.is_none() && path_segments_equal(&value.path, expected))
}

fn path_segments_equal(path: &syn::Path, expected: &[&str]) -> bool {
    path.segments.len() == expected.len()
        && path
            .segments
            .iter()
            .zip(expected)
            .all(|(segment, expected)| segment.ident == expected)
}

fn call_named(call: &ExprCall, expected: &str) -> bool {
    matches!(call.func.as_ref(), Expr::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == expected))
}

fn ensure_input_bound(label: &str, source: &str) -> Result<(), Report<IntegrationError>> {
    if source.len() > MAX_INVENTORY_INPUT_BYTES {
        Err(invalid_manifest(format!(
            "{label} exceeds the 4 MiB input limit"
        )))
    } else {
        Ok(())
    }
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
            self.insert("prebid");
        } else if name == "validate_integration" {
            match call.args.iter().nth(1).and_then(literal_string) {
                Some(id) => self.insert(&id),
                None => {
                    self.error = Some("deploy validator ID must be a string literal".to_owned())
                }
            }
        } else if !matches!(
            name.as_str(),
            "validate_config_for_startup"
                | "validate_config_for_deploy"
                | "validate_datadome"
                | "Some"
                | "Ok"
        ) {
            self.error = Some(format!("unsupported deploy validator call: {name}"));
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "integration_config" {
            match call.args.first().and_then(literal_string) {
                Some(id) => self.insert(&id),
                None => {
                    self.error =
                        Some("deploy integration_config ID must be a string literal".to_owned());
                }
            }
        }
        visit::visit_expr_method_call(self, call);
    }
}

impl DeployVisitor {
    fn insert(&mut self, id: &str) {
        if !self.ids.insert(id.to_owned()) && self.error.is_none() {
            self.error = Some(format!("duplicate deploy integration ID: {id}"));
        }
    }
}

#[derive(Default)]
struct PlanRegistrationVisitor {
    ids: BTreeSet<String>,
    error: Option<String>,
}

impl<'ast> syn::visit::Visit<'ast> for PlanRegistrationVisitor {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let segments = function.path.segments.iter().collect::<Vec<_>>();
            if segments
                .last()
                .is_some_and(|segment| segment.ident == "register_for_plan")
                && let Some(module) = segments.iter().rev().nth(1)
                && !self.ids.insert(module.ident.to_string())
            {
                self.error = Some(format!("duplicate plan registration ID: {}", module.ident));
            }
        }
        visit::visit_expr_call(self, call);
    }
}

#[derive(Default)]
struct MediatorVisitor {
    ids: BTreeSet<String>,
    error: Option<String>,
}

impl<'ast> syn::visit::Visit<'ast> for MediatorVisitor {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let segments = function.path.segments.iter().collect::<Vec<_>>();
            if segments
                .last()
                .is_some_and(|segment| segment.ident == "register_providers")
                && let Some(module) = segments.iter().rev().nth(1)
                && !self.ids.insert(module.ident.to_string())
            {
                self.error = Some(format!("duplicate mediator ID: {}", module.ident));
            }
        }
        visit::visit_expr_call(self, call);
    }
}

fn extract_builder_ids(items: &[Item]) -> Result<BTreeSet<String>, Report<IntegrationError>> {
    let block = exact_top_level_function(items, "builders", "builder registry")?;
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
    let mut constants = BTreeMap::new();
    let mut registration = None;
    for item in items {
        let Item::Const(constant) = item else {
            continue;
        };
        if !is_production_item(&constant.attrs, "profile registry")? {
            continue;
        }
        if let Some(value) = literal_string(&constant.expr)
            && constants
                .insert(constant.ident.to_string(), value)
                .is_some()
        {
            return Err(invalid_manifest(format!(
                "duplicate profile string constant: {}",
                constant.ident
            )));
        }
        if constant.ident == "PROFILE_REGISTRATIONS" && registration.replace(constant).is_some() {
            return Err(invalid_manifest("duplicate profile registry constant"));
        }
    }
    let registration =
        registration.ok_or_else(|| invalid_manifest("missing profile registry constant"))?;
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

fn unique_values(
    values: Vec<String>,
    axis: &'static str,
) -> Result<BTreeSet<String>, Report<IntegrationError>> {
    if values.len() > MAX_INVENTORY_ENTRIES {
        return Err(invalid_manifest(format!(
            "{axis} cardinality limit exceeded"
        )));
    }
    if values
        .iter()
        .any(|value| value.len() > MAX_INVENTORY_STRING_BYTES)
    {
        return Err(invalid_manifest(format!(
            "{axis} contains an oversized string"
        )));
    }
    let count = values.len();
    let unique = values.into_iter().collect::<BTreeSet<_>>();
    if unique.len() != count {
        Err(invalid_manifest(format!("duplicate {axis} value")))
    } else {
        Ok(unique)
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

fn ensure_manifest_strings<'a>(
    axis: &str,
    values: impl IntoIterator<Item = &'a String>,
) -> Result<(), Report<IntegrationError>> {
    if values
        .into_iter()
        .any(|value| value.len() > MAX_INVENTORY_STRING_BYTES)
    {
        Err(invalid_manifest(format!(
            "{axis} contains an oversized string"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY_LOOP: &str = r#"
        for proxy in registration.proxies {
            for route in proxy.routes() {
                let value = (proxy.clone(), registration.integration_id);
                let matchit_path = if route.path.ends_with("/*") {
                    format!(
                        "{}/{{*rest}}",
                        route.path.strip_suffix("/*").expect("path should end with '/*'")
                    )
                } else {
                    route.path.clone()
                };
                let router = match route.method {
                    Method::GET => &mut inner.get_router,
                    Method::POST => &mut inner.post_router,
                    Method::PUT => &mut inner.put_router,
                    Method::DELETE => &mut inner.delete_router,
                    Method::PATCH => &mut inner.patch_router,
                    Method::HEAD => &mut inner.head_router,
                    Method::OPTIONS => &mut inner.options_router,
                    _ => {
                        log::warn!(
                            "Unsupported HTTP method {} for route {}",
                            route.method,
                            route.path
                        );
                        continue;
                    }
                };
                if let Err(e) = router.insert(&matchit_path, value) {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "Integration route registration failed for {} {}: {:?}",
                            route.method, route.path, e
                        ),
                    }));
                }
                inner.routes.push((route, registration.integration_id));
            }
        }
    "#;

    fn proxy_loop(source: &str) -> syn::ExprForLoop {
        let expression = syn::parse_str::<Expr>(source).expect("proxy loop fixture should parse");
        let Expr::ForLoop(expression) = expression else {
            panic!("proxy loop fixture should be a for expression");
        };
        expression
    }

    #[test]
    fn proxy_registration_loop_binds_every_nested_dataflow_step() {
        let proxy = proxy_loop(PROXY_LOOP);
        let Some(syn::Stmt::Expr(Expr::ForLoop(routes), _)) = proxy.body.stmts.first() else {
            panic!("proxy fixture should contain the route loop");
        };
        assert!(exact_proxy_value_local(&routes.body.stmts[0]), "value");
        let Some(Expr::If(path)) = exact_local_initializer(&routes.body.stmts[1], "matchit_path")
            .map(strip_expression_parens)
        else {
            panic!("path fixture should be an if expression");
        };
        assert!(
            matches!(strip_expression_parens(&path.cond), Expr::MethodCall(call)
                if call.method == "ends_with"
                    && exact_field(&call.receiver, "route", "path")
                    && call.args.len() == 1
                    && call.args.first().and_then(literal_string).as_deref() == Some("/*")),
            "path condition"
        );
        let Some(wildcard) = path
            .then_branch
            .stmts
            .as_slice()
            .first()
            .filter(|_| path.then_branch.stmts.len() == 1)
            .and_then(statement_macro)
        else {
            panic!("path wildcard should be a macro");
        };
        let parser = Punctuated::<Expr, Token![,]>::parse_terminated;
        let arguments = parser
            .parse2(wildcard.tokens.clone())
            .expect("path wildcard macro should parse");
        assert_eq!(arguments.len(), 2, "path wildcard arguments");
        assert_eq!(
            arguments.first().and_then(literal_string).as_deref(),
            Some("{}/{{*rest}}"),
            "path wildcard format"
        );
        assert!(
            arguments
                .iter()
                .nth(1)
                .is_some_and(exact_matchit_path_expression),
            "path wildcard expression"
        );
        assert!(
            safe_registration_macro("plan", "format", wildcard),
            "path wildcard"
        );
        assert!(
            path.else_branch.as_ref().is_some_and(|(_, alternative)| {
                matches!(strip_expression_parens(alternative), Expr::Block(block)
                    if matches!(block.block.stmts.as_slice(), [syn::Stmt::Expr(Expr::MethodCall(call), None)]
                        if call.method == "clone"
                            && call.args.is_empty()
                            && exact_field(&call.receiver, "route", "path")))
            }),
            "path literal"
        );
        assert!(exact_matchit_path_local(&routes.body.stmts[1]), "path");
        assert!(exact_proxy_router_local(&routes.body.stmts[2]), "router");
        assert!(exact_proxy_insert_branch(&routes.body.stmts[3]), "insert");
        assert!(exact_registered_route_push(&routes.body.stmts[4]), "record");
        assert!(exact_proxy_registration_loop(&syn::Stmt::Expr(
            Expr::ForLoop(proxy),
            None,
        )));

        for (original, replacement) in [
            (
                "let value = (proxy.clone(), registration.integration_id);",
                "let value = ();",
            ),
            (
                "let matchit_path = if route.path.ends_with(\"/*\") {",
                "let matchit_path = if false {",
            ),
            (
                "let router = match route.method {",
                "let router = match Method::GET {",
            ),
            (
                "if let Err(e) = router.insert(&matchit_path, value) {",
                "if false {",
            ),
            (
                "inner.routes.push((route, registration.integration_id));",
                "inner.routes.push((route, \"decoy\"));",
            ),
        ] {
            let changed = PROXY_LOOP.replacen(original, replacement, 1);
            assert_ne!(changed, PROXY_LOOP, "fixture must change");
            assert!(
                !exact_proxy_registration_loop(&syn::Stmt::Expr(
                    Expr::ForLoop(proxy_loop(&changed)),
                    None,
                )),
                "nested replacement must fail: {replacement}"
            );
        }
    }
}
