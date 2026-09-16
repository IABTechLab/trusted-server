//! Groups fetched responses by content type, evaluates each against the
//! cacheability rules, and rolls the per-header verdicts up into a per-type
//! report.

use std::collections::BTreeMap;

use serde::Serialize;
use url::Url;

use super::rules::{ContentTypeGroup, HeaderVerdict, ResponseHeaders, Verdict, evaluate};

/// One fetched origin response, paired with the URL it came from.
#[derive(Debug, Clone)]
pub(crate) struct FetchedResponse {
    /// The URL that was fetched.
    pub(crate) url: Url,
    /// The cache-relevant headers of the response.
    pub(crate) headers: ResponseHeaders,
}

/// The audit outcome for a single content-type group.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GroupReport {
    /// The content-type family this report covers.
    pub(crate) content_type: ContentTypeGroup,
    /// The URLs sampled into this group.
    pub(crate) urls_sampled: Vec<String>,
    /// Group-level (per-type) verdict: worst-of rollup over `verdicts`.
    pub(crate) verdict: Verdict,
    /// The per-header verdicts, merged worst-of across sampled responses.
    pub(crate) verdicts: Vec<HeaderVerdict>,
}

/// Per-content-type-group tallies. `pass + warn + fail == total_groups`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuditSummary {
    /// Number of evaluated content-type groups.
    pub(crate) total_groups: usize,
    /// Groups whose rollup verdict is [`Verdict::Pass`].
    pub(crate) pass: usize,
    /// Groups whose rollup verdict is [`Verdict::Warn`].
    pub(crate) warn: usize,
    /// Groups whose rollup verdict is [`Verdict::Fail`].
    pub(crate) fail: usize,
}

/// The full audit report for one origin.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuditReport {
    /// The origin that was audited.
    pub(crate) origin: String,
    /// One report per evaluated content-type group.
    pub(crate) groups: Vec<GroupReport>,
    /// Group-level tallies.
    pub(crate) summary: AuditSummary,
}

impl AuditReport {
    /// The process exit code the CLI should return: 1 if any group failed,
    /// 2 if any warned (and none failed), 0 otherwise.
    pub(crate) fn exit_code(&self) -> i32 {
        if self.summary.fail > 0 {
            1
        } else if self.summary.warn > 0 {
            2
        } else {
            0
        }
    }
}

/// Builds an [`AuditReport`] from fetched responses.
///
/// Responses classified as [`ContentTypeGroup::Other`] are ignored — that group
/// is informational and never evaluated. Within a group, per-header verdicts are
/// merged worst-of across every sampled response, so a single bad response
/// downgrades the group.
pub(crate) fn run_analysis(origin: &str, responses: &[FetchedResponse]) -> AuditReport {
    // Preserve a stable, deterministic group ordering for output.
    let mut grouped: BTreeMap<usize, GroupAccumulator> = BTreeMap::new();

    for response in responses {
        let content_type = response.headers.content_type.as_deref().unwrap_or_default();
        let group = ContentTypeGroup::classify(content_type);
        if matches!(group, ContentTypeGroup::Other) {
            continue;
        }

        let accumulator = grouped
            .entry(group_order(group))
            .or_insert_with(|| GroupAccumulator::new(group));
        accumulator.add(response.url.as_str(), evaluate(group, &response.headers));
    }

    let groups: Vec<GroupReport> = grouped
        .into_values()
        .map(GroupAccumulator::finish)
        .collect();

    let summary = summarize(&groups);

    AuditReport {
        origin: origin.to_owned(),
        groups,
        summary,
    }
}

/// A stable sort key so groups always render in the same order.
fn group_order(group: ContentTypeGroup) -> usize {
    match group {
        ContentTypeGroup::Html => 0,
        ContentTypeGroup::JavaScript => 1,
        ContentTypeGroup::Image => 2,
        ContentTypeGroup::StaticAsset => 3,
        ContentTypeGroup::RtbJson => 4,
        ContentTypeGroup::Other => 5,
    }
}

/// Accumulates sampled URLs and merges per-header verdicts for one group.
struct GroupAccumulator {
    content_type: ContentTypeGroup,
    urls_sampled: Vec<String>,
    // Header name -> merged worst-of verdict, preserving first-seen order.
    verdicts: Vec<HeaderVerdict>,
}

