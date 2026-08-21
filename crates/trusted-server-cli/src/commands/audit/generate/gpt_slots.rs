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

use std::collections::{BTreeMap, BTreeSet};
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
static UUID_SEGMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(?:-|$)")
        .expect("should compile UUID regex")
});

/// Matches a React `useId` token, which changes on every render.
///
/// React emits these in both cases — `_R_3f_` from a server render and `_r_0_`
/// from a client one — so matching only the uppercase form leaves the lowercase
/// variant in the stem. That is not merely untidy: the suffix differs per
/// render, so one logical slot fragments into a new key on every page, which
/// both breaks runtime div matching and starves template inference of the
/// repeated observations it needs.
///
/// The uppercase form is distinctive enough to match bare, and its hash is
/// included so the match spans the whole ephemeral token — [`normalize_div_stem`]
/// only reads the match *start*, but [`ephemeral_marker_residue`] excises the
/// match, and a residue that still carried the hash would make two renders of one
/// element look like two elements. The lowercase form is anchored (`_r_`, a short
/// alphanumeric run, `_`) so an ordinary id that merely contains `_r_` keeps its
/// full stem.
static REACT_USE_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"_R_[0-9a-z]*_?|_r_[0-9a-z]{1,8}_").expect("should compile react id regex")
});

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
    /// Whether the page exposed any otherwise usable slot evidence, including
    /// ambiguous placements that were intentionally omitted from `slots`.
    pub(crate) had_slot_evidence: bool,
    /// The reconstructed slots, deduplicated by div id in first-seen order.
    pub(crate) slots: Vec<DiscoveredSlot>,
    /// Div stems refused because several live elements normalized onto them.
    ///
    /// Carried separately from `slots` because the verdict is a property of the
    /// *site*, not of this page: another page that happens to render only one
    /// member of the group must not resurrect the ambiguous prefix.
    pub(crate) ambiguous_stems: BTreeSet<String>,
    /// Diagnostics for placements whose normalized stable stems collided.
    pub(crate) warnings: Vec<String>,
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
    let mut warnings = Vec::new();
    let mut ambiguous_stems = BTreeSet::new();
    let mut gam_network_id = None;
    let mut had_slot_evidence = false;
    let mut registry_residues: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Stems refused outright, so the request fallback cannot re-add them. Kept
    // apart from `registry_residues` so a later registry entry cannot read a
    // refused stem as a one-member collision group.
    let mut refused_stems: BTreeSet<String> = BTreeSet::new();

    for entry in registry {
        let Some(slot) = slot_from_registry(entry, page_has_prebid) else {
            continue;
        };
        had_slot_evidence = true;
        if gam_network_id.is_none() {
            gam_network_id = network_id_from_unit_path(&entry.gam_unit_path);
        }
        if let Some(prefix) = volatile_prefix_before_placement(&entry.div_id) {
            refused_stems.insert(slot.div_id.clone());
            push_unique_warning(&mut warnings, volatile_prefix_warning(&prefix));
            continue;
        }
        if let Some(prefix) =
            push_slot_refusing_collisions(&mut slots, &mut registry_residues, slot, &entry.div_id)
        {
            warnings.push(ambiguous_collision_warning(&prefix));
            ambiguous_stems.insert(prefix);
        }
    }

    let registry_stems: BTreeSet<String> = registry_residues
        .keys()
        .cloned()
        .chain(refused_stems)
        .collect();
    let mut request_residues: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for request in requests {
        let Some((network_id, slot, raw_div)) = parse_gampad_request(&request.url) else {
            continue;
        };
        had_slot_evidence = true;
        if gam_network_id.is_none() {
            gam_network_id = Some(network_id);
        }
        if registry_stems.contains(&slot.div_id) {
            continue;
        }
        if let Some(prefix) = volatile_prefix_before_placement(&raw_div) {
            push_unique_warning(&mut warnings, volatile_prefix_warning(&prefix));
            continue;
        }
        if let Some(prefix) =
            push_slot_refusing_collisions(&mut slots, &mut request_residues, slot, &raw_div)
        {
            warnings.push(ambiguous_collision_warning(&prefix));
            ambiguous_stems.insert(prefix);
        }
    }
    make_slot_ids_unique(&mut slots);

    DiscoveredSlots {
        gam_network_id,
        had_slot_evidence,
        slots,
        ambiguous_stems,
        warnings,
    }
}

