//! Pure crawl planning: turn discovered links and sitemap entries into the
//! bounded set of pages worth loading in a browser.
//!
//! The goal is deliberately *not* site coverage. Ad slots repeat per site
//! section, and the generated config needs one glob pair per section
//! (`/news` and `/news/*`), so one representative page per section is enough.
//! That keeps the crawl proportional to the publisher's taxonomy (a dozen
//! sections) rather than its catalog (tens of thousands of articles).
//!
//! Two sources feed the plan and each supplies a half the other cannot:
//!
//! - **Navigation links** give section *landing* paths (`/news`), which
//!   sitemaps routinely omit, and are the publisher's own taxonomy declaration.
//! - **Sitemap entries** give a real *article* per section (`/news/story-abc`),
//!   which is where in-content slots live, and reveal sections hidden behind a
//!   navigation overflow menu.

use std::collections::BTreeMap;

use url::Url;

use super::collector::CollectedLink;

/// Path segments that are never a content section worth sampling.
///
/// These carry either no ad stack at all or an unrepresentative one, and
/// crawling them spends budget that a real section needs.
const NOISE_SEGMENTS: &[&str] = &[
    "about",
    "about-us",
    "account",
    "author",
    "cart",
    "contact",
    "editorial-policy",
    "login",
    "logout",
    "newsletter",
    "page",
    "press",
    "privacy",
    "register",
    "search",
    "sitemap",
    "subscribe",
    "terms",
];

/// File extensions that are assets rather than pages.
const NON_PAGE_EXTENSIONS: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".avif", ".svg", ".ico", ".css", ".js", ".json",
    ".xml", ".pdf", ".zip", ".mp4", ".mp3", ".rss",
];

const DIRECTORY_INDEX_NAMES: &[&str] = &[
    "index.html",
    "index.htm",
    "index.php",
    "default.html",
    "default.htm",
    "default.php",
    "home.html",
    "home.htm",
    "home.php",
];

/// Bounds on how much of a site a single run will load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrawlBudget {
    /// Maximum number of sections to sample.
    pub(crate) max_sections: usize,
    /// Maximum number of pages to load in total, including the root.
    pub(crate) max_pages: usize,
}

impl Default for CrawlBudget {
    fn default() -> Self {
        Self {
            max_sections: 8,
            max_pages: 17,
        }
    }
}

/// One section selected for sampling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedSection {
    /// The path segment at [`CrawlPlan::section_segment`] identifying the section.
    pub(super) segment: String,
    /// The section landing page, when one was observed.
    pub(super) landing: Option<Url>,
    /// A representative content page inside the section, when one was observed.
    pub(super) article: Option<Url>,
}

impl PlannedSection {
    /// The pages to load for this section, landing first.
    fn targets(&self) -> impl Iterator<Item = &Url> {
        self.landing.iter().chain(self.article.iter())
    }
}

/// The bounded outcome of planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CrawlPlan {
    /// Sections selected for sampling, highest confidence first.
    pub(super) sections: Vec<PlannedSection>,
    /// Sections found but dropped because the budget was already spent.
    pub(super) dropped_sections: Vec<String>,
    /// Human-readable notes about how the plan was reached.
    pub(super) notes: Vec<String>,
    /// Path segment used to distinguish sections in this crawl.
    pub(super) section_segment: usize,
}

impl CrawlPlan {
    /// Page URLs to load, in crawl order. The root is *not* included — the
    /// caller has already collected it in order to plan at all.
    pub(super) fn targets(&self) -> Vec<Url> {
        self.sections
            .iter()
            .flat_map(PlannedSection::targets)
            .cloned()
            .collect()
    }
}

/// Evidence gathered about one candidate section before ranking.
#[derive(Debug, Default)]
struct SectionCandidate {
    landing: Option<Url>,
    article: Option<Url>,
    in_nav: bool,
    in_sitemap: bool,
    link_count: usize,
}

impl SectionCandidate {
    /// Confidence ordering: corroborated by both sources beats either alone,
    /// and navigation beats a sitemap-only hit because navigation is the
    /// publisher's own statement of what its sections are.
    fn rank(&self) -> u8 {
        match (self.in_nav, self.in_sitemap) {
            (true, true) => 3,
            (true, false) => 2,
            (false, true) => 1,
            (false, false) => 0,
        }
    }
}

