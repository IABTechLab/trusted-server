//! Tests for `ts origin probe-shareability` against a local fixture origin.
//!
//! Run with:
//! `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target <host-triple>`
//! or `./scripts/test-cli.sh`.

mod support_origin;

use support_origin::{FixtureResponse, FixtureServer};

fn fetch(url: &str) -> String {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build a Tokio runtime for the fixture fetch");
    runtime.block_on(async {
        reqwest::get(url)
            .await
            .expect("should reach the fixture origin")
            .text()
            .await
            .expect("should read the fixture body")
    })
}

#[test]
fn fixture_server_answers_repeated_requests() {
    let server = FixtureServer::start(|_request| FixtureResponse::html("<html></html>"));

    for attempt in 0..3 {
        let body = fetch(&server.url("/"));
        assert_eq!(
            body, "<html></html>",
            "every request must be answered, not just the first (attempt {attempt})"
        );
    }

    assert_eq!(
        server.request_count(),
        3,
        "the fixture should have counted every request it served"
    );
}

#[test]
fn fixture_server_can_vary_its_answer_per_request() {
    // The self-identity axis needs an origin that is *not* stable against itself, so the
    // fixture has to be able to differ across requests on demand.
    let server = FixtureServer::start(|request| {
        FixtureResponse::html(format!("<html>{}</html>", request.request_index))
    });

    assert_ne!(
        fetch(&server.url("/")),
        fetch(&server.url("/")),
        "a fixture that cannot vary per request cannot exercise the self-identity axis"
    );
}

#[test]
fn fixture_server_sees_request_headers_and_cookies() {
    let server = FixtureServer::start(|request| {
        let ec = if request.has_cookie("ts-ec") {
            "with-ec"
        } else {
            "no-ec"
        };
        let agent = request.header("user-agent").unwrap_or("none").to_owned();
        FixtureResponse::html(format!("<html>{ec}|{agent}</html>"))
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build a Tokio runtime");
    let body = runtime.block_on(async {
        reqwest::Client::new()
            .get(server.url("/"))
            .header("cookie", "ts-ec=abc; other=1")
            .header("user-agent", "FictionalBrowser/1.0")
            .send()
            .await
            .expect("should reach the fixture origin")
            .text()
            .await
            .expect("should read the fixture body")
    });

    assert_eq!(
        body, "<html>with-ec|FictionalBrowser/1.0</html>",
        "the cookie and user-agent axes both depend on the fixture seeing request headers"
    );
}

// ---------------------------------------------------------------------------
// The probe itself, driven against the fixture origin.
// ---------------------------------------------------------------------------

use trusted_server_cli::commands::origin::{
    OriginCommand, ProbeShareabilityArgs, report::ProbeReport, run,
};

/// A fixture that is shareable on every axis and verdict.
fn shareable(body: &'static str) -> impl Fn(&support_origin::FixtureRequest) -> FixtureResponse {
    move |_request| FixtureResponse::html(body).with_header("cache-control", "public, max-age=300")
}

fn probe(server: &FixtureServer, args: ProbeShareabilityArgs) -> (bool, ProbeReport) {
    let _ = server;
    let mut out = Vec::new();
    let outcome = run(OriginCommand::ProbeShareability(args), &mut out);
    let rendered = String::from_utf8(out).expect("probe output should be UTF-8");
    let report: ProbeReport =
        serde_json::from_str(rendered.trim()).expect("probe should emit parseable JSON");
    (outcome.is_ok(), report)
}

fn json_args(server: &FixtureServer) -> ProbeShareabilityArgs {
    ProbeShareabilityArgs {
        url: vec![server.url("/article")],
        repeat: 1,
        cookie: Vec::new(),
        vary_header: Vec::new(),
        admission_cookie: None,
        json: true,
    }
}

fn axis<'a>(
    report: &'a ProbeReport,
    name: &str,
) -> &'a trusted_server_cli::commands::origin::report::AxisResult {
    let found = report.urls[0].axes.iter().find(|axis| axis.name == name);
    assert!(found.is_some(), "report should contain the {name} axis");
    found.expect("should be present, asserted above")
}

fn verdict<'a>(
    report: &'a ProbeReport,
    name: &str,
) -> &'a trusted_server_cli::commands::origin::report::VerdictResult {
    let found = report.urls[0]
        .verdicts
        .iter()
        .find(|verdict| verdict.name == name);
    assert!(found.is_some(), "report should contain the {name} verdict");
    found.expect("should be present, asserted above")
}

