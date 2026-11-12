//! Listing the signing keys a service publishes at `/.well-known/trusted-server.json`.

use core::fmt::Write as _;

use serde::Deserialize;

use crate::ad_templates::output::escape_terminal_text;
use crate::commands::keys::{KeysRequest, ListArgs, send};
use crate::error::{CliResult, cli_error};

const DISCOVERY_PATH: &str = "/.well-known/trusted-server.json";

/// The parts of the discovery document this command reads.
#[derive(Deserialize)]
struct Discovery {
    jwks: Jwks,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: Option<String>,
    crv: Option<String>,
}

/// Render one line per published key: kid, key type, and curve.
fn render_keys(body: &str) -> CliResult<String> {
    let discovery: Discovery = serde_json::from_str(body)
        .map_err(|error| format!("invalid discovery document: {error}"))?;
    if discovery.jwks.keys.is_empty() {
        return Ok("no active signing keys are published\n".to_owned());
    }
    let mut rendered = String::new();
    for key in &discovery.jwks.keys {
        writeln!(
            rendered,
            "{}\t{}\t{}",
            escape_terminal_text(key.kid.as_deref().unwrap_or("<no kid>")),
            escape_terminal_text(key.kty.as_deref().unwrap_or("-")),
            escape_terminal_text(key.crv.as_deref().unwrap_or("-")),
        )
        .expect("should write to a String");
    }
    Ok(rendered)
}

/// Execute `ts keys list`.
///
/// # Errors
///
/// Returns an error when the service URL is unsafe or malformed, the service cannot be
/// reached, or it answers with a non-success status or a body that is not a discovery
/// document.
pub fn run_list(args: &ListArgs, out: &mut impl std::io::Write) -> CliResult<()> {
    let endpoint = crate::url_guard::service_endpoint(&args.service, DISCOVERY_PATH)?;
    let (status, body) = send(&endpoint, &KeysRequest::Get)?;
    if !status.is_success() {
        // Every adapter registers the discovery route, so a 404 means `--service` names
        // something other than a Trusted Server, while unreadable signing stores answer 500.
        let hint = match status.as_u16() {
            404 => " — check that --service names a Trusted Server deployment",
            500 => " — the service could not read its signing keys; check its jwks_store",
            _ => "",
        };
        return cli_error(format!(
            "fetching the discovery document failed: {status}{hint}\n{}",
            body.trim()
        ));
    }

    // Validated even for --json, so a script never mistakes an error page for keys.
    let rendered = render_keys(&body)?;
    let mut output = if args.json { body } else { rendered };
    if !output.ends_with('\n') {
        output.push('\n');
    }
    write!(out, "{output}").map_err(|error| format!("failed to write the key list: {error}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn each_key_renders_its_kid_type_and_curve() {
        let body = json!({
            "version": "1.0",
            "jwks": { "keys": [
                { "kty": "OKP", "crv": "Ed25519", "x": "example", "kid": "ts-2024-01-15" },
                { "kty": "OKP", "crv": "Ed25519", "x": "example", "kid": "ts-2024-02-15" },
            ]}
        })
        .to_string();

        let rendered = render_keys(&body).expect("should render a valid document");

        assert_eq!(
            rendered, "ts-2024-01-15\tOKP\tEd25519\nts-2024-02-15\tOKP\tEd25519\n",
            "should print one line per key"
        );
    }

    #[test]
    fn an_empty_key_set_says_so() {
        let body = json!({ "version": "1.0", "jwks": { "keys": [] } }).to_string();

        let rendered = render_keys(&body).expect("should render an empty set");

        assert!(
            rendered.contains("no active signing keys"),
            "should say no keys are published, got: {rendered}"
        );
    }

    #[test]
    fn service_controlled_fields_cannot_forge_lines_or_columns() {
        let body = json!({
            "version": "1.0",
            "jwks": { "keys": [
                {
                    "kty": "OKP\u{7}",
                    "crv": "Ed25519\u{1b}[1G",
                    "kid": "\u{1b}[2Kts-2024-09-09\tforged"
                },
            ]}
        })
        .to_string();

        let rendered = render_keys(&body).expect("should render a valid document");

        assert_eq!(
            rendered,
            "\\u{001B}[2Kts-2024-09-09\\u{0009}forged\tOKP\\u{0007}\tEd25519\\u{001B}[1G\n",
            "should escape control characters, tabs included, in service-controlled fields"
        );
    }

    #[test]
    fn a_body_that_is_not_a_discovery_document_is_an_error() {
        assert!(render_keys("<html></html>").is_err(), "should reject HTML");
        assert!(
            render_keys(&json!({ "keys": [] }).to_string()).is_err(),
            "should reject a bare JWKS without the discovery wrapper"
        );
    }
}
