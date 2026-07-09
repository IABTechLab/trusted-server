//! Reconstructs `[creative_opportunities]` slots from a live page's GPT state.
//!
//! Two complementary sources feed the reconstruction:
//!
//! 1. The **live GPT registry** (`googletag.pubads().getSlots()`) is the primary
//!    source. It exposes each defined slot's ad-unit path, div id, and sizes
//!    directly, and is populated at `defineSlot` time — so it captures slots even
//!    when the ad request never fires (consent-gated stacks, iframe-issued
//!    requests). It carries no per-slot header-bidding signal, so Prebid is
//!    inferred from page-level detection.
//! 2. Captured **`gampad/ads` requests** are a fallback for any div the registry
//!    did not report. Each request URL encodes the ad-unit path (`iu_parts`), div
//!    id (`dids`), sizes (`prev_iu_szs`), and targeting (`prev_scp`, which does
//!    carry a per-slot Prebid signal).
//!
//! Neither source executes the page's ad-stack logic ourselves; both read state
//! the page's own GPT/Prebid setup produced.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use trusted_server_core::creative_opportunities::validate_slot_id;
use url::Url;

use crate::commands::audit::generate::collector::{CollectedGptSlot, CollectedRequest};

/// A hyphen-delimited hex hash *segment* (16+ hex chars bounded by `-` or end),
/// e.g. the UUID GPT embeds in `ad-in_content-<uuid>-in_content-0`. Marks the
/// start of ephemeral div-id noise, like the React `_R_` hash. The trailing
/// boundary avoids truncating a legit token that merely starts with hex-like
/// characters (only `start()` of the match is used).
static HEX_HASH_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-[0-9a-f]{16,}(?:-|$)").expect("should compile hex hash regex"));

/// Hosts that serve GPT `gampad/ads` requests.
const GAMPAD_HOSTS: &[&str] = &["securepubads.g.doubleclick.net", "pubads.g.doubleclick.net"];

/// Common GPT div-id prefix stripped when deriving a slot id.
const GPT_DIV_PREFIX: &str = "div-gpt-ad-";

/// Minimum width/height for a format to be treated as a real creative size.
///
/// GPT encodes fluid/native aspect-ratio markers (e.g. `4x1`, `8x1`) alongside
/// pixel sizes in `prev_iu_szs`; those are not banner dimensions, so they are
/// dropped from the drafted `formats`.
const MIN_FORMAT_DIMENSION: u32 = 50;

/// A slot reconstructed from a single GPT ad request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredSlot {
    /// Slot id derived from the div id (GPT prefix stripped).
    pub(crate) id: String,
    /// The HTML div id that holds the creative.
    pub(crate) div_id: String,
    /// The full GAM ad-unit path (e.g. `/123/desktop/homepage/leaderboard`).
    pub(crate) gam_unit_path: String,
    /// Candidate creative sizes as `(width, height)` pixel pairs.
    pub(crate) formats: Vec<(u32, u32)>,
    /// Whether the slot's targeting shows Prebid/header-bidding signals.
    pub(crate) has_prebid: bool,
}

/// The result of scanning captured requests for GPT slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DiscoveredSlots {
    /// GAM network id shared by the discovered slots, if any were found.
    pub(crate) gam_network_id: Option<String>,
    /// The reconstructed slots, deduplicated by div id in first-seen order.
    pub(crate) slots: Vec<DiscoveredSlot>,
}

