//! Infers a `{network_id}`/`{section}` ad-unit template from observed evidence.
//!
//! The generator otherwise writes the literal path each page happened to
//! request, which pins a slot to the one section it was scraped from. A template
//! generalizes across sections — but a *wrong* template makes the publisher bid
//! against inventory that does not exist, which is worse than a narrow literal.
//! So this module is built to refuse rather than guess.
//!
//! The inference applies three evidence rules:
//!
//! 1. **Positional binding.** `{network_id}` is bound to unit segment 0 and only
//!    if that segment is the resolved network id. Substring replacement would
//!    corrupt `/123/sports123/home` into `/{network_id}/sports{network_id}/home`.
//! 2. **Exactly one varying segment.** Zero means nothing was proven and the
//!    path stays literal; two means the unit varies along a dimension the
//!    request path cannot supply (device, geo, experiment), so it is refused.
//! 3. **Cross-page variation.** Two pages must show *different* derived sections
//!    and different unit segments. A single-page crawl is
//!    indistinguishable from a static path — literal, `{network_id}`-only and
//!    `{section}` all reproduce one observation equally well, and round-trip
//!    verification cannot tell them apart. Only variation can.
//!
//! Every accepted template is then replayed through the runtime's own
//! [`render_gam_unit_path`](CreativeOpportunitySlot::render_gam_unit_path) and
//! [`derive_section`] against every observation. A template that does not
//! reproduce what the live page actually requested is downgraded, not written.

use std::collections::{BTreeMap, BTreeSet};

use trusted_server_core::creative_opportunities::{CreativeOpportunitySlot, derive_section};

use super::evidence::{EvidenceTable, SlotEvidence};
use super::slot_toml::toml_string;

/// Candidate `section_segment` values considered, `0..=MAX_SECTION_SEGMENT`.
///
/// A locale-prefixed site (`/en/news/story`) needs 1. Beyond 2 the "section" is
/// no longer a taxonomy the operator would recognise, and every extra candidate
/// is another chance for two indices to both fit and force a refusal.
const MAX_SECTION_SEGMENT: usize = 2;

/// The config-level section policy an inferred template depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SectionPolicy {
    /// Value substituted for `{section}` on paths with no section segment.
    pub(super) section_root: String,
    /// Index of the path segment `{section}` is taken from.
    pub(super) section_segment: usize,
}

/// What to write for one slot's `gam_unit_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SlotDecision {
    /// Write this templated path; it reproduced every observation.
    Template(String),
    /// Write this literal path; nothing generalizable was proven.
    Literal(String),
    /// Write no path at all — the observations cannot be represented.
    Refuse {
        /// Operator-facing explanations, one per reason.
        reasons: Vec<String>,
    },
}

/// The outcome of inference across the whole evidence table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InferenceOutcome {
    /// Section policy to write, present only when some slot templated.
    pub(super) policy: Option<SectionPolicy>,
    /// Per-slot decision, keyed by div stem, in evidence order.
    pub(super) decisions: Vec<(String, SlotDecision)>,
    /// Operator-facing notes about why inference went the way it did.
    pub(super) diagnostics: Vec<String>,
}

impl InferenceOutcome {
    /// The decision for a slot, by div stem.
    pub(super) fn decision(&self, div_id: &str) -> Option<&SlotDecision> {
        self.decisions
            .iter()
            .find(|(key, _)| key == div_id)
            .map(|(_, decision)| decision)
    }
}

/// Per-slot analysis under one candidate `section_segment`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SlotAnalysis {
    /// Templatable: unit segment `varying` tracks the derived section, and root
    /// pages agreed on `section_root`.
    Templatable {
        varying: usize,
        section_root: String,
    },
    /// The unit path never varied, so nothing about `{section}` was proven.
    Static,
    /// Cannot be represented; carries the operator-facing reason.
    Refuse(String),
    /// Would be templatable but no root page was observed, so `section_root`
    /// is undetermined under this candidate.
    RootUnwitnessed,
}

