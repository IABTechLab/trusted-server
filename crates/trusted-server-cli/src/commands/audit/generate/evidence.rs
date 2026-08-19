//! Cross-page slot evidence: what each slot looked like on every page it was
//! observed on.
//!
//! A single page cannot distinguish a literal ad-unit path from a templated one,
//! so inference needs the *set* of observations per slot rather than one
//! snapshot. This module accumulates that set and is deliberately the only place
//! that reconciles a slot seen more than once:
//!
//! - **Formats union.** A size that appears only on article pages (a 300x600
//!   rail, say) must survive alongside the homepage's sizes. Taking the first
//!   page's formats would silently narrow the slot.
//! - **Unit paths are kept, not collapsed.** Divergence across pages is the
//!   signal inference reads; discarding it is what makes templating impossible.
//! - **Network ids must agree.** Two different GAM networks in one crawl means
//!   the pages are not one property, and writing either one would be a guess.
//!
//! Slots are keyed on the *normalized div stem* produced by
//! [`discover_gpt_slots`](super::gpt_slots::discover_gpt_slots), because raw GPT
//! div ids carry per-render framework hashes and would otherwise look like a new
//! slot on every page.

use std::collections::{BTreeMap, BTreeSet};

use super::gpt_slots::DiscoveredSlots;
use crate::error::{CliResult, cli_error};

/// One observation of a slot on one page.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct EvidenceRow {
    /// The page path the slot was observed on, normalized (leading `/`, no
    /// query or fragment).
    pub(super) path: String,
    /// The literal GAM ad-unit path the live page used for this slot.
    pub(super) unit_path: String,
}

/// Everything observed about one slot across the crawl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SlotEvidence {
    /// Config slot id derived from the div stem.
    pub(super) id: String,
    /// Normalized div stem, used as the runtime `div_id` prefix.
    pub(super) div_id: String,
    /// Union of every pixel size observed for this slot, smallest first.
    pub(super) formats: BTreeSet<(u32, u32)>,
    /// Whether any page carrying this slot showed header-bidding signals.
    pub(super) has_prebid: bool,
    /// Distinct `(path, unit_path)` observations, in a stable order.
    pub(super) rows: BTreeSet<EvidenceRow>,
}

impl SlotEvidence {
    /// The distinct literal unit paths observed for this slot.
    pub(super) fn unit_paths(&self) -> BTreeSet<&str> {
        self.rows.iter().map(|row| row.unit_path.as_str()).collect()
    }

    /// The distinct page paths this slot was observed on.
    pub(super) fn paths(&self) -> BTreeSet<&str> {
        self.rows.iter().map(|row| row.path.as_str()).collect()
    }
}

/// Slots grouped by the shape that would make them one placement: an identical
/// ad-unit path and an identical format set.
type SlotsByShape<'a> = BTreeMap<(String, Vec<(u32, u32)>), Vec<&'a SlotEvidence>>;

/// Several observed slots that are really one placement under volatile div ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FragmentGroup {
    /// The volatile div ids observed, in evidence order.
    pub(super) div_ids: Vec<String>,
    /// The ad-unit path every fragment shared.
    pub(super) unit_path: String,
    /// The stable prefix the ids share, when they share a useful one.
    ///
    /// Offered to the operator as a starting point only. It is deliberately not
    /// written as a `div_id`: the shared prefix reaches only as far as the
    /// *observed* tokens happen to agree, so it would keep matching this crawl's
    /// ids and stop matching the next render's.
    pub(super) suggested_prefix: Option<String>,
}

/// Whether no two slots were ever seen on the same page.
fn pages_are_disjoint(slots: &[&SlotEvidence]) -> bool {
    for (index, slot) in slots.iter().enumerate() {
        let pages = slot.paths();
        if slots[index + 1..]
            .iter()
            .any(|other| other.paths().intersection(&pages).next().is_some())
        {
            return false;
        }
    }
    true
}