#[test]
fn a_shareable_origin_passes_every_axis_and_verdict() {
    let server = FixtureServer::start(shareable("<html>stable</html>"));
    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        ok,
        "a stable, cookie-independent, freshness-declaring origin must pass: {}",
        report.render_text()
    );
    assert!(report.passed());
}

#[test]
fn an_unstable_origin_fails_self_identity_and_exits_non_zero() {
    let server = FixtureServer::start(|request| {
        // A per-request timestamp or CSRF nonce looks like this.
        FixtureResponse::html(format!("<html>{}</html>", request.request_index))
            .with_header("cache-control", "public, max-age=300")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok, "an unstable origin must exit non-zero");
    assert!(!axis(&report, "self-identity").passed());
}

#[test]
fn a_cookie_personalized_origin_fails_the_cookie_axis() {
    let server = FixtureServer::start(|request| {
        let body = if request.has_cookie("ts-ec") {
            "<html>signed in</html>"
        } else {
            "<html>anonymous</html>"
        };
        FixtureResponse::html(body).with_header("cache-control", "public, max-age=300")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok);
    assert!(!axis(&report, "cookie").passed());
    assert!(
        axis(&report, "self-identity").passed(),
        "cookie personalization must not be misreported as instability"
    );
}

#[test]
fn a_user_agent_varying_origin_fails_unless_it_declares_vary() {
    let undeclared = FixtureServer::start(|request| {
        let mobile = request
            .header("user-agent")
            .is_some_and(|agent| agent.contains("Phone"));
        FixtureResponse::html(if mobile {
            "<html>m</html>"
        } else {
            "<html>d</html>"
        })
        .with_header("cache-control", "public, max-age=300")
    });
    let (ok, report) = probe(&undeclared, json_args(&undeclared));
    assert!(!ok);
    assert!(!axis(&report, "user-agent").passed());
    assert!(
        !verdict(&report, "vary-coverage").passed,
        "an undeclared varying axis is exactly what gets cross-served"
    );

    let declared = FixtureServer::start(|request| {
        let mobile = request
            .header("user-agent")
            .is_some_and(|agent| agent.contains("Phone"));
        FixtureResponse::html(if mobile {
            "<html>m</html>"
        } else {
            "<html>d</html>"
        })
        .with_header("cache-control", "public, max-age=300")
        .with_header("vary", "User-Agent")
    });
    let (_, declared_report) = probe(&declared, json_args(&declared));
    assert!(
        verdict(&declared_report, "vary-coverage").passed,
        "a declared axis is keyed by the platform cache and is therefore safe"
    );
}

#[test]
fn an_rsc_varying_origin_fails_the_rsc_axis() {
    // RSC fetches already flow through the readthrough cache while HTML navigations are
    // passed, so this is the axis specific to removing the bypass.
    let server = FixtureServer::start(|request| {
        let body = if request.header("rsc").is_some() {
            "<html>flight payload</html>"
        } else {
            "<html>document</html>"
        };
        FixtureResponse::html(body).with_header("cache-control", "public, max-age=300")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok);
    assert!(!axis(&report, "rsc").passed());
}

#[test]
fn gzip_and_identity_are_compared_after_decoding() {
    use std::io::Write as _;

    let server = FixtureServer::start(|request| {
        let body = "<html>same document either way</html>";
        let wants_gzip = request
            .header("accept-encoding")
            .is_some_and(|value| value.contains("gzip"));
        if wants_gzip {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(body.as_bytes())
                .expect("should gzip the fixture body");
            let compressed = encoder.finish().expect("should finish gzipping");
            FixtureResponse::html("")
                .with_header("content-encoding", "gzip")
                .with_header("cache-control", "public, max-age=300")
                .with_body(compressed)
        } else {
            FixtureResponse::html(body).with_header("cache-control", "public, max-age=300")
        }
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        ok,
        "compressed and identity arms carry the same document, so this must pass: {}",
        report.render_text()
    );
    assert!(axis(&report, "accept-encoding").passed());
}

#[test]
fn an_origin_without_freshness_fails_its_verdict() {
    let server = FixtureServer::start(|_request| FixtureResponse::html("<html>stable</html>"));
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok);
    assert!(
        !verdict(&report, "freshness").passed,
        "readthrough would store this on a platform default where the template cache declines it"
    );
}

#[test]
fn a_private_origin_fails_freshness_even_with_a_max_age() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "private, max-age=300")
    });
    let (ok, _) = probe(&server, json_args(&server));
    assert!(
        !ok,
        "an origin that marks HTML private must not be declared shareable"
    );
}

