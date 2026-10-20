//! The sellers a deployment adds to the publisher's `ads.txt`.
//!
//! The publisher's own file at the origin stays authoritative.

use std::sync::Arc;

use error_stack::Report;
use serde::Deserialize;
use validator::Validate;

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};
use trusted_server_core::middleware::{
    Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase,
};
use trusted_server_core::settings::{IntegrationConfig, Settings};
use trusted_server_core::streaming_processor::StreamProcessor;

/// The id the module's registration carries.
const SELLERS_INTEGRATION_ID: &str = "ads_txt_sellers";

/// The name `[ads-txt] modules` selects the module by and a `[[fetch]]` entry
/// names its middleware by, from this crate's folder. Its settings are the
/// table `[ads-txt.sellers]`.
pub const MODULE: &str = "ads-txt.sellers";

/// The media type an `ads.txt` is served as, and the one the middleware
/// works on.
pub const MEDIA_TYPE: &str = "text/plain";

/// The variables the specification names, each written as `NAME=value`.
const VARIABLES: [&str; 5] = [
    "CONTACT",
    "SUBDOMAIN",
    "INVENTORYPARTNERDOMAIN",
    "OWNERDOMAIN",
    "MANAGERDOMAIN",
];

/// Configuration for the sellers added to `ads.txt`.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct SellersConfig {
    /// The lines written at the end of the publisher's file, each a data
    /// record or a variable, as the file's grammar has them.
    pub lines: Vec<String>,
}

impl IntegrationConfig for SellersConfig {}

/// Refuses a line that is not a data record or a variable, saying which part
/// is wrong.
///
/// A comment or an empty line is refused too, because the settings carry
/// what the deployment declares and a comment declares nothing.
fn check_line(line: &str) -> Result<(), String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err("is empty".to_owned());
    }
    if trimmed.contains('#') {
        return Err("carries a comment, and a line in the settings declares something".to_owned());
    }
    if trimmed.contains(['\n', '\r']) {
        return Err("is more than one line".to_owned());
    }
    if let Some((name, value)) = trimmed.split_once('=')
        && !name.contains(',')
    {
        let name = name.trim();
        if VARIABLES
            .iter()
            .any(|variable| variable.eq_ignore_ascii_case(name))
        {
            if value.trim().is_empty() {
                return Err(format!("gives `{name}` no value"));
            }
            return Ok(());
        }
        return Err(format!(
            "names the variable `{name}`, which is not one of {}",
            VARIABLES.join(", ")
        ));
    }
    let fields: Vec<&str> = trimmed.split(',').map(str::trim).collect();
    if fields.len() < 3 || fields.len() > 4 {
        return Err(format!(
            "has {} fields, and a data record has three or four: the domain, the account id, \
             DIRECT or RESELLER, and the certification authority id",
            fields.len()
        ));
    }
    if fields[0].is_empty() {
        return Err("has no domain where its first field should be one".to_owned());
    }
    if fields[0].contains(char::is_whitespace) {
        return Err(format!(
            "has `{}` as its domain, and a domain holds no space",
            fields[0]
        ));
    }
    if fields[1].is_empty() {
        return Err("has no account id in its second field".to_owned());
    }
    if !fields[2].eq_ignore_ascii_case("DIRECT") && !fields[2].eq_ignore_ascii_case("RESELLER") {
        return Err(format!(
            "says the relationship is `{}`, and it is DIRECT or RESELLER",
            fields[2]
        ));
    }
    if fields.len() == 4 && fields[3].is_empty() {
        return Err("ends with a comma and no certification authority id".to_owned());
    }
    Ok(())
}

/// Refuses settings whose lines are not each a record or a variable, or that
/// give no line.
fn validate_lines(config: &SellersConfig) -> Result<(), Report<TrustedServerError>> {
    if config.lines.is_empty() {
        return Err(Report::new(TrustedServerError::Settings {
            message: "`lines` in [ads-txt.sellers] is empty, so the module would add nothing. \
                      Give the lines to add, or do not select the module"
                .to_owned(),
        }));
    }
    for (index, line) in config.lines.iter().enumerate() {
        if let Err(problem) = check_line(line) {
            return Err(Report::new(TrustedServerError::Settings {
                message: format!(
                    "`lines` in [ads-txt.sellers], line {}, `{}`, {problem}",
                    index + 1,
                    line.trim()
                ),
            }));
        }
    }
    Ok(())
}