/// Reconstructs GPT slots from the page's live registry and ad requests.
///
/// The live registry (`googletag.pubads().getSlots()`) is the primary source: it
/// carries the authoritative path/div/size for every defined slot and is present
/// even when the ad request never fires. Captured `gampad/ads` requests are a
/// fallback for any div the registry did not report, and also supply per-slot
/// Prebid signals. Slots are deduplicated by div id in first-seen order.
///
/// `page_has_prebid` marks registry slots as Prebid-enabled when the page as a
/// whole was detected running Prebid (the registry alone carries no such signal).
pub(crate) fn discover_gpt_slots(
    registry: &[CollectedGptSlot],
    requests: &[CollectedRequest],
    page_has_prebid: bool,
) -> DiscoveredSlots {
    let mut slots = Vec::new();
    let mut gam_network_id = None;
    let mut seen_divs = BTreeSet::new();

    for entry in registry {
        let Some(slot) = slot_from_registry(entry, page_has_prebid) else {
            continue;
        };
        if !seen_divs.insert(slot.div_id.clone()) {
            continue;
        }
        if gam_network_id.is_none() {
            gam_network_id = network_id_from_unit_path(&slot.gam_unit_path);
        }
        slots.push(slot);
    }

    for request in requests {
        let Some((network_id, slot)) = parse_gampad_request(&request.url) else {
            continue;
        };
        if !seen_divs.insert(slot.div_id.clone()) {
            continue;
        }
        if gam_network_id.is_none() {
            gam_network_id = Some(network_id);
        }
        slots.push(slot);
    }
    make_slot_ids_unique(&mut slots);

    DiscoveredSlots {
        gam_network_id,
        slots,
    }
}

/// Converts a live-registry slot into a [`DiscoveredSlot`].
///
/// Returns `None` when the slot has no usable pixel size or its div id is a
/// multi-slot (SRA) concatenation rather than a single element.
fn slot_from_registry(entry: &CollectedGptSlot, page_has_prebid: bool) -> Option<DiscoveredSlot> {
    if is_multi_slot_div(&entry.div_id) {
        return None;
    }
    let formats: Vec<(u32, u32)> = entry
        .sizes
        .iter()
        .copied()
        .filter(|(width, height)| *width >= MIN_FORMAT_DIMENSION && *height >= MIN_FORMAT_DIMENSION)
        .collect();
    if formats.is_empty() {
        return None;
    }
    let div_stem = normalize_div_stem(&entry.div_id);
    Some(DiscoveredSlot {
        id: slot_id_from_div(&div_stem),
        div_id: div_stem,
        gam_unit_path: entry.gam_unit_path.clone(),
        formats,
        has_prebid: page_has_prebid,
    })
}

/// Whether a div id is a GPT single-request (SRA) concatenation of multiple
/// slots (joined with `~`) rather than one element.
fn is_multi_slot_div(div_id: &str) -> bool {
    div_id.contains('~')
}

/// Strips ephemeral GPT div-id noise so the stored id is stable across renders.
///
/// Removes a trailing `-container` wrapper, then truncates at the first ephemeral
/// marker — a React SSR hash (`_R_<hash>`) or a hex-UUID segment — since both
/// change on every page load. Truncating (rather than excising) keeps the result
/// a valid **prefix** of the live div id, which is how verify matches slots.
///
/// `div-gpt-ad-leaderboard-1` (stable) is unchanged; `ad-header-0-_R_9sl…-container`
/// → `ad-header-0`; `ad-in_content-de66…f272-in_content-0` → `ad-in_content`.
fn normalize_div_stem(div_id: &str) -> String {
    let stem = div_id.strip_suffix("-container").unwrap_or(div_id);
    let mut cut = stem.len();
    if let Some(pos) = stem.find("_R_") {
        cut = cut.min(pos);
    }
    if let Some(matched) = HEX_HASH_SEGMENT.find(stem) {
        cut = cut.min(matched.start());
    }
    stem[..cut].trim_end_matches('-').to_string()
}

/// Extracts the leading network id from a GAM ad-unit path (`/<network>/...`).
fn network_id_from_unit_path(path: &str) -> Option<String> {
    let segment = path.trim_start_matches('/').split('/').next()?;
    (!segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| segment.to_string())
}