impl GroupAccumulator {
    fn new(content_type: ContentTypeGroup) -> Self {
        Self {
            content_type,
            urls_sampled: Vec::new(),
            verdicts: Vec::new(),
        }
    }

    fn add(&mut self, url: &str, verdicts: Vec<HeaderVerdict>) {
        if !self.urls_sampled.iter().any(|sampled| sampled == url) {
            self.urls_sampled.push(url.to_owned());
        }
        for incoming in verdicts {
            match self
                .verdicts
                .iter_mut()
                .find(|existing| existing.header == incoming.header)
            {
                // Keep the more severe verdict when the same header is seen on
                // multiple sampled responses in this group.
                Some(existing) if is_more_severe(incoming.verdict, existing.verdict) => {
                    *existing = incoming;
                }
                Some(_) => {}
                None => self.verdicts.push(incoming),
            }
        }
    }

    fn finish(self) -> GroupReport {
        let verdict = Verdict::rollup(self.verdicts.iter().map(|entry| entry.verdict));
        GroupReport {
            content_type: self.content_type,
            urls_sampled: self.urls_sampled,
            verdict,
            verdicts: self.verdicts,
        }
    }
}

/// Whether `candidate` is strictly more severe than `current`.
fn is_more_severe(candidate: Verdict, current: Verdict) -> bool {
    severity(candidate) > severity(current)
}

fn severity(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::Pass => 0,
        Verdict::Warn => 1,
        Verdict::Fail => 2,
    }
}

fn summarize(groups: &[GroupReport]) -> AuditSummary {
    let mut summary = AuditSummary {
        total_groups: groups.len(),
        pass: 0,
        warn: 0,
        fail: 0,
    };
    for group in groups {
        match group.verdict {
            Verdict::Pass => summary.pass += 1,
            Verdict::Warn => summary.warn += 1,
            Verdict::Fail => summary.fail += 1,
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(url: &str, content_type: &str, cache_control: &str) -> FetchedResponse {
        FetchedResponse {
            url: Url::parse(url).expect("should parse test url"),
            headers: ResponseHeaders {
                content_type: Some(content_type.to_owned()),
                cache_control: Some(cache_control.to_owned()),
                ..ResponseHeaders::default()
            },
        }
    }

    #[test]
    fn ignores_other_content_types() {
        let responses = vec![response(
            "https://origin.example/data.bin",
            "application/octet-stream",
            "public, max-age=60",
        )];
        let report = run_analysis("https://origin.example", &responses);
        assert_eq!(
            report.summary.total_groups, 0,
            "Other group should not be counted"
        );
    }

    #[test]
    fn counts_groups_by_rollup_verdict() {
        let responses = vec![
            response("https://origin.example/", "text/html", "no-store"),
            response(
                "https://origin.example/rtb",
                "application/json",
                "public, max-age=60",
            ),
        ];
        let report = run_analysis("https://origin.example", &responses);
        assert_eq!(report.summary.total_groups, 2, "should count HTML and RTB");
        assert_eq!(report.summary.pass, 1, "HTML no-store should pass");
        assert_eq!(report.summary.fail, 1, "cacheable RTB should fail");
        assert_eq!(report.exit_code(), 1, "any fail should exit 1");
    }

    #[test]
    fn worst_of_merge_within_group() {
        // Two HTML responses: one safe, one cacheable. The group must fail.
        let responses = vec![
            response("https://origin.example/a", "text/html", "no-store"),
            response(
                "https://origin.example/b",
                "text/html",
                "public, max-age=3600",
            ),
        ];
        let report = run_analysis("https://origin.example", &responses);
        assert_eq!(report.groups.len(), 1, "both HTML pages share one group");
        assert_eq!(
            report.groups[0].verdict,
            Verdict::Fail,
            "a single cacheable HTML response should fail the group"
        );
        assert_eq!(
            report.groups[0].urls_sampled.len(),
            2,
            "both sampled URLs should be recorded"
        );
    }

    #[test]
    fn warn_only_exits_two() {
        let responses = vec![response(
            "https://origin.example/app.js",
            "application/javascript",
            "public, max-age=600",
        )];
        let report = run_analysis("https://origin.example", &responses);
        assert_eq!(report.summary.warn, 1, "non-immutable JS should warn");
        assert_eq!(report.exit_code(), 2, "warn-only should exit 2");
    }
}