#[test]
fn a_set_cookie_response_fails_its_verdict() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("set-cookie", "sid=abc123; Path=/")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok);
    assert!(
        !verdict(&report, "set-cookie").passed,
        "a cached Set-Cookie is replayed to every later cookieless reader"
    );
}

#[test]
fn a_csp_nonce_response_fails_its_verdict() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("content-security-policy", "script-src 'nonce-r4nd0m'")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok);
    assert!(!verdict(&report, "csp-nonce").passed);
}

#[test]
fn human_output_states_the_limits_and_the_verdict() {
    let server = FixtureServer::start(shareable("<html>stable</html>"));
    let mut args = json_args(&server);
    args.json = false;

    let mut out = Vec::new();
    let outcome = run(OriginCommand::ProbeShareability(args), &mut out);
    let rendered = String::from_utf8(out).expect("probe output should be UTF-8");

    assert!(outcome.is_ok());
    assert!(rendered.contains("VERDICT: shareable"));
    assert!(
        rendered.contains("one client address"),
        "IP-keyed personalization is invisible to this tool and the output must say so"
    );
}

#[test]
fn a_malformed_cookie_argument_is_rejected_before_any_fetch() {
    let server = FixtureServer::start(shareable("<html>stable</html>"));
    let mut args = json_args(&server);
    args.cookie = vec!["not-a-pair".to_owned()];

    let mut out = Vec::new();
    let outcome = run(OriginCommand::ProbeShareability(args), &mut out);

    assert!(outcome.is_err());
    assert_eq!(
        server.request_count(),
        0,
        "argument validation should happen before the origin is touched"
    );
}

#[test]
fn an_axis_override_replaces_the_default_header_rather_than_appending_to_it() {
    // `RequestBuilder::header` appends. If an arm's override is layered on top of the
    // default, the origin receives the header twice, and one that reads the first instance
    // never sees the override — the axis then compares two identical responses and passes
    // an origin it never actually varied.
    let server = FixtureServer::start(|request| {
        FixtureResponse::html(format!(
            "<html>ua={} ae={}</html>",
            request.header_count("user-agent"),
            request.header_count("accept-encoding")
        ))
        .with_header("cache-control", "public, max-age=300")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        ok,
        "every arm must send exactly one user-agent and one accept-encoding: {}",
        report.render_text()
    );
}

#[test]
fn a_later_private_directive_is_not_hidden_by_an_earlier_permissive_one() {
    // A proxy in front of the origin can append its own Cache-Control rather than
    // replacing the origin's. Judging only the first instance would store a private
    // response in a shared cache.
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("cache-control", "private")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok, "a private response must not be declared shareable");
    assert!(
        !verdict(&report, "freshness").passed,
        "the freshness verdict must read every Cache-Control instance, not just the first"
    );
}

#[test]
fn a_response_served_from_a_fronting_cache_is_not_declared_shareable() {
    // Every axis compares two fetches. A cache in front of the origin can answer both from
    // one object, so the axes agree and say nothing about the origin behind it.
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("age", "42")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok, "a cached answer is not evidence about the origin");
    assert!(!verdict(&report, "fronting-cache").passed);
    assert!(
        report.urls[0]
            .axes
            .iter()
            .all(trusted_server_cli::commands::origin::report::AxisResult::passed),
        "the axes agreeing is exactly the symptom, so the verdict must be what fails"
    );
}

#[test]
fn a_vendor_cache_hit_header_is_caught_even_without_an_age() {
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("x-cache", "HIT")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok);
    assert!(!verdict(&report, "fronting-cache").passed);
}

#[test]
fn an_age_of_zero_is_not_treated_as_a_cache_hit() {
    // A conforming cache on a miss sends `Age: 0`. Failing that would make the probe
    // unusable against any origin that reports age at all.
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("age", "0")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(ok, "{}", report.render_text());
    assert!(verdict(&report, "fronting-cache").passed);
}

