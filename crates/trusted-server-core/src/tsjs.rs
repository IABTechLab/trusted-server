use std::collections::HashSet;
use std::sync::OnceLock;

use error_stack::Report;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use trusted_server_js::{
    MAX_MANIFEST_MODULES, TsjsModulePhase, all_integration_metadata, all_module_ids,
    concatenated_hash, release_id, single_module_hash,
};
use validator::Validate;

use crate::auction::types::{BidRenderSourceV1, BrowserAuctionProjectionV1, SlotAuctionDecisionV1};
use crate::error::TrustedServerError;
use crate::settings::{IntegrationConfig, Settings};

/// Serialize one exact `BootManifestV1` without publishing it into HTML.
///
/// `module_ids` contains enabled integration bundles in catalog-filtered injection
/// order; core is implicit and therefore rejected here. Unknown, duplicate,
/// malformed, phase-overridden, or over-capacity inventories fail closed.
///
/// # Errors
///
/// Returns an error when the integration inventory exceeds the bounded capacity,
/// contains an invalid module ID, or cannot be serialized.
pub fn tsjs_boot_manifest_v1(module_ids: &[&str]) -> Result<String, Report<TrustedServerError>> {
    tsjs_boot_manifest_with_critical_src_v1(module_ids, None)
}

fn tsjs_boot_manifest_with_critical_src_v1(
    module_ids: &[&str],
    precomputed_critical_src: Option<&str>,
) -> Result<String, Report<TrustedServerError>> {
    if module_ids.len() > MAX_MANIFEST_MODULES {
        return Err(boot_manifest_error(
            "integration inventory exceeds catalog capacity",
        ));
    }
    let catalog = all_integration_metadata();
    let catalog_order = catalog
        .iter()
        .enumerate()
        .map(|(index, metadata)| (metadata.id, index))
        .collect::<std::collections::HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut integrations = Vec::with_capacity(module_ids.len());
    let mut critical_ids = Vec::new();
    let mut provided = HashSet::from(["runtime.v1"]);
    let mut previous_order = None;
    for id in module_ids {
        let Some(order) = catalog_order.get(id).copied() else {
            return Err(boot_manifest_error("invalid integration inventory"));
        };
        if *id == "core"
            || !valid_integration_id(id)
            || !seen.insert(*id)
            || previous_order.is_some_and(|previous| order <= previous)
        {
            return Err(boot_manifest_error(
                "invalid integration inventory or catalog order",
            ));
        }
        previous_order = Some(order);
        let metadata = catalog
            .get(order)
            .ok_or_else(|| boot_manifest_error("catalog metadata is unavailable"))?;
        if metadata
            .inputs
            .iter()
            .any(|declaration| !declaration.contains('?') && !provided.contains(declaration))
        {
            return Err(boot_manifest_error(
                "integration inventory omits a required provider",
            ));
        }
        provided.extend(metadata.outputs.iter().copied());
        let encoded = serde_json::to_string(id)
            .map_err(|_| boot_manifest_error("integration id serialization failed"))?;
        match metadata.phase {
            Some(TsjsModulePhase::Critical) => {
                critical_ids.push(*id);
                integrations.push(format!(r#"{{"id":{encoded},"phase":"critical"}}"#));
            }
            Some(TsjsModulePhase::Deferred) => {
                let src = tsjs_single_module_script_src(id)
                    .ok_or_else(|| boot_manifest_error("deferred module src is unavailable"))?;
                let encoded_src = serde_json::to_string(&src)
                    .map_err(|_| boot_manifest_error("module src serialization failed"))?;
                integrations.push(format!(
                    r#"{{"id":{encoded},"phase":"deferred","trigger":"first_display_or_idle","src":{encoded_src}}}"#
                ));
            }
            Some(TsjsModulePhase::FirstDisplay) => {
                return Err(boot_manifest_error(
                    "integration cannot use the provisional first-display phase",
                ));
            }
            None => return Err(boot_manifest_error("integration phase is unavailable")),
        }
    }
    if module_ids.first() != Some(&"render_runtime") {
        return Err(boot_manifest_error(
            "integration inventory must begin with render_runtime",
        ));
    }
    let critical_src =
        precomputed_critical_src.map_or_else(|| tsjs_script_src(&critical_ids), ToOwned::to_owned);
    Ok(format!(
        r#"{{"version":1,"releaseId":"{}","criticalSrc":"{}","integrations":[{}]}}"#,
        release_id(),
        critical_src,
        integrations.join(",")
    ))
}

/// Serialize a dormant phase-aware `BootManifestV1` without publishing it.
///
/// This remains separate from the legacy production manifest serializer. It lets
/// test-only prospective routes use generated release metadata before cutover.
///
/// # Errors
///
/// Returns an error when the requested inventory is invalid, omits an earlier
/// required capability provider, or cannot be serialized.
pub fn prospective_tsjs_boot_manifest_v1(
    module_ids: &[&str],
) -> Result<String, Report<TrustedServerError>> {
    let selected = prospective_selected_metadata(module_ids)?;
    let critical_ids = selected
        .iter()
        .filter_map(|metadata| {
            (metadata.phase == Some(TsjsModulePhase::Critical)).then_some(metadata.id)
        })
        .collect::<Vec<_>>();
    let mut provided = HashSet::from(["runtime.v1"]);
    let mut integrations = Vec::with_capacity(selected.len());

    for metadata in selected {
        for dependency in metadata.inputs {
            if dependency.contains('?') {
                continue;
            }
            if !provided.contains(*dependency) {
                return Err(boot_manifest_error(
                    "selected catalog inventory omits a required capability provider",
                ));
            }
        }

        let id = serde_json::to_string(metadata.id)
            .map_err(|_| boot_manifest_error("integration id serialization failed"))?;
        match metadata.phase {
            Some(TsjsModulePhase::Critical) => {
                integrations.push(format!(r#"{{"id":{id},"phase":"critical"}}"#));
            }
            Some(TsjsModulePhase::Deferred) => {
                let trigger = metadata.trigger.ok_or_else(|| {
                    boot_manifest_error("deferred catalog trigger is unavailable")
                })?;
                let hash = single_module_hash(metadata.id)
                    .ok_or_else(|| boot_manifest_error("deferred module hash is unavailable"))?;
                let trigger = serde_json::to_string(trigger)
                    .map_err(|_| boot_manifest_error("deferred trigger serialization failed"))?;
                let src = serde_json::to_string(&format!(
                    "/static/tsjs=tsjs-{}.min.js?v={hash}",
                    metadata.id
                ))
                .map_err(|_| boot_manifest_error("deferred source serialization failed"))?;
                integrations.push(format!(
                    r#"{{"id":{id},"phase":"deferred","trigger":{trigger},"src":{src}}}"#
                ));
            }
            Some(TsjsModulePhase::FirstDisplay) => {
                return Err(boot_manifest_error(
                    "catalog integration cannot use the provisional first-display phase",
                ));
            }
            None => {
                return Err(boot_manifest_error(
                    "catalog integration phase is unavailable",
                ));
            }
        }

        for capability in metadata.outputs {
            provided.insert(*capability);
        }
    }

    Ok(format!(
        r#"{{"version":1,"releaseId":"{}","criticalSrc":"/static/tsjs=tsjs-unified.min.js?v={}","integrations":[{}]}}"#,
        release_id(),
        concatenated_hash(&critical_ids),
        integrations.join(",")
    ))
}

/// Serialize a dormant phase-aware controller fragment and one critical script tag.
///
/// HTML processing and production routes intentionally do not call this helper.
/// Browser fixtures use it to exercise the prospective controller contract.
///
/// # Errors
///
/// Returns an error for an invalid manifest, projection, or boot bits that
/// disagree with prospective catalog membership.
pub fn prospective_tsjs_boot_controller_fragment_v1(
    config: TsjsBootScriptConfigV1<'_>,
    publisher_origin: &str,
) -> Result<String, Report<TrustedServerError>> {
    let manifest = prospective_tsjs_boot_manifest_v1(config.module_ids)?;
    let projection = crate::auction::formats::coordinated_cutover_v1::canonicalize_browser_auction_projection_json_v1(
        config.auction_projection_json,
        publisher_origin,
    )
    .map_err(|_| boot_manifest_error("auction projection violates the version-1 contract"))?;

    let selected = prospective_selected_metadata(config.module_ids)?;
    let contains = |id: &str| selected.iter().any(|metadata| metadata.id == id);
    let creative_required =
        config.creative.enabled && (config.creative.click_guard || config.creative.render_guard);
    if contains("creative") != creative_required
        || (!config.creative.enabled
            && (config.creative.click_guard || config.creative.render_guard))
    {
        return Err(boot_manifest_error(
            "creative boot bits disagree with prospective manifest membership",
        ));
    }
    if contains("gpt_diagnostics") != config.gpt_diagnostics_active {
        return Err(boot_manifest_error(
            "GPT diagnostics boot bit disagrees with prospective manifest membership",
        ));
    }
    if contains("diagnostics_presentation")
        != (config.render_trace_overlay || config.gpt_diagnostics_active)
    {
        return Err(boot_manifest_error(
            "diagnostics presentation membership disagrees with prospective boot bits",
        ));
    }

    let critical_ids = selected
        .iter()
        .filter_map(|metadata| {
            (metadata.phase == Some(TsjsModulePhase::Critical)).then_some(metadata.id)
        })
        .collect::<Vec<_>>();
    let critical_src = tsjs_script_src(&critical_ids);
    let manifest = escape_json_for_inline_script(&manifest);
    let projection = escape_json_for_inline_script(&projection);
    let controller = format!(
        "<script>(function(){{var t=window.tsjs=window.tsjs||{{}};t.boot={{\"abi\":1,\"releaseId\":\"{}\",\"manifest\":{},\"auctionProjection\":{},\"creative\":{{\"version\":1,\"enabled\":{},\"clickGuard\":{},\"renderGuard\":{}}},\"diagnostics\":{{\"version\":1,\"renderTraceOverlay\":{},\"gpt\":{{\"active\":{}}}}}}};(function(){{try{{window.performance.mark(\"tsjs:bids-script\");}}catch(_){{}}}})();}})();</script>",
        release_id(),
        manifest,
        projection,
        config.creative.enabled,
        config.creative.click_guard,
        config.creative.render_guard,
        config.render_trace_overlay,
        config.gpt_diagnostics_active,
    );
    Ok(format!(
        r#"{controller}<script src="{critical_src}" id="trustedserver-js"></script>"#
    ))
}

fn prospective_selected_metadata(
    module_ids: &[&str],
) -> Result<Vec<trusted_server_js::TsjsArtifactMetadata>, Report<TrustedServerError>> {
    if module_ids.len() > trusted_server_js::MAX_MANIFEST_MODULES {
        return Err(boot_manifest_error("more than 20 integration modules"));
    }
    let requested = module_ids.iter().copied().collect::<HashSet<_>>();
    if requested.len() != module_ids.len()
        || requested
            .iter()
            .any(|id| *id == "core" || !valid_integration_id(id))
    {
        return Err(boot_manifest_error(
            "invalid prospective integration inventory",
        ));
    }

    let metadata = all_integration_metadata();
    if requested
        .iter()
        .any(|id| !metadata.iter().any(|entry| entry.id == *id))
    {
        return Err(boot_manifest_error(
            "invalid prospective integration inventory",
        ));
    }
    if metadata
        .iter()
        .filter(|entry| entry.include == Some("always"))
        .any(|entry| !requested.contains(entry.id))
    {
        return Err(boot_manifest_error(
            "prospective integration inventory omits an always-on catalog member",
        ));
    }
    Ok(metadata
        .into_iter()
        .filter(|entry| requested.contains(entry.id))
        .collect())
}

/// Exact immutable creative boot bits emitted for one document generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreativeBootConfigV1 {
    /// Whether the creative integration is a required manifest member.
    pub enabled: bool,
    /// Whether automatic click interception activates after kernel commit.
    pub click_guard: bool,
    /// Whether automatic render interception activates after kernel commit.
    pub render_guard: bool,
}

/// Trusted server-owned inputs for selecting the closed provisional artifact.
#[derive(Clone, Copy, Debug)]
pub struct FirstDisplaySelectionConfigV1<'a> {
    /// Enabled browser integration products; never publisher supplied.
    pub enabled_integrations: &'a [&'a str],
    /// Exact server-resolved parser-time creative guard policy.
    pub creative: CreativeBootConfigV1,
}

/// Exact ordered first-display slice mask selected for one immutable projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstDisplaySelectionV1 {
    mask: u16,
    slices: Vec<&'static str>,
}