/// Plans the crawl from the root page's links and any sitemap entries.
///
/// `root` bounds the crawl: every candidate must share its origin, which also
/// stops a hostile or misconfigured `robots.txt` from redirecting the crawl (and
/// the operator's cookies) at an unrelated host.
pub(super) fn plan_crawl(
    root: &Url,
    links: &[CollectedLink],
    sitemap_locs: &[String],
    budget: CrawlBudget,
) -> CrawlPlan {
    let section_segment = usize::from(root_is_locale_prefix(root));
    let mut candidates: BTreeMap<String, SectionCandidate> = BTreeMap::new();
    let mut notes = Vec::new();

    for link in links {
        let Some(url) = same_origin_page_url(root, &link.url, section_segment) else {
            continue;
        };
        let Some(segment) = section_at(&url, section_segment) else {
            continue;
        };
        let entry = candidates.entry(segment).or_default();
        entry.in_nav |= link.in_nav;
        entry.link_count += 1;
        record_url(entry, &url, section_segment);
    }

    let mut sitemap_pages = 0_usize;
    for loc in sitemap_locs {
        let Some(url) = same_origin_page_url(root, loc, section_segment) else {
            continue;
        };
        let Some(segment) = section_at(&url, section_segment) else {
            continue;
        };
        sitemap_pages += 1;
        let entry = candidates.entry(segment).or_default();
        entry.in_sitemap = true;
        record_url(entry, &url, section_segment);
    }

    if !sitemap_locs.is_empty() {
        notes.push(format!(
            "sitemap contributed {sitemap_pages} same-origin page(s) across {} section(s)",
            candidates.values().filter(|c| c.in_sitemap).count()
        ));
    }
    if links.iter().all(|link| !link.in_nav) && !links.is_empty() {
        notes.push(
            "no navigation links were found; sections were inferred from body links only"
                .to_string(),
        );
    }

    // Rank before truncating: confidence first, then how heavily the section is
    // linked, then the segment name so runs are reproducible.
    let mut ranked: Vec<(String, SectionCandidate)> = candidates.into_iter().collect();
    ranked.sort_by(|(left_segment, left), (right_segment, right)| {
        right
            .rank()
            .cmp(&left.rank())
            .then(right.link_count.cmp(&left.link_count))
            .then(left_segment.cmp(right_segment))
    });

    let mut sections = Vec::new();
    let mut dropped_sections = Vec::new();
    // The root page is already collected and counts against the page budget.
    let mut pages_used = 1_usize;
    for (segment, candidate) in ranked {
        let planned = PlannedSection {
            segment: segment.clone(),
            landing: candidate.landing,
            article: candidate.article,
        };
        let cost = planned.targets().count();
        if cost == 0 {
            continue;
        }
        if sections.len() >= budget.max_sections || pages_used + cost > budget.max_pages {
            dropped_sections.push(segment);
            continue;
        }
        pages_used += cost;
        sections.push(planned);
    }

    if !dropped_sections.is_empty() {
        let shown = dropped_sections
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let remainder = dropped_sections.len().saturating_sub(10);
        let suffix = if remainder == 0 {
            String::new()
        } else {
            format!(", and {remainder} more")
        };
        notes.push(format!(
            "budget reached: {} section(s) not sampled ({shown}{suffix}); raise --max-sections/--max-pages to include them",
            dropped_sections.len(),
        ));
    }

    CrawlPlan {
        sections,
        dropped_sections,
        notes,
        section_segment,
    }
}

/// Files a URL as the section's landing page or its representative article.
///
/// The first candidate of each kind wins, so a run is stable given stable input.
fn record_url(entry: &mut SectionCandidate, url: &Url, section_segment: usize) {
    if segment_count(url) == section_segment + 1 {
        if entry.landing.is_none() {
            entry.landing = Some(url.clone());
        }
    } else if entry.article.is_none() {
        entry.article = Some(url.clone());
    }
}

/// Parses `raw` against `root` and keeps it only if it is a same-origin page.
///
/// Rejects other origins, non-HTTP schemes, asset extensions, and paginated or
/// utility paths. Query and fragment are dropped so `/news?page=2` and
/// `/news#top` collapse onto `/news`.
fn same_origin_page_url(root: &Url, raw: &str, section_segment: usize) -> Option<Url> {
    let mut url = root.join(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.origin() != root.origin() {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);

    let path = percent_decode_for_filtering(url.path()).to_ascii_lowercase();
    if NON_PAGE_EXTENSIONS
        .iter()
        .any(|extension| path.ends_with(extension))
    {
        return None;
    }
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    if DIRECTORY_INDEX_NAMES.contains(&segments.last().copied().unwrap_or_default()) {
        return None;
    }
    if NOISE_SEGMENTS.contains(&segments.get(section_segment).copied().unwrap_or_default()) {
        return None;
    }
    if section_segment > 0 {
        let root_path = percent_decode_for_filtering(root.path()).to_ascii_lowercase();
        let root_segments: Vec<&str> = root_path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        if !segments.starts_with(&root_segments) {
            return None;
        }
    }
    // `/news/page/2` is the same inventory as `/news`, so it is not a second
    // sample worth spending a page load on.
    if segments.contains(&"page") {
        return None;
    }
    Some(url)
}

/// The non-empty path segment at `index`, percent-decoded and lowercased.
fn section_at(url: &Url, index: usize) -> Option<String> {
    percent_decode_for_filtering(url.path())
        .split('/')
        .filter(|part| !part.is_empty())
        .nth(index)
        .map(str::to_ascii_lowercase)
}

fn root_is_locale_prefix(root: &Url) -> bool {
    let segments: Vec<&str> = root
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    matches!(segments.as_slice(), [locale] if is_locale_segment(locale))
}

fn is_locale_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    matches!(bytes, [a, b] if a.is_ascii_alphabetic() && b.is_ascii_alphabetic())
        || matches!(bytes, [a, b, b'-', c, d]
            if a.is_ascii_alphabetic()
                && b.is_ascii_alphabetic()
                && c.is_ascii_alphabetic()
                && d.is_ascii_alphabetic())
}