/// The longest prefix the div ids share, trimmed back to a separator.
///
/// Trimming matters: the raw common prefix usually ends mid-token (the leading
/// digits of a timestamp two fragments happen to share), which is worse than
/// useless as a suggestion. Cutting at the last `-` or `_` yields the part a
/// human would recognise as the placement's name.
fn shared_div_prefix(slots: &[&SlotEvidence]) -> Option<String> {
    let mut prefix: &str = slots.first()?.div_id.as_str();
    for slot in &slots[1..] {
        let mut shared_end = 0;
        for ((byte_index, left), right) in prefix.char_indices().zip(slot.div_id.chars()) {
            if left != right {
                break;
            }
            shared_end = byte_index + left.len_utf8();
        }
        prefix = &prefix[..shared_end];
    }
    let trimmed = prefix.trim_end_matches(|ch: char| ch != '-' && ch != '_');
    let candidate = trimmed.trim_end_matches(['-', '_']);
    (!candidate.is_empty()).then(|| candidate.to_string())
}

/// Slot evidence accumulated across every collected page.
#[derive(Debug, Clone, Default)]
pub(super) struct EvidenceTable {
    slots: BTreeMap<String, SlotEvidence>,
    /// Div stems in first-seen order, so generated config keeps crawl order
    /// rather than alphabetical order.
    order: Vec<String>,
    network_ids: BTreeSet<String>,
    /// Every page path folded in, including those that yielded no slots.
    pages: BTreeSet<String>,
    /// Page paths that produced no slot evidence at all.
    empty_pages: BTreeSet<String>,
    /// Page paths that produced slot evidence on at least one selected profile.
    non_empty_pages: BTreeSet<String>,
}

impl EvidenceTable {
    /// Folds one page's discovered slots into the table.
    ///
    /// `path` is the page's normalized request path; it is what page patterns
    /// and `{section}` derivation are computed from later, so it must be the
    /// post-redirect path actually audited.
    pub(super) fn fold_page(&mut self, path: &str, discovered: &DiscoveredSlots) {
        self.pages.insert(path.to_string());
        if let Some(network_id) = &discovered.gam_network_id {
            self.network_ids.insert(network_id.clone());
        }
        if !discovered.had_slot_evidence {
            if !self.non_empty_pages.contains(path) {
                self.empty_pages.insert(path.to_string());
            }
            return;
        }
        self.non_empty_pages.insert(path.to_string());
        self.empty_pages.remove(path);

        for slot in &discovered.slots {
            let entry = self.slots.entry(slot.div_id.clone()).or_insert_with(|| {
                self.order.push(slot.div_id.clone());
                SlotEvidence {
                    id: slot.id.clone(),
                    div_id: slot.div_id.clone(),
                    formats: BTreeSet::new(),
                    has_prebid: false,
                    rows: BTreeSet::new(),
                }
            });
            // Union rather than replace: a size seen only on one page type is
            // still a size this slot serves.
            entry.formats.extend(slot.formats.iter().copied());
            entry.has_prebid |= slot.has_prebid;
            entry.rows.insert(EvidenceRow {
                path: path.to_string(),
                unit_path: slot.gam_unit_path.clone(),
            });
        }
    }

    /// Slots in first-seen order.
    pub(super) fn slots(&self) -> impl Iterator<Item = &SlotEvidence> {
        self.order
            .iter()
            .filter_map(|div_id| self.slots.get(div_id))
    }

    /// Number of distinct slots observed.
    pub(super) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Every page path folded in, whether or not it yielded slots.
    pub(super) fn pages(&self) -> &BTreeSet<String> {
        &self.pages
    }

    /// Page paths that produced no slot evidence.
    ///
    /// A high proportion of these is the signature of a bot challenge serving
    /// interstitials instead of the real site, which is worth refusing to write
    /// from rather than persisting a half-empty config.
    pub(super) fn empty_pages(&self) -> &BTreeSet<String> {
        &self.empty_pages
    }

