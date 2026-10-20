//! The file served at `/robots.txt`, held as a document rather than as text.
//!
//! Contributors hand core groups of records, and core writes every byte. So a
//! contributor cannot produce a malformed file, and the rules that have to hold
//! whatever a contributor says are applied here, in one place.
//!
//! 1. A refusal cannot be loosened. When `refuse_all` is selected the file is
//!    the refusal and nothing else, see [`refusal`].
//! 2. A path in `always_allow` stays open to every crawler a contribution
//!    refuses the whole site to, see [`assemble`].
//! 3. The publisher's own text and the `Sitemap` line go where the publisher
//!    put them, around what the contributors gave.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::RobotsTxtConfig;

/// One record in a group, in the order it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Record {
    /// A path the group's crawlers may fetch.
    Allow(String),
    /// A path the group's crawlers may not fetch.
    Disallow(String),
    /// A line of explanation, without its leading `#`.
    Comment(String),
    /// Any other `name: value` record, kept where it was given, a Terms
    /// Document Locator for example.
    Other {
        /// The record's name, as it was written.
        name: String,
        /// Its value.
        value: String,
    },
}

impl Record {
    fn line(&self) -> String {
        match self {
            Self::Allow(path) => format!("Allow: {}", one_line(path)),
            Self::Disallow(path) => format!("Disallow: {}", one_line(path)),
            Self::Comment(text) => format!("#{}", one_line(text)),
            Self::Other { name, value } => format!("{}: {}", one_line(name), one_line(value)),
        }
    }

    /// `Disallow: /`, or the same rule written with wildcards, such as
    /// `Disallow: /*`, which RFC 9309 section 2.2.3 reads as matching every path.
    fn refuses_the_whole_site(&self) -> bool {
        matches!(self, Self::Disallow(path)
            if one_line(path).strip_prefix('/').is_some_and(|rest| rest.bytes().all(|b| b == b'*')))
    }
}

/// `value` with every CR and LF taken out, so that it cannot end its line and
/// start a record of its own.
fn one_line(value: &str) -> Cow<'_, str> {
    if value.contains(['\r', '\n']) {
        Cow::Owned(value.replace(['\r', '\n'], ""))
    } else {
        Cow::Borrowed(value)
    }
}

/// The crawlers a group is for, and the records that apply to them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// Each written on its own `User-Agent` line. An empty name is kept as it
    /// was given, because the file is written back as it was read.
    pub user_agents: Vec<String>,
    /// The group's records, in order.
    pub records: Vec<Record>,
}

/// What one contributor adds to the file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contribution {
    /// Records before the first group, such as a comment introducing the rules.
    pub preamble: Vec<Record>,
    /// The groups, in order.
    pub groups: Vec<Group>,
}

