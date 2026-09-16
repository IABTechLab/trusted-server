//! The probe's result model and its two renderings.
//!
//! Every axis and every verdict is **blocking**. That is not a style choice: the origin
//! readthrough gate is decided before the origin responds, and no post-response hook is
//! reachable on the Fastly adapter, so none of the template cache's response-side refusals
//! can be applied to the readthrough path. This probe is the only control, which is why a
//! failure exits non-zero rather than printing a warning.

use serde::{Deserialize, Serialize};

/// How far apart two responses were, when they differed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Difference {
    /// Byte offset of the first divergence.
    pub offset: usize,
    /// Short, escaped context from the first arm.
    pub left: String,
    /// Short, escaped context from the second arm.
    pub right: String,
}

/// One comparison between two fetches that differ in exactly one request signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisResult {
    /// Axis name, as printed.
    pub name: String,
    /// What the two arms varied.
    pub description: String,
    /// `None` when the arms matched.
    pub difference: Option<Difference>,
}

impl AxisResult {
    /// Whether this axis passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.difference.is_none()
    }
}

/// One check on the origin's response headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictResult {
    /// Verdict name, as printed.
    pub name: String,
    /// Whether the origin satisfied it.
    pub passed: bool,
    /// What was observed, pass or fail.
    pub detail: String,
}

/// Everything the probe learned about one URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlReport {
    /// The URL probed.
    pub url: String,
    /// Comparison axes, in the order they ran.
    pub axes: Vec<AxisResult>,
    /// Response-header verdicts.
    pub verdicts: Vec<VerdictResult>,
}

impl UrlReport {
    /// Whether every axis and verdict passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.axes.iter().all(AxisResult::passed) && self.verdicts.iter().all(|v| v.passed)
    }
}

/// The whole probe run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    /// One entry per `--url`.
    pub urls: Vec<UrlReport>,
}

impl ProbeReport {
    /// Whether the origin may be declared shareable on the evidence gathered.
    ///
    /// A single failing axis or verdict on a single URL is enough to say no.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.urls.is_empty() && self.urls.iter().all(UrlReport::passed)
    }

    /// Render for a human reader.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        for url in &self.urls {
            out.push_str(&format!("{}\n", url.url));
            for axis in &url.axes {
                match &axis.difference {
                    None => {
                        out.push_str(&format!("  PASS  {:<16} {}\n", axis.name, axis.description))
                    }
                    Some(difference) => {
                        out.push_str(&format!(
                            "  FAIL  {:<16} {} — differs at byte {}\n",
                            axis.name, axis.description, difference.offset
                        ));
                        out.push_str(&format!("          a: {}\n", difference.left));
                        out.push_str(&format!("          b: {}\n", difference.right));
                    }
                }
            }
            for verdict in &url.verdicts {
                let label = if verdict.passed { "PASS" } else { "FAIL" };
                out.push_str(&format!(
                    "  {label}  {:<16} {}\n",
                    verdict.name, verdict.detail
                ));
            }
            out.push('\n');
        }

        out.push_str(if self.passed() {
            "VERDICT: shareable on the URLs sampled.\n"
        } else {
            "VERDICT: NOT shareable. Do not enable origin_is_cookie_independent.\n"
        });
        out.push_str(LIMITS);
        out
    }
}

/// Printed on every run, pass or fail.
///
/// An operator who reads only a green verdict will over-generalize it, and both of these
/// limits are invisible from the output itself.
pub const LIMITS: &str = "\nLimits of this result:\n  \
     - Runs from one client address, so origin personalization keyed on the forwarded\n    \
       client IP (geo, rate-class) is undetectable here.\n  \
     - Covers the URLs sampled, not the origin as a whole.\n  \
     - Sends synthetic cookies. An origin that personalizes only for a genuine\n    \
       authenticated session shows no difference unless you pass that session's\n    \
       cookies with --cookie.\n  \
     - Varies only the signals it has axes for. Accept-Language, Referer and client\n    \
       hints are never varied, so locale-based personalization would not be seen.\n  \
     - Compares a handful of back-to-back requests, so variation on a slower cycle\n    \
       (an hourly rotation, a low-frequency experiment bucket) can fall between them.\n";

/// First byte at which two bodies diverge, with a short escaped window from each.
///
/// Returns `None` when the bodies are identical.
#[must_use]
pub fn first_difference(left: &[u8], right: &[u8]) -> Option<Difference> {
    if left == right {
        return None;
    }
    let offset = left
        .iter()
        .zip(right.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| left.len().min(right.len()));
    Some(Difference {
        offset,
        left: context_window(left, offset),
        right: context_window(right, offset),
    })
}

