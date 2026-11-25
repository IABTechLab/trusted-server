//! `ts ec` — operator diagnostics for Edge Cookie identity through a deployed service's
//! admin routes.
//!
//! The responses carry raw consent strings and partner UIDs. A successful response is
//! written only to the command's output: never to disk, and never through the logger. A
//! refusal is reported like any other CLI error, with the service's `error` text.

pub mod eids;
pub mod show;

use std::borrow::Cow;

use clap::Subcommand;

use crate::error::CliResult;

/// Subcommands under `ts ec`.
#[derive(Debug, Subcommand)]
pub enum EcCommand {
    /// Show the identity-graph record stored for an EC ID and its auction EID preview.
    Show(ShowArgs),
    /// Preview which configured partners would match `ts-eids` and `sharedId` cookie values.
    Eids(EidsArgs),
}

/// Arguments for `ts ec show`.
#[derive(Debug, clap::Args)]
pub struct ShowArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// EC ID to look up: 64 lowercase hex characters, a dot, then 6 alphanumerics.
    pub ec_id: String,

    /// Admin username for the service's `^/_ts/admin` basic auth.
    #[arg(long, default_value = "admin")]
    pub username: String,

    /// Print the service's full JSON response unchanged.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `ts ec eids`.
///
/// The values are what a browser sends: copy them from the cookie table in the browser's
/// developer tools. At least one is required.
#[derive(Debug, clap::Args)]
#[command(group(
    clap::ArgGroup::new("cookies")
        .required(true)
        .multiple(true)
        .args(["ts_eids", "shared_id"])
))]
pub struct EidsArgs {
    /// HTTPS base URL of the service; HTTP is permitted only for loopback development.
    #[arg(long)]
    pub service: String,

    /// Value of the `ts-eids` cookie.
    #[arg(long)]
    pub ts_eids: Option<String>,

    /// Value of the `sharedId` cookie.
    #[arg(long)]
    pub shared_id: Option<String>,

    /// Admin username for the service's `^/_ts/admin` basic auth.
    #[arg(long, default_value = "admin")]
    pub username: String,

    /// Print the service's full JSON response unchanged.
    #[arg(long)]
    pub json: bool,
}

/// Run a `ts ec` subcommand.
///
/// # Errors
///
/// Returns an error when an argument is invalid, the admin password is absent from the
/// environment, the service cannot be reached, or it refuses the request.
pub fn run(command: EcCommand, out: &mut impl std::io::Write) -> CliResult<()> {
    match command {
        EcCommand::Show(args) => show::run_show(&args, out),
        EcCommand::Eids(args) => eids::run_eids(&args, out),
    }
}

/// Describe a success body that does not match the expected response.
///
/// Only the position is reported. `serde_json`'s message can quote the offending value,
/// which here could be a consent string or partner UID, and CLI errors are logged.
fn unexpected_answer(expected: &str, error: &serde_json::Error) -> String {
    format!(
        "the service's answer is not {expected} (JSON line {}, column {})",
        error.line(),
        error.column()
    )
}

/// The service's body exactly as received, ending in a newline so the shell prompt does
/// not run into it. A newline is added only when the body lacks one.
fn json_passthrough(body: &str) -> String {
    if body.ends_with('\n') {
        body.to_owned()
    } else {
        format!("{body}\n")
    }
}

/// Escape response control and invisible formatting characters before placing a value in
/// a terminal report.
///
/// Escape only data fields, so the report's own line breaks and JSON passthrough stay
/// unchanged. Cookie decoding can produce controls even when the cookie itself is ASCII.
/// Bidirectional controls, zero-width and invisible characters, line separators and soft
/// hyphens are not controls, but they can reorder, hide or break part of a UID on screen,
/// so they are escaped too.
fn terminal_text(value: &str) -> Cow<'_, str> {
    if !value.chars().any(needs_terminal_escape) {
        return Cow::Borrowed(value);
    }
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if needs_terminal_escape(character) {
            escaped.extend(character.escape_debug());
        } else {
            escaped.push(character);
        }
    }
    Cow::Owned(escaped)
}

/// Whether a character could change terminal state or how a value reads on screen.
fn needs_terminal_escape(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// Write the rendered report, mapping an I/O failure to a CLI error.
fn write_report(out: &mut impl std::io::Write, report: &str) -> CliResult<()> {
    out.write_all(report.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|error| format!("failed to write the result: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unexpected_answer_does_not_quote_response_values() {
        let error = serde_json::from_str::<u64>(r#""uid-fictional-secret""#)
            .expect_err("should fail to parse a string as a number");
        assert!(
            error.to_string().contains("uid-fictional-secret"),
            "precondition: serde_json quotes the value"
        );

        let message = unexpected_answer("an EC lookup response", &error);

        assert!(
            !message.contains("uid-fictional-secret"),
            "should not echo response data, got: {message}"
        );
        assert!(message.contains("line 1"), "should give the position");
    }

    #[test]
    fn json_passthrough_keeps_the_body_and_adds_only_a_missing_newline() {
        assert_eq!(
            json_passthrough("{} "),
            "{} \n",
            "should keep trailing spaces"
        );
        assert_eq!(
            json_passthrough("{}\n"),
            "{}\n",
            "should not double the newline"
        );
    }

    #[test]
    fn terminal_text_escapes_bidi_overrides_and_invisible_characters() {
        for character in [
            '\u{00AD}', '\u{061C}', '\u{200B}', '\u{200F}', '\u{2028}', '\u{2029}', '\u{202A}',
            '\u{202E}', '\u{2060}', '\u{2064}', '\u{2066}', '\u{2069}', '\u{FEFF}',
        ] {
            let value = format!("uid{character}cba");

            let escaped = terminal_text(&value);

            assert_eq!(
                escaped,
                format!("uid\\u{{{:x}}}cba", u32::from(character)),
                "should escape U+{:04X} as text",
                u32::from(character)
            );
        }
    }

    #[test]
    fn terminal_text_escapes_controls_and_borrows_plain_text() {
        assert_eq!(
            terminal_text("uid\n\u{1b}[2J"),
            r"uid\n\u{1b}[2J",
            "should escape controls"
        );
        for plain in [
            "uid-fictional",
            "café\u{00AC}\u{061B}\u{200A}\u{2010}\u{2027}\u{202F}\u{205F}\u{2065}",
        ] {
            assert!(
                matches!(terminal_text(plain), Cow::Borrowed(_)),
                "should leave characters outside the escaped ranges unchanged: {plain:?}"
            );
        }
    }
}
