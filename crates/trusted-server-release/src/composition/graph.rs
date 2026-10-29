//! Conservative package graph checks; compiler observations retain unit authority.

use std::collections::{BTreeMap, BTreeSet};

use error_stack::Report;
use serde::Serialize;
use serde_json::Value;

use super::{
    CargoObservations, MAX_PACKAGES, UnitContext, array, composition_error, package, strings, text,
};
use crate::documents::{Role, parse_json};
use crate::error::ReleaseError;

// A non-deduplicated tree repeats package paths far more often than compilation
// emits units. Keep this independent of the compiler-message count limit.
const MAX_TREE_OCCURRENCES: usize = 1_048_576;

/// Reconciled package membership, without claiming a compiler unit dependency DAG.
#[derive(Debug, Serialize)]
pub(crate) struct GraphEvidence {
    selected_package_id: String,
    tree_package_ids: Vec<String>,
    observed_package_ids: Vec<String>,
    edges: Vec<GraphEdge>,
}

/// A potential metadata dependency edge, without compiled-unit authority.
#[derive(Debug, Serialize)]
pub(crate) struct GraphEdge {
    parent: String,
    dependency: String,
    kinds: Vec<String>,
}

impl GraphEvidence {
    /// Returns the exact selected binary's Cargo package identity.
    pub(crate) fn selected_package_id(&self) -> &str {
        &self.selected_package_id
    }

    /// Returns potential package membership from the reconciled Cargo tree.
    pub(crate) fn tree_package_ids(&self) -> &[String] {
        &self.tree_package_ids
    }

    /// Returns package membership proved by actual compiler messages.
    pub(crate) fn observed_package_ids(&self) -> &[String] {
        &self.observed_package_ids
    }

    /// Returns potential metadata edges, without claiming a compiled unit DAG.
    pub(crate) fn potential_edges(&self) -> &[GraphEdge] {
        &self.edges
    }
}

impl GraphEdge {
    /// Returns the exact potential parent package identity.
    pub(crate) fn parent(&self) -> &str {
        &self.parent
    }

    /// Returns the exact potential dependency package identity.
    pub(crate) fn dependency(&self) -> &str {
        &self.dependency
    }

    /// Returns declared dependency kinds without inferring embedded membership.
    pub(crate) fn kinds(&self) -> &[String] {
        &self.kinds
    }
}

type Edges<'a> = BTreeMap<(&'a str, &'a str), BTreeSet<&'a str>>;
type IdentityIndex<'a> = BTreeMap<(&'a str, &'a str), Vec<&'a str>>;

struct ResolvedGraph<'a> {
    nodes: BTreeSet<&'a str>,
    edges: Edges<'a>,
}

fn metadata_edges<'a>(
    metadata: &'a Value,
    catalog: &BTreeMap<&str, &Value>,
) -> Result<ResolvedGraph<'a>, Report<ReleaseError>> {
    let nodes = array(&metadata["resolve"]["nodes"], "resolve nodes")?;
    if nodes.len() > MAX_PACKAGES {
        return Err(composition_error("too many resolve nodes"));
    }
    let mut node_ids = BTreeSet::new();
    let mut edges = BTreeMap::new();
    for node in nodes {
        let id = text(&node["id"], "resolve node ID")?;
        if !catalog.contains_key(id) || !node_ids.insert(id) {
            return Err(composition_error("unknown or duplicate resolve node"));
        }
        let dependencies = strings(&node["dependencies"], "resolve dependencies", true)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut endpoints = BTreeSet::new();
        for dep in array(&node["deps"], "resolve dependency edges")? {
            text(&dep["name"], "resolve dependency name")?;
            let endpoint = text(&dep["pkg"], "resolve dependency package")?;
            if !catalog.contains_key(endpoint) {
                return Err(composition_error(
                    "resolve edge has unknown package endpoint",
                ));
            }
            endpoints.insert(endpoint.to_owned());
            let kinds = array(&dep["dep_kinds"], "dependency kinds")?;
            if kinds.is_empty() {
                return Err(composition_error("resolve dependency has no kinds"));
            }
            let edge = edges.entry((id, endpoint)).or_insert_with(BTreeSet::new);
            for kind in kinds {
                match kind.get("target") {
                    Some(Value::Null) => {}
                    Some(target) => {
                        text(target, "dependency target condition")?;
                    }
                    None => {
                        return Err(composition_error("dependency kind lacks target condition"));
                    }
                }
                let kind = match kind.get("kind") {
                    Some(Value::Null) => "normal",
                    Some(Value::String(kind)) if matches!(kind.as_str(), "build" | "dev") => {
                        kind.as_str()
                    }
                    _ => return Err(composition_error("unknown dependency kind")),
                };
                edge.insert(kind);
            }
        }
        if endpoints != dependencies {
            return Err(composition_error(
                "resolve dependency endpoint lists disagree",
            ));
        }
    }
    if edges
        .keys()
        .any(|(_, dependency)| !node_ids.contains(dependency))
    {
        return Err(composition_error("resolve dependency has no node"));
    }
    Ok(ResolvedGraph {
        nodes: node_ids,
        edges,
    })
}

