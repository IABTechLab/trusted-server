//! Derives `page_patterns` globs from the paths a slot was actually observed on.
//!
//! A slot seen on `/news/story-abc` should serve every article in that section,
//! not just that one URL — but nothing here extrapolates beyond a *witnessed*
//! section. Each observed path contributes the section prefix it belongs to and
//! nothing else, so a crawl that never visited `/reviews` never claims it.
//!
//! Each section yields a pair, because one glob cannot cover both halves:
//! `*` crosses `/` in this glob dialect, so `/news/*` matches `/news/a/b` but
//! **not** the bare `/news` landing page. Emitting only the star form silently
//! drops the landing page from the slot.

use std::collections::BTreeSet;

/// The root pattern, matching only the site root.
const ROOT_PATTERN: &str = "/";

/// Expands observed page paths into the glob set a slot should carry.
///
/// `section_segment` is the index the section is taken from, matching the
/// config key of the same name: a path is reduced to its first
/// `section_segment + 1` segments, which is the prefix every page of that
/// section shares. A shorter observed landing path is emitted literally; only
/// the actual site root contributes `/`.
///
/// Results are deduplicated and ordered with `/` first, then alphabetically, so
/// re-running against unchanged evidence produces an unchanged file.
pub(super) fn patterns_for_paths<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    section_segment: usize,
) -> Vec<String> {
    let mut patterns: BTreeSet<String> = BTreeSet::new();
    let mut has_root = false;

    for path in paths {
        let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        if segments.len() <= section_segment {
            if segments.is_empty() {
                has_root = true;
            } else {
                patterns.insert(glob::Pattern::escape(path));
            }
            continue;
        }
        let prefix = glob::Pattern::escape(&format!("/{}", segments[..=section_segment].join("/")));
        // The landing page and everything beneath it.
        patterns.insert(prefix.clone());
        patterns.insert(format!("{prefix}/*"));
    }

    let mut out = Vec::with_capacity(patterns.len() + usize::from(has_root));
    if has_root {
        out.push(ROOT_PATTERN.to_string());
    }
    out.extend(patterns);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_article_yields_both_halves_of_the_pair() {
        // `/news/*` alone would not match the bare `/news` landing page, because
        // `*` crosses `/` but does not match the empty remainder.
        let patterns = patterns_for_paths(["/news/story-abc"], 0);

        assert_eq!(patterns, ["/news", "/news/*"]);
    }

    #[test]
    fn the_root_path_contributes_the_root_pattern_first() {
        let patterns = patterns_for_paths(["/deals/x", "/", "/news/y"], 0);

        assert_eq!(
            patterns,
            ["/", "/deals", "/deals/*", "/news", "/news/*"],
            "root first, then sections alphabetically"
        );
    }

    #[test]
    fn a_landing_page_and_its_article_collapse_to_one_pair() {
        let patterns = patterns_for_paths(["/news", "/news/story-abc"], 0);

        assert_eq!(patterns, ["/news", "/news/*"], "no duplicate entries");
    }

    #[test]
    fn a_locale_prefixed_site_keeps_the_locale_in_the_prefix() {
        // section_segment = 1 means the section is the second segment, so the
        // shared prefix every page of that section carries includes the locale.
        let patterns = patterns_for_paths(["/en/news/story", "/en/deals/x", "/en"], 1);

        assert_eq!(
            patterns,
            ["/en", "/en/deals", "/en/deals/*", "/en/news", "/en/news/*"]
        );
    }

    #[test]
    fn literal_glob_metacharacters_are_escaped_and_match_the_source() {
        let source = "/news[local]/story";
        let patterns = patterns_for_paths([source], 0);

        assert_eq!(patterns, ["/news[[]local[]]", "/news[[]local[]]/*"]);
        assert!(patterns.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .expect("should compile emitted glob")
                .matches(source)
        }));
    }

    #[test]
    fn unwitnessed_sections_are_never_invented() {
        let patterns = patterns_for_paths(["/news/story"], 0);

        assert_eq!(
            patterns,
            ["/news", "/news/*"],
            "only the crawled section may appear"
        );
    }

    #[test]
    fn output_is_stable_regardless_of_input_order() {
        let one = patterns_for_paths(["/news/a", "/deals/b", "/"], 0);
        let two = patterns_for_paths(["/", "/deals/b", "/news/a"], 0);

        assert_eq!(one, two, "re-running should not reorder the written file");
    }

    #[test]
    fn every_emitted_pattern_compiles_as_a_runtime_glob() {
        let patterns = patterns_for_paths(["/", "/news/story", "/car-research/x"], 0);

        for pattern in &patterns {
            trusted_server_core::creative_opportunities::validate_page_pattern(pattern)
                .unwrap_or_else(|error| {
                    panic!("emitted pattern `{pattern}` must compile: {error}")
                });
        }
    }

    #[test]
    fn no_paths_yield_no_patterns() {
        assert!(patterns_for_paths([], 0).is_empty());
    }
}