/// Adds one source-local slot unless two distinct *elements* share its stem.
///
/// Sharing a stem is not by itself ambiguity: one element re-rendered under a
/// fresh framework token is exactly what normalization exists to absorb, and it
/// produces two raw ids that collapse onto one stem. Ambiguity is two elements,
/// which [`ephemeral_marker_residue`] separates from two renders of one.
///
/// The first distinct residue removes the tentatively accepted slot and returns
/// its stem for one diagnostic. Repeats and later collision members stay
/// suppressed and return `None`.
fn push_slot_refusing_collisions(
    slots: &mut Vec<DiscoveredSlot>,
    seen_residues: &mut BTreeMap<String, BTreeSet<String>>,
    slot: DiscoveredSlot,
    raw_div: &str,
) -> Option<String> {
    let normalized = slot.div_id.clone();
    let residue = ephemeral_marker_residue(raw_div);
    match seen_residues.get_mut(&normalized) {
        None => {
            seen_residues.insert(normalized, BTreeSet::from([residue]));
            slots.push(slot);
            None
        }
        Some(residues) if residues.contains(&residue) => None,
        Some(residues) => {
            let became_ambiguous = residues.len() == 1;
            residues.insert(residue);
            if became_ambiguous {
                slots.retain(|entry| entry.div_id != normalized);
                Some(normalized)
            } else {
                None
            }
        }
    }
}

/// Operator-facing text for a stem several live elements normalized onto.
fn ambiguous_collision_warning(prefix: &str) -> String {
    format!(
        "skipped ambiguous div-id prefix `{prefix}`: multiple active elements normalized to it, \
         but the runtime can resolve a prefix to only one active element and exact div ids change \
         across renders; expose distinct stable div ids in publisher markup before configuring \
         these placements"
    )
}

/// The stable prefix of a div id whose per-render token precedes more of the id.
///
/// Some ad stacks build ids as `<family>_<per-render token>_<placement>` — a
/// millisecond timestamp plus a random suffix sitting *before* the part that
/// distinguishes one placement from the next. Such an id can be written neither
/// literally (the token changes on the next render) nor as a prefix: the only
/// stable prefix stops at the token, and that prefix reaches every placement in
/// the family, while the runtime resolves a prefix to a single element. So the
/// slot is refused from a single observation, without waiting for a second
/// placement to prove the collision.
///
/// The shape decides, not the vendor: any segment that is a long digit run
/// followed by more alphanumerics counts, so a new stack with the same layout
/// needs no code change. A token in *trailing* position is deliberately not this
/// case — everything before it still identifies the element — and is left to
/// normalization and the same-page collision check.
fn volatile_prefix_before_placement(div_id: &str) -> Option<String> {
    let div_id = div_id.strip_suffix("-container").unwrap_or(div_id);
    let mut start = 0_usize;
    for (index, character) in div_id.char_indices() {
        if character != '_' && character != '-' {
            continue;
        }
        if is_per_render_token(&div_id[start..index]) {
            let prefix = div_id[..start].trim_end_matches(['_', '-']);
            // A delimiter is one byte, so the remainder starts just past it.
            return (!prefix.is_empty() && !div_id[index + 1..].is_empty())
                .then(|| prefix.to_string());
        }
        start = index + character.len_utf8();
    }
    None
}