fn tree_identity<'a>(
    display: &str,
    catalog: &BTreeMap<&'a str, &Value>,
    identities: &IdentityIndex<'a>,
) -> Result<&'a str, Report<ReleaseError>> {
    if display.ends_with(" (*)") {
        return Err(composition_error("tree contains a deduplicated subtree"));
    }
    let (name, rest) = display
        .split_once(" v")
        .ok_or_else(|| composition_error("tree package lacks name/version"))?;
    let (version, presentation) = rest.split_once(' ').unwrap_or((rest, ""));
    let candidates = identities
        .get(&(name, version))
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [id] = candidates else {
        return Err(composition_error(
            "tree package has no unique catalog identity",
        ));
    };
    let candidate = catalog[id];
    if candidate["source"].is_null() {
        let manifest = package(candidate)?.manifest_path;
        let parent = manifest
            .parent()
            .ok_or_else(|| composition_error("tree path package has no manifest parent"))?;
        if presentation != format!("({})", parent.display()) {
            return Err(composition_error(
                "tree path presentation differs from metadata manifest",
            ));
        }
    } else if !(presentation.is_empty()
        || presentation.starts_with('(') && presentation.ends_with(')'))
    {
        return Err(composition_error("tree source presentation is malformed"));
    }
    Ok(id)
}

fn contexts(parent: &[UnitContext], kinds: &BTreeSet<&str>, proc_macro: bool) -> Vec<UnitContext> {
    let mut result = Vec::new();
    for context in parent {
        for kind in kinds {
            let context = if *kind == "build" || proc_macro {
                UnitContext::Host
            } else {
                *context
            };
            if *kind != "dev" && !result.contains(&context) {
                result.push(context);
            }
        }
    }
    result
}