    /// Whether any slot was observed at all.
    pub(super) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Groups of slots that are one slot wearing a different div id per page.
    ///
    /// Some ad stacks build div ids from a per-render token — a timestamp, a
    /// framework id — so the same placement arrives under a new key on every
    /// page. Written verbatim those ids never match at runtime, and the
    /// fragmentation also starves template inference, which needs to see one
    /// slot more than once.
    ///
    /// Detection is by evidence rather than by guessing at token shapes, because
    /// each stack invents its own. Candidates share an identical ad-unit path and
    /// identical formats; what separates a fragmented slot from two legitimate
    /// siblings on the same unit is **co-occurrence**. Real siblings appear
    /// together on a page; fragments of one slot never do, because each page
    /// produces exactly one of them.
    pub(super) fn fragmented_slots(&self) -> Vec<FragmentGroup> {
        let mut by_shape: SlotsByShape<'_> = BTreeMap::new();
        for slot in self.slots() {
            // Only slots pinned to exactly one unit path can be compared this
            // way; a slot whose unit varies is inference's problem, not this one.
            let units = slot.unit_paths();
            if units.len() != 1 {
                continue;
            }
            let unit = (*units.iter().next().expect("should have one unit path")).to_string();
            let formats: Vec<(u32, u32)> = slot.formats.iter().copied().collect();
            by_shape.entry((unit, formats)).or_default().push(slot);
        }

        by_shape
            .into_iter()
            .filter(|(_, slots)| slots.len() > 1)
            .filter(|(_, slots)| pages_are_disjoint(slots))
            .filter_map(|((unit_path, _), slots)| {
                let suggested_prefix = shared_div_prefix(&slots);
                (suggested_prefix.is_some() || slots.len() >= 3).then(|| FragmentGroup {
                    div_ids: slots.iter().map(|slot| slot.div_id.clone()).collect(),
                    unit_path,
                    suggested_prefix,
                })
            })
            .collect()
    }

    /// The single GAM network id observed across the crawl.
    ///
    /// # Errors
    ///
    /// Returns an error when pages disagreed. Two networks in one crawl means
    /// the pages are not one property (a syndicated subdomain, a child network,
    /// an off-origin redirect that slipped through), and picking either would be
    /// a guess that silently bids against the wrong inventory.
    pub(super) fn network_id(&self) -> CliResult<Option<String>> {
        let mut found = self.network_ids.iter();
        let Some(first) = found.next() else {
            return Ok(None);
        };
        if self.network_ids.len() > 1 {
            let all: Vec<&str> = self.network_ids.iter().map(String::as_str).collect();
            return cli_error(format!(
                "the crawled pages reported more than one GAM network id ({}); \
                 they do not appear to be one property, so no network id can be \
                 chosen safely. Audit a single property, or pass explicit URLs",
                all.join(", ")
            ));
        }
        Ok(Some(first.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::audit::generate::collector::CollectedGptSlot;
    use crate::commands::audit::generate::gpt_slots::discover_gpt_slots;

    /// One live slot as `(unit path, div id, sizes)`.
    type SlotFixture<'a> = (&'a str, &'a str, &'a [(u32, u32)]);

    fn page(slots: &[SlotFixture<'_>], has_prebid: bool) -> DiscoveredSlots {
        let registry: Vec<CollectedGptSlot> = slots
            .iter()
            .map(|(unit_path, div_id, sizes)| CollectedGptSlot {
                gam_unit_path: (*unit_path).to_string(),
                div_id: (*div_id).to_string(),
                sizes: sizes.to_vec(),
            })
            .collect();
        discover_gpt_slots(&registry, &[], has_prebid)
    }

    #[test]
    fn formats_union_across_pages_instead_of_first_seen_winning() {
        // The 300x600 rail only ever renders on article pages. Keeping the
        // homepage's format list alone would silently narrow the slot.
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/",
            &page(&[("/123/site/home", "ad-rail", &[(300, 250)])], false),
        );
        table.fold_page(
            "/news/story",
            &page(&[("/123/site/news", "ad-rail", &[(300, 600)])], false),
        );

        let slot = table.slots().next().expect("should have one slot");
        assert_eq!(
            slot.formats.iter().copied().collect::<Vec<_>>(),
            [(300, 250), (300, 600)],
            "both pages' sizes should survive"
        );
        assert_eq!(table.slot_count(), 1, "one div stem is one slot");
    }

    #[test]
    fn divergent_unit_paths_are_preserved_as_separate_rows() {
        // This divergence is the entire signal template inference reads.
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/",
            &page(&[("/123/site/home", "ad-header", &[(728, 90)])], false),
        );
        table.fold_page(
            "/news/story",
            &page(&[("/123/site/news", "ad-header", &[(728, 90)])], false),
        );

        let slot = table.slots().next().expect("should have one slot");
        assert_eq!(
            slot.unit_paths().into_iter().collect::<Vec<_>>(),
            ["/123/site/home", "/123/site/news"],
            "both observed unit paths must be retained"
        );
        assert_eq!(
            slot.paths().into_iter().collect::<Vec<_>>(),
            ["/", "/news/story"]
        );
    }

    #[test]
    fn repeated_identical_observations_collapse() {
        let mut table = EvidenceTable::default();
        let observed = page(&[("/123/site/home", "ad-header", &[(728, 90)])], false);
        table.fold_page("/", &observed);
        table.fold_page("/", &observed);

        let slot = table.slots().next().expect("should have one slot");
        assert_eq!(slot.rows.len(), 1, "the same page twice is one observation");
    }

    #[test]
    fn prebid_is_sticky_once_any_page_shows_it() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/",
            &page(&[("/123/site/home", "ad-header", &[(728, 90)])], false),
        );
        table.fold_page(
            "/news/story",
            &page(&[("/123/site/news", "ad-header", &[(728, 90)])], true),
        );