/// Parses a single `gampad/ads` request URL into `(network_id, slot)`.
///
/// Returns `None` when the URL is not a GPT ad request or is missing the fields
/// needed to describe a slot (ad-unit path, div id, and at least one size).
fn parse_gampad_request(raw_url: &str) -> Option<(String, DiscoveredSlot)> {
    let url = Url::parse(raw_url).ok()?;
    let host = url.host_str()?;
    if !GAMPAD_HOSTS.contains(&host) || !url.path().ends_with("/gampad/ads") {
        return None;
    }

    let mut iu_parts = None;
    let mut dids = None;
    let mut sizes_raw = None;
    let mut fallback_sizes_raw = None;
    let mut scp = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "iu_parts" => iu_parts = Some(value.into_owned()),
            "dids" => dids = Some(value.into_owned()),
            "prev_iu_szs" => sizes_raw = Some(value.into_owned()),
            "pb_szs" => fallback_sizes_raw = Some(value.into_owned()),
            "prev_scp" => scp = Some(value.into_owned()),
            _ => {}
        }
    }

    let iu_parts = iu_parts?;
    let mut parts = iu_parts.split(',').filter(|part| !part.is_empty());
    // Mirror the registry path's validation: a GAM network id is digits only.
    // The percent-decoded query value is page-controlled and gets spliced into
    // generated TOML, so reject anything else.
    let network_id = parts
        .next()
        .filter(|segment| segment.bytes().all(|byte| byte.is_ascii_digit()))?
        .to_string();
    let gam_unit_path = format!("/{}", iu_parts.replace(',', "/"));
    // A usable unit path needs the network id plus at least one path segment.
    parts.next()?;

    let raw_div = dids?
        .split(',')
        .map(str::trim)
        .find(|did| !did.is_empty())?
        .to_string();
    if is_multi_slot_div(&raw_div) {
        return None;
    }
    let div_id = normalize_div_stem(&raw_div);

    let formats = parse_sizes(sizes_raw.as_deref().or(fallback_sizes_raw.as_deref())?);
    if formats.is_empty() {
        return None;
    }

    let id = slot_id_from_div(&div_id);
    let has_prebid = scp.as_deref().is_some_and(scp_shows_prebid);

    Some((
        network_id,
        DiscoveredSlot {
            id,
            div_id,
            gam_unit_path,
            formats,
            has_prebid,
        },
    ))
}

/// Parses a GPT size list (e.g. `970x250|4x1|620x366`) into pixel pairs.
///
/// Accepts `|` or `,` separators, ignores non-`WxH` tokens, and drops
/// fluid/native ratio markers below [`MIN_FORMAT_DIMENSION`].
fn parse_sizes(raw: &str) -> Vec<(u32, u32)> {
    let mut sizes = Vec::new();
    for token in raw.split(['|', ',']) {
        let Some((width, height)) = token.trim().split_once('x') else {
            continue;
        };
        let (Ok(width), Ok(height)) = (width.parse::<u32>(), height.parse::<u32>()) else {
            continue;
        };
        if width < MIN_FORMAT_DIMENSION || height < MIN_FORMAT_DIMENSION {
            continue;
        }
        if !sizes.contains(&(width, height)) {
            sizes.push((width, height));
        }
    }
    sizes
}

/// Derives a runtime-safe slot id from a div id.
///
/// The common GPT prefix is stripped, invalid character runs become one
/// hyphen, and an all-invalid value falls back to `slot`.
fn slot_id_from_div(div_id: &str) -> String {
    let candidate = div_id.strip_prefix(GPT_DIV_PREFIX).unwrap_or(div_id);
    let mut id = String::with_capacity(candidate.len());
    let mut previous_was_hyphen = false;
    for character in candidate.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            id.push(character);
            previous_was_hyphen = false;
        } else if !id.is_empty() && !previous_was_hyphen {
            id.push('-');
            previous_was_hyphen = true;
        }
    }
    while id.ends_with('-') {
        id.pop();
    }
    if id.is_empty() {
        id.push_str("slot");
    }

    if validate_slot_id(&id).is_ok() {
        id
    } else {
        "slot".to_string()
    }
}

/// Adds deterministic numeric suffixes when sanitization produces duplicate ids.
fn make_slot_ids_unique(slots: &mut [DiscoveredSlot]) {
    let mut used = BTreeSet::new();
    for slot in slots {
        if used.insert(slot.id.clone()) {
            continue;
        }

        let base = slot.id.clone();
        let mut suffix = 2_usize;
        loop {
            let candidate = format!("{base}-{suffix}");
            if used.insert(candidate.clone()) {
                slot.id = candidate;
                break;
            }
            suffix += 1;
        }
    }
}