/// Infers unit-path templates for every slot in `table`.
///
/// `network_id` is the resolved GAM network id; `{network_id}` is only ever
/// bound to a unit segment that already equals it.
pub(super) fn infer_unit_templates(table: &EvidenceTable, network_id: &str) -> InferenceOutcome {
    let slots: Vec<&SlotEvidence> = table.slots().collect();
    let mut diagnostics = Vec::new();

    // Evaluate every candidate index independently; ambiguity between two that
    // both fit is a refusal, not a preference for the smaller one.
    let mut qualifying: Vec<(usize, String, BTreeMap<String, SlotAnalysis>)> = Vec::new();
    for segment in 0..=MAX_SECTION_SEGMENT {
        let analyses: BTreeMap<String, SlotAnalysis> = slots
            .iter()
            .map(|slot| (slot.div_id.clone(), analyse_slot(slot, network_id, segment)))
            .collect();

        let roots: BTreeSet<&str> = analyses
            .values()
            .filter_map(|analysis| match analysis {
                SlotAnalysis::Templatable { section_root, .. } => Some(section_root.as_str()),
                _ => None,
            })
            .collect();
        // Slots must agree: `section_root` is one config-level value, so two
        // slots claiming different roots means this index is not the real one.
        let Some(root) = roots.iter().next().copied() else {
            continue;
        };
        if roots.len() > 1 {
            continue;
        }
        qualifying.push((segment, root.to_string(), analyses));
    }

    let chosen = match qualifying.len() {
        0 => None,
        1 => qualifying.into_iter().next(),
        _ => {
            let indices: Vec<String> = qualifying
                .iter()
                .map(|(segment, _, _)| segment.to_string())
                .collect();
            diagnostics.push(format!(
                "more than one section_segment ({}) explains the observed ad-unit paths \
                 equally well, so no template can be chosen safely; slots without one safe literal path are omitted",
                indices.join(", ")
            ));
            None
        }
    };

    let Some((section_segment, section_root, analyses)) = chosen else {
        if diagnostics.is_empty() {
            diagnostics.push(
                "no ad-unit path varied by page section across the crawl, so paths were kept \
                 literal; crawl more sections to enable a {section} template"
                    .to_string(),
            );
        }
        return InferenceOutcome {
            policy: None,
            decisions: literal_decisions(&slots),
            diagnostics,
        };
    };

    let mut decisions = Vec::with_capacity(slots.len());
    let mut templated = 0_usize;
    for slot in &slots {
        let analysis = analyses
            .get(&slot.div_id)
            .cloned()
            .unwrap_or(SlotAnalysis::Static);
        let decision = match analysis {
            SlotAnalysis::Templatable { varying, .. } => {
                let template = build_template(slot, varying);
                match verify_round_trip(&template, slot, network_id, &section_root, section_segment)
                {
                    Ok(()) => {
                        templated += 1;
                        SlotDecision::Template(template)
                    }
                    Err(reason) => {
                        diagnostics.push(format!(
                            "slot `{}` template `{template}` did not reproduce the observed \
                             ad-unit paths ({reason}); refusing any unsafe fallback",
                            slot.id
                        ));
                        literal_decision(slot)
                    }
                }
            }
            SlotAnalysis::Static | SlotAnalysis::RootUnwitnessed => literal_decision(slot),
            SlotAnalysis::Refuse(reason) => SlotDecision::Refuse {
                reasons: vec![reason],
            },
        };
        decisions.push((slot.div_id.clone(), decision));
    }

    if templated == 0 {
        return InferenceOutcome {
            policy: None,
            decisions,
            diagnostics,
        };
    }

    diagnostics.push(format!(
        "inferred section_segment = {section_segment} and section_root = \"{section_root}\" \
         from {} page(s); {templated} slot(s) templated",
        table.pages().len()
    ));
    InferenceOutcome {
        policy: Some(SectionPolicy {
            section_root,
            section_segment,
        }),
        decisions,
        diagnostics,
    }
}