/// A bounded, escaped excerpt starting at `offset`.
///
/// Bounded because this is publisher HTML: it can be large, and it can contain bytes that
/// would corrupt a terminal.
fn context_window(bytes: &[u8], offset: usize) -> String {
    const WINDOW: usize = 48;
    let end = bytes.len().min(offset + WINDOW);
    let slice = bytes.get(offset..end).unwrap_or_default();
    let mut rendered = String::with_capacity(slice.len());
    for &byte in slice {
        match byte {
            b'\n' => rendered.push_str("\\n"),
            b'\r' => rendered.push_str("\\r"),
            b'\t' => rendered.push_str("\\t"),
            0x20..=0x7e => rendered.push(byte as char),
            _ => rendered.push_str(&format!("\\x{byte:02x}")),
        }
    }
    if end < bytes.len() {
        rendered.push('…');
    }
    if rendered.is_empty() {
        "<end of body>".to_owned()
    } else {
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(name: &str, difference: Option<Difference>) -> AxisResult {
        AxisResult {
            name: name.to_owned(),
            description: "test".to_owned(),
            difference,
        }
    }

    fn verdict(name: &str, passed: bool) -> VerdictResult {
        VerdictResult {
            name: name.to_owned(),
            passed,
            detail: "test".to_owned(),
        }
    }

    #[test]
    fn identical_bodies_have_no_difference() {
        assert_eq!(
            first_difference(b"<html>ok</html>", b"<html>ok</html>"),
            None
        );
    }

    #[test]
    fn difference_reports_the_first_diverging_byte() {
        let difference = first_difference(b"<html>aaa</html>", b"<html>bbb</html>")
            .expect("should report a difference");
        assert_eq!(difference.offset, 6, "divergence starts after `<html>`");
        assert!(difference.left.starts_with("aaa"));
        assert!(difference.right.starts_with("bbb"));
    }

    #[test]
    fn difference_handles_one_body_being_a_prefix_of_the_other() {
        let difference =
            first_difference(b"<html>", b"<html>more").expect("should report a difference");
        assert_eq!(difference.offset, 6);
        assert_eq!(
            difference.left, "<end of body>",
            "a truncated arm must say so rather than render an empty window"
        );
    }

    #[test]
    fn context_window_escapes_bytes_that_would_corrupt_a_terminal() {
        let difference = first_difference(b"a\x00\x01", b"b\x00\x01").expect("should differ");
        assert_eq!(difference.left, "a\\x00\\x01");
    }

    #[test]
    fn context_window_is_bounded() {
        let long = vec![b'x'; 500];
        let mut other = long.clone();
        other[0] = b'y';
        let difference = first_difference(&long, &other).expect("should differ");
        assert!(
            difference.left.chars().count() <= 49,
            "a 500-byte body must not dump 500 bytes into the terminal"
        );
        assert!(
            difference.left.ends_with('…'),
            "truncation should be visible"
        );
    }

    #[test]
    fn one_failing_axis_fails_the_whole_report() {
        let report = ProbeReport {
            urls: vec![UrlReport {
                url: "https://example.com/a".to_owned(),
                axes: vec![
                    axis("cookie", None),
                    axis(
                        "user-agent",
                        Some(Difference {
                            offset: 0,
                            left: "a".to_owned(),
                            right: "b".to_owned(),
                        }),
                    ),
                ],
                verdicts: vec![verdict("freshness", true)],
            }],
        };

        assert!(
            !report.passed(),
            "any axis failing must fail the run; the verdicts are the only control on this path"
        );
    }

    #[test]
    fn one_failing_verdict_fails_the_whole_report() {
        let report = ProbeReport {
            urls: vec![UrlReport {
                url: "https://example.com/a".to_owned(),
                axes: vec![axis("cookie", None)],
                verdicts: vec![verdict("freshness", true), verdict("set-cookie", false)],
            }],
        };

        assert!(!report.passed());
    }

    #[test]
    fn one_failing_url_fails_a_multi_url_run() {
        let good = UrlReport {
            url: "https://example.com/a".to_owned(),
            axes: vec![axis("cookie", None)],
            verdicts: vec![verdict("freshness", true)],
        };
        let mut bad = good.clone();
        bad.url = "https://example.com/b".to_owned();
        bad.verdicts = vec![verdict("freshness", false)];

        assert!(
            !ProbeReport {
                urls: vec![good, bad]
            }
            .passed(),
            "a clean result on one URL says nothing about another"
        );
    }

    #[test]
    fn an_empty_run_is_not_a_pass() {
        assert!(
            !ProbeReport { urls: Vec::new() }.passed(),
            "probing nothing must not read as evidence of shareability"
        );
    }

    #[test]
    fn rendered_text_always_states_the_limits() {
        let report = ProbeReport {
            urls: vec![UrlReport {
                url: "https://example.com/a".to_owned(),
                axes: vec![axis("cookie", None)],
                verdicts: vec![verdict("freshness", true)],
            }],
        };
        let rendered = report.render_text();

        assert!(rendered.contains("VERDICT: shareable"));
        assert!(
            rendered.contains("one client address"),
            "a green verdict is the most likely to be over-generalized, so the limits print too"
        );
    }
}