impl FirstDisplaySelectionV1 {
    /// Return the four-hex-digit transport mask value.
    #[must_use]
    pub const fn mask(&self) -> u16 {
        self.mask
    }

    /// Return selected slice ids in the fixed build order.
    #[must_use]
    pub fn slices(&self) -> &[&'static str] {
        &self.slices
    }
}

const FIRST_DISPLAY_KNOWN_INTEGRATIONS: &[&str] = &[
    "aps",
    "creative",
    "datadome",
    "didomi",
    "google_tag_manager",
    "gpt",
    "gpt_diagnostics",
    "lockr",
    "osano",
    "permutive",
    "prebid",
    "sourcepoint",
    "testlight",
];

/// Select the agent only for a complete, closed GPT-mediated initial projection.
///
/// An absent selection means the page must boot the persistent runtime directly.
/// PBS Cache, direct/programmatic projections, unknown obligations, incomplete
/// joins, and non-GPT configurations fail closed here.
#[must_use]
pub fn select_first_display_slices_v1(
    projection: &BrowserAuctionProjectionV1,
    config: FirstDisplaySelectionConfigV1<'_>,
) -> Option<FirstDisplaySelectionV1> {
    if projection.version != 1
        || projection.auction.version != 1
        || projection.auction.results.is_empty()
        || projection.auction.results.len() > crate::auction::types::MAX_BROWSER_AUCTION_RESULTS
        || projection.slots.len() != projection.auction.results.len()
    {
        return None;
    }

    let mut enabled = HashSet::new();
    for integration in config.enabled_integrations {
        if !FIRST_DISPLAY_KNOWN_INTEGRATIONS.contains(integration) || !enabled.insert(*integration)
        {
            return None;
        }
    }
    if !enabled.contains("gpt") {
        return None;
    }

    let mut winner_count = 0_usize;
    let mut aps_participates = false;
    let mut seen_slots = HashSet::new();
    for (index, decision) in projection.auction.results.iter().enumerate() {
        let slot = projection.slots.get(index)?;
        if slot.slot != decision.slot() || !seen_slots.insert(slot.slot.as_str()) {
            return None;
        }
        if let SlotAuctionDecisionV1::Winner { candidate_id, slot } = decision {
            let mut matching = projection
                .bids
                .iter()
                .filter(|bid| bid.candidate_id == *candidate_id && bid.slot == *slot);
            let bid = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            match &bid.render_source {
                BidRenderSourceV1::Aps(_) => {
                    if !enabled.contains("aps") {
                        return None;
                    }
                    aps_participates = true;
                }
                BidRenderSourceV1::Adm(_) => {}
                BidRenderSourceV1::PbsCache(_) => return None,
            }
            winner_count += 1;
        }
    }
    if winner_count != projection.bids.len() {
        return None;
    }

    let creative_guard = enabled.contains("creative")
        && config.creative.enabled
        && (config.creative.click_guard || config.creative.render_guard);
    let gpt_participates = winner_count > 0;
    let prebid_participates =
        enabled.contains("prebid") && projection.bids.iter().any(|bid| bid.provider == "prebid");
    let mut mask = 0_u16;
    let mut slices = Vec::new();
    for (index, metadata) in trusted_server_js::all_first_display_metadata()
        .into_iter()
        .enumerate()
    {
        let selected = match metadata.include {
            Some("eligible_batch") => true,
            Some("aps_participates") => aps_participates,
            Some("creative_guard") => creative_guard,
            Some("gpt_initial") => gpt_participates,
            Some("prebid_participates") => prebid_participates,
            Some(predicate) => predicate
                .strip_prefix("integration:")
                .is_some_and(|integration| enabled.contains(integration)),
            None => false,
        };
        if selected {
            mask |= 1_u16 << index;
            slices.push(metadata.id);
        }
    }
    Some(FirstDisplaySelectionV1 { mask, slices })
}