#[test]
fn a_bot_wall_aborts_the_probe_instead_of_judging_the_challenge_page() {
    // Measured against a real protected origin: the baseline arm was answered with a
    // challenge page, and every verdict then described that page rather than the origin —
    // a confident FAIL on an origin that sends `max-age=60` with no `Set-Cookie`.
    let server = FixtureServer::start(|request| {
        if request
            .header("cookie")
            .is_some_and(|c| c.contains("admit=1"))
        {
            FixtureResponse::html("<html>real content</html>")
                .with_header("cache-control", "public, max-age=300")
        } else {
            FixtureResponse::html("<html>are you a robot</html>").with_status(403)
        }
    });

    let mut out = Vec::new();
    let outcome = run(
        OriginCommand::ProbeShareability(json_args(&server)),
        &mut out,
    );

    let error = outcome.expect_err("a challenge page must not be judged");
    let message = error.to_string();
    assert!(
        message.contains("403") && message.contains("admission-cookie"),
        "the error must name the status and how to get past it, got: {message}"
    );
}

#[test]
fn an_admission_cookie_lets_the_probe_reach_real_content() {
    let server = FixtureServer::start(|request| {
        if request
            .header("cookie")
            .is_some_and(|c| c.contains("admit=1"))
        {
            FixtureResponse::html("<html>real content</html>")
                .with_header("cache-control", "public, max-age=300")
        } else {
            FixtureResponse::html("<html>are you a robot</html>").with_status(403)
        }
    });

    let mut args = json_args(&server);
    args.admission_cookie = Some("admit=1".to_owned());
    let (ok, report) = probe(&server, args);

    assert!(
        ok,
        "with the wall passed, the origin's own headers decide: {}",
        report.render_text()
    );
}

#[test]
fn an_axis_the_origin_declares_in_vary_is_not_a_failure() {
    // Measured against a real origin: it declared `Vary: accept-encoding, rsc` and varied
    // on both, exactly as it should, and the probe failed it for doing so. A declared
    // signal is part of the cache key, so each value gets its own stored object.
    let server = FixtureServer::start(|request| {
        let gzip = request
            .header("accept-encoding")
            .is_some_and(|value| value.contains("gzip"));
        FixtureResponse::html(if gzip {
            "<html>compressed variant</html>"
        } else {
            "<html>identity variant</html>"
        })
        .with_header("cache-control", "public, max-age=300")
        .with_header("vary", "Accept-Encoding")
    });

    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        ok,
        "an origin that declares what it varies on must pass: {}",
        report.render_text()
    );
    let axis = axis(&report, "accept-encoding");
    assert!(axis.differs(), "the arms did differ");
    assert!(axis.passed(), "but the origin declared it, so it is keyed");
}

#[test]
fn an_undeclared_varying_axis_still_fails() {
    // The safety half: the same variance without the declaration is what gets cross-served.
    let server = FixtureServer::start(|request| {
        let gzip = request
            .header("accept-encoding")
            .is_some_and(|value| value.contains("gzip"));
        FixtureResponse::html(if gzip {
            "<html>compressed variant</html>"
        } else {
            "<html>identity variant</html>"
        })
        .with_header("cache-control", "public, max-age=300")
    });

    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok);
    assert!(!axis(&report, "accept-encoding").passed());
    assert!(!verdict(&report, "vary-coverage").passed);
}

#[test]
fn vary_cookie_does_not_excuse_the_cookie_axis() {
    // `Vary: Cookie` is keyed by a conforming cache, so it is not unsafe — but this axis
    // answers "does the origin ignore cookies", and the origin is saying it does not.
    // Passing would print a green verdict whose closing line says not to enable the flag,
    // and would contradict the template cache, which refuses `Vary: Cookie` at runtime.
    let server = FixtureServer::start(|request| {
        let signed_in = request.has_cookie("ts-ec");
        FixtureResponse::html(if signed_in {
            "<html>signed in</html>"
        } else {
            "<html>anonymous</html>"
        })
        .with_header("cache-control", "public, max-age=300")
        .with_header("vary", "Cookie")
    });

    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        !ok,
        "a cookie-varying origin must not read as cookie-independent"
    );
    assert!(!axis(&report, "cookie").passed());
}