/// Checks the properties of a slot's observations that do not depend on which
/// `section_segment` is being considered.
///
/// Kept separate because these refusals are final: no candidate index can
/// rescue a slot whose observations are not one template with a single hole in
/// them, and the operator needs the specific reason rather than a generic one.
///
/// Returns the single varying unit segment, `None` when nothing varied, or the
/// reason the observations cannot be represented at all.
fn structural_check(slot: &SlotEvidence) -> Result<Option<usize>, String> {
    // One page reporting two different ad-unit paths for the same slot means the
    // unit varies along something the request path cannot express — a device or
    // geo split, or two profiles disagreeing. Nothing here can represent that.
    let mut per_path: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in &slot.rows {
        per_path
            .entry(row.path.as_str())
            .or_default()
            .insert(row.unit_path.as_str());
    }
    if let Some((path, units)) = per_path.iter().find(|(_, units)| units.len() > 1) {
        let observed: Vec<&str> = units.iter().copied().collect();
        return Err(format!(
            "page `{path}` requested more than one ad-unit path for this slot ({}); \
             the unit varies by something the request path cannot derive",
            observed.join(", ")
        ));
    }

    let split: Vec<Vec<&str>> = slot
        .rows
        .iter()
        .map(|row| segments(&row.unit_path))
        .collect();
    let Some(first) = split.first() else {
        return Ok(None);
    };
    // Differing shapes are not one template with a hole in it.
    if split.iter().any(|parts| parts.len() != first.len()) {
        return Err(
            "the observed ad-unit paths have different segment counts, so they are not \
             one template"
                .to_string(),
        );
    }

    let varying: Vec<usize> = (0..first.len())
        .filter(|index| {
            split
                .iter()
                .map(|parts| parts[*index])
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .collect();
    match varying.len() {
        0 => Ok(None),
        1 if varying[0] == 0 => {
            Err("the network-id segment of the ad-unit path varied across pages".to_string())
        }
        1 => Ok(Some(varying[0])),
        count => Err(format!(
            "{count} ad-unit segments vary across pages, so the path does not track the \
             page section alone"
        )),
    }
}

/// Analyses one slot under a candidate `section_segment`.
///
/// [`structural_check`] has already established that a templatable candidate
/// contains more than one observed unit path. Therefore a successful derived
/// section match here is itself the required variation witness; a second
/// witness predicate would only restate that invariant.
fn analyse_slot(slot: &SlotEvidence, network_id: &str, section_segment: usize) -> SlotAnalysis {
    let varying = match structural_check(slot) {
        Err(reason) => return SlotAnalysis::Refuse(reason),
        Ok(None) => return SlotAnalysis::Static,
        Ok(Some(varying)) => varying,
    };

    let split: Vec<Vec<&str>> = slot
        .rows
        .iter()
        .map(|row| segments(&row.unit_path))
        .collect();
    // `{network_id}` binds positionally and only to the resolved id. Substring
    // replacement would rewrite an unrelated segment that merely contains it.
    if split.first().and_then(|parts| parts.first()) != Some(&network_id) {
        return SlotAnalysis::Static;
    }

    // Partition observations into pages that have a section segment and pages
    // that do not; the latter are what determine `section_root`.
    let mut root_values = BTreeSet::new();
    for (row, parts) in slot.rows.iter().zip(split.iter()) {
        let observed = parts[varying];
        if path_segments(&row.path).len() > section_segment {
            // The empty root is unused here: the path has this segment.
            if derive_section(&row.path, "", section_segment) != observed {
                return SlotAnalysis::Static;
            }
        } else {
            root_values.insert(observed);
        }
    }

    let mut roots = root_values.into_iter();
    let Some(section_root) = roots.next() else {
        // Without a root observation, `section_root` would be a guess that
        // silently mis-renders every short path.
        return SlotAnalysis::RootUnwitnessed;
    };
    if roots.next().is_some() {
        return SlotAnalysis::Static;
    }
    // A root that is not `[A-Za-z0-9_-]+` makes any `{section}` template fail
    // config load; catch it here rather than at push time.
    if section_root.is_empty()
        || !section_root
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return SlotAnalysis::Static;
    }

    SlotAnalysis::Templatable {
        varying,
        section_root: section_root.to_string(),
    }
}

/// Builds the template text by substituting the two proven placeholders.
fn build_template(slot: &SlotEvidence, varying: usize) -> String {
    let first = slot
        .rows
        .iter()
        .next()
        .map(|row| row.unit_path.as_str())
        .unwrap_or_default();
    let rendered: Vec<String> = segments(first)
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            if index == 0 {
                "{network_id}".to_string()
            } else if index == varying {
                "{section}".to_string()
            } else {
                value.to_string()
            }
        })
        .collect();
    format!("/{}", rendered.join("/"))
}