impl Default for CreativeBootConfigV1 {
    fn default() -> Self {
        Self {
            enabled: true,
            click_guard: true,
            render_guard: false,
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
struct CreativeBrowserConfigV1 {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_true")]
    click_guard: bool,
    #[serde(default)]
    render_guard: bool,
}

impl IntegrationConfig for CreativeBrowserConfigV1 {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

const fn default_true() -> bool {
    true
}

/// Resolve the exact server-owned creative browser boot policy.
pub(crate) fn creative_boot_config_v1(
    settings: &Settings,
) -> Result<CreativeBootConfigV1, Report<TrustedServerError>> {
    if !settings.integrations.contains_key("creative") {
        return Ok(CreativeBootConfigV1::default());
    }
    let Some(config) = settings.integration_config::<CreativeBrowserConfigV1>("creative")? else {
        return Ok(CreativeBootConfigV1 {
            enabled: false,
            click_guard: false,
            render_guard: false,
        });
    };
    Ok(CreativeBootConfigV1 {
        enabled: true,
        click_guard: config.click_guard,
        render_guard: config.render_guard,
    })
}

/// Inputs for the one hard-cutover browser boot transport.
#[derive(Clone, Copy, Debug)]
pub struct TsjsBootScriptConfigV1<'a> {
    /// Enabled integration bundles in their actual injection order.
    pub module_ids: &'a [&'a str],
    /// Canonical exact [`BrowserAuctionProjectionV1`](crate::auction::types::BrowserAuctionProjectionV1)
    /// JSON produced by the auction projection boundary.
    pub auction_projection_json: &'a str,
    /// Exact creative integration boot configuration.
    pub creative: CreativeBootConfigV1,
    /// Whether the local render-trace overlay is active for this document.
    pub render_trace_overlay: bool,
    /// Whether request/session-scoped GPT diagnostics is active.
    pub gpt_diagnostics_active: bool,
}

/// Serialize the sole pre-core `TsjsBootV1` assignment and bids-ready mark.
///
/// The returned inline script keeps the publisher-created `window.tsjs` object,
/// writes only the exact boot transport, and escapes every HTML-significant JSON
/// character before insertion into a script element.
///
/// # Errors
///
/// Returns an error for an invalid manifest, a projection that is not the exact
/// canonical browser contract for `publisher_origin`, or a creative/diagnostics
/// enabled bit that disagrees with manifest membership.
pub fn tsjs_boot_script_v1(
    config: TsjsBootScriptConfigV1<'_>,
    publisher_origin: &str,
) -> Result<String, Report<TrustedServerError>> {
    let manifest = tsjs_boot_manifest_v1(config.module_ids)?;
    tsjs_boot_script_with_manifest_v1(config, &manifest, publisher_origin)
}

/// Serialize boot transport while reusing a registry-owned critical URL.
///
/// The registry derives this URL from the same selected catalog inventory at
/// construction, so HTML processing does not concatenate or hash bundles.
pub(crate) fn tsjs_boot_script_with_critical_src_v1(
    config: TsjsBootScriptConfigV1<'_>,
    critical_src: &str,
    publisher_origin: &str,
) -> Result<String, Report<TrustedServerError>> {
    let manifest = tsjs_boot_manifest_with_critical_src_v1(config.module_ids, Some(critical_src))?;
    tsjs_boot_script_with_manifest_v1(config, &manifest, publisher_origin)
}

fn tsjs_boot_script_with_manifest_v1(
    config: TsjsBootScriptConfigV1<'_>,
    manifest: &str,
    publisher_origin: &str,
) -> Result<String, Report<TrustedServerError>> {
    let projection = crate::auction::formats::coordinated_cutover_v1::canonicalize_browser_auction_projection_json_v1(
        config.auction_projection_json,
        publisher_origin,
    )
    .map_err(|_| boot_manifest_error("auction projection violates the version-1 contract"))?;

    let creative_in_manifest = config.module_ids.contains(&"creative");
    let creative_required =
        config.creative.enabled && (config.creative.click_guard || config.creative.render_guard);
    if creative_in_manifest != creative_required
        || (!config.creative.enabled
            && (config.creative.click_guard || config.creative.render_guard))
    {
        return Err(boot_manifest_error(
            "creative boot bits disagree with manifest membership",
        ));
    }
    let diagnostics_in_manifest = config.module_ids.contains(&"gpt_diagnostics");
    if diagnostics_in_manifest != config.gpt_diagnostics_active {
        return Err(boot_manifest_error(
            "GPT diagnostics boot bit disagrees with manifest membership",
        ));
    }

    let manifest = escape_json_for_inline_script(manifest);
    let projection = escape_json_for_inline_script(&projection);
    Ok(format!(
        "<script>(function(){{var t=window.tsjs=window.tsjs||{{}};t.boot={{\"abi\":1,\"releaseId\":\"{}\",\"manifest\":{},\"auctionProjection\":{},\"creative\":{{\"version\":1,\"enabled\":{},\"clickGuard\":{},\"renderGuard\":{}}},\"diagnostics\":{{\"version\":1,\"renderTraceOverlay\":{},\"gpt\":{{\"active\":{}}}}}}};(function(){{try{{window.performance.mark(\"tsjs:bids-script\");}}catch(_){{}}}})();}})();</script>",
        release_id(),
        manifest,
        projection,
        config.creative.enabled,
        config.creative.click_guard,
        config.creative.render_guard,
        config.render_trace_overlay,
        config.gpt_diagnostics_active,
    ))
}

fn escape_json_for_inline_script(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len());
    for character in json.chars() {
        match character {
            '&' => escaped.push_str("\\u0026"),
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn valid_integration_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
        })
}

fn boot_manifest_error(message: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("TSJS boot manifest: {message}"),
    })
}

/// `/static` URL for the tsjs bundle with cache-busting hash based on
/// the concatenated content of the given module set.
#[must_use]
pub fn tsjs_script_src(module_ids: &[&str]) -> String {
    let hash = concatenated_hash(module_ids);
    format!("/static/tsjs=tsjs-unified.min.js?v={hash}")
}