/// Refuses a document that selects the module but names it in no entry, where
/// the lines would never be written.
fn require_an_entry(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    let placed = settings.fetch.entries().iter().any(|entry| {
        entry.media_type == MEDIA_TYPE && entry.middleware.iter().any(|name| name == MODULE)
    });
    if placed {
        return Ok(());
    }
    Err(Report::new(TrustedServerError::Settings {
        message: format!(
            "[ads-txt] selects `sellers`, but no [[fetch]] entry for `{MEDIA_TYPE}` names \
             `{MODULE}`, so the lines would never be written. Add an entry with media_type = \
             `{MEDIA_TYPE}`, path = `/ads.txt` and the middleware. The path is a prefix, so \
             the entry covers every path that starts with `/ads.txt`"
        ),
    }))
}

/// Validates the sellers' configuration and reports whether a section selects
/// the module.
///
/// # Errors
///
/// Returns an error when the table cannot be read, a line is not a record or
/// a variable, no line is given, or no entry places the middleware.
pub fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<SellersConfig>(MODULE)? else {
        return Ok(false);
    };
    validate_lines(&config)?;
    require_an_entry(settings)?;
    Ok(true)
}

/// Registers the sellers' middleware when a section selects the module.
///
/// # Errors
///
/// Returns the errors [`validate`] returns.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<SellersConfig>(MODULE)? else {
        return Ok(None);
    };
    validate_lines(&config)?;
    require_an_entry(settings)?;
    Ok(Some(
        IntegrationRegistration::builder(SELLERS_INTEGRATION_ID)
            .without_js()
            .with_middleware(Arc::new(Sellers::new(&config.lines)))
            .build(),
    ))
}

/// The builder a deployment installs to offer this module.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(
        SELLERS_INTEGRATION_ID,
        env!("CARGO_PKG_NAME"),
        register,
        validate,
    )
    .with_module_name(MODULE)
}

/// The lines written at the end of the file, as the middleware adds them.
#[derive(Debug)]
pub struct Sellers {
    /// The lines, each ending in a line break.
    tail: Arc<str>,
}

impl Sellers {
    /// The middleware for `lines`, which the caller has checked.
    #[must_use]
    pub fn new(lines: &[String]) -> Self {
        let mut tail = String::new();
        for line in lines {
            tail.push_str(line.trim());
            tail.push('\n');
        }
        Self {
            tail: Arc::from(tail),
        }
    }
}

impl Middleware for Sellers {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == MEDIA_TYPE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        MiddlewareAction {
            stream: Some(Box::new(AppendLines {
                tail: Arc::clone(&self.tail),
                ended_with_break: true,
            })),
            ..MiddlewareAction::pass()
        }
    }
}

/// Passes the file through and writes the lines after its last byte, on a
/// line of their own.
struct AppendLines {
    tail: Arc<str>,
    /// Whether the file so far ends with a line break.
    ended_with_break: bool,
}