impl Contribution {
    /// A contribution read from `robots.txt` text, keeping every group, record
    /// and comment in the order it came.
    ///
    /// A `User-Agent` line after a group's rules starts the next group, and
    /// consecutive `User-Agent` lines name the crawlers of one group, as RFC
    /// 9309 section 2.1 describes. A `#` starts a comment wherever it is, as
    /// section 2.2 allows, and the comment is kept as a record of its own after
    /// the rule it followed. Blank lines and lines with no `:` carry nothing
    /// and are passed over. A record before the first group is kept in the
    /// preamble rather than dropped, so nothing given is lost.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut contribution = Self::default();
        let mut current: Option<Group> = None;
        for raw in text.split('\n') {
            let (rule, comment) = match raw.split_once('#') {
                Some((rule, comment)) => (rule.trim(), Some(comment.trim_end())),
                None => (raw.trim(), None),
            };
            if let Some((name, value)) = rule.split_once(':') {
                let (name, value) = (name.trim(), value.trim().to_owned());
                match name.to_ascii_lowercase().as_str() {
                    "user-agent" => {
                        if current.as_ref().is_some_and(|group| {
                            group
                                .records
                                .iter()
                                .any(|record| !matches!(record, Record::Comment(_)))
                        }) && let Some(finished) = current.take()
                        {
                            contribution.groups.push(finished);
                        }
                        current
                            .get_or_insert_with(Group::default)
                            .user_agents
                            .push(value);
                    }
                    "allow" => push(&mut contribution, &mut current, Record::Allow(value)),
                    "disallow" => push(&mut contribution, &mut current, Record::Disallow(value)),
                    _ => push(
                        &mut contribution,
                        &mut current,
                        Record::Other {
                            name: name.to_owned(),
                            value,
                        },
                    ),
                }
            }
            if let Some(comment) = comment {
                push(
                    &mut contribution,
                    &mut current,
                    Record::Comment(comment.to_owned()),
                );
            }
        }
        if let Some(group) = current {
            contribution.groups.push(group);
        }
        contribution
    }

    /// Whether this contribution has anything to write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.preamble.is_empty() && self.groups.is_empty()
    }

    /// The contribution as text, with a blank line between its parts and none
    /// at either end.
    fn write(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.preamble.is_empty() {
            parts.push(
                self.preamble
                    .iter()
                    .map(Record::line)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        for group in &self.groups {
            let mut lines: Vec<String> = group
                .user_agents
                .iter()
                .map(|agent| format!("User-Agent: {}", one_line(agent)))
                .collect();
            lines.extend(group.records.iter().map(Record::line));
            parts.push(lines.join("\n"));
        }
        parts.join("\n\n")
    }
}

fn push(contribution: &mut Contribution, current: &mut Option<Group>, record: Record) {
    match current {
        Some(group) => group.records.push(record),
        None => contribution.preamble.push(record),
    }
}

/// What `allow_all` contributes, which is every crawler allowed everywhere.
///
/// Permission the document says, rather than permission that follows from a
/// lookup coming back empty.
#[must_use]
pub fn allow_all() -> Contribution {
    Contribution {
        preamble: Vec::new(),
        groups: vec![Group {
            user_agents: vec!["*".to_owned()],
            records: vec![Record::Allow("/".to_owned())],
        }],
    }
}

/// The file `refuse_all` serves, which refuses every crawler except for the
/// paths in `always_allow`.
///
/// Nothing a contributor gives, and none of the publisher's own text, is
/// added, because a refusal cannot be loosened. `Allow` beats `Disallow` on
/// the longest match under RFC 9309 section 2.2.2, so each allowance lets that
/// one path through.
#[must_use]
pub fn refusal(always_allow: &[String]) -> String {
    let mut file = String::from("User-agent: *");
    for path in always_allow {
        file.push_str("\nAllow: ");
        file.push_str(path);
    }
    file.push_str("\nDisallow: /\n");
    file
}

/// The whole file, being the publisher's text above, each contribution in the
/// order `modules` names it, the group that keeps `always_allow` open, the
/// `Sitemap` line, and the publisher's text below.
#[must_use]
pub fn assemble(config: &RobotsTxtConfig, contributions: &[Contribution]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if let Some(top) = config
        .top_text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        sections.push(top.trim_end().to_owned());
    }
    for contribution in contributions.iter().filter(|c| !c.is_empty()) {
        sections.push(contribution.write());
    }
    if let Some(segment) = kept_open(contributions, &config.always_allow) {
        sections.push(segment);
    }
    if let Some(sitemap) = &config.sitemap {
        sections.push(format!("Sitemap: {sitemap}"));
    }
    if let Some(bottom) = config
        .bottom_text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        sections.push(bottom.trim_end().to_owned());
    }
    let mut file = sections.join("\n\n");
    file.push('\n');
    file
}

/// Whether `file` holds a rule, being an `Allow` or `Disallow` line in a group
/// that names a crawler, read as a crawler reads the file. A file with none
/// reads to a crawler as permission to crawl everything.
#[must_use]
pub(crate) fn holds_a_rule(file: &str) -> bool {
    Contribution::parse(file).groups.iter().any(|group| {
        group.user_agents.iter().any(|name| !name.is_empty())
            && group
                .records
                .iter()
                .any(|record| matches!(record, Record::Allow(_) | Record::Disallow(_)))
    })
}

/// Every crawler the contributions refuse the whole site to, by the name on
/// its `User-Agent` line, in order and without repeats. `*` is included, so
/// the paths stay open to the crawlers no group names. A group with no name
/// is passed over, because there is nothing to address a rule to.
fn refused_entirely(contributions: &[Contribution]) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for group in contributions.iter().flat_map(|c| c.groups.iter()) {
        if !group.records.iter().any(Record::refuses_the_whole_site) {
            continue;
        }
        for name in &group.user_agents {
            if !name.is_empty() && !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
    }
    names
}

