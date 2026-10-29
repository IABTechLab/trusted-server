//! Export fictional CWV pages through the production HTML transform and JS route.
//!
//! Run on the native host: `cwv_html OUTPUT [unreserved|reserved]`.

use core::error::Error;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use edgezero_core::body::Body as EdgeBody;
use http::Request;
use scraper::{Html, Selector};
use serde_json::{Value, json};
use trusted_server_core::cache_policy::EdgeCacheHeader;
use trusted_server_core::creative_opportunities::{
    AssemblyMode, CreativeOpportunityInitialization,
};
use trusted_server_core::html_processor::{
    BodyCloseInjection, HtmlProcessorConfig, create_html_processor,
};
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::publisher::{fixture_ad_scripts, handle_tsjs_dynamic};
use trusted_server_core::settings::Settings;
use trusted_server_core::streaming_processor::StreamProcessor as _;

type FixtureError = Box<dyn Error + Send + Sync>;

const CONFIG: &str = r#"
[[handlers]]
path = "^/_ts/admin"
username = "fixture-admin"
password = "fictional-cwv-admin-password"
[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
origin_url = "https://origin.example.com"
proxy_secret = "fictional-cwv-proxy-secret"
[ec]
passphrase = "fictional-cwv-secret-at-least-32-bytes"
ec_store = "ec_identity_store"
[request_signing]
enabled = false
config_store_id = "app_config"
secret_store_id = "secrets"
[integrations.gpt]
enabled = true
gam_attribution_enabled = false
rewrite_script = false
[creative_opportunities]
enabled = true
gam_network_id = "123456789"
[[creative_opportunities.slot]]
id = "fixture-header"
div_id = "fixture-ad"
page_patterns = ["/news/*"]
formats = [{ width = 300, height = 250 }, { width = 300, height = 600 }]
"#;

const RESERVATION: &str = r#"
[creative_opportunities.slot.reservation]
element_id = "fixture-ad-space"
min_height = 250
[[creative_opportunities.slot.reservation.breakpoints]]
min_width = 1024
min_height = 600
"#;

fn command(args: &[&str]) -> String {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn creative(height: u32) -> String {
    format!(
        r#"<!doctype html><html><body style="margin:0;width:300px;height:{height}px;background:#b6e3ff"><div class="marker">Server result</div><script>requestAnimationFrame(()=>top.postMessage({{type:'fixture-creative-ready',label:'Server result'}},'*'));</script></body></html>"#
    )
}

fn origin_page(absent: bool, replacement: bool, registered: bool) -> String {
    let slot = if absent {
        "<div id=\"fixture-ad\"></div>"
    } else {
        "<div id=\"fixture-ad-space\"><div id=\"fixture-ad\"></div></div>"
    };
    let replacement = if replacement {
        "document.getElementById('fixture-ad-space').innerHTML='<div id=\"fixture-ad\"></div>';"
    } else {
        ""
    };
    let registration = if registered {
        "googletag.cmd.push(function(){googletag.setConfig({disableInitialLoad:true});googletag.enableServices();var s=googletag.defineSlot('/123456789/fixture-header',[[300,250]],'fixture-ad').addService(googletag.pubads());s.setTargeting('publisher_key','keep');window.fixturePublisherSlot=s;window.fixtureHandle=tsjs.registerPublisherSlot({slotId:'fixture-header',gptSlot:s,navigationGeneration:tsjs.navGeneration||0,eligibleFormats:[[300,250]]});});"
    } else {
        ""
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Fictional CWV article</title><style>body{{margin:0;font:18px sans-serif}}h1{{font-size:24px;margin:12px}}main{{padding:12px}}#hero{{display:block;width:100%;height:180px}}iframe{{display:block;border:0}}#fixture-ad-space{{width:300px}}</style><script>window.fixtureEarly={{core:typeof tsjs.setConfig==='function',queued:false}};tsjs.setConfig({{debug:false}});tsjs.que.push(function(){{window.fixtureEarly.queued=true}});</script></head><body><h1>Fictional article</h1>{slot}<main id="article"><img id="hero" src="/hero.svg" width="800" height="180" alt="Fictional landscape"><h2>Readable article content</h2><p>This fixed fictional article makes ad-space movement visible.</p></main><script>{replacement}{registration}</script></body></html>"#
    )
}

fn main() -> Result<(), FixtureError> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.len() > 2 {
        return Err("usage: cwv_html OUTPUT [unreserved|reserved]".into());
    }
    let reserved = match args.get(1).map(String::as_str).unwrap_or("unreserved") {
        "unreserved" => false,
        "reserved" => true,
        _ => return Err("reservation must be unreserved or reserved".into()),
    };
    let out = Path::new(&args[0]);
    fs::create_dir_all(out.join("assets"))?;
    let config_source = format!("{CONFIG}{}", if reserved { RESERVATION } else { "" });
    let settings = Settings::from_toml(&config_source)?;
    let registry = IntegrationRegistry::with_plan(
        &settings,
        Arc::new(trusted_server_core::auction::compile_auction_plan(
            &settings,
        )?),
    )?;
    fs::write(out.join("app.toml"), config_source)?;
    let selector = Selector::parse("script[src]").expect("should parse a static script selector");
    let mut assets = BTreeMap::<String, String>::new();
    let mut documents = Vec::<Value>::new();
    for (name, path, shared, absent, replacement, height) in [
        ("inline", "/news/article", false, false, false, 250),
        ("csp", "/news/article", false, false, false, 250),
        ("shared", "/news/article", true, false, false, 250),
        ("registered", "/news/article", false, false, false, 250),
        ("tall", "/news/article", false, false, false, 600),
        ("unknown-height", "/news/article", false, false, false, 420),
        ("absent-wrapper", "/news/article", false, true, false, 250),
        ("unmatched", "/other/article", false, false, false, 250),
        (
            "child-replacement",
            "/news/article",
            false,
            false,
            true,
            250,
        ),
    ] {
        let mut bids = json!({"fixture-header": {
            "hb_adid":"fixture-server-ad", "hb_bidder":"fixtureServer", "hb_pb":"1.00",
            "w":300, "h":height, "adm":creative(height)
        }});
        if name == "unknown-height" {
            let bid = bids["fixture-header"]
                .as_object_mut()
                .expect("should construct a bid");
            bid.remove("w");
            bid.remove("h");
        }
        let mut page_settings = settings.clone();
        if name == "registered" {
            page_settings
                .creative_opportunities
                .as_mut()
                .expect("should configure slots")
                .slot[0]
                .initialization = CreativeOpportunityInitialization::PublisherRegistered;
        }
        let (slots, body_script) = fixture_ad_scripts(
            &page_settings,
            path,
            if shared {
                AssemblyMode::Esi
            } else {
                AssemblyMode::Inline
            },
            bids.as_object()
                .expect("should construct an object bid map"),
        );
        let config = HtmlProcessorConfig::from_settings(
            &page_settings,
            &registry,
            "origin.example.com",
            "publisher.example.com",
            "https",
        )
        .with_request_path(path)
        .with_enforced_csp(name == "csp")
        .with_ad_state(slots, Arc::new(Mutex::new(Some(body_script))))
        .with_body_close(BodyCloseInjection::InlineBids);
        let bytes = create_html_processor(config).process_chunk(
            origin_page(absent, replacement, name == "registered").as_bytes(),
            true,
        )?;
        let html = String::from_utf8(bytes)?;
        for script in Html::parse_document(&html).select(&selector) {
            let src = script
                .value()
                .attr("src")
                .expect("should select script src");
            if assets.contains_key(src) {
                continue;
            }
            let request = Request::builder().uri(src).body(EdgeBody::empty())?;
            let response = handle_tsjs_dynamic(&request, &registry, EdgeCacheHeader::None)?;
            if !response.status().is_success() {
                return Err(format!(
                    "production JS route did not serve {src}: {}",
                    response.status()
                )
                .into());
            }
            let body = response
                .into_body()
                .into_bytes()
                .ok_or("expected buffered JS body")?;
            let filename = format!("assets/{}.js", assets.len());
            fs::write(out.join(&filename), body)?;
            assets.insert(src.to_owned(), filename);
        }
        let filename = format!("{name}.html");
        fs::write(out.join(&filename), html)?;
        documents.push(json!({
            "name": name, "file": filename, "requestPath": path, "shared": shared,
            "absentWrapper": absent, "replacement": replacement, "creativeHeight": height,
            "matched": path.starts_with("/news/"), "registered": name == "registered",
            "enforcedCsp": name == "csp"
        }));
    }
    let source_files = [
        "Cargo.lock",
        "rust-toolchain.toml",
        ".tool-versions",
        "crates/trusted-server-core/src/html_processor.rs",
        "crates/trusted-server-core/src/publisher.rs",
        "crates/trusted-server-core/src/integrations/registry.rs",
        "crates/trusted-server-core/src/creative_opportunities.rs",
        "crates/trusted-server-core/src/tsjs.rs",
        "crates/trusted-server-js/lib/build-module-options.mjs",
        "crates/trusted-server-js/lib/build-all.mjs",
        "crates/trusted-server-core/src/integrations/gpt_bootstrap.js",
        "crates/trusted-server-integration-tests/examples/cwv_html.rs",
    ];
    let manifest = json!({
        "schemaVersion": 1, "reserved": reserved,
        "sourceRevision": env::var("CWV_SOURCE_REVISION").unwrap_or_else(|_| command(&["git", "rev-parse", "HEAD"])),
        "sourceStatus": env::var("CWV_SOURCE_STATUS").unwrap_or_else(|_| command(&["git", "status", "--short"])),
        "sourceMetadataOverrides": {
            "revision": env::var("CWV_SOURCE_REVISION").ok(),
            "status": env::var("CWV_SOURCE_STATUS").ok(),
            "origin": "CWV_SOURCE_REVISION/CWV_SOURCE_STATUS if present, otherwise current git checkout"
        },
        "jsSourceFiles": command(&["find", "crates/trusted-server-js/lib/src", "-type", "f"]),
        "sourceFingerprints": command(&["sha256sum"].into_iter().chain(source_files).collect::<Vec<_>>()),
        "rustc": command(&["rustc", "-Vv"]),
        "exporterSource": include_str!("cwv_html.rs"),
        "assets": assets, "documents": documents,
        "creativeInlineScripts": Html::parse_document(&creative(250)).select(&Selector::parse("script").expect("should parse script selector")).map(|script| script.inner_html()).collect::<Vec<_>>(),
        "boundary": "Real Rust HTML transform, registry, payload builders and JS serving. Fictional bid input and simulated GPT transport; no auction execution or real GAM."
    });
    fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}