/// Whether one div-id segment is a per-render token: a long leading digit run (a
/// millisecond timestamp) followed by more alphanumerics (a random suffix).
///
/// Both halves are required. A bare digit run is how publishers write stable
/// placement indices, and a token with a non-alphanumeric character is some
/// other structure than a generated id.
fn is_per_render_token(segment: &str) -> bool {
    let leading_digits = segment.bytes().take_while(u8::is_ascii_digit).count();
    leading_digits >= 8
        && segment.len() > leading_digits
        && segment.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Operator-facing text for a div-id family carrying a per-render token.
fn volatile_prefix_warning(prefix: &str) -> String {
    format!(
        "skipped volatile div-id family `{prefix}`: a per-render token sits before the placement \
         suffix, so exact div ids change across renders and no distinct stable element prefix is \
         available; expose distinct stable div ids in publisher markup before configuring these \
         placements"
    )
}

/// Records `warning` unless the same text was already recorded for this page.
fn push_unique_warning(warnings: &mut Vec<String>, warning: String) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
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
    if !is_usable_unit_path(&entry.gam_unit_path) {
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
    // Normalization truncates at the first ephemeral marker, so a div id that is
    // *entirely* ephemeral (`_R_9sl…`, or exactly `-container`) reduces to the
    // empty string. An empty `div_id` override fails config load outright, and
    // an empty prefix would bind the slot to the first id-bearing element on the
    // page, so such a slot is unusable rather than merely imprecise.
    if div_stem.is_empty() {
        return None;
    }
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

/// Whether a scraped GAM ad-unit path can be represented in config.
///
/// `gam_unit_path` is a template: `{` and `}` delimit placeholders and
/// [`parse_unit_template`](trusted_server_core::creative_opportunities) offers no
/// escape syntax. A live path containing a brace would either fail config load
/// or, worse, be silently reinterpreted as a placeholder-bearing template. A
/// blank path is rejected for the same reason config load rejects it.
fn is_usable_unit_path(path: &str) -> bool {
    !path.trim().is_empty() && !path.contains(['{', '}'])
}

/// Strips ephemeral GPT div-id noise so the stored id is stable across renders.
///
/// Removes a trailing `-container` wrapper, then truncates at the first ephemeral
/// marker — a React SSR hash (`_R_<hash>`) or a hex-UUID segment — since both
/// change on every page load. Truncating (rather than excising) keeps the result
/// a valid **prefix** of the live div id, which is how verify matches slots.
///
/// `div-gpt-ad-leaderboard-1` (stable) is unchanged; `ad-header-0-_R_9sl…-container`
/// and `ad-header-0-_r_8_` → `ad-header-0`; `ad-in_content-de66…f272-in_content-0`
/// → `ad-in_content`.
fn normalize_div_stem(div_id: &str) -> String {
    let stem = div_id.strip_suffix("-container").unwrap_or(div_id);
    let cut = ephemeral_marker_ranges(stem)
        .first()
        .map_or(stem.len(), |range| range.start);
    stem[..cut].trim_end_matches('-').to_string()
}

/// Byte ranges of every ephemeral per-render marker in `stem`, in order and
/// without overlaps.
///
/// A hex-hash candidate must contain at least one `a`-`f`; a run of 16+ digits
/// is how publishers write stable ids, not a hash.
fn ephemeral_marker_ranges(stem: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = REACT_USE_ID
        .find_iter(stem)
        .chain(UUID_SEGMENT.find_iter(stem))
        .chain(HEX_HASH_SEGMENT.find_iter(stem).filter(|matched| {
            matched
                .as_str()
                .bytes()
                .any(|byte| matches!(byte, b'a'..=b'f'))
        }))
        .map(|matched| matched.range())
        .collect();
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The parts of a raw div id that no ephemeral marker covered, NUL-joined.
///
/// [`normalize_div_stem`] truncates at the first marker, so two ids differing
/// only *inside* a marker collapse onto one stem — the signature of one element
/// re-rendered. What the markers did not cover separates that from two elements:
/// `ad-header-0-_R_3f_` and `ad-header-0-_r_0_` leave the same residue (one
/// element, two renders), while `…-in_content-0` and `…-in_content-1` do not
/// (two siblings). A live div id cannot contain NUL, so joining on it cannot
/// make two different residues compare equal.
fn ephemeral_marker_residue(div_id: &str) -> String {
    let stem = div_id.strip_suffix("-container").unwrap_or(div_id);
    let mut residue = String::with_capacity(stem.len());
    let mut previous = 0_usize;
    for range in ephemeral_marker_ranges(stem) {
        residue.push_str(&stem[previous..range.start]);
        residue.push('\0');
        previous = range.end;
    }
    residue.push_str(&stem[previous..]);
    residue
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
fn parse_gampad_request(raw_url: &str) -> Option<(String, DiscoveredSlot, String)> {
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
    if !is_usable_unit_path(&gam_unit_path) {
        return None;
    }
    // A usable unit path needs the network id plus at least one path segment.
    parts.next()?;

    let raw_div = dids?;
    if raw_div.contains(',') {
        return None;
    }
    let raw_div = raw_div.trim().to_string();
    if raw_div.is_empty() {
        return None;
    }
    if is_multi_slot_div(&raw_div) {
        return None;
    }
    let div_id = normalize_div_stem(&raw_div);
    // See `slot_from_registry`: a fully ephemeral div id normalizes to nothing,
    // which is neither a valid config value nor a usable runtime prefix.
    if div_id.is_empty() {
        return None;
    }

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
        raw_div,
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
    url::form_urlencoded::parse(scp.as_bytes()).any(|(key, value)| {
        let key = key.to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        (key == "test" && value == "prebid")
            || (key == "tude" && value == "true")
            || key.starts_with("prebid")
    })
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
    fn prebid_detection_requires_a_targeting_key_not_a_substring() {
        assert!(scp_shows_prebid("test=prebid"));
        assert!(!scp_shows_prebid("noprebid=true"));
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
    fn lowercase_react_use_id_suffixes_collapse_to_one_slot() {
        // React emits `_r_0_` client-side and `_R_3f_` server-side, and the
        // token changes per render. Leaving it in the stem fragments one slot
        // into a new key on every page, which starves template inference.
        for volatile in [
            "ad-header-0-_r_0_",
            "ad-header-0-_r_8_",
            "ad-header-0-_r_a_",
            "ad-header-0-_R_3f_",
        ] {
            let registry = vec![registry_slot("/123/site/news", volatile, &[(728, 90)])];
            let discovered = discover_gpt_slots(&registry, &[], false);
            assert_eq!(
                discovered.slots[0].div_id, "ad-header-0",
                "`{volatile}` should normalize to a stable stem"
            );
        }
    }

    #[test]
    fn an_ordinary_id_containing_r_is_left_alone() {
        // The React shape is anchored, so a legitimate id keeps its full stem.
        let registry = vec![registry_slot("/123/site/news", "ad_r_rail", &[(300, 250)])];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(discovered.slots[0].div_id, "ad_r_rail");
    }

    #[test]
    fn registry_slot_with_brace_in_unit_path_is_skipped() {
        // `gam_unit_path` is a template and there is no escape syntax, so a
        // literal brace either fails config load or is silently reinterpreted as
        // a placeholder. Neither is acceptable to persist.
        let registry = vec![
            registry_slot("/123/home/{section}", "div-gpt-ad-a", &[(300, 250)]),
            registry_slot("/123/home/ok", "div-gpt-ad-b", &[(300, 250)]),
        ];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(
            discovered.slots.len(),
            1,
            "the brace-bearing slot should be dropped, the clean one kept"
        );
        assert_eq!(discovered.slots[0].gam_unit_path, "/123/home/ok");
    }

    #[test]
    fn registry_slot_whose_div_id_is_entirely_ephemeral_is_skipped() {
        // `_R_…` is a React SSR marker; normalizing truncates at it, leaving an
        // empty stem. An empty div_id fails config load, and as a runtime prefix
        // it would match the first id-bearing element on the page.
        let registry = vec![registry_slot(
            "/123/home/header",
            "_R_9slkta7pd6",
            &[(728, 90)],
        )];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert!(
            discovered.slots.is_empty(),
            "a slot with no stable div stem should be dropped, got {:?}",
            discovered.slots
        );
    }

    #[test]
    fn volatile_guid_div_id_still_normalizes_to_a_usable_prefix() {
        // A GUID between two copies of the placement name must still yield a
        // usable stable stem; only an entirely ephemeral id is dropped.
        let registry = vec![registry_slot(
            "/123456789/publisher/homepage",
            "ad-in_content-0949b6c5726343bf8bbec2ac47b494b4-in_content-0",
            &[(300, 250)],
        )];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(discovered.slots.len(), 1);
        assert_eq!(
            discovered.slots[0].div_id, "ad-in_content",
            "the GUID and trailing index should be truncated to a stable prefix"
        );
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
    fn long_numeric_segments_are_stable_ids_not_hex_hashes() {
        assert_eq!(
            normalize_div_stem("ad-slot-1234567890123456-tail"),
            "ad-slot-1234567890123456-tail"
        );
    }

    #[test]
    fn comma_separated_sra_dids_are_ignored() {
        let discovered = from_requests(&[request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123%2Cnews%2Catf&dids=ad-a%2Cad-b&prev_iu_szs=300x250",
        )]);

        assert!(
            discovered.slots.is_empty(),
            "a comma-joined SRA did list is not one element"
        );
    }

    #[test]
    fn one_element_under_two_render_tokens_is_not_a_collision() {
        // Both ids describe in-content placement 0; only the hash between the
        // two copies of the placement name differs, which is what one element
        // re-rendered looks like. Refusing here would refuse the very shape
        // normalization exists to absorb.
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
            "two renders of one element are one slot, got {:?}",
            discovered.slots
        );
        assert_eq!(discovered.slots[0].div_id, "ad-in_content");
        assert!(
            discovered.warnings.is_empty(),
            "a re-render is not an ambiguity to report, got {:?}",
            discovered.warnings
        );
        assert!(discovered.ambiguous_stems.is_empty());
    }

    #[test]
    fn sibling_placements_sharing_one_stem_are_refused() {
        // Same shape as above, but the trailing placement index differs: these
        // are two live elements, and one prefix cannot resolve to both.
        let registry = vec![
            registry_slot(
                "/987654321/site/homepage",
                "ad-in_content-de669245b2ea4b05826dc96f07a36272-in_content-0",
                &[(300, 250)],
            ),
            registry_slot(
                "/987654321/site/homepage",
                "ad-in_content-8aec8129a83d4e5abc197423120cb19e-in_content-1",
                &[(300, 250)],
            ),
        ];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert!(
            discovered.had_slot_evidence,
            "a refused placement is still evidence of an ad stack"
        );
        assert!(
            discovered.slots.is_empty(),
            "neither a broad prefix nor per-render exact IDs are safe"
        );
        assert_ambiguous_collision_warning(&discovered, "ad-in_content");
        assert!(
            discovered.ambiguous_stems.contains("ad-in_content"),
            "the verdict must travel with the evidence, got {:?}",
            discovered.ambiguous_stems
        );
    }

    #[test]
    fn react_server_and_client_render_tokens_are_one_slot() {
        // A hydrating publisher reports the SSR id and the client id for the
        // same element. Both must collapse rather than refuse each other.
        let registry = vec![
            registry_slot("/123456789/site/news", "ad-header-0-_R_3f_", &[(728, 90)]),
            registry_slot("/123456789/site/news", "ad-header-0-_r_0_", &[(728, 90)]),
        ];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert_eq!(
            discovered.slots.len(),
            1,
            "SSR and client renders of one element are one slot, got {:?}",
            discovered.slots
        );
        assert_eq!(discovered.slots[0].div_id, "ad-header-0");
        assert!(discovered.warnings.is_empty());
    }

    #[test]
    fn repeated_raw_div_after_a_normalization_collision_is_deduplicated() {
        let first = "ad-x-aaaaaaaaaaaaaaaa-0";
        let second = "ad-x-bbbbbbbbbbbbbbbb-1";
        let third = "ad-x-cccccccccccccccc-2";
        let registry = vec![
            registry_slot("/123456789/site/home", first, &[(300, 250)]),
            registry_slot("/123456789/site/home", second, &[(300, 250)]),
            registry_slot("/123456789/site/home", first, &[(300, 250)]),
            registry_slot("/123456789/site/home", second, &[(300, 250)]),
            registry_slot("/123456789/site/home", third, &[(300, 250)]),
        ];

        let discovered = discover_gpt_slots(&registry, &[], false);

        assert!(
            discovered.had_slot_evidence,
            "a refused placement is still evidence of an ad stack"
        );
        assert!(
            discovered.slots.is_empty(),
            "no repeat or later collision member may resurrect the group"
        );
        assert_ambiguous_collision_warning(&discovered, "ad-x");
    }

    #[test]
    fn request_normalization_collision_is_refused() {
        let discovered = from_requests(&[
            request(
                "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123456789%2Csite%2Chome&dids=ad-x-aaaaaaaaaaaaaaaa-0&prev_iu_szs=300x250",
            ),
            request(
                "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123456789%2Csite%2Chome&dids=ad-x-bbbbbbbbbbbbbbbb-1&prev_iu_szs=300x250",
            ),
        ]);

        assert!(
            discovered.had_slot_evidence,
            "a refused placement is still evidence of an ad stack"
        );
        assert!(
            discovered.slots.is_empty(),
            "a refused placement must not be written, got {:?}",
            discovered.slots
        );
        assert_eq!(
            discovered.gam_network_id.as_deref(),
            Some("123456789"),
            "refusing a slot must not discard the network id"
        );
        assert_ambiguous_collision_warning(&discovered, "ad-x");
    }

    #[test]
    fn single_volatile_family_registry_slot_is_refused() {
        let discovered = discover_gpt_slots(
            &[registry_slot(
                "/123456789/site_in-article_desktop_1",
                "vendor-tag_12345678AbCdEfGh_slot_inarticle_1",
                &[(300, 250)],
            )],
            &[],
            false,
        );

        assert!(
            discovered.had_slot_evidence,
            "a refused placement is still evidence of an ad stack"
        );
        assert!(
            discovered.slots.is_empty(),
            "one observation of a per-render family must not be written literally"
        );
        assert_eq!(discovered.gam_network_id.as_deref(), Some("123456789"));
        assert_volatile_prefix_warning(&discovered, "vendor-tag");
    }

    #[test]
    fn single_volatile_family_request_slot_is_refused() {
        let discovered = from_requests(&[request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123456789%2Csite_in-article_desktop_1&dids=vendor-tag_12345678AbCdEfGh_slot_inarticle_1&prev_iu_szs=300x250",
        )]);

        assert!(
            discovered.had_slot_evidence,
            "a refused placement is still evidence of an ad stack"
        );
        assert!(
            discovered.slots.is_empty(),
            "a refused placement must not be written, got {:?}",
            discovered.slots
        );
        assert_eq!(
            discovered.gam_network_id.as_deref(),
            Some("123456789"),
            "refusing a slot must not discard the network id"
        );
        assert_volatile_prefix_warning(&discovered, "vendor-tag");
    }

    #[test]
    fn volatile_prefix_covers_every_placement_after_the_token() {
        // The token's position is what makes the id unusable, so the placement
        // that follows it is irrelevant: every one of these leaves `vendor-tag`
        // as the only stable prefix, and that prefix reaches all of them.
        for volatile in [
            "vendor-tag_12345678AbCdEfGh_slot_inarticle_1",
            "vendor-tag_12345678AbCdEfGh_slot_overlay_1-container",
            "vendor-tag_12345678AbCdEfGh_slot_sidebar_1",
            "vendor-tag_12345678AbCdEfGh_slot_overlay_stable",
            "vendor-tag_12345678AbCdEfGh_slot_overlay_1_extra",
        ] {
            assert_eq!(
                volatile_prefix_before_placement(volatile).as_deref(),
                Some("vendor-tag"),
                "`{volatile}` should be refused as a volatile family"
            );
        }
    }

    #[test]
    fn volatile_prefix_does_not_claim_stable_div_ids() {
        for stable in [
            // No per-render token at all.
            "vendor-tag_stable_slot_inarticle_1",
            // A bare digit run is how stable placement indices are written.
            "vendor-tag_12345678_slot_inarticle_1",
            "ad-slot-1234567890123456-tail",
            // The token is trailing, so the prefix before it still identifies
            // this element and normalization/collision handling own the case.
            "vendor-tag_slot_inarticle_12345678AbCdEfGh",
            "vendor-tag-header",
        ] {
            assert_eq!(
                volatile_prefix_before_placement(stable),
                None,
                "`{stable}` should stay eligible"
            );
        }
    }

    #[test]
    fn ambiguous_registry_stem_still_suppresses_request_fallback() {
        let registry = vec![
            registry_slot(
                "/123456789/site/home",
                "ad-x-aaaaaaaaaaaaaaaa-0",
                &[(300, 250)],
            ),
            registry_slot(
                "/123456789/site/home",
                "ad-x-bbbbbbbbbbbbbbbb-1",
                &[(300, 250)],
            ),
        ];
        let requests = vec![request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123456789%2Csite%2Chome&dids=ad-x-cccccccccccccccc-2&prev_iu_szs=300x250",
        )];

        let discovered = discover_gpt_slots(&registry, &requests, false);

        assert!(
            discovered.had_slot_evidence,
            "a refused placement is still evidence of an ad stack"
        );
        assert!(
            discovered.slots.is_empty(),
            "request fallback must not resurrect an ambiguous registry stem"
        );
        assert_eq!(discovered.gam_network_id.as_deref(), Some("123456789"));
        assert_ambiguous_collision_warning(&discovered, "ad-x");
    }

    #[test]
    fn request_rerender_does_not_rewrite_a_stable_registry_slot() {
        let registry = vec![registry_slot(
            "/123456789/site/home",
            "ad-x-aaaaaaaaaaaaaaaa-0",
            &[(300, 250)],
        )];
        let requests = vec![request(
            "https://securepubads.g.doubleclick.net/gampad/ads?iu_parts=123456789%2Csite%2Chome&dids=ad-x-bbbbbbbbbbbbbbbb-1&prev_iu_szs=300x250",
        )];

        let discovered = discover_gpt_slots(&registry, &requests, false);

        assert_eq!(discovered.slots.len(), 1, "registry evidence should win");
        assert_eq!(
            discovered.slots[0].div_id, "ad-x",
            "request fallback must not destabilize a registry-derived prefix"
        );
    }

    fn assert_ambiguous_collision_warning(discovered: &DiscoveredSlots, prefix: &str) {
        assert_eq!(discovered.warnings.len(), 1);
        let warning = &discovered.warnings[0];
        assert!(warning.contains(prefix), "warning should name the prefix");
        assert!(
            warning.contains("one active element"),
            "warning should explain why the broad prefix is unsafe"
        );
        assert!(
            warning.contains("change across renders"),
            "warning should explain why raw IDs are unsafe"
        );
        assert!(
            warning.contains("distinct stable div ids"),
            "warning should tell the operator how to make the placements configurable"
        );
    }

    fn assert_volatile_prefix_warning(discovered: &DiscoveredSlots, prefix: &str) {
        assert_eq!(
            discovered.warnings.len(),
            1,
            "should report the family once, got {:?}",
            discovered.warnings
        );
        let warning = &discovered.warnings[0];
        assert!(
            warning.contains(prefix),
            "warning should name the family prefix, got {warning}"
        );
        assert!(
            warning.contains("change across renders"),
            "warning should explain why the exact ids are unsafe, got {warning}"
        );
        assert!(
            warning.contains("distinct stable div ids"),
            "warning should tell the operator how to make the placements configurable, got {warning}"
        );
    }
}