/// A group of core's own that keeps each path in `always_allow` open to the
/// crawlers the contributions refuse the whole site to, or `None` when there is
/// nothing to keep open.
///
/// **The contributions are never edited.** RFC 9309 section 2.2.1 has a
/// crawler combine every group that matches it, and section 2.2.2 gives the
/// match with the most octets, so a second group naming the same crawlers with
/// an `Allow` for the path beats `Disallow: /` for that path alone.
///
/// It goes below the contributions. A crawler that obeys only the first group
/// it finds then still meets `Disallow: /` first, so the worst it can do is
/// fail to read the one page. Written above, it would read the whole site.
fn kept_open(contributions: &[Contribution], paths: &[String]) -> Option<String> {
    let names = refused_entirely(contributions);
    if names.is_empty() || paths.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = names
        .iter()
        .map(|name| format!("User-Agent: {}", one_line(name)))
        .collect();
    lines.extend(paths.iter().map(|path| format!("Allow: {path}")));
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RobotsTxtConfig {
        RobotsTxtConfig {
            modules: vec!["a_contributor".to_owned()],
            ..RobotsTxtConfig::refuse_all()
        }
    }

    /// Text a contributor might read from somewhere else, with the shapes the
    /// file has to survive: a record inside a group, crawlers sharing a group,
    /// and a group with no name.
    const GIVEN: &str = "User-Agent: ExampleBot\nDisallow: /\n\nUser-Agent: \nDisallow: /\n\n\
                         User-Agent: OtherBot\nDisallow: /private/\n\nUser-Agent: ThirdBot\n\
                         User-Agent: FourthBot\nDisallow: /\n\nUser-Agent: *\n\
                         TDL: https://terms.example.com/socw/2.txt\nAllow: /";

    #[test]
    fn a_contribution_is_written_back_exactly_as_it_was_read() {
        assert_eq!(Contribution::parse(GIVEN).write(), GIVEN);
    }

    #[test]
    fn crawlers_named_together_share_one_group_and_a_record_keeps_its_place() {
        let parsed = Contribution::parse(GIVEN);

        assert_eq!(parsed.groups.len(), 5);
        assert_eq!(parsed.groups[3].user_agents, vec!["ThirdBot", "FourthBot"]);
        assert_eq!(
            parsed.groups[4].records,
            vec![
                Record::Other {
                    name: "TDL".to_owned(),
                    value: "https://terms.example.com/socw/2.txt".to_owned(),
                },
                Record::Allow("/".to_owned()),
            ]
        );
    }

    #[test]
    fn blank_lines_at_either_end_are_not_part_of_the_file() {
        let parsed = Contribution::parse("\nUser-Agent: ExampleBot\nDisallow: /\n\n\n");

        assert_eq!(parsed.write(), "User-Agent: ExampleBot\nDisallow: /");
    }

    #[test]
    fn a_comment_and_a_record_before_any_group_are_kept() {
        let parsed = Contribution::parse("# rules for crawlers\nUser-Agent: *\nAllow: /");

        assert_eq!(
            parsed.preamble,
            vec![Record::Comment(" rules for crawlers".to_owned())]
        );
        assert_eq!(
            parsed.write(),
            "# rules for crawlers\n\nUser-Agent: *\nAllow: /"
        );
    }

    #[test]
    fn the_publishers_text_and_the_sitemap_go_around_the_contributions() {
        let mut settings = config();
        settings.top_text = Some("# top".to_owned());
        settings.sitemap = Some("https://publisher.example.com/sitemap.xml".to_owned());
        settings.bottom_text = Some("# bottom".to_owned());

        assert_eq!(
            assemble(
                &settings,
                &[Contribution::parse("User-agent: *\nAllow: /\n")]
            ),
            "# top\n\nUser-Agent: *\nAllow: /\n\n\
             Sitemap: https://publisher.example.com/sitemap.xml\n\n# bottom\n",
        );
    }

    /// The page a refused crawler is sent to stays open, by a group of core's
    /// own below the contributions, which are not edited.
    #[test]
    fn an_always_allowed_path_is_kept_open_to_every_crawler_refused_the_site() {
        let mut settings = config();
        settings.always_allow = vec!["/ai-notice/".to_owned()];

        let file = assemble(&settings, &[Contribution::parse(GIVEN)]);

        assert!(
            file.starts_with(GIVEN),
            "the contribution is not edited: {file}"
        );
        assert!(
            file.ends_with(
                "\n\nUser-Agent: ExampleBot\nUser-Agent: ThirdBot\nUser-Agent: FourthBot\n\
                 Allow: /ai-notice/\n"
            ),
            "the group naming every crawler refused the whole site goes last: {file}"
        );
    }

    /// The path is kept open to `*` as it is to a crawler refused by name, so
    /// the crawlers no group names can still read it.
    #[test]
    fn an_always_allowed_path_is_kept_open_when_every_crawler_is_refused() {
        let mut settings = config();
        settings.always_allow = vec!["/ai-notice/".to_owned()];

        assert_eq!(
            assemble(
                &settings,
                &[Contribution::parse("User-Agent: *\nDisallow: /")]
            ),
            "User-Agent: *\nDisallow: /\n\nUser-Agent: *\nAllow: /ai-notice/\n",
            "a group for `*` below the contribution keeps the path open"
        );
    }

    #[test]
    fn a_wildcard_that_matches_every_path_refuses_the_whole_site() {
        let mut settings = config();
        settings.always_allow = vec!["/ai-notice/".to_owned()];

        assert_eq!(
            assemble(
                &settings,
                &[Contribution::parse("User-Agent: ExampleBot\nDisallow: /*")]
            ),
            "User-Agent: ExampleBot\nDisallow: /*\n\n\
             User-Agent: ExampleBot\nAllow: /ai-notice/\n",
            "`Disallow: /*` is the same rule as `Disallow: /`"
        );
    }

    #[test]
    fn a_comment_after_a_rule_does_not_hide_a_whole_site_refusal() {
        let mut settings = config();
        settings.always_allow = vec!["/ads.txt".to_owned()];
        let parsed = Contribution::parse("User-Agent: *\nDisallow: / # temporarily closed");

        assert_eq!(
            parsed.groups[0].records,
            vec![
                Record::Disallow("/".to_owned()),
                Record::Comment(" temporarily closed".to_owned()),
            ]
        );
        assert!(
            assemble(&settings, &[parsed]).ends_with("\n\nUser-Agent: *\nAllow: /ads.txt\n"),
            "the path stays open to the crawlers refused the site"
        );
    }

    #[test]
    fn a_comment_between_user_agent_lines_keeps_their_group() {
        let mut settings = config();
        settings.always_allow = vec!["/ads.txt".to_owned()];
        let parsed = Contribution::parse(
            "User-Agent: ExampleBot\n# the same rules for its sibling\nUser-Agent: OtherBot\nDisallow: /",
        );

        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.groups[0].user_agents, vec!["ExampleBot", "OtherBot"]);
        assert!(
            assemble(&settings, &[parsed])
                .ends_with("\n\nUser-Agent: ExampleBot\nUser-Agent: OtherBot\nAllow: /ads.txt\n"),
            "every crawler of the group is kept open to the path"
        );
    }

    #[test]
    fn a_line_break_in_a_value_cannot_start_a_record() {
        let contribution = Contribution {
            preamble: vec![Record::Comment(" note\nAllow: /".to_owned())],
            groups: vec![Group {
                user_agents: vec!["ExampleBot\r\nUser-Agent: *".to_owned()],
                records: vec![
                    Record::Disallow("/x\nUser-Agent: *\nAllow: /".to_owned()),
                    Record::Other {
                        name: "TDL\r".to_owned(),
                        value: "a\rAllow: /".to_owned(),
                    },
                ],
            }],
        };

        assert_eq!(
            assemble(&config(), &[contribution]),
            "# noteAllow: /\n\n\
             User-Agent: ExampleBotUser-Agent: *\n\
             Disallow: /xUser-Agent: *Allow: /\n\
             TDL: aAllow: /\n",
            "every value stays on the line its record starts"
        );
    }

    #[test]
    fn nothing_is_kept_open_when_nothing_is_refused() {
        let mut settings = config();
        settings.always_allow = vec!["/ai-notice/".to_owned()];

        assert_eq!(
            assemble(&settings, &[allow_all()]),
            "User-Agent: *\nAllow: /\n"
        );
    }

    #[test]
    fn contributions_appear_in_the_order_given() {
        let first = Contribution::parse("User-Agent: FirstBot\nDisallow: /");
        let second = Contribution::parse("User-Agent: SecondBot\nDisallow: /");

        assert_eq!(
            assemble(&config(), &[first, second]),
            "User-Agent: FirstBot\nDisallow: /\n\nUser-Agent: SecondBot\nDisallow: /\n"
        );
    }

    #[test]
    fn an_empty_contribution_leaves_no_gap() {
        assert_eq!(
            assemble(&config(), &[Contribution::default(), allow_all()]),
            "User-Agent: *\nAllow: /\n"
        );
    }

    /// Only an `Allow` or `Disallow` in a group that names a crawler is a
    /// rule. Anything else leaves a crawler free to crawl everything.
    #[test]
    fn a_file_holds_a_rule_only_when_a_group_allows_or_disallows() {
        for file in [
            "\n",
            "# nothing to add\n\nSitemap: https://publisher.example/sitemap.xml\n",
            "Disallow: /\n",
            "User-Agent: *\nCrawl-delay: 10\n",
            "User-Agent: \nDisallow: /\n",
        ] {
            assert!(!holds_a_rule(file), "{file:?} holds no rule");
        }
        for file in [
            "User-Agent: *\nAllow: /\n",
            "User-Agent: ExampleBot\nDisallow:\n",
            "# the publisher's own\nUser-agent: *\nDisallow: /private/\n",
            refusal(&[]).as_str(),
        ] {
            assert!(holds_a_rule(file), "{file:?} holds a rule");
        }
    }

    #[test]
    fn refusing_with_no_allowances_refuses_everything() {
        assert_eq!(refusal(&[]), "User-agent: *\nDisallow: /\n");
    }

    #[test]
    fn each_allowance_is_written_in_order_ahead_of_the_refusal() {
        assert_eq!(
            refusal(&["/ads.txt".to_owned(), "/ai-notice/".to_owned()]),
            "User-agent: *\nAllow: /ads.txt\nAllow: /ai-notice/\nDisallow: /\n"
        );
    }
}