/// `<script>` tag for injecting the tsjs bundle.
#[must_use]
pub fn tsjs_script_tag(module_ids: &[&str]) -> String {
    tsjs_script_tag_from_src(&tsjs_script_src(module_ids))
}

/// `<script>` tag for a precomputed exact TSJS artifact URL.
#[must_use]
pub(crate) fn tsjs_script_tag_from_src(src: &str) -> String {
    format!(r#"<script src="{src}" id="trustedserver-js"></script>"#)
}

/// Exact in-memory bytes and content identity for a unified TSJS artifact.
#[derive(Clone, Debug)]
pub(crate) struct TsjsStaticArtifactV1 {
    body: bytes::Bytes,
    hash: String,
    src: String,
}

impl TsjsStaticArtifactV1 {
    /// Build one exact content-addressed artifact from its canonical modules.
    #[must_use]
    pub(crate) fn new(module_ids: &[&str]) -> Self {
        let body = bytes::Bytes::from(trusted_server_js::concatenate_modules(module_ids));
        let hash = hex::encode(Sha256::digest(&body));
        let src = format!("/static/tsjs=tsjs-unified.min.js?v={hash}");
        Self { body, hash, src }
    }

    /// Build one exact dormant first-display composition for a validated mask.
    #[must_use]
    pub(crate) fn new_first_display(mask: u16, selected_ids: &[&str]) -> Option<Self> {
        if selected_ids.first() != Some(&"first_display") {
            return None;
        }
        let body = bytes::Bytes::from(trusted_server_js::concatenate_first_display_slices(
            selected_ids.get(1..).unwrap_or_default(),
        )?);
        let hash = hex::encode(Sha256::digest(&body));
        let src = format!("/static/tsjs=tsjs-first-display.min.js?m={mask:04x}&v={hash}");
        Some(Self { body, hash, src })
    }

    /// Return immutable response bytes; cloning [`bytes::Bytes`] shares storage.
    #[must_use]
    pub(crate) const fn body(&self) -> &bytes::Bytes {
        &self.body
    }

    /// Return the SHA-256 identity of the exact response bytes.
    #[must_use]
    pub(crate) fn hash(&self) -> &str {
        &self.hash
    }

    /// Return the exact content-addressed static URL.
    #[must_use]
    pub(crate) fn src(&self) -> &str {
        &self.src
    }
}

const CREATIVE_TSJS_MODULE_IDS: &[&str] = &["render_runtime", "creative"];
const EMPTY_CREATIVE_AUCTION_PROJECTION_JSON: &str = r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#;

/// Build the complete hard-cutover bootstrap for a rewritten creative document.
///
/// Rewritten creatives are independent documents, so they need their own exact
/// boot transport and critical artifact. The inventory is intentionally limited
/// to the always-on render runtime and creative integration; publisher-page
/// integrations and deferred presentation code do not belong in creative HTML.
///
/// # Errors
///
/// Returns an error when the creative settings or exact boot manifest are invalid.
pub(crate) fn creative_tsjs_bootstrap_v1(
    settings: &Settings,
) -> Result<Option<String>, Report<TrustedServerError>> {
    let creative = creative_boot_config_v1(settings)?;
    if !creative.enabled || (!creative.click_guard && !creative.render_guard) {
        return Ok(None);
    }
    let artifact = creative_tsjs_static_artifact_v1();
    let controller = tsjs_boot_script_with_critical_src_v1(
        TsjsBootScriptConfigV1 {
            module_ids: CREATIVE_TSJS_MODULE_IDS,
            auction_projection_json: EMPTY_CREATIVE_AUCTION_PROJECTION_JSON,
            creative,
            render_trace_overlay: false,
            gpt_diagnostics_active: false,
        },
        artifact.src(),
        "https://creative.invalid",
    )?;
    Ok(Some(format!(
        "{controller}{}",
        tsjs_script_tag_from_src(artifact.src())
    )))
}

/// Return the exact critical module inventory admitted for rewritten creatives.
#[must_use]
pub(crate) const fn creative_tsjs_module_ids() -> &'static [&'static str] {
    CREATIVE_TSJS_MODULE_IDS
}

/// Return the process-wide exact artifact used by rewritten creative documents.
#[must_use]
pub(crate) fn creative_tsjs_static_artifact_v1() -> &'static TsjsStaticArtifactV1 {
    static ARTIFACT: OnceLock<TsjsStaticArtifactV1> = OnceLock::new();
    ARTIFACT.get_or_init(|| TsjsStaticArtifactV1::new(CREATIVE_TSJS_MODULE_IDS))
}

/// `/static` URL for the unified bundle with a conservative cache-busting hash.
///
/// This intentionally omits `?v=` because the serving path can only mark a URL
/// immutable when the hash matches the exact enabled module set. Use
/// [`tsjs_script_src`] with exact module IDs when [`IntegrationRegistry`] is
/// available.
///
/// [`IntegrationRegistry`]: crate::integrations::IntegrationRegistry
#[must_use]
pub fn tsjs_unified_script_src() -> String {
    "/static/tsjs=tsjs-unified.min.js".to_string()
}

/// `<script>` tag for the unified bundle when exact module IDs are unavailable.
///
/// See [`tsjs_unified_script_src`] for details.
#[must_use]
pub fn tsjs_unified_script_tag() -> String {
    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"></script>",
        tsjs_unified_script_src()
    )
}

/// `/static` URL for one module with its own cache-busting hash.
#[must_use]
pub fn tsjs_single_module_script_src(module_id: &str) -> Option<String> {
    let metadata = trusted_server_js::integration_metadata(module_id)?;
    if metadata.phase != Some(TsjsModulePhase::Deferred) {
        return None;
    }
    let hash = single_module_hash(module_id)?;
    Some(format!("/static/tsjs=tsjs-{module_id}.min.js?v={hash}"))
}

/// `/static` URL for a single deferred module with its own cache-busting hash.
#[must_use]
pub fn tsjs_deferred_script_src(module_id: &str) -> Option<String> {
    tsjs_single_module_script_src(module_id)
}

/// `<script defer>` tag for a single deferred module.
#[must_use]
pub fn tsjs_deferred_script_tag(module_id: &str) -> Option<String> {
    tsjs_deferred_script_src(module_id).map(|src| format!("<script src=\"{src}\" defer></script>"))
}