/// Detects Prebid/header-bidding signals in a slot's `prev_scp` targeting.
fn scp_shows_prebid(scp: &str) -> bool {
    let scp = scp.to_ascii_lowercase();
    scp.contains("test=prebid") || scp.contains("tude=true") || scp.contains("prebid")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample GPT leaderboard ad request (truncated to the fields the
    /// parser reads; values are otherwise unmodified live output).
    const SAMPLE_LEADERBOARD: &str = "https://securepubads.g.doubleclick.net/gampad/ads?\
        gdfp_req=1&iu_parts=123456789%2Cdesktop%2Chomepage%2Cleaderboard1\
        &prev_iu_szs=970x250%7C4x1%7C8x1%7C620x366%7C325x508%7C325x204\
        &dids=div-gpt-ad-leaderboard-1\
        &prev_scp=ad-loc%3Dleaderboard-1%26baseDivId%3Ddiv-gpt-ad-leaderboard-1%26test%3Dprebid%26tude%3Dtrue\
        &pb_szs=970x250%7C620x366";

    fn request(url: &str) -> CollectedRequest {
        CollectedRequest {
            url: url.to_string(),
            resource_type: Some("fetch".to_string()),
        }
    }

    /// Discovers slots from ad requests only (no live registry).
    fn from_requests(requests: &[CollectedRequest]) -> DiscoveredSlots {
        discover_gpt_slots(&[], requests, false)
    }

    #[test]
    fn parses_leaderboard_slot() {
        let discovered = from_requests(&[request(SAMPLE_LEADERBOARD)]);

        assert_eq!(discovered.gam_network_id.as_deref(), Some("123456789"));
        assert_eq!(discovered.slots.len(), 1, "should find one slot");
        let slot = &discovered.slots[0];
        assert_eq!(slot.id, "leaderboard-1", "should strip the GPT div prefix");
        assert_eq!(slot.div_id, "div-gpt-ad-leaderboard-1");
        assert_eq!(
            slot.gam_unit_path,
            "/123456789/desktop/homepage/leaderboard1"
        );
        assert_eq!(
            slot.formats,
            vec![(970, 250), (620, 366), (325, 508), (325, 204)],
            "should keep pixel sizes and drop 4x1/8x1 fluid markers"
        );
        assert!(slot.has_prebid, "prev_scp test=prebid should flag prebid");
    }

    #[test]
    fn deduplicates_refreshed_slot_requests() {
        // GPT refreshes the same slot; a second identical request must not
        // produce a duplicate slot.
        let discovered = from_requests(&[request(SAMPLE_LEADERBOARD), request(SAMPLE_LEADERBOARD)]);

        assert_eq!(
            discovered.slots.len(),
            1,
            "repeat requests for the same div should collapse"
        );
    }

    #[test]
    fn ignores_non_gampad_requests() {
        let discovered = from_requests(&[
            request("https://securepubads.g.doubleclick.net/tag/js/gpt.js"),
            request("https://cdn.example.com/app.js"),
            request("https://analytics.example.com/collect?iu_parts=1%2Cfoo&dids=x"),
        ]);

        assert!(
            discovered.slots.is_empty(),
            "only doubleclick gampad/ads requests should yield slots"
        );
        assert_eq!(discovered.gam_network_id, None);
    }

    #[test]
    fn skips_requests_missing_sizes() {
        let discovered = from_requests(&[request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123%2Cslot&dids=div-gpt-ad-x",
        )]);

        assert!(
            discovered.slots.is_empty(),
            "a slot with no usable size should be skipped"
        );
    }

    #[test]
    fn skips_requests_with_only_network_id() {
        // iu_parts with just the network id yields no unit path segment.
        let discovered = from_requests(&[request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123&dids=div-gpt-ad-x&prev_iu_szs=300x250",
        )]);

        assert!(
            discovered.slots.is_empty(),
            "a bare network id is not a usable ad-unit path"
        );
    }

    #[test]
    fn skips_requests_with_non_numeric_network_id() {
        // A page-controlled iu_parts value must not smuggle a non-numeric
        // network id (it gets spliced into generated TOML).
        let discovered = from_requests(&[request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123%22evil%2Cslot&dids=div-gpt-ad-x&prev_iu_szs=300x250",
        )]);

        assert!(
            discovered.slots.is_empty(),
            "a non-numeric network id should be rejected"
        );
        assert_eq!(discovered.gam_network_id, None);
    }

    #[test]
    fn falls_back_to_pb_szs_when_prev_iu_szs_absent() {
        let discovered = from_requests(&[request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123%2Cslot&dids=div-gpt-ad-x&pb_szs=300x250%7C728x90",
        )]);

        assert_eq!(discovered.slots.len(), 1);
        assert_eq!(discovered.slots[0].formats, vec![(300, 250), (728, 90)]);
    }

    fn registry_slot(path: &str, div: &str, sizes: &[(u32, u32)]) -> CollectedGptSlot {
        CollectedGptSlot {
            gam_unit_path: path.to_string(),
            div_id: div.to_string(),
            sizes: sizes.to_vec(),
        }
    }

    #[test]
    fn reads_slots_from_live_registry() {
        let registry = vec![registry_slot(
            "/123456789/desktop/homepage/leaderboard1",
            "div-gpt-ad-leaderboard-1",
            &[(970, 250), (1, 1), (620, 366)],
        )];

        let discovered = discover_gpt_slots(&registry, &[], true);

        assert_eq!(
            discovered.gam_network_id.as_deref(),
            Some("123456789"),
            "network id should come from the unit path"
        );
        assert_eq!(discovered.slots.len(), 1);
        let slot = &discovered.slots[0];
        assert_eq!(slot.id, "leaderboard-1");
        assert_eq!(
            slot.formats,
            vec![(970, 250), (620, 366)],
            "should drop the 1x1 out-of-page marker"
        );
        assert!(
            slot.has_prebid,
            "page-level prebid should mark registry slots"
        );
    }

    #[test]
    fn registry_wins_and_requests_fill_gaps() {
        // The registry reports the leaderboard; a gampad request reports a
        // different div that the registry missed. Both should appear once.
        let registry = vec![registry_slot(
            "/123456789/desktop/homepage/leaderboard1",
            "div-gpt-ad-leaderboard-1",
            &[(970, 250)],
        )];
        let requests = vec![
            // Same div as the registry — must not duplicate.
            request(SAMPLE_LEADERBOARD),
            // A div the registry did not report — must be added.
            request(
                "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123456789%2Cdesktop%2Chomepage%2Csidebar1&dids=div-gpt-ad-sidebar-1&prev_iu_szs=300x600",
            ),
        ];

        let discovered = discover_gpt_slots(&registry, &requests, false);

        let ids: Vec<&str> = discovered
            .slots
            .iter()
            .map(|slot| slot.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["leaderboard-1", "sidebar-1"],
            "registry slot kept, request fills the missing div, no duplicate"
        );
    }

    #[test]
    fn registry_slot_without_pixel_sizes_is_skipped() {
        let registry = vec![registry_slot("/123/fluid", "div-gpt-ad-fluid", &[(1, 1)])];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert!(
            discovered.slots.is_empty(),
            "a registry slot with only fluid markers is not usable"
        );
    }

    #[test]
    fn normalizes_ephemeral_hash_and_container_and_dedups() {
        // A framework-hashed div: the same placement appears as a hashed inner div,
        // a `-container` wrapper, and re-rendered with a different hash. All must
        // collapse to one stable stem.
        let registry = vec![
            registry_slot(
                "/987654321/homepage/header-0",
                "ad-header-0-_R_9slinpflik6lb_",
                &[(728, 90)],
            ),
            registry_slot(
                "/987654321/homepage/header-0",
                "ad-header-0-_R_9slinpflik6lb_-container",
                &[(728, 90)],
            ),
        ];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(
            discovered.slots.len(),
            1,
            "hash + container variants collapse"
        );
        assert_eq!(
            discovered.slots[0].div_id, "ad-header-0",
            "ephemeral React hash and -container are stripped to a stable stem"
        );
        assert_eq!(discovered.slots[0].id, "ad-header-0");
    }

    #[test]
    fn drops_sra_multi_slot_concatenations() {
        let registry = vec![registry_slot(
            "/987654321/homepage/header-0/fixed_bottom-0",
            "ad-header-0-_R_9slin~ad-fixed_bottom-0-_R_ainp",
            &[(728, 90)],
        )];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert!(
            discovered.slots.is_empty(),
            "tilde-joined SRA multi-slot divs are not real single elements"
        );
    }

    #[test]
    fn leaves_clean_div_ids_unchanged() {
        assert_eq!(
            normalize_div_stem("div-gpt-ad-leaderboard-1"),
            "div-gpt-ad-leaderboard-1"
        );
    }

    #[test]
    fn sanitizes_page_controlled_div_ids_for_runtime_slot_ids() {
        let registry = vec![registry_slot(
            "/123456789/homepage/header",
            "div-gpt-ad-header.main: 1",
            &[(728, 90)],
        )];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(discovered.slots[0].id, "header-main-1");
        assert_eq!(
            discovered.slots[0].div_id, "div-gpt-ad-header.main: 1",
            "matching should retain the original normalized div stem"
        );
        trusted_server_core::creative_opportunities::validate_slot_id(&discovered.slots[0].id)
            .expect("generated id should pass runtime validation");
    }

    #[test]
    fn uses_fallback_for_div_id_without_safe_slot_id_characters() {
        let registry = vec![registry_slot(
            "/123456789/homepage/fallback",
            "div-gpt-ad-...",
            &[(300, 250)],
        )];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(discovered.slots[0].id, "slot");
    }

    #[test]
    fn makes_colliding_sanitized_slot_ids_unique() {
        let registry = vec![
            registry_slot(
                "/123456789/homepage/dotted",
                "div-gpt-ad-header.main",
                &[(728, 90)],
            ),
            registry_slot(
                "/123456789/homepage/colon",
                "div-gpt-ad-header:main",
                &[(300, 250)],
            ),
        ];

        let discovered = discover_gpt_slots(&registry, &[], false);
        let ids = discovered
            .slots
            .iter()
            .map(|slot| slot.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["header-main", "header-main-2"]);
    }

    #[test]
    fn normalizes_react_and_hex_hashes_to_stable_prefixes() {
        assert_eq!(
            normalize_div_stem("ad-header-0-_R_9slinpflik6lb_-container"),
            "ad-header-0"
        );
        let stem =
            normalize_div_stem("ad-in_content-de669245b2ea4b05826dc96f07a36272-in_content-0");
        assert_eq!(stem, "ad-in_content");
        assert!(
            "ad-in_content-de669245b2ea4b05826dc96f07a36272-in_content-0".starts_with(&stem),
            "stem must prefix-match any re-rendered hex variant"
        );
    }

    #[test]
    fn hex_hash_truncation_requires_a_segment_boundary() {
        // Hex UUID bounded by `-` → truncated to the stem.
        assert_eq!(
            normalize_div_stem("ad-x-de669245b2ea4b05826dc96f07a36272-y"),
            "ad-x"
        );
        // A token that merely starts with 16 hex chars (no boundary) is left intact.
        assert_eq!(
            normalize_div_stem("ad-de669245b2ea4b05z"),
            "ad-de669245b2ea4b05z"
        );
    }

    #[test]
    fn hex_normalized_in_content_slots_dedup() {
        // Same in_content placement, different per-render hex — one stable slot.
        let registry = vec![
            registry_slot(
                "/987654321/site/homepage",
                "ad-in_content-de669245b2ea4b05826dc96f07a36272-in_content-0",
                &[(300, 250)],
            ),
            registry_slot(
                "/987654321/site/homepage",
                "ad-in_content-8aec8129a83d4e5abc197423120cb19e-in_content-0",
                &[(300, 250)],
            ),
        ];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(
            discovered.slots.len(),
            1,
            "hex variants collapse to one slot"
        );
        assert_eq!(discovered.slots[0].div_id, "ad-in_content");
    }
}