/// Decodes percent escapes solely for normalized path classification.
fn percent_decode_for_filtering(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Converts one ASCII hexadecimal digit to its numeric value.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Count of non-empty path segments.
fn segment_count(url: &Url) -> usize {
    url.path()
        .split('/')
        .filter(|part| !part.is_empty())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Url {
        Url::parse("https://publisher.example/").expect("valid root")
    }

    fn nav(path: &str) -> CollectedLink {
        CollectedLink {
            url: format!("https://publisher.example{path}"),
            in_nav: true,
        }
    }

    fn body(path: &str) -> CollectedLink {
        CollectedLink {
            url: format!("https://publisher.example{path}"),
            in_nav: false,
        }
    }

    fn segments(plan: &CrawlPlan) -> Vec<&str> {
        plan.sections
            .iter()
            .map(|section| section.segment.as_str())
            .collect()
    }

    #[test]
    fn pairs_a_landing_page_with_an_article_from_the_sitemap() {
        let plan = plan_crawl(
            &root(),
            &[nav("/news")],
            &["https://publisher.example/news/story-abc".to_string()],
            CrawlBudget::default(),
        );

        assert_eq!(segments(&plan), ["news"]);
        let section = &plan.sections[0];
        assert_eq!(
            section.landing.as_ref().map(Url::as_str),
            Some("https://publisher.example/news")
        );
        assert_eq!(
            section.article.as_ref().map(Url::as_str),
            Some("https://publisher.example/news/story-abc")
        );
        assert_eq!(plan.targets().len(), 2, "should load landing then article");
    }

    #[test]
    fn cross_origin_candidates_are_dropped() {
        // Guards both the sitemap (a `Sitemap:` directive can point anywhere)
        // and links: the crawl carries operator cookies, so it must not leave
        // the requested origin.
        let plan = plan_crawl(
            &root(),
            &[CollectedLink {
                url: "https://tracker.example/news".to_string(),
                in_nav: true,
            }],
            &["https://other.example/deals/x".to_string()],
            CrawlBudget::default(),
        );

        assert!(
            plan.sections.is_empty(),
            "no off-origin section should survive, got {:?}",
            segments(&plan)
        );
    }

    #[test]
    fn utility_paths_and_assets_are_filtered() {
        let plan = plan_crawl(
            &root(),
            &[
                nav("/about-us"),
                nav("/search"),
                nav("/editorial-policy"),
                nav("/logo.png"),
                nav("/feed.xml"),
                nav("/news/page/2"),
                nav("/news"),
            ],
            &[],
            CrawlBudget::default(),
        );

        assert_eq!(
            segments(&plan),
            ["news"],
            "only the real content section should remain"
        );
    }

    #[test]
    fn query_and_fragment_collapse_onto_one_landing_page() {
        let plan = plan_crawl(
            &root(),
            &[nav("/news?utm_source=x"), nav("/news#top"), nav("/news")],
            &[],
            CrawlBudget::default(),
        );

        assert_eq!(segments(&plan), ["news"]);
        assert_eq!(
            plan.sections[0].landing.as_ref().map(Url::as_str),
            Some("https://publisher.example/news"),
            "tracking query and fragment should be stripped"
        );
    }

    #[test]
    fn nav_and_sitemap_corroboration_outranks_either_alone() {
        let plan = plan_crawl(
            &root(),
            &[nav("/features"), body("/reviews")],
            &[
                "https://publisher.example/features/story".to_string(),
                "https://publisher.example/deals/x".to_string(),
            ],
            CrawlBudget::default(),
        );

        assert_eq!(
            segments(&plan)[0],
            "features",
            "nav + sitemap should rank first, got {:?}",
            segments(&plan)
        );
    }

    #[test]
    fn budget_truncates_and_reports_what_was_dropped() {
        let links: Vec<CollectedLink> = ["a", "b", "c", "d"]
            .iter()
            .map(|segment| nav(&format!("/{segment}")))
            .collect();

        let plan = plan_crawl(
            &root(),
            &links,
            &[],
            CrawlBudget {
                max_sections: 2,
                max_pages: 17,
            },
        );

        assert_eq!(plan.sections.len(), 2, "section cap should be honoured");
        assert_eq!(plan.dropped_sections.len(), 2);
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("budget reached")),
            "dropping sections must be reported, not silent: {:?}",
            plan.notes
        );
    }

    #[test]
    fn page_budget_counts_the_already_collected_root() {
        // max_pages = 3 leaves room for exactly one landing+article pair on top
        // of the root page the caller already loaded.
        let plan = plan_crawl(
            &root(),
            &[nav("/news"), nav("/deals")],
            &[
                "https://publisher.example/news/a".to_string(),
                "https://publisher.example/deals/b".to_string(),
            ],
            CrawlBudget {
                max_sections: 8,
                max_pages: 3,
            },
        );

        assert_eq!(
            plan.targets().len(),
            2,
            "root + 2 pages fills max_pages = 3"
        );
        assert_eq!(plan.dropped_sections.len(), 1);
    }

    #[test]
    fn body_only_links_still_yield_sections_with_a_note() {
        let plan = plan_crawl(
            &root(),
            &[body("/news"), body("/deals")],
            &[],
            CrawlBudget::default(),
        );

        assert_eq!(segments(&plan), ["deals", "news"]);
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("no navigation links")),
            "a nav-less page should say so: {:?}",
            plan.notes
        );
    }

    #[test]
    fn empty_input_plans_nothing_rather_than_panicking() {
        let plan = plan_crawl(&root(), &[], &[], CrawlBudget::default());

        assert!(plan.sections.is_empty());
        assert!(plan.targets().is_empty());
    }

    #[test]
    fn locale_root_plans_sections_from_the_second_segment() {
        let locale_root = Url::parse("https://publisher.example/en").expect("should parse root");
        let plan = plan_crawl(
            &locale_root,
            &[nav("/en/news"), nav("/en/deals")],
            &[
                "https://publisher.example/en/news/story".to_string(),
                "https://publisher.example/en/deals/item".to_string(),
            ],
            CrawlBudget::default(),
        );

        assert_eq!(plan.section_segment, 1);
        assert_eq!(segments(&plan), ["deals", "news"]);
        assert_eq!(plan.targets().len(), 4);
    }

    #[test]
    fn encoded_noise_and_page_extensions_are_filtered() {
        let plan = plan_crawl(
            &root(),
            &[
                nav("/%70rivacy"),
                nav("/index.html"),
                nav("/archive.htm"),
                nav("/story.php"),
                nav("/news"),
            ],
            &[],
            CrawlBudget::default(),
        );

        assert_eq!(
            segments(&plan),
            ["archive.htm", "news", "story.php"],
            "only directory-index documents should be excluded by extension"
        );
    }

    #[test]
    fn locale_root_rejects_candidates_outside_its_path_prefix() {
        let locale_root = Url::parse("https://publisher.example/en").expect("should parse root");
        let plan = plan_crawl(
            &locale_root,
            &[nav("/en/news"), nav("/fr/deals")],
            &[],
            CrawlBudget::default(),
        );

        assert_eq!(
            segments(&plan),
            ["news"],
            "a locale-root crawl must not mix another locale on the same origin"
        );
    }

    #[test]
    fn a_section_root_does_not_treat_article_slugs_as_sections() {
        let section_root = Url::parse("https://publisher.example/news").expect("should parse root");
        let plan = plan_crawl(
            &section_root,
            &[nav("/news/story-one"), nav("/news/story-two")],
            &[],
            CrawlBudget::default(),
        );

        assert_eq!(
            plan.section_segment, 0,
            "a generic one-segment root is a section, not necessarily a locale"
        );
        assert_eq!(
            segments(&plan),
            ["news"],
            "articles below a section root should remain one section"
        );
    }

    #[test]
    fn dropped_section_note_is_capped() {
        let links: Vec<_> = (0..15)
            .map(|index| nav(&format!("/section-{index:02}")))
            .collect();
        let plan = plan_crawl(
            &root(),
            &links,
            &[],
            CrawlBudget {
                max_sections: 0,
                max_pages: 1,
            },
        );
        let note = plan
            .notes
            .iter()
            .find(|note| note.contains("budget reached"))
            .expect("should report dropped sections");

        assert!(note.contains("and 5 more"));
        assert!(!note.contains("section-14"));
    }
}