/// Reconciles explicit-target tree edges with the catalog and actual compilation.
///
/// # Errors
///
/// Rejects ambiguous package joins, invalid tree/metadata edges, and observations
/// without selected normal/build graph reachability in their compiler context.
pub(crate) fn reconcile(
    metadata: &[u8],
    tree: &[u8],
    observed: &CargoObservations,
    selected_package_id: &str,
) -> Result<GraphEvidence, Report<ReleaseError>> {
    let metadata = parse_json(metadata, Role::CargoObservation)
        .map_err(|error| error.change_context(ReleaseError::Composition))?;
    if metadata["version"].as_u64() != Some(1) {
        return Err(composition_error("unsupported metadata version"));
    }
    let packages = array(&metadata["packages"], "metadata packages")?;
    if packages.len() > MAX_PACKAGES {
        return Err(composition_error("too many metadata packages"));
    }
    let mut catalog = BTreeMap::new();
    let mut identities = IdentityIndex::new();
    for value in packages {
        let id = text(&value["id"], "catalog package ID")?;
        package(value)?;
        if catalog.insert(id, value).is_some() {
            return Err(composition_error("duplicate catalog package ID"));
        }
        identities
            .entry((
                text(&value["name"], "catalog package name")?,
                text(&value["version"], "catalog package version")?,
            ))
            .or_default()
            .push(id);
    }
    let metadata_edges = metadata_edges(&metadata, &catalog)?;
    if !catalog.contains_key(selected_package_id) {
        return Err(composition_error("selected package is absent from catalog"));
    }
    if tree.is_empty() || tree.len() > 16 * 1024 * 1024 || !tree.ends_with(b"\n") {
        return Err(composition_error(
            "tree is not bounded newline-terminated text",
        ));
    }
    let tree = std::str::from_utf8(tree).map_err(composition_error)?;
    let mut stack: Vec<(&str, Vec<UnitContext>)> = Vec::new();
    let mut reachable: BTreeMap<&str, Vec<UnitContext>> = BTreeMap::new();
    let mut selected_edges = BTreeMap::new();
    for (line_index, line) in tree.lines().enumerate() {
        if line_index >= MAX_TREE_OCCURRENCES
            || line.len() > 16_384
            || line.chars().any(char::is_control)
        {
            return Err(composition_error("tree line violates bounds"));
        }
        let prefix = line.bytes().take_while(u8::is_ascii_digit).count();
        let depth = line[..prefix].parse::<usize>().map_err(composition_error)?;
        if depth > stack.len() || (line_index != 0 && depth == 0) || (line_index == 0 && depth != 0)
        {
            return Err(composition_error(
                "tree depth does not describe one selected root",
            ));
        }
        let id = tree_identity(&line[prefix..], &catalog, &identities)?;
        if !metadata_edges.nodes.contains(id) {
            return Err(composition_error("tree package has no resolve node"));
        }
        stack.truncate(depth);
        if stack.iter().any(|(ancestor, _)| *ancestor == id) {
            return Err(composition_error("tree dependency cycle"));
        }
        let unit_contexts = if let Some((parent, parent_contexts)) = stack.last() {
            let kinds = metadata_edges
                .edges
                .get(&(*parent, id))
                .ok_or_else(|| composition_error("tree edge is absent from metadata resolve"))?;
            let kinds = kinds
                .iter()
                .copied()
                .filter(|kind| *kind != "dev")
                .collect::<BTreeSet<_>>();
            if kinds.is_empty() {
                return Err(composition_error("tree edge is dev-only"));
            }
            let proc_macro = array(&catalog[id]["targets"], "package targets")?
                .iter()
                .any(|target| {
                    target["kind"].as_array().is_some_and(|kinds| {
                        kinds.iter().any(|kind| kind.as_str() == Some("proc-macro"))
                    })
                });
            selected_edges.insert((*parent, id), kinds.clone());
            contexts(parent_contexts, &kinds, proc_macro)
        } else {
            if id != selected_package_id {
                return Err(composition_error("tree root differs from selected package"));
            }
            vec![UnitContext::Target]
        };
        let all = reachable.entry(id).or_default();
        for context in &unit_contexts {
            if !all.contains(context) {
                all.push(*context);
            }
        }
        stack.push((id, unit_contexts));
    }
    let mut observed_ids = BTreeSet::new();
    for package in &observed.packages {
        if !reachable.contains_key(package.id.as_str()) || !observed_ids.insert(package.id.as_str())
        {
            return Err(composition_error(
                "compiled package lacks unique selected tree membership",
            ));
        }
    }
    let mut compiled_ids = BTreeSet::new();
    let mut custom_build_ids = BTreeSet::new();
    let mut consumer_contexts = BTreeSet::new();
    let mut binaries = 0;
    for unit in &observed.units {
        let contexts = reachable
            .get(unit.package_id.as_str())
            .ok_or_else(|| composition_error("compiler unit is absent from selected tree"))?;
        if !observed_ids.contains(unit.package_id.as_str()) {
            return Err(composition_error("compiler unit has no observed package"));
        }
        let custom_build = unit.target_kind.iter().any(|kind| kind == "custom-build");
        if custom_build {
            custom_build_ids.insert(unit.package_id.as_str());
        } else {
            consumer_contexts.insert((
                unit.package_id.as_str(),
                unit.context == UnitContext::Target,
            ));
        }
        if (custom_build && unit.context != UnitContext::Host)
            || (!custom_build && !contexts.contains(&unit.context))
        {
            return Err(composition_error(
                "compiler context is unreachable through normal/build graph",
            ));
        }
        if unit.target_kind.iter().any(|kind| kind == "bin") {
            binaries += 1;
            if unit.package_id != selected_package_id || unit.context != UnitContext::Target {
                return Err(composition_error(
                    "compiler binary is outside selected target package",
                ));
            }
        }
        compiled_ids.insert(unit.package_id.as_str());
    }
    if binaries != 1 || compiled_ids != observed_ids {
        return Err(composition_error(
            "observed membership differs from compiled selected binary graph",
        ));
    }
    let executed_ids: BTreeSet<_> = observed
        .build_scripts
        .iter()
        .map(|script| script.package_id.as_str())
        .collect();
    if executed_ids != custom_build_ids {
        return Err(composition_error(
            "compiled build scripts differ from observed executions",
        ));
    }
    let executed_contexts: BTreeSet<_> = observed
        .build_scripts
        .iter()
        .map(|script| {
            (
                script.package_id.as_str(),
                script.context == UnitContext::Target,
            )
        })
        .collect();
    consumer_contexts.retain(|(id, _)| custom_build_ids.contains(id));
    if executed_contexts != consumer_contexts {
        return Err(composition_error(
            "build-script execution contexts differ from observed consumers",
        ));
    }
    Ok(GraphEvidence {
        selected_package_id: selected_package_id.to_owned(),
        tree_package_ids: reachable.keys().map(|id| (*id).to_owned()).collect(),
        observed_package_ids: observed_ids.into_iter().map(str::to_owned).collect(),
        edges: selected_edges
            .into_iter()
            .map(|((parent, dependency), kinds)| GraphEdge {
                parent: parent.to_owned(),
                dependency: dependency.to_owned(),
                kinds: kinds.into_iter().map(str::to_owned).collect(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests;