impl StreamProcessor for AppendLines {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, std::io::Error> {
        let mut output = chunk.to_vec();
        if let Some(last) = chunk.last() {
            self.ended_with_break = *last == b'\n';
        }
        if is_last {
            if !self.ended_with_break {
                output.push(b'\n');
            }
            output.extend_from_slice(self.tail.as_bytes());
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::html_processor::test_support::page_settings;
    use trusted_server_core::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use trusted_server_core::middleware::MiddlewareChain;
    use trusted_server_core::middleware::test_support::context;

    const RECORD: &str = "ssp.example, 12345, DIRECT, f08c47fec0942fa0";

    /// A document selecting the module with `lines`, placed on the file.
    fn settings_with(lines: &[&str]) -> Settings {
        let quoted: Vec<String> = lines.iter().map(|line| format!("\"{line}\"")).collect();
        page_settings(&format!(
            "[ads-txt]\nmodules = [\"sellers\"]\n\n[ads-txt.sellers]\nlines = [{}]\n\n\
             [[fetch]]\nmedia_type = \"text/plain\"\npath = \"/ads.txt\"\n\
             middleware = [\"ads-txt.sellers\"]\n",
            quoted.join(", ")
        ))
    }

    /// What `body` becomes through the middleware, read `chunk` bytes at a
    /// time.
    fn through(lines: &[&str], body: &str, chunk: usize) -> String {
        let lines: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
        let document_state = IntegrationDocumentState::default();
        let plan = MiddlewareChain::new(
            MiddlewarePhase::Fetch,
            MEDIA_TYPE,
            vec![Arc::new(Sellers::new(&lines))],
        )
        .plan(&context(MiddlewarePhase::Fetch, &document_state))
        .expect("the chain should plan");
        let mut processors = plan.processors;
        assert_eq!(processors.len(), 1, "the middleware adds one processor");
        let processor = &mut processors[0];
        let bytes = body.as_bytes();
        let mut output = Vec::new();
        if bytes.is_empty() {
            output.extend(processor.process_chunk(&[], true).expect("should process"));
        } else {
            let chunks: Vec<&[u8]> = bytes.chunks(chunk).collect();
            for (index, piece) in chunks.iter().enumerate() {
                let is_last = index + 1 == chunks.len();
                output.extend(
                    processor
                        .process_chunk(piece, is_last)
                        .expect("should process"),
                );
            }
        }
        String::from_utf8(output).expect("text")
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }

    #[test]
    fn an_unselected_module_does_not_register() {
        let settings = page_settings("");
        assert!(register(&settings).expect("should read").is_none());
        assert!(!validate(&settings).expect("should read"));
    }

    #[test]
    fn the_lines_are_offered_as_the_modules_middleware_on_text() {
        let settings = settings_with(&[RECORD]);
        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("should build the registry");

        assert_eq!(
            registry
                .middleware_chain(
                    &settings.fetch,
                    MiddlewarePhase::Fetch,
                    MEDIA_TYPE,
                    "/ads.txt"
                )
                .ids(),
            [MODULE],
            "should run on the file"
        );
        assert!(
            registry
                .middleware_chain(
                    &settings.fetch,
                    MiddlewarePhase::Fetch,
                    MEDIA_TYPE,
                    "/robots.txt"
                )
                .is_empty(),
            "should run on no other path"
        );
        let registration = register(&settings)
            .expect("should parse")
            .expect("should register");
        assert!(
            registration.js_disabled,
            "nothing of this module runs in the browser"
        );
    }

    #[test]
    fn a_document_naming_the_module_in_no_entry_is_refused() {
        let settings = page_settings(&format!(
            "[ads-txt]\nmodules = [\"sellers\"]\n\n[ads-txt.sellers]\nlines = [\"{RECORD}\"]\n"
        ));

        let refused = validate(&settings).expect_err("should refuse a module no entry places");
        assert!(
            format!("{refused:?}")
                .contains("no [[fetch]] entry for `text/plain` names `ads-txt.sellers`"),
            "{refused:?}"
        );
        assert!(register(&settings).is_err());
    }

    #[test]
    fn a_line_that_is_not_a_record_or_a_variable_is_refused_for_its_reason() {
        let cases = [
            ("", "line 2, ``, is empty"),
            ("# a comment", "carries a comment"),
            ("ssp.example, 12345", "has 2 fields"),
            ("ssp.example, 12345, DIRECT, abc, extra", "has 5 fields"),
            (", 12345, DIRECT", "has no domain"),
            (
                "ssp example, 12345, DIRECT",
                "has `ssp example` as its domain, and a domain holds no space",
            ),
            ("ssp.example, , DIRECT", "has no account id"),
            (
                "ssp.example, 12345, PARTNER",
                "says the relationship is `PARTNER`",
            ),
            ("ssp.example, 12345, DIRECT, ", "ends with a comma"),
            ("CONTACT=", "gives `CONTACT` no value"),
            ("EMAIL=ads@publisher.example", "names the variable `EMAIL`"),
        ];
        for (line, reason) in cases {
            let settings = settings_with(&[RECORD, line]);
            for (call, refusal) in [
                ("validate", validate(&settings).err()),
                ("register", register(&settings).err()),
            ] {
                let message = refusal
                    .unwrap_or_else(|| panic!("{call} should refuse {line:?}"))
                    .current_context()
                    .to_string();
                assert!(
                    message.contains("`lines` in [ads-txt.sellers], line 2")
                        && message.contains(reason),
                    "{call} should say {reason:?} of {line:?}, and said {message:?}"
                );
            }
        }
    }

    #[test]
    fn no_line_at_all_is_refused() {
        let settings = settings_with(&[]);
        let message = validate(&settings)
            .expect_err("should refuse an empty list")
            .current_context()
            .to_string();
        assert!(
            message.contains("is empty, so the module would add nothing"),
            "{message}"
        );
    }

    #[test]
    fn records_and_variables_are_accepted_as_the_grammar_has_them() {
        for line in [
            "ssp.example, 12345, DIRECT",
            "ssp.example,12345,reseller,f08c47fec0942fa0",
            "  ssp.example , 12345 , Direct  ",
            "CONTACT=ads@publisher.example",
            "contact=ads@publisher.example",
            "Subdomain=news.publisher.example",
            "ssp.example, pub=12345, DIRECT",
            "SUBDOMAIN=news.publisher.example",
            "INVENTORYPARTNERDOMAIN=partner.example",
            "OWNERDOMAIN=publisher.example",
            "MANAGERDOMAIN=manager.example, GB",
        ] {
            let settings = settings_with(&[line]);
            assert!(validate(&settings).unwrap_or_else(|error| panic!("{line}: {error:?}")));
        }
    }

    #[test]
    fn the_lines_follow_the_publishers_file_on_a_line_of_their_own() {
        let lines = [RECORD, "CONTACT=ads@publisher.example"];
        let added = format!("{RECORD}\nCONTACT=ads@publisher.example\n");

        assert_eq!(
            through(&lines, "own.example, 1, DIRECT\n", 8192),
            format!("own.example, 1, DIRECT\n{added}"),
            "a file ending with a line break takes the lines straight after it"
        );
        assert_eq!(
            through(&lines, "own.example, 1, DIRECT", 8192),
            format!("own.example, 1, DIRECT\n{added}"),
            "a file without a final line break gets one before the lines"
        );
        assert_eq!(
            through(&lines, "", 8192),
            added,
            "an empty file is the lines"
        );
    }

    #[test]
    fn a_file_whose_lines_end_with_a_carriage_return_takes_the_lines_after_its_last_one() {
        assert_eq!(
            through(&[RECORD], "own.example, 1, DIRECT\r\n", 8192),
            format!("own.example, 1, DIRECT\r\n{RECORD}\n"),
            "a file ending with a carriage return and a line feed takes no second break"
        );
        assert_eq!(
            through(&[RECORD], "a.example, 1, DIRECT\r\nb.example, 2, DIRECT", 3),
            format!("a.example, 1, DIRECT\r\nb.example, 2, DIRECT\n{RECORD}\n"),
            "a file without a final line break gets one before the lines"
        );
    }

    #[test]
    fn a_file_read_in_pieces_is_the_same_file() {
        let lines = [RECORD];
        let body = "a.example, 1, DIRECT\nb.example, 2, RESELLER";
        let whole = through(&lines, body, 8192);

        for chunk in [1, 3, 7, 20] {
            assert_eq!(
                through(&lines, body, chunk),
                whole,
                "read {chunk} bytes at a time"
            );
        }
        assert_eq!(whole, format!("{body}\n{RECORD}\n"));
    }

    #[test]
    fn the_lines_are_written_as_given_and_trimmed() {
        assert_eq!(
            through(&["  ssp.example, 1, direct  "], "x\n", 8192),
            "x\nssp.example, 1, direct\n",
            "the relationship is written as the deployment wrote it"
        );
    }

    #[test]
    fn the_middleware_works_on_text_alone_in_the_fetch_phase() {
        let sellers = Sellers::new(&[RECORD.to_owned()]);
        assert!(sellers.handles_media_type(MEDIA_TYPE));
        assert!(!sellers.handles_media_type("text/html"));
        assert_eq!(sellers.phases(), [MiddlewarePhase::Fetch]);
    }
}