        let slot = table.slots().next().expect("should have one slot");
        assert!(
            slot.has_prebid,
            "a slot proven to run prebid on any page runs prebid"
        );
    }

    #[test]
    fn slots_keep_first_seen_order_not_alphabetical_order() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/",
            &page(
                &[
                    ("/123/site/home", "zeta-slot", &[(728, 90)]),
                    ("/123/site/home", "alpha-slot", &[(300, 250)]),
                ],
                false,
            ),
        );

        let ids: Vec<&str> = table.slots().map(|slot| slot.div_id.as_str()).collect();
        assert_eq!(
            ids,
            ["zeta-slot", "alpha-slot"],
            "generated config should follow crawl order"
        );
    }

    #[test]
    fn one_placement_under_per_render_div_ids_is_detected() {
        // A timestamped token means each page yields a new key
        // for the same placement. Same unit, same formats, never co-occurring.
        let mut table = EvidenceTable::default();
        for (path, div) in [
            ("/features/a", "ex_slot_26329268ce6Bj0uc8sL0_overlay_1"),
            ("/news/b", "ex_slot_26329269aoYmv4RQyN3n_overlay_1"),
            ("/deals/c", "ex_slot_26329270mYPDB3tz8cpB_overlay_1"),
        ] {
            table.fold_page(
                path,
                &page(&[("/99/site_Overlay", div, &[(300, 250)])], false),
            );
        }

        let groups = table.fragmented_slots();

        assert_eq!(groups.len(), 1, "the three fragments should form one group");
        assert_eq!(groups[0].div_ids.len(), 3);
        assert_eq!(groups[0].unit_path, "/99/site_Overlay");
        assert_eq!(
            groups[0].suggested_prefix.as_deref(),
            Some("ex_slot"),
            "the suggestion should be trimmed back off the volatile token"
        );
    }

    #[test]
    fn genuine_siblings_on_one_unit_are_not_treated_as_fragments() {
        // Two real in-content positions can share a unit path and formats. What
        // distinguishes them from fragments is that they appear *together* on a
        // page, so refusing to write them would lose real inventory.
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/news/story",
            &page(
                &[
                    ("/99/site/news", "ad-in_content-1", &[(300, 250)]),
                    ("/99/site/news", "ad-in_content-2", &[(300, 250)]),
                ],
                false,
            ),
        );

        assert!(
            table.fragmented_slots().is_empty(),
            "co-occurring slots are siblings, not fragments"
        );
    }

    #[test]
    fn slots_differing_in_formats_are_not_fragments() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/a",
            &page(&[("/99/site/x", "slot-aaaa", &[(300, 250)])], false),
        );
        table.fold_page(
            "/b",
            &page(&[("/99/site/x", "slot-bbbb", &[(728, 90)])], false),
        );

        assert!(
            table.fragmented_slots().is_empty(),
            "a differing format set means these are different placements"
        );
    }

    #[test]
    fn a_slot_seen_alone_is_never_a_fragment() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/a",
            &page(&[("/99/site/x", "only-slot", &[(300, 250)])], false),
        );

        assert!(table.fragmented_slots().is_empty());
    }

    #[test]
    fn fragments_with_no_shared_prefix_report_none() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/a",
            &page(&[("/99/site/x", "alpha-1111", &[(300, 250)])], false),
        );
        table.fold_page(
            "/b",
            &page(&[("/99/site/x", "beta-2222", &[(300, 250)])], false),
        );

        let groups = table.fragmented_slots();

        assert!(
            groups.is_empty(),
            "two unrelated placements are too ambiguous to classify as fragments"
        );
    }

    #[test]
    fn three_disjoint_same_shape_ids_are_fragment_evidence_without_a_prefix() {
        let mut table = EvidenceTable::default();
        for (path, div_id) in [("/a", "alpha"), ("/b", "bravo"), ("/c", "charlie")] {
            table.fold_page(path, &page(&[("/99/site/x", div_id, &[(300, 250)])], false));
        }

        let groups = table.fragmented_slots();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].suggested_prefix, None);
    }

    #[test]
    fn unicode_shared_prefix_uses_a_utf8_boundary() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/a",
            &page(&[("/99/site/x", "ünicode-ad-a", &[(300, 250)])], false),
        );
        table.fold_page(
            "/b",
            &page(&[("/99/site/x", "ünicode-ad-b", &[(300, 250)])], false),
        );

        let groups = table.fragmented_slots();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].suggested_prefix.as_deref(), Some("ünicode-ad"));
    }

    #[test]
    fn a_later_non_empty_profile_clears_the_empty_page_marker() {
        let mut table = EvidenceTable::default();
        table.fold_page("/news", &page(&[], false));
        table.fold_page(
            "/news",
            &page(&[("/99/site/news", "ad-atf", &[(300, 250)])], false),
        );

        assert!(table.empty_pages().is_empty());
    }

    #[test]
    fn a_later_empty_profile_does_not_re_mark_a_non_empty_page() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/news",
            &page(&[("/99/site/news", "ad-atf", &[(300, 250)])], false),
        );
        table.fold_page("/news", &page(&[], false));

        assert!(
            table.empty_pages().is_empty(),
            "emptiness is a page-level fact across all selected profiles"
        );
    }

    #[test]
    fn conflicting_network_ids_are_a_hard_error() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/",
            &page(&[("/111/site/home", "ad-header", &[(728, 90)])], false),
        );
        table.fold_page(
            "/news/story",
            &page(&[("/222/site/news", "ad-header", &[(728, 90)])], false),
        );

        let error = table
            .network_id()
            .expect_err("two networks in one crawl should not resolve");

        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("111") && rendered.contains("222"),
            "the error should name both observed ids, got {rendered}"
        );
    }

    #[test]
    fn agreeing_network_ids_resolve_to_one_value() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/",
            &page(&[("/123/site/home", "ad-header", &[(728, 90)])], false),
        );
        table.fold_page(
            "/news/story",
            &page(&[("/123/site/news", "ad-header", &[(728, 90)])], false),
        );

        assert_eq!(
            table.network_id().expect("agreeing ids should resolve"),
            Some("123".to_string())
        );
    }

    #[test]
    fn pages_without_slots_are_recorded_for_challenge_detection() {
        let mut table = EvidenceTable::default();
        table.fold_page(
            "/",
            &page(&[("/123/site/home", "ad-header", &[(728, 90)])], false),
        );
        table.fold_page("/blocked", &page(&[], false));

        assert_eq!(
            table
                .empty_pages()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["/blocked"],
            "a slot-less page must be visible to the caller, not silently dropped"
        );
        assert_eq!(
            table.pages().len(),
            2,
            "every folded page should be counted"
        );
    }

    #[test]
    fn collision_only_page_is_not_classified_as_empty() {
        let discovered = page(
            &[
                ("/123/site/home", "ad-x-aaaaaaaaaaaaaaaa-0", &[(300, 250)]),
                ("/123/site/home", "ad-x-bbbbbbbbbbbbbbbb-1", &[(300, 250)]),
            ],
            false,
        );
        let mut table = EvidenceTable::default();

        table.fold_page("/collision-only", &discovered);

        assert!(discovered.had_slot_evidence);
        assert!(discovered.slots.is_empty());
        assert!(
            table.empty_pages().is_empty(),
            "intentionally omitted GPT evidence must not look like a bot challenge"
        );
    }

    #[test]
    fn empty_table_resolves_no_network_id_rather_than_erroring() {
        let table = EvidenceTable::default();

        assert!(table.is_empty());
        assert_eq!(table.network_id().expect("empty is not a conflict"), None);
    }
}