/// Replays `template` through the runtime renderer against every observation.
///
/// Defense in depth rather than the primary gate: [`analyse_slot`] already
/// refuses to call a slot templatable when the derived section and the observed
/// segment disagree — a publisher whose `/site-news` pages request
/// `.../sitenews`, say — so a mismatch reaching here would mean inference and
/// the runtime renderer disagree. The template is then dropped instead of
/// written, and the diagnostic names the paths that did not reproduce.
fn verify_round_trip(
    template: &str,
    slot: &SlotEvidence,
    network_id: &str,
    section_root: &str,
    section_segment: usize,
) -> Result<(), String> {
    let probe = probe_slot(template)?;
    for row in &slot.rows {
        let section = derive_section(&row.path, section_root, section_segment);
        match probe.render_gam_unit_path(network_id, &section) {
            Some(rendered) if rendered == row.unit_path => {}
            Some(rendered) => {
                return Err(format!(
                    "on `{}` it renders `{rendered}` but the page requested `{}`",
                    row.path, row.unit_path
                ));
            }
            None => {
                return Err(format!(
                    "on `{}` it renders past the GAM ad-unit path byte limit",
                    row.path
                ));
            }
        }
    }
    Ok(())
}

/// Builds a throwaway slot carrying `template`, for rendering only.
///
/// Deserializing is how the runtime itself builds slots, so this exercises the
/// same template parsing rather than a parallel implementation.
fn probe_slot(template: &str) -> Result<CreativeOpportunitySlot, String> {
    let document = format!(
        "id = \"probe\"\ngam_unit_path = {}\npage_patterns = [\"/\"]\n\
         formats = [{{ width = 1, height = 1 }}]\n",
        toml_string(template)
    );
    toml::from_str::<CreativeOpportunitySlot>(&document)
        .map_err(|error| format!("template is not representable in config: {error}"))
}

/// The decision for a slot no template was proven for.
///
/// A structural refusal wins over the generic "several paths" message, so the
/// operator sees *why* the slot could not be represented (a device split, an
/// extra varying dimension) rather than only that it could not.
fn literal_decision(slot: &SlotEvidence) -> SlotDecision {
    if let Err(reason) = structural_check(slot) {
        return SlotDecision::Refuse {
            reasons: vec![reason],
        };
    }
    let units = slot.unit_paths();
    let mut found = units.iter();
    match (found.next(), found.next()) {
        (Some(only), None) => SlotDecision::Literal((*only).to_string()),
        (Some(_), Some(_)) => SlotDecision::Refuse {
            reasons: vec![format!(
                "the slot used several ad-unit paths ({}) and none generalized, so no \
                 single literal path is correct",
                units.into_iter().collect::<Vec<_>>().join(", ")
            )],
        },
        _ => SlotDecision::Refuse {
            reasons: vec!["no ad-unit path was observed for this slot".to_string()],
        },
    }
}

fn literal_decisions(slots: &[&SlotEvidence]) -> Vec<(String, SlotDecision)> {
    slots
        .iter()
        .map(|slot| (slot.div_id.clone(), literal_decision(slot)))
        .collect()
}