/// Generate all deferred `<script defer>` tags for the given module IDs.
///
/// Returns an empty string when no deferred modules are present.
#[must_use]
pub fn tsjs_deferred_script_tags(module_ids: &[&str]) -> String {
    module_ids
        .iter()
        .filter_map(|id| tsjs_deferred_script_tag(id))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

    use super::*;

    const VALID_BROWSER_AUCTION_PROJECTION_JSON: &str = r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#;
    const PERFORMANCE_ORIGIN: &str = "https://performance.example";

    fn prospective_aps_projection(creative_url: &str) -> String {
        use crate::auction::types::{
            ApsRendererV1, ApsTagType, AuctionDecisionSetV1, BidRenderSourceV1,
            BrowserAuctionBidV1, BrowserAuctionProjectionV1, BrowserAuctionSlotV1,
            SlotAuctionDecisionV1,
        };

        let envelope = BASE64_STANDARD.encode(include_str!(
            "../../trusted-server-js/lib/test/fixtures/aps-renderer-v1.json"
        ));
        serde_json::to_string(&BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "performance-initial".to_string(),
                results: vec![SlotAuctionDecisionV1::Winner {
                    slot: "perf-slot".to_string(),
                    candidate_id: "AAAAAAAAAAAA".to_string(),
                }],
            },
            slots: vec![BrowserAuctionSlotV1 {
                slot: "perf-slot".to_string(),
                gam_unit_path: "/123/performance".to_string(),
                div_id: "perf-slot".to_string(),
                formats: vec![[300, 250]],
                targeting: Default::default(),
            }],
            bids: vec![BrowserAuctionBidV1 {
                candidate_id: "AAAAAAAAAAAA".to_string(),
                slot: "perf-slot".to_string(),
                provider: "aps".to_string(),
                upstream_bid_id: "fictional-selected-bid-id".to_string(),
                cpm: 1.23,
                currency: "USD".to_string(),
                targeting: Default::default(),
                renderer_reservation_id: Some("r1_aaaaaaaaaaaaaaaaaaaaaa".to_string()),
                render_source: BidRenderSourceV1::Aps(ApsRendererV1 {
                    version: 1,
                    account_id: "example-account-id".to_string(),
                    bid_id: "fictional-selected-bid-id".to_string(),
                    creative_id: None,
                    tag_type: ApsTagType::Iframe,
                    creative_url: creative_url.to_string(),
                    aax_response: envelope,
                    width: 300,
                    height: 250,
                }),
            }],
        })
        .expect("should serialize a canonical APS projection")
    }

    fn projection_with_adm(adm: &str) -> String {
        use crate::auction::types::{
            AdmRenderSourceV1, AuctionDecisionSetV1, BidRenderSourceV1, BrowserAuctionBidV1,
            BrowserAuctionProjectionV1, BrowserAuctionSlotV1, SlotAuctionDecisionV1,
        };

        serde_json::to_string(&BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "initial".to_string(),
                results: vec![SlotAuctionDecisionV1::Winner {
                    slot: "slot-1".to_string(),
                    candidate_id: "AAAAAAAAAAAA".to_string(),
                }],
            },
            slots: vec![BrowserAuctionSlotV1 {
                slot: "slot-1".to_string(),
                gam_unit_path: "/123/slot-1".to_string(),
                div_id: "slot-1".to_string(),
                formats: vec![[300, 250]],
                targeting: Default::default(),
            }],
            bids: vec![BrowserAuctionBidV1 {
                candidate_id: "AAAAAAAAAAAA".to_string(),
                slot: "slot-1".to_string(),
                provider: "test".to_string(),
                upstream_bid_id: "bid-1".to_string(),
                cpm: 1.0,
                currency: "USD".to_string(),
                targeting: Default::default(),
                renderer_reservation_id: Some("r1_aaaaaaaaaaaaaaaaaaaaaa".to_string()),
                render_source: BidRenderSourceV1::Adm(AdmRenderSourceV1 {
                    version: 1,
                    adm: adm.to_string(),
                    width: 300,
                    height: 250,
                }),
            }],
        })
        .expect("should serialize a canonical ADM projection")
    }

    fn hash_query_value(src: &str) -> &str {
        src.split_once("?v=")
            .map(|(_, hash)| hash)
            .expect("should contain cache-busting hash query")
    }

    fn assert_sha256_hex_hash(value: &str) {
        assert_eq!(value.len(), 64, "should be a SHA-256 hex digest");
        assert!(
            value.chars().all(|ch| ch.is_ascii_hexdigit()),
            "should contain only ASCII hex digits"
        );
    }

    #[test]
    fn release_id_is_shared_by_generated_metadata_and_every_bundle() {
        let release = release_id();

        assert_eq!(release.len(), 64, "should be one SHA-256 release id");
        assert!(
            release
                .chars()
                .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character)),
            "should use lowercase hexadecimal"
        );
        for id in all_module_ids() {
            let bundle = trusted_server_js::module_bundle(id).expect("should include known module");
            assert_eq!(
                bundle.matches(release).count(),
                1,
                "module {id} should carry the shared release id exactly once"
            );
        }
    }

    #[test]
    fn boot_manifest_serializer_preserves_enabled_injection_order() {
        let value = tsjs_boot_manifest_v1(&[
            "render_runtime",
            "creative",
            "gpt",
            "prebid",
            "prebid_later",
        ])
        .expect("should serialize known unique integrations");

        assert_eq!(
            value,
            format!(
                "{{\"version\":1,\"releaseId\":\"{}\",\"criticalSrc\":\"{}\",\"integrations\":[{{\"id\":\"render_runtime\",\"phase\":\"critical\"}},{{\"id\":\"creative\",\"phase\":\"critical\"}},{{\"id\":\"gpt\",\"phase\":\"critical\"}},{{\"id\":\"prebid\",\"phase\":\"critical\"}},{{\"id\":\"prebid_later\",\"phase\":\"deferred\",\"trigger\":\"first_display_or_idle\",\"src\":\"{}\"}}]}}",
                release_id(),
                tsjs_script_src(&["render_runtime", "creative", "gpt", "prebid"]),
                tsjs_single_module_script_src("prebid_later")
                    .expect("should name catalogued deferred module")
            ),
            "should emit the exact BootManifestV1 field and integration order"
        );
    }

    #[test]
    fn boot_manifest_rejects_non_catalog_order_and_capacity_boundaries() {
        assert!(tsjs_boot_manifest_v1(&trusted_server_js::all_integration_ids()[..19]).is_ok());
        assert!(tsjs_boot_manifest_v1(&trusted_server_js::all_integration_ids()[..20]).is_ok());
        let mut twenty_one = trusted_server_js::all_integration_ids().to_vec();
        twenty_one.push("render_runtime");
        assert!(tsjs_boot_manifest_v1(&twenty_one).is_err());
        assert!(tsjs_boot_manifest_v1(&["creative", "render_runtime"]).is_err());
    }

    #[test]
    fn boot_manifest_serializer_rejects_duplicate_unknown_and_core_ids() {
        for ids in [
            &["creative", "creative"][..],
            &["unknown"] as &[&str],
            &["core"] as &[&str],
        ] {
            assert!(tsjs_boot_manifest_v1(ids).is_err(), "should reject {ids:?}");
        }
    }

    #[test]
    fn prospective_manifest_serializes_generated_phase_order_and_deferred_sources() {
        let manifest = prospective_tsjs_boot_manifest_v1(&[
            "gpt_later",
            "gpt",
            "diagnostics_presentation",
            "render_runtime",
        ])
        .expect("should serialize a dependency-complete catalog selection");

        assert_eq!(
            manifest,
            format!(
                concat!(
                    r#"{{"version":1,"releaseId":"{}","criticalSrc":"/static/tsjs=tsjs-unified.min.js?v={}","integrations":["#,
                    r#"{{"id":"render_runtime","phase":"critical"}},"#,
                    r#"{{"id":"gpt","phase":"critical"}},"#,
                    r#"{{"id":"diagnostics_presentation","phase":"deferred","trigger":"first_display_or_idle","src":"/static/tsjs=tsjs-diagnostics_presentation.min.js?v={}"}},"#,
                    r#"{{"id":"gpt_later","phase":"deferred","trigger":"first_display_or_idle","src":"/static/tsjs=tsjs-gpt_later.min.js?v={}"}}]}}"#
                ),
                release_id(),
                concatenated_hash(&["render_runtime", "gpt"]),
                single_module_hash("diagnostics_presentation")
                    .expect("should hash generated diagnostics presentation"),
                single_module_hash("gpt_later").expect("should hash generated GPT lifecycle")
            ),
            "should canonicalize the requested selection to generated catalog order"
        );
    }

    #[test]
    fn prospective_manifest_rejects_missing_required_catalog_capability() {
        assert!(
            prospective_tsjs_boot_manifest_v1(&["render_runtime", "gpt_later"]).is_err(),
            "gpt_later requires the earlier GPT provider"
        );
    }

    #[test]
    fn prospective_manifest_requires_generated_always_catalog_members() {
        assert!(
            prospective_tsjs_boot_manifest_v1(&["creative"]).is_err(),
            "the generated always-on render runtime cannot be omitted"
        );
    }

    #[test]
    fn prospective_controller_rejects_noncanonical_or_oversized_browser_projections() {
        let noncanonical = r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[],"unexpected":true}"#;
        let padding = " ".repeat(crate::auction::types::MAX_BROWSER_AUCTION_PROJECTION_BYTES + 1);
        let oversized = format!("{VALID_BROWSER_AUCTION_PROJECTION_JSON}{padding}");

        for projection in [noncanonical, oversized.as_str()] {
            assert!(
                prospective_tsjs_boot_controller_fragment_v1(
                    TsjsBootScriptConfigV1 {
                        module_ids: &["render_runtime"],
                        auction_projection_json: projection,
                        creative: CreativeBootConfigV1 {
                            enabled: false,
                            click_guard: false,
                            render_guard: false,
                        },
                        render_trace_overlay: false,
                        gpt_diagnostics_active: false,
                    },
                    PERFORMANCE_ORIGIN
                )
                .is_err(),
                "the prospective controller must reject {projection:?} before emission"
            );
        }
    }

    #[test]
    fn prospective_controller_validates_aps_creative_urls_against_its_publisher_origin() {
        let same_origin = prospective_aps_projection("https://performance.example/creative");
        let foreign_origin = prospective_aps_projection("https://creative.example/render");
        let config = |auction_projection_json| TsjsBootScriptConfigV1 {
            module_ids: &["render_runtime"],
            auction_projection_json,
            creative: CreativeBootConfigV1 {
                enabled: false,
                click_guard: false,
                render_guard: false,
            },
            render_trace_overlay: false,
            gpt_diagnostics_active: false,
        };

        assert!(
            prospective_tsjs_boot_controller_fragment_v1(config(&same_origin), PERFORMANCE_ORIGIN)
                .is_err(),
            "the publisher origin must reject an APS creative URL on the same origin"
        );
        assert!(
            prospective_tsjs_boot_controller_fragment_v1(
                config(&foreign_origin),
                PERFORMANCE_ORIGIN
            )
            .is_ok(),
            "a valid foreign HTTPS APS creative URL should remain accepted"
        );
    }

    #[test]
    fn prospective_controller_requires_creative_membership_to_match_enabled_guards() {
        for (module_ids, creative) in [
            (
                &["render_runtime", "creative"][..],
                CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: false,
                    render_guard: false,
                },
            ),
            (
                &["render_runtime"][..],
                CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: true,
                    render_guard: false,
                },
            ),
        ] {
            assert!(
                prospective_tsjs_boot_controller_fragment_v1(
                    TsjsBootScriptConfigV1 {
                        module_ids,
                        auction_projection_json: VALID_BROWSER_AUCTION_PROJECTION_JSON,
                        creative,
                        render_trace_overlay: false,
                        gpt_diagnostics_active: false,
                    },
                    PERFORMANCE_ORIGIN
                )
                .is_err(),
                "creative membership must match enabled and at least one guard"
            );
        }
    }

    #[test]
    fn prospective_controller_keeps_deferred_modules_out_of_html_script_tags() {
        let controller = prospective_tsjs_boot_controller_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &[
                    "render_runtime",
                    "gpt",
                    "diagnostics_presentation",
                    "gpt_later",
                ],
                auction_projection_json: VALID_BROWSER_AUCTION_PROJECTION_JSON,
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: true,
                gpt_diagnostics_active: false,
            },
            PERFORMANCE_ORIGIN,
        )
        .expect("should serialize the dormant phase-aware controller");

        assert!(
            controller.contains(&format!(
                r#""criticalSrc":"/static/tsjs=tsjs-unified.min.js?v={}""#,
                concatenated_hash(&["render_runtime", "gpt"])
            )),
            "should carry the exact critical artifact source in BootManifestV1"
        );
        assert!(
            controller.ends_with(&format!(
                r#"<script src="/static/tsjs=tsjs-unified.min.js?v={}" id="trustedserver-js"></script>"#,
                concatenated_hash(&["render_runtime", "gpt"])
            )),
            "should emit exactly one critical script tag"
        );
        assert!(
            controller.matches("<script").count() == 2
                && !controller
                    .contains(r#"<script src="/static/tsjs=tsjs-diagnostics_presentation"#)
                && !controller.contains(r#"<script src="/static/tsjs=tsjs-gpt_later"#),
            "deferred modules must be present only in the manifest, not parser-time tags"
        );
    }

    #[test]
    fn boot_script_serializes_the_exact_hard_cutover_transport_and_mark() {
        let script = tsjs_boot_script_v1(TsjsBootScriptConfigV1 {
            module_ids: &["render_runtime", "creative", "gpt", "gpt_diagnostics"],
            auction_projection_json:
                r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#,
            creative: CreativeBootConfigV1 {
                enabled: true,
                click_guard: true,
                render_guard: false,
            },
            render_trace_overlay: true,
            gpt_diagnostics_active: true,
        }, "https://publisher.example")
        .expect("should serialize boot transport");

        assert!(script.starts_with("<script>(function(){var t=window.tsjs=window.tsjs||{};"));
        assert!(script.contains(&format!(
            r#"t.boot={{"abi":1,"releaseId":"{}","manifest":{{"version":1,"releaseId":"{}","criticalSrc":"{}","integrations":[{{"id":"render_runtime","phase":"critical"}},{{"id":"creative","phase":"critical"}},{{"id":"gpt","phase":"critical"}},{{"id":"gpt_diagnostics","phase":"critical"}}]}},"auctionProjection":{{"version":1,"auction":{{"version":1,"auctionId":"initial","results":[]}},"slots":[],"bids":[]}},"creative":{{"version":1,"enabled":true,"clickGuard":true,"renderGuard":false}},"diagnostics":{{"version":1,"renderTraceOverlay":true,"gpt":{{"active":true}}}}}};"#,
            release_id(),
            release_id(),
            tsjs_script_src(&["render_runtime", "creative", "gpt", "gpt_diagnostics"])
        )));
        assert_eq!(script.matches("tsjs:bids-script").count(), 1);
        assert!(!script.contains("__tsjs"));
        assert!(script.ends_with("})();</script>"));
    }

    #[test]
    fn boot_script_rejects_manifest_diagnostics_mismatch_and_escapes_projection_markup() {
        let mismatched = tsjs_boot_script_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "creative"],
                auction_projection_json: r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#,
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: true,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: true,
            },
            "https://publisher.example",
        );
        assert!(
            mismatched.is_err(),
            "should reject an active diagnostics bit without its module"
        );

        let projection = projection_with_adm("</ScRiPt><script>&");
        let script = tsjs_boot_script_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "creative"],
                auction_projection_json: &projection,
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: true,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            "https://publisher.example",
        )
        .expect("should escape valid projection JSON");
        let inner = script
            .trim_start_matches("<script>")
            .trim_end_matches("</script>");

        assert!(!inner.contains('<'));
        assert!(!inner.contains('>'));
        assert!(!inner.contains('&'));
        assert!(inner.contains(r#"\u003c/ScRiPt\u003e\u003cscript\u003e\u0026"#));
    }

    #[test]
    fn production_boot_rejects_a_noncanonical_browser_projection() {
        let script = tsjs_boot_script_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime"],
                auction_projection_json: r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[],"unexpected":true}"#,
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            "https://publisher.example",
        );

        assert!(
            script.is_err(),
            "production boot must enforce the same canonical projection contract as fixtures"
        );
    }

    #[test]
    fn inline_script_json_escaping_is_byte_exact_for_every_guarded_character() {
        assert_eq!(
            escape_json_for_inline_script("plain &<>\u{2028}\u{2029} tail"),
            r"plain \u0026\u003c\u003e\u2028\u2029 tail",
            "should preserve plain UTF-8 while escaping every script-significant character"
        );
    }

    #[test]
    fn tsjs_script_src_formats_unified_bundle_url_with_hash() {
        let src = tsjs_script_src(&["creative"]);

        assert!(
            src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
    }

    #[test]
    fn tsjs_script_src_empty_module_list_matches_core_only_bundle() {
        let empty_src = tsjs_script_src(&[]);

        assert!(
            empty_src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&empty_src));
        assert_eq!(
            empty_src,
            tsjs_script_src(&["core"]),
            "should include core exactly once for an empty module list"
        );
    }

    #[test]
    fn tsjs_script_src_hash_changes_with_module_set() {
        let creative_src = tsjs_script_src(&["creative"]);
        let creative_datadome_src = tsjs_script_src(&["creative", "datadome"]);

        assert_ne!(
            creative_src, creative_datadome_src,
            "should include requested modules in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_hash_depends_on_module_order() {
        assert_ne!(
            tsjs_script_src(&["creative", "datadome"]),
            tsjs_script_src(&["datadome", "creative"]),
            "should include module order in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_deduplicates_core_module() {
        assert_eq!(
            tsjs_script_src(&["core", "datadome"]),
            tsjs_script_src(&["datadome"]),
            "should not hash core twice when requested explicitly"
        );
    }

    #[test]
    fn tsjs_script_src_is_stable_for_identical_module_ids() {
        let module_ids = ["core", "lockr", "permutive"];
        let src = tsjs_script_src(&module_ids);

        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(
            src,
            tsjs_script_src(&module_ids),
            "should produce a stable URL for identical module IDs"
        );
    }

    #[test]
    fn tsjs_script_tag_wraps_source_in_single_trustedserver_tag() {
        let module_ids = ["creative"];
        let src = tsjs_script_src(&module_ids);

        assert_eq!(
            tsjs_script_tag(&module_ids),
            format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>"),
            "should generate exactly one trusted server script tag"
        );
    }

    #[test]
    fn creative_bootstrap_uses_only_the_required_critical_modules() {
        let settings = crate::test_support::tests::create_test_settings();
        let bootstrap = creative_tsjs_bootstrap_v1(&settings)
            .expect("should build the creative bootstrap")
            .expect("the default creative integration should be enabled");
        let expected_src = tsjs_script_src(&["render_runtime", "creative"]);

        assert!(bootstrap.contains("t.boot="), "{bootstrap}");
        assert!(
            bootstrap.contains(r#"{"id":"render_runtime","phase":"critical"}"#),
            "{bootstrap}"
        );
        assert!(
            bootstrap.contains(r#"{"id":"creative","phase":"critical"}"#),
            "{bootstrap}"
        );
        assert!(
            bootstrap.contains(&format!(r#"src="{expected_src}""#)),
            "{bootstrap}"
        );
        assert_eq!(bootstrap.matches("id=\"trustedserver-js\"").count(), 1);
        assert!(!bootstrap.contains(r#""id":"gpt""#), "{bootstrap}");
        assert!(!bootstrap.contains(r#""phase":"deferred""#), "{bootstrap}");
    }

    #[test]
    fn creative_bootstrap_is_absent_when_both_guards_are_disabled() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "creative",
                &serde_json::json!({
                    "enabled": true,
                    "click_guard": false,
                    "render_guard": false,
                }),
            )
            .expect("should configure creative browser policy");

        assert_eq!(
            creative_tsjs_bootstrap_v1(&settings)
                .expect("should resolve the creative bootstrap policy"),
            None,
            "zero browser work must not inject a runtime into creative HTML"
        );
    }

    #[test]
    fn boot_script_accepts_enabled_creative_with_no_guards_and_no_module() {
        assert!(
            tsjs_boot_script_v1(
                TsjsBootScriptConfigV1 {
                    module_ids: &["render_runtime"],
                    auction_projection_json: EMPTY_CREATIVE_AUCTION_PROJECTION_JSON,
                    creative: CreativeBootConfigV1 {
                        enabled: true,
                        click_guard: false,
                        render_guard: false,
                    },
                    render_trace_overlay: false,
                    gpt_diagnostics_active: false,
                },
                "https://publisher.example"
            )
            .is_ok(),
            "creative membership is required only when a guard has browser work"
        );
    }

    #[test]
    fn tsjs_unified_helpers_use_all_module_ids() {
        let ids = all_module_ids();

        assert_eq!(
            tsjs_script_tag_with_attributes(&module_ids, &[("data-ts-gam-attribution", "true")]),
            format!(
                "<script src=\"{src}\" id=\"trustedserver-js\" data-ts-gam-attribution=\"true\"></script>"
            ),
            "should render trusted static attributes on the publisher bundle tag"
        );
        assert_eq!(
            tsjs_script_tag(&module_ids),
            format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>"),
            "should keep the generic tag byte-for-byte unmarked"
        );
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_invalid_attribute_name() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-bad_name", "true")]);
    }

    #[test]
    #[should_panic(
        expected = "attribute name should contain only lowercase ASCII letters, digits, and hyphens"
    )]
    fn publisher_tsjs_script_tag_rejects_empty_attribute_name() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("", "true")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_double_quote_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad\"value")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_ampersand_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad&value")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_less_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad<value")]);
    }

    #[test]
    #[should_panic(expected = "attribute value should not contain HTML-sensitive characters")]
    fn publisher_tsjs_script_tag_rejects_greater_than_in_attribute_value() {
        let _ = tsjs_script_tag_with_attributes(&["gpt"], &[("data-safe-name", "bad>value")]);
    }

    #[test]
    fn tsjs_unified_helpers_use_unversioned_fallback_without_registry() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "registry-free unified helper should not emit an unverifiable hash"
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            format!(r#"<script src="{src}" id="trustedserver-js"></script>"#),
            "should wrap the registry-free unified source"
        );
    }

    #[test]
    fn tsjs_single_module_script_src_formats_known_module_url_with_hash() {
        let src =
            tsjs_single_module_script_src("prebid_later").expect("should name a deferred module");

        assert!(
            src.starts_with("/static/tsjs=tsjs-prebid_later.min.js?v="),
            "should use per-module static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(tsjs_single_module_script_src("creative"), None);
    }

    #[test]
    fn tsjs_deferred_script_src_hashes_only_catalogued_deferred_modules() {
        let prebid_src =
            tsjs_deferred_script_src("prebid_later").expect("should name Prebid later slice");
        assert!(
            prebid_src.starts_with("/static/tsjs=tsjs-prebid_later.min.js?v="),
            "Prebid later slice should use the deferred TSJS route"
        );
        assert_sha256_hex_hash(hash_query_value(&prebid_src));
        assert_eq!(tsjs_deferred_script_src("unknown-module"), None);
        assert_eq!(tsjs_deferred_script_src("prebid"), None);
    }

    #[test]
    fn tsjs_deferred_script_tag_marks_script_defer() {
        let src = tsjs_deferred_script_src("prebid_later").expect("should name Prebid later slice");

        assert_eq!(
            tsjs_deferred_script_tag("prebid_later"),
            Some(format!("<script src=\"{src}\" defer></script>")),
            "should generate a deferred script tag"
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_returns_empty_for_empty_input() {
        assert_eq!(
            tsjs_deferred_script_tags(&[]),
            "",
            "should not emit tags when no deferred modules exist"
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_preserves_input_order() {
        assert_eq!(
            tsjs_deferred_script_tags(&["prebid_later", "sourcepoint_lifecycle"]),
            format!(
                "{}{}",
                tsjs_deferred_script_tag("prebid_later").expect("should build Prebid tag"),
                tsjs_deferred_script_tag("sourcepoint_lifecycle")
                    .expect("should build Sourcepoint tag")
            ),
            "should preserve caller-provided deferred module order"
        );
    }

    #[test]
    fn tsjs_unified_script_src_and_tag_omit_unverifiable_cache_busting_hash() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "should use the unified script URL without an unverifiable hash"
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            format!(r#"<script src="{src}" id="trustedserver-js"></script>"#),
            "should wrap the unified source in a trusted server script tag"
        );
    }

    #[test]
    fn tsjs_script_src_differs_for_different_module_sets() {
        assert_ne!(
            tsjs_script_src(&["lockr"]),
            tsjs_script_src(&["lockr", "permutive"]),
            "should bust the cache when the module set content changes"
        );
    }

    #[test]
    fn tsjs_deferred_script_src_rejects_unknown_module() {
        assert_eq!(tsjs_deferred_script_src("does-not-exist"), None);
    }

    fn first_display_projection(source: Option<BidRenderSourceV1>) -> BrowserAuctionProjectionV1 {
        use crate::auction::types::{
            AuctionDecisionSetV1, BrowserAuctionBidV1, BrowserAuctionSlotV1, SlotAuctionDecisionV1,
        };

        let slot = BrowserAuctionSlotV1 {
            slot: "slot-1".to_string(),
            gam_unit_path: "/123/home".to_string(),
            div_id: "div-1".to_string(),
            formats: vec![[300, 250]],
            targeting: std::collections::BTreeMap::new(),
        };
        let (results, bids) = source.map_or_else(
            || {
                (
                    vec![SlotAuctionDecisionV1::NoBid {
                        slot: "slot-1".to_string(),
                    }],
                    vec![],
                )
            },
            |render_source| {
                (
                    vec![SlotAuctionDecisionV1::Winner {
                        slot: "slot-1".to_string(),
                        candidate_id: "candidate-1".to_string(),
                    }],
                    vec![BrowserAuctionBidV1 {
                        candidate_id: "candidate-1".to_string(),
                        slot: "slot-1".to_string(),
                        provider: "prebid".to_string(),
                        upstream_bid_id: "bid-1".to_string(),
                        cpm: 1.0,
                        currency: "USD".to_string(),
                        targeting: std::collections::BTreeMap::new(),
                        renderer_reservation_id: match render_source {
                            BidRenderSourceV1::Aps(_) | BidRenderSourceV1::Adm(_) => {
                                Some("reservation-1".to_string())
                            }
                            BidRenderSourceV1::PbsCache(_) => None,
                        },
                        render_source,
                    }],
                )
            },
        );
        BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "initial".to_string(),
                results,
            },
            slots: vec![slot],
            bids,
        }
    }

    fn adm_source() -> BidRenderSourceV1 {
        use crate::auction::types::AdmRenderSourceV1;
        BidRenderSourceV1::Adm(AdmRenderSourceV1 {
            version: 1,
            adm: "<div>creative</div>".to_string(),
            width: 300,
            height: 250,
        })
    }

    fn cache_source() -> BidRenderSourceV1 {
        use crate::auction::types::BaselinePbsCacheSourceV1;
        BidRenderSourceV1::PbsCache(BaselinePbsCacheSourceV1 {
            version: 1,
            cache_id: "cache-1".to_string(),
            cache_host: "cache.example".to_string(),
            cache_path: "/cache".to_string(),
            width: 300,
            height: 250,
        })
    }

    #[test]
    fn first_display_selection_admits_only_the_closed_projected_batch() {
        let no_bid = first_display_projection(None);
        let adm = first_display_projection(Some(adm_source()));
        let enabled = ["gpt"];

        let no_bid_selected = select_first_display_slices_v1(
            &no_bid,
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &enabled,
                creative: CreativeBootConfigV1::default(),
            },
        )
        .expect("closed no-bid batch should select the base agent");
        assert_eq!(no_bid_selected.mask(), 0x0001);
        assert_eq!(no_bid_selected.slices(), &["first_display"]);

        let adm_selected = select_first_display_slices_v1(
            &adm,
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &enabled,
                creative: CreativeBootConfigV1::default(),
            },
        )
        .expect("closed GPT ADM batch should select GPT initial ownership");
        assert_eq!(adm_selected.mask(), 0x0041);
        assert_eq!(adm_selected.slices(), &["first_display", "gpt_initial"]);

        assert!(
            select_first_display_slices_v1(
                &first_display_projection(Some(cache_source())),
                FirstDisplaySelectionConfigV1 {
                    enabled_integrations: &enabled,
                    creative: CreativeBootConfigV1::default(),
                },
            )
            .is_none(),
            "PBS Cache must stay on the direct persistent GPT path"
        );
        let mut direct = no_bid.clone();
        direct.slots.clear();
        assert!(
            select_first_display_slices_v1(
                &direct,
                FirstDisplaySelectionConfigV1 {
                    enabled_integrations: &enabled,
                    creative: CreativeBootConfigV1::default(),
                },
            )
            .is_none(),
            "direct/programmatic projection must not select the agent"
        );
    }

    #[test]
    fn first_display_selection_derives_exact_slices_only_from_trusted_configuration() {
        let enabled = [
            "aps",
            "creative",
            "datadome",
            "didomi",
            "google_tag_manager",
            "gpt",
            "lockr",
            "osano",
            "permutive",
            "prebid",
            "sourcepoint",
            "testlight",
        ];
        let mut projection = first_display_projection(Some(adm_source()));
        projection.bids[0].provider = "prebid".to_owned();
        let selected = select_first_display_slices_v1(
            &projection,
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &enabled,
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: false,
                    render_guard: true,
                },
            },
        )
        .expect("complete closed configuration should select the agent");

        assert_eq!(
            selected.slices(),
            &[
                "first_display",
                "creative_initial",
                "datadome_initial",
                "didomi_initial",
                "google_tag_manager_initial",
                "gpt_initial",
                "lockr_initial",
                "osano_initial",
                "permutive_initial",
                "sourcepoint_initial",
                "prebid_initial",
                "testlight_initial",
            ]
        );
        assert_eq!(selected.mask(), 0x1ffd);

        let unknown = ["gpt", "publisher_supplied"];
        assert!(
            select_first_display_slices_v1(
                &first_display_projection(None),
                FirstDisplaySelectionConfigV1 {
                    enabled_integrations: &unknown,
                    creative: CreativeBootConfigV1::default(),
                },
            )
            .is_none(),
            "an unknown obligation must force direct persistent boot"
        );
    }
}
