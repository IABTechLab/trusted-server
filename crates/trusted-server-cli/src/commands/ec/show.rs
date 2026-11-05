//! `ts ec show` — read one EC identity-graph record through `GET /_ts/admin/ec/{id}`.

use serde::Deserialize;
use trusted_server_core::ec::generation::is_valid_ec_id;

use crate::admin_client::{AdminClient, AdminRequest, UNAUTHORIZED_HINT};
use crate::commands::ec::{ShowArgs, json_passthrough, unexpected_answer, write_report};
use crate::error::{CliResult, cli_error};

const ADMIN_EC_PATH: &str = "/_ts/admin/ec";

/// The fields of the lookup response this command summarizes.
///
/// Deliberately not the server's own type: unknown fields are ignored, so a service that
/// adds detail does not break an older CLI. `--json` prints the complete body instead.
#[derive(Debug, Deserialize)]
pub(super) struct LookupResponse {
    ec_id: String,
    store: String,
    generation: u64,
    tombstone: Option<bool>,
    entry_error: Option<String>,
    metadata_error: Option<String>,
    auction: Option<AuctionView>,
}

#[derive(Debug, Deserialize)]
struct AuctionView {
    eids: Vec<EidView>,
    skipped: Vec<SkippedPartnerId>,
}

#[derive(Debug, Deserialize)]
pub(super) struct EidView {
    pub(super) source: String,
    pub(super) uids: Vec<UidView>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UidView {
    pub(super) id: String,
    pub(super) atype: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SkippedPartnerId {
    source_domain: String,
    reason: String,
}

/// Execute `ts ec show`.
///
/// # Errors
///
/// Returns an error when the EC ID is malformed (checked before anything is sent), the
/// admin password is missing, the service cannot be reached, it answers with a
/// non-success status, or its answer is not an EC lookup response.
pub fn run_show(args: &ShowArgs, out: &mut impl std::io::Write) -> CliResult<()> {
    if !is_valid_ec_id(&args.ec_id) {
        return cli_error(
            "invalid EC ID: expected 64 lowercase hex characters, a dot, then 6 alphanumerics",
        );
    }
    let client = AdminClient::new(&args.service, &args.username)?;
    let path = format!("{ADMIN_EC_PATH}/{}", args.ec_id);
    let response = client.send(AdminRequest::Get {
        path: &path,
        cookie: None,
    })?;

    if !response.status.is_success() {
        let explanation = match response.status.as_u16() {
            401 => UNAUTHORIZED_HINT,
            404 => " — no record for this EC ID",
            501 => {
                " — this deployment has no EC identity graph; only the Fastly adapter holds \
                 one, and only when an EC store is configured"
            }
            _ => "",
        };
        return cli_error(format!(
            "EC lookup failed: {}{explanation}\n{}",
            response.status,
            response.error_text()
        ));
    }

    // Parsed in both modes, so a non-lookup `200` (a proxy's page, say) is refused rather
    // than printed as if it were the record.
    let lookup: LookupResponse = serde_json::from_str(&response.body)
        .map_err(|error| unexpected_answer("an EC lookup response", &error))?;

    let report = if args.json {
        json_passthrough(&response.body)
    } else {
        render(&lookup)
    };
    write_report(out, &report)
}

/// Render the lookup as an operator-readable summary.
fn render(lookup: &LookupResponse) -> String {
    let mut lines = vec![
        format!("EC ID:      {}", lookup.ec_id),
        format!(
            "Store:      {} (generation {})",
            lookup.store, lookup.generation
        ),
        format!(
            "Tombstone:  {}",
            match lookup.tombstone {
                Some(true) => "yes (consent withdrawn)",
                Some(false) => "no",
                None => "unknown (the entry could not be read as an EC record)",
            }
        ),
    ];
    if let Some(error) = &lookup.entry_error {
        lines.push(format!("Entry error: {error}"));
    }
    if let Some(error) = &lookup.metadata_error {
        lines.push(format!("Metadata error: {error}"));
    }

    match &lookup.auction {
        None => lines.push(
            "Auction preview: none (the entry did not validate, so the auction attaches no \
             EIDs from it)"
                .to_owned(),
        ),
        Some(auction) => {
            lines.push(String::new());
            lines.push(format!("Auction EIDs ({}):", auction.eids.len()));
            lines.extend(render_eids(&auction.eids));
            lines.push(String::new());
            lines.push(format!("Skipped partner IDs ({}):", auction.skipped.len()));
            lines.extend(auction.skipped.iter().map(|skipped| {
                format!(
                    "  {}: {}",
                    skipped.source_domain,
                    skipped_reason(&skipped.reason)
                )
            }));
            lines.push(String::new());
            lines.push(
                "Live auctions also apply per-request consent checks, which this preview \
                 cannot reproduce."
                    .to_owned(),
            );
        }
    }

    let mut report = lines.join("\n");
    report.push('\n');
    report
}

/// One line per UID, grouped under its source.
pub(super) fn render_eids(eids: &[EidView]) -> Vec<String> {
    eids.iter()
        .flat_map(|eid| {
            eid.uids.iter().map(|uid| match uid.atype {
                Some(atype) => format!("  {}: {} (atype {atype})", eid.source, uid.id),
                None => format!("  {}: {}", eid.source, uid.id),
            })
        })
        .collect()
}

/// Explain a skip reason the server reports, keeping its code for searching.
fn skipped_reason(reason: &str) -> String {
    let explanation = match reason {
        "empty_uid" => "the stored UID is empty",
        "not_in_registry" => "no configured partner has this source domain",
        "bidstream_disabled" => "the partner has bidstream_enabled = false",
        _ => return reason.to_owned(),
    };
    format!("{reason} ({explanation})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(json: serde_json::Value) -> LookupResponse {
        serde_json::from_value(json).expect("should parse lookup response")
    }

    #[test]
    fn summary_shows_tombstone_eids_and_skipped_reasons() {
        let report = render(&lookup(serde_json::json!({
            "ec_id": "id.abc123",
            "store": "ec_store",
            "generation": 7,
            "tombstone": false,
            "entry": {"ignored": true},
            "auction": {
                "eids": [{"source": "live.example", "uids": [{"id": "uid-live", "atype": 3}]}],
                "skipped": [
                    {"source_domain": "off.example", "reason": "bidstream_disabled"},
                    {"source_domain": "new.example", "reason": "future_reason"}
                ]
            }
        })));

        assert!(report.contains("Tombstone:  no"), "got: {report}");
        assert!(report.contains("Store:      ec_store (generation 7)"));
        assert!(report.contains("Auction EIDs (1):\n  live.example: uid-live (atype 3)"));
        assert!(
            report.contains("off.example: bidstream_disabled (the partner has bidstream_enabled"),
            "should explain a known reason, got: {report}"
        );
        assert!(
            report.contains("new.example: future_reason\n"),
            "should print an unknown reason verbatim, got: {report}"
        );
    }

    #[test]
    fn a_tombstone_and_a_corrupt_entry_are_spelled_out() {
        let tombstone = render(&lookup(serde_json::json!({
            "ec_id": "id", "store": "s", "generation": 1, "tombstone": true,
            "auction": {"eids": [], "skipped": []}
        })));
        assert!(tombstone.contains("Tombstone:  yes (consent withdrawn"));

        let corrupt = render(&lookup(serde_json::json!({
            "ec_id": "id", "store": "s", "generation": 1,
            "entry_error": "failed to parse entry JSON", "raw_body": "{"
        })));
        assert!(corrupt.contains("Tombstone:  unknown"), "got: {corrupt}");
        assert!(corrupt.contains("Entry error: failed to parse entry JSON"));
        assert!(
            corrupt.contains("Auction preview: none"),
            "should say no EIDs would be attached, got: {corrupt}"
        );
    }
}