/// Non-empty path segments of an ad-unit path.
fn segments(unit_path: &str) -> Vec<&str> {
    unit_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

/// Non-empty path segments of a request path.
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|part| !part.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::audit::generate::collector::CollectedGptSlot;
    use crate::commands::audit::generate::gpt_slots::discover_gpt_slots;

    /// Folds `(path, unit_path)` observations for one div into a table.
    fn table_for(div_id: &str, observations: &[(&str, &str)]) -> EvidenceTable {
        let mut table = EvidenceTable::default();
        for (path, unit_path) in observations {
            let registry = vec![CollectedGptSlot {
                gam_unit_path: (*unit_path).to_string(),
                div_id: div_id.to_string(),
                sizes: vec![(728, 90)],
            }];
            table.fold_page(path, &discover_gpt_slots(&registry, &[], false));
        }
        table
    }

    fn only_decision(outcome: &InferenceOutcome) -> &SlotDecision {
        assert_eq!(outcome.decisions.len(), 1, "fixture should have one slot");
        &outcome.decisions[0].1
    }

    #[test]
    fn templates_a_section_varying_unit_path() {
        // The shape the operator writes by hand today.
        let table = table_for(
            "ad-header",
            &[
                ("/", "/123456789/publisher/homepage"),
                ("/news/story-abc", "/123456789/publisher/news"),
                ("/deals/thing", "/123456789/publisher/deals"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123456789");

        assert_eq!(
            outcome.policy,
            Some(SectionPolicy {
                section_root: "homepage".to_string(),
                section_segment: 0,
            })
        );
        assert_eq!(
            only_decision(&outcome),
            &SlotDecision::Template("/{network_id}/publisher/{section}".to_string())
        );
    }

    #[test]
    fn a_single_page_never_templates() {
        // Literal, {network_id}-only and {section} all reproduce one observation,
        // so only variation can distinguish them. This is the witness rule.
        let table = table_for("ad-header", &[("/news/story", "/123/site/news")]);

        let outcome = infer_unit_templates(&table, "123");

        assert_eq!(outcome.policy, None);
        assert_eq!(
            only_decision(&outcome),
            &SlotDecision::Literal("/123/site/news".to_string())
        );
    }

    #[test]
    fn a_static_unit_path_across_sections_stays_literal() {
        let table = table_for(
            "ad-header",
            &[
                ("/", "/123/site/fixed"),
                ("/news/story", "/123/site/fixed"),
                ("/deals/x", "/123/site/fixed"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        assert_eq!(outcome.policy, None, "nothing varied, so nothing is proven");
        assert_eq!(
            only_decision(&outcome),
            &SlotDecision::Literal("/123/site/fixed".to_string())
        );
    }

    #[test]
    fn a_device_split_is_refused_rather_than_guessed() {
        // Two units for the SAME path: the desktop/mobile cross-check surfaces
        // here, and the request path cannot express the difference.
        let table = table_for(
            "ad-header",
            &[
                ("/news/story", "/123/desktop/news"),
                ("/news/story", "/123/mobile/news"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        let SlotDecision::Refuse { reasons } = only_decision(&outcome) else {
            panic!(
                "a device split must refuse, got {:?}",
                only_decision(&outcome)
            );
        };
        assert!(
            reasons[0].contains("more than one ad-unit path"),
            "reason should name the conflict, got {reasons:?}"
        );
    }

    #[test]
    fn two_varying_segments_are_refused() {
        let table = table_for(
            "ad-header",
            &[
                ("/news/story", "/123/desktop/news"),
                ("/deals/x", "/123/mobile/deals"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        let SlotDecision::Refuse { reasons } = only_decision(&outcome) else {
            panic!("two varying dimensions must refuse");
        };
        assert!(
            reasons[0].contains("segments vary"),
            "reason should name the extra dimension, got {reasons:?}"
        );
    }

    #[test]
    fn a_slug_the_path_cannot_reproduce_is_refused() {
        // `/site-news` requests `.../sitenews`: the derived section and
        // the observed segment differ, so the template would render the wrong
        // unit. Candidate analysis rejects the inconsistent section mapping.
        let table = table_for(
            "ad-header",
            &[
                ("/", "/123/site/homepage"),
                ("/news/story", "/123/site/news"),
                ("/site-news/x", "/123/site/sitenews"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        assert_eq!(
            outcome.policy, None,
            "a section whose slug is not derivable must not template"
        );
        assert!(matches!(
            only_decision(&outcome),
            SlotDecision::Refuse { .. }
        ));
    }

    #[test]
    fn an_unwitnessed_root_is_refused() {
        // Every crawled page had a section, so `section_root` would be a guess
        // that silently mis-renders the homepage.
        let table = table_for(
            "ad-header",
            &[
                ("/news/story", "/123/site/news"),
                ("/deals/x", "/123/site/deals"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        assert_eq!(outcome.policy, None);
        let SlotDecision::Refuse { .. } = only_decision(&outcome) else {
            panic!("two literal paths and no template is not representable as one literal");
        };
    }

    #[test]
    fn a_locale_prefixed_site_infers_the_deeper_segment() {
        let table = table_for(
            "ad-header",
            &[
                ("/en", "/123/site/homepage"),
                ("/en/news/story", "/123/site/news"),
                ("/en/deals/x", "/123/site/deals"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        assert_eq!(
            outcome.policy,
            Some(SectionPolicy {
                section_root: "homepage".to_string(),
                section_segment: 1,
            }),
            "the locale prefix should push the section one segment deeper"
        );
    }

    #[test]
    fn network_id_is_bound_positionally_not_by_substring() {
        // `sports123` merely contains the network id; substring replacement
        // would corrupt it into `sports{network_id}`.
        let table = table_for(
            "ad-header",
            &[
                ("/", "/123/sports123/homepage"),
                ("/news/story", "/123/sports123/news"),
                ("/deals/x", "/123/sports123/deals"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        assert_eq!(
            only_decision(&outcome),
            &SlotDecision::Template("/{network_id}/sports123/{section}".to_string()),
            "only segment 0 may become {{network_id}}"
        );
    }

    #[test]
    fn a_unit_path_not_starting_with_the_network_id_stays_literal() {
        let table = table_for(
            "ad-header",
            &[
                ("/", "/999/site/homepage"),
                ("/news/story", "/999/site/news"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        assert_eq!(
            outcome.policy, None,
            "segment 0 must equal the resolved network id"
        );
    }

    #[test]
    fn differing_segment_counts_are_refused() {
        let table = table_for(
            "ad-header",
            &[
                ("/", "/123/site/homepage"),
                ("/news/story", "/123/site/news/extra"),
            ],
        );

        let outcome = infer_unit_templates(&table, "123");

        let SlotDecision::Refuse { reasons } = only_decision(&outcome) else {
            panic!("differing shapes are not one template");
        };
        assert!(
            reasons[0].contains("segment counts"),
            "reason should name the shape mismatch, got {reasons:?}"
        );
    }

    #[test]
    fn a_static_slot_stays_literal_alongside_a_templated_one() {
        let mut table = EvidenceTable::default();
        for (path, section_unit) in [
            ("/", "homepage"),
            ("/news/story", "news"),
            ("/deals/x", "deals"),
        ] {
            let registry = vec![
                CollectedGptSlot {
                    gam_unit_path: format!("/123/site/{section_unit}"),
                    div_id: "ad-header".to_string(),
                    sizes: vec![(728, 90)],
                },
                CollectedGptSlot {
                    gam_unit_path: "/123/site/sticky".to_string(),
                    div_id: "ad-sticky".to_string(),
                    sizes: vec![(300, 250)],
                },
            ];
            table.fold_page(path, &discover_gpt_slots(&registry, &[], false));
        }

        let outcome = infer_unit_templates(&table, "123");

        assert!(outcome.policy.is_some(), "the varying slot should template");
        assert_eq!(
            outcome.decision("ad-header"),
            Some(&SlotDecision::Template(
                "/{network_id}/site/{section}".to_string()
            ))
        );
        assert_eq!(
            outcome.decision("ad-sticky"),
            Some(&SlotDecision::Literal("/123/site/sticky".to_string())),
            "a genuinely static slot must not be dragged into the template"
        );
    }

    #[test]
    fn diagnostics_explain_why_nothing_templated() {
        let table = table_for("ad-header", &[("/news/story", "/123/site/news")]);

        let outcome = infer_unit_templates(&table, "123");

        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|note| note.contains("crawl more sections")),
            "the operator should learn why, got {:?}",
            outcome.diagnostics
        );
    }
}
