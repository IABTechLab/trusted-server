use std::env;
use std::error::Error;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use trusted_server_core::tsjs::{CreativeBootConfigV1, tsjs_bootstrap_fixture_fragment_v1};

type DynError = Box<dyn Error + Send + Sync + 'static>;
const PERFORMANCE_ORIGIN: &str = "https://performance.example";

#[derive(Debug, Eq, PartialEq)]
struct Args {
    ids: Vec<String>,
    projection: PathBuf,
}

fn main() -> Result<(), DynError> {
    let args = parse_args(env::args().skip(1))?;
    let html = run(&args)?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    output.write_all(html.as_bytes())?;
    Ok(())
}

fn run(args: &Args) -> Result<String, DynError> {
    let projection = fs::read_to_string(&args.projection).map_err(|error| {
        error_box(format!(
            "failed to read production projection '{}': {error}",
            args.projection.display()
        ))
    })?;
    let ids = args.ids.iter().map(String::as_str).collect::<Vec<_>>();
    let controller = tsjs_bootstrap_fixture_fragment_v1(
        &ids,
        &projection,
        CreativeBootConfigV1 {
            enabled: true,
            click_guard: ids.contains(&"creative"),
            render_guard: false,
        },
        true,
        false,
        PERFORMANCE_ORIGIN,
    )
    .map_err(|error| {
        error_box(format!(
            "failed to build production TSJS fixture: {error:?}"
        ))
    })?;
    Ok(format!(
        "<!doctype html><html><head>{controller}</head><body><div id=\"perf-slot\"></div></body></html>"
    ))
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, DynError> {
    let mut ids = None;
    let mut projection = None;
    let mut iter = args.into_iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--ids" => ids = Some(next_string_arg(&mut iter, "--ids")?),
            "--projection" => {
                projection = Some(PathBuf::from(next_string_arg(&mut iter, "--projection")?));
            }
            "--help" | "-h" => return Err(error_box(usage())),
            other => {
                return Err(error_box(format!(
                    "unknown argument '{other}'\n\n{}",
                    usage()
                )));
            }
        }
    }

    let ids = ids
        .ok_or_else(|| error_box(format!("missing --ids\n\n{}", usage())))?
        .split(',')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if ids.iter().any(String::is_empty) {
        return Err(error_box(
            "--ids must be one comma-separated non-empty list",
        ));
    }
    Ok(Args {
        ids,
        projection: projection
            .ok_or_else(|| error_box(format!("missing --projection\n\n{}", usage())))?,
    })
}

fn next_string_arg(
    iter: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> Result<String, DynError> {
    iter.next()
        .ok_or_else(|| error_box(format!("{flag} requires a value")))
}

fn usage() -> String {
    "usage: generate-tsjs-fixture --projection <path> --ids <comma-separated-integration-ids>"
        .to_string()
}

fn error_box(message: impl Into<String>) -> DynError {
    std::io::Error::other(message.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::NamedTempFile;

    const CANONICAL_PROJECTION: &str = r#"{
  "version": 1,
  "auction": {
    "version": 1,
    "auctionId": "performance-initial",
    "results": [{
      "slot": "perf-slot",
      "outcome": "winner",
      "candidateId": "AAAAAAAAAAAA"
    }]
  },
  "slots": [{
    "slot": "perf-slot",
    "gamUnitPath": "/123/performance",
    "divId": "perf-slot",
    "formats": [[300, 250]],
    "targeting": {}
  }],
  "bids": [{
    "candidateId": "AAAAAAAAAAAA",
    "slot": "perf-slot",
    "provider": "trusted",
    "upstreamBidId": "performance-upstream",
    "cpm": 1,
    "currency": "USD",
    "targeting": {"hb_bidder": "trusted"},
    "rendererReservationId": "r1_aaaaaaaaaaaaaaaaaaaaaa",
    "renderSource": {
      "type": "adm",
      "version": 1,
      "adm": "<main>fictional performance creative</main>",
      "width": 300,
      "height": 250
    }
  }]
}"#;

    fn bootstrap_transport(html: &str) -> serde_json::Value {
        let encoded = html
            .split_once("const __TSJS_SERVER_BOOT_TRANSPORT_V1__=")
            .expect("fixture should contain the sealed transport")
            .1;
        let decoded = serde_json::Deserializer::from_str(encoded)
            .into_iter::<String>()
            .next()
            .expect("transport should contain one JSON string")
            .expect("transport should be a JSON string literal");
        serde_json::from_str(&decoded).expect("decoded transport should be exact JSON")
    }

    #[test]
    fn fixture_uses_the_size_admitted_agent_before_the_post_paint_runtime() {
        let mut projection = NamedTempFile::new().expect("should create canonical projection");
        projection
            .write_all(CANONICAL_PROJECTION.as_bytes())
            .expect("should write canonical projection");
        let args = Args {
            ids: vec![
                "render_runtime".to_string(),
                "creative".to_string(),
                "gpt".to_string(),
                "diagnostics_presentation".to_string(),
                "gpt_later".to_string(),
            ],
            projection: projection.path().to_path_buf(),
        };
        let html = run(&args).expect("should serialize an production TSJS fixture");
        let transport = bootstrap_transport(&html);

        assert!(html.contains(r#"id="perf-slot""#));
        assert_eq!(
            transport["boot"]["creative"],
            json!({"version":1,"enabled":true,"clickGuard":true,"renderGuard":false})
        );
        assert_eq!(transport["boot"]["diagnostics"]["renderTraceOverlay"], true);
        assert_eq!(
            transport["boot"]["integrations"]["entries"][0],
            json!({"id":"gpt","config":{"gamAttributionEnabled":false,"pageBidsEnabled":true}})
        );
        assert_eq!(
            transport["boot"]["manifest"]["firstDisplay"]["slices"],
            json!([
                "first_display",
                "render_owner_initial",
                "creative_initial",
                "gpt_initial"
            ])
        );
        let first_display_src = transport["boot"]["manifest"]["firstDisplay"]["src"]
            .as_str()
            .expect("first-display source should be a string");
        assert!(first_display_src.starts_with("/static/tsjs=tsjs-first-display.min.js?m=008b&v="));
        let runtime_src = transport["boot"]["manifest"]["runtimeSrc"]
            .as_str()
            .expect("runtime source should be a string");
        assert!(runtime_src.starts_with("/static/tsjs=tsjs-unified.min.js?v="));
        assert_eq!(
            transport["boot"]["manifest"]["integrations"][3]["id"],
            "diagnostics_presentation"
        );
        assert_eq!(
            transport["boot"]["manifest"]["integrations"][4]["id"],
            "gpt_later"
        );
        assert_eq!(html.matches("<script").count(), 2);
        assert!(html.contains(r#"<script src="/static/tsjs=tsjs-first-display.min.js?m=008b&v="#));
        assert!(!html.contains(r#"<script src="/static/tsjs=tsjs-unified.min.js"#));
        assert!(!html.contains(r#"<script src="/static/tsjs=tsjs-gpt_later"#));
    }

    #[test]
    fn projection_and_ids_are_required() {
        assert!(parse_args(["--ids".to_string(), "gpt".to_string()]).is_err());
        assert!(parse_args(["--projection".to_string(), "fixture.json".to_string()]).is_err());
    }
}
