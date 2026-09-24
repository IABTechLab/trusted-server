//! Tests for `ts origin probe-shareability` against a local fixture origin.
//!
//! Run with:
//! `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target <host-triple>`
//! or `./scripts/test-cli.sh`.

mod support_origin;

use std::io::Write as _;

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
fn a_bot_varying_origin_fails_the_bot_axis() {
    // Bots are excluded from the ad stack but not from shareability, so a crawler or
    // challenge document an origin serves without Vary can be stored and then handed to a
    // human navigation.
    let server = FixtureServer::start(|request| {
        let bot = request
            .header("user-agent")
            .is_some_and(|agent| agent.contains("Googlebot"));
        FixtureResponse::html(if bot {
            "<html>crawler document</html>"
        } else {
            "<html>reader document</html>"
        })
        .with_header("cache-control", "public, max-age=300")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok, "a crawler-specific document must not be cross-served");
    assert!(!axis(&report, "bot").passed());
    assert!(
        !verdict(&report, "vary-coverage").passed,
        "the undeclared signal is what gets cross-served"
    );
}

#[test]
fn a_prefetch_varying_origin_fails_the_prefetch_axis() {
    let server = FixtureServer::start(|request| {
        let prefetch = request
            .header("sec-purpose")
            .is_some_and(|purpose| purpose.contains("prefetch"));
        FixtureResponse::html(if prefetch {
            "<html>prefetch shell</html>"
        } else {
            "<html>reader document</html>"
        })
        .with_header("cache-control", "public, max-age=300")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok, "a prefetch-specific document must not be cross-served");
    assert!(!axis(&report, "prefetch").passed());
}

#[test]
fn a_declared_vary_still_excuses_the_bot_axis() {
    // The axis is named for the classification; the cache keys on the header it varied.
    let server = FixtureServer::start(|request| {
        let bot = request
            .header("user-agent")
            .is_some_and(|agent| agent.contains("Googlebot"));
        FixtureResponse::html(if bot {
            "<html>crawler document</html>"
        } else {
            "<html>reader document</html>"
        })
        .with_header("cache-control", "public, max-age=300")
        .with_header("vary", "User-Agent")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        ok,
        "an origin that declares User-Agent is keyed on it: {}",
        report.render_text()
    );
}

#[test]
fn a_stable_body_with_a_varying_policy_header_is_not_shareable() {
    // Fastly stores response headers with the body. A per-audience CSP is cross-served
    // exactly as a per-audience document would be: the weaker policy removes a browser
    // protection for readers the origin meant to protect.
    let server = FixtureServer::start(|request| {
        let mobile = request
            .header("user-agent")
            .is_some_and(|agent| agent.contains("iPhone"));
        FixtureResponse::html("<html>one document for everyone</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header(
                "content-security-policy",
                if mobile {
                    "default-src *"
                } else {
                    "default-src 'self'"
                },
            )
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        !ok,
        "identical HTML is not identical cached representations"
    );
    assert!(
        !axis(&report, "user-agent").passed(),
        "the axis compares what the cache stores, not just the body"
    );
}

#[test]
fn a_plain_http_url_is_refused_before_any_cookie_is_sent() {
    // The probe carries publisher session cookies. Sending them to a non-loopback origin
    // over HTTP puts them on the wire in the clear.
    let mut args = json_args(&FixtureServer::start(shareable("<html>stable</html>")));
    args.url = vec!["http://origin.example.com/article".to_owned()];

    let mut out = Vec::new();
    let outcome = run(OriginCommand::ProbeShareability(args), &mut out);

    let error = outcome.expect_err("plain HTTP must be refused");
    assert!(
        error.contains("HTTPS"),
        "the error must name the requirement, got: {error}"
    );
}

#[test]
fn a_url_carrying_credentials_is_refused() {
    let mut args = json_args(&FixtureServer::start(shareable("<html>stable</html>")));
    args.url = vec!["https://reader:example-password@origin.example.com/article".to_owned()];

    let mut out = Vec::new();
    let outcome = run(OriginCommand::ProbeShareability(args), &mut out);

    assert!(
        outcome.is_err(),
        "userinfo reaches proxy logs and shell history"
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
fn an_age_of_zero_cannot_establish_origin_shareability() {
    // Fresh cache hits can mask origin personalization within the first second.
    let server = FixtureServer::start(|_request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("age", "0")
    });
    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok, "should reject a fresh cached response");
    assert!(!verdict(&report, "fronting-cache").passed);
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
        message.contains("403") && message.contains("TRUSTED_SERVER_PROBE_ADMISSION_COOKIE"),
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
        !ok,
        "should keep admission-only diagnostics from passing the gate"
    );
    assert!(
        verdict(&report, "status").passed,
        "should reach real content"
    );
    assert!(!verdict(&report, "cookieless-coverage").passed);
    assert!(report.render_text().contains("cookieless"));
}

#[test]
fn decoded_encoding_differences_fail_even_when_vary_declares_encoding() {
    let server = FixtureServer::start(|request| {
        let mut response = FixtureResponse::html("<html>identity document</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("vary", "Accept-Encoding");
        if request.header("accept-encoding") == Some("gzip") {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(b"<html>different document</html>")
                .expect("should gzip the variant");
            response = response
                .with_header("content-encoding", "gzip")
                .with_body(encoder.finish().expect("should finish gzip"));
        }
        response
    });

    let (ok, report) = probe(&server, json_args(&server));

    assert!(
        !ok,
        "should reject different decoded documents for the template cache"
    );
    assert!(
        !axis(&report, "accept-encoding").passed(),
        "should keep encoding differences blocking"
    );
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

#[test]
fn every_sample_is_checked_for_unsafe_headers() {
    for (header, value, expected_verdict) in [
        ("x-cache", "HIT", "fronting-cache"),
        ("age", "0", "fronting-cache"),
        ("age", "invalid", "fronting-cache"),
        ("set-cookie", "session=example-session", "set-cookie"),
        ("cache-control", "private", "freshness"),
        (
            "content-security-policy",
            "script-src 'nonce-example'",
            "csp-nonce",
        ),
    ] {
        for on_repeat in [true, false] {
            let server = FixtureServer::start(move |request| {
                let response = FixtureResponse::html("<html>stable</html>")
                    .with_header("cache-control", "public, max-age=300");
                let unsafe_sample = if on_repeat {
                    request.request_index == 1
                } else {
                    request.header("accept-encoding") == Some("gzip")
                };
                if unsafe_sample {
                    response.with_header(header, value)
                } else {
                    response
                }
            });

            let (ok, report) = probe(&server, json_args(&server));

            assert!(
                !ok,
                "should reject {header} on a later sample (repeat={on_repeat})"
            );
            assert!(
                !verdict(&report, expected_verdict).passed,
                "should report the unsafe header"
            );
        }
    }
}

#[test]
fn a_non_success_variant_cannot_pass_with_an_identical_body() {
    let server = FixtureServer::start(|request| {
        FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_status(if request.header("rsc").is_some() {
                403
            } else {
                200
            })
    });
    let mut out = Vec::new();

    let outcome = run(
        OriginCommand::ProbeShareability(json_args(&server)),
        &mut out,
    );

    assert!(
        outcome.is_err(),
        "should reject a non-200 variant even when its body matches"
    );
}

#[test]
fn configured_headers_are_not_excused_by_vary_rsc() {
    let server = FixtureServer::start(|request| {
        FixtureResponse::html(if request.header("x-layout").is_some() {
            "<html>alternate</html>"
        } else {
            "<html>default</html>"
        })
        .with_header("cache-control", "public, max-age=300")
        .with_header("vary", "rsc")
    });
    let mut args = json_args(&server);
    args.vary_header = vec!["x-layout".to_owned()];

    let (ok, report) = probe(&server, args);

    assert!(
        !ok,
        "should reject an undeclared custom signal even with Vary: rsc"
    );
    assert!(
        !axis(&report, "x-layout").passed(),
        "should identify the actual varying header"
    );
}

#[test]
fn a_variant_must_declare_its_own_vary_coverage() {
    let server = FixtureServer::start(|request| {
        if request.header("rsc").is_some() {
            FixtureResponse::html("<html>flight</html>")
                .with_header("cache-control", "public, max-age=300")
        } else {
            FixtureResponse::html("<html>document</html>")
                .with_header("cache-control", "public, max-age=300")
                .with_header("vary", "rsc")
        }
    });

    let (ok, _) = probe(&server, json_args(&server));

    assert!(!ok, "should require Vary coverage on both representations");
}

#[test]
fn the_mobile_axis_reaches_a_recognizable_mobile_browser_variant() {
    let server = FixtureServer::start(|request| {
        let mobile = request
            .header("user-agent")
            .is_some_and(|agent| agent.contains("iPhone") || agent.contains("Android"));
        FixtureResponse::html(if mobile {
            "<html>mobile</html>"
        } else {
            "<html>desktop</html>"
        })
        .with_header("cache-control", "public, max-age=300")
    });

    let (ok, report) = probe(&server, json_args(&server));

    assert!(!ok, "should discover undeclared mobile document variation");
    assert!(
        !axis(&report, "user-agent").passed(),
        "should test a recognizable mobile browser"
    );
}

#[test]
fn declared_custom_signals_are_probed_independently_without_duplicate_axes() {
    let server = FixtureServer::start(|request| {
        FixtureResponse::html(format!(
            "<html>rsc={} layout={}</html>",
            request.header("rsc").unwrap_or("absent"),
            request.header("x-layout").unwrap_or("absent")
        ))
        .with_header("cache-control", "public, max-age=300")
        .with_header("vary", "rsc, x-layout")
    });
    let mut args = json_args(&server);
    args.vary_header = [
        "X-Layout",
        "x-layout",
        "RSC",
        "Cookie",
        "Accept-Encoding",
        "User-Agent",
    ]
    .map(str::to_owned)
    .to_vec();

    let (ok, report) = probe(&server, args);

    assert!(
        ok,
        "should accept independently declared signals: {}",
        report.render_text()
    );
    assert!(
        axis(&report, "rsc").differs(),
        "should vary RSC independently"
    );
    assert!(
        axis(&report, "x-layout").differs(),
        "should vary the configured signal independently"
    );
    assert_eq!(
        server.request_count(),
        11,
        "should sample each signal independently and the configured header with RSC"
    );
}

#[test]
fn unsafe_headers_are_still_checked_after_self_identity_first_differs() {
    let server = FixtureServer::start(|request| {
        let response = FixtureResponse::html(format!("<html>{}</html>", request.request_index))
            .with_header("cache-control", "public, max-age=300");
        if request.request_index == 2 {
            response.with_header("set-cookie", "session=example-session")
        } else {
            response
        }
    });
    let mut args = json_args(&server);
    args.repeat = 3;

    let (ok, report) = probe(&server, args);

    assert!(!ok, "should reject unstable responses");
    assert!(
        !axis(&report, "self-identity").passed(),
        "should retain the first body difference"
    );
    assert!(
        !verdict(&report, "set-cookie").passed,
        "should inspect headers after the first mismatch"
    );
    assert_eq!(
        server.request_count(),
        11,
        "should complete every requested sample"
    );
}

#[test]
fn configured_headers_are_also_compared_with_rsc_held_constant() {
    let server = FixtureServer::start(|request| {
        let variant = request.header("rsc").is_some() && request.header("x-layout").is_some();
        FixtureResponse::html(if variant {
            "<html>alternate flight</html>"
        } else {
            "<html>default</html>"
        })
        .with_header("cache-control", "public, max-age=300")
        .with_header("vary", "rsc")
    });
    let mut args = json_args(&server);
    args.vary_header = vec!["x-layout".to_owned()];

    let (ok, report) = probe(&server, args);

    assert!(
        !ok,
        "should discover undeclared variation within an RSC representation"
    );
    assert!(
        !axis(&report, "x-layout").passed(),
        "should attribute the difference to the configured header"
    );
}

#[test]
fn admission_cookie_cannot_hide_first_visitor_session_issuance() {
    let server = FixtureServer::start(|request| {
        let response = FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300");
        if request.header("cookie").is_none() {
            response.with_header("set-cookie", "session=example-session")
        } else {
            response
        }
    });
    let (ok, report) = probe(&server, json_args(&server));
    assert!(!ok, "should reject first-visitor session issuance");
    assert!(!verdict(&report, "set-cookie").passed);

    let mut args = json_args(&server);
    args.admission_cookie = Some("session=existing-reader".to_owned());
    let (ok, report) = probe(&server, args);
    assert!(!ok, "should not certify unobserved cookieless responses");
    assert!(!verdict(&report, "cookieless-coverage").passed);
    assert!(
        !report.passed(),
        "should fail the machine-readable gate too"
    );
}

#[test]
fn navigation_negotiation_cannot_hide_session_cookies_behind_json() {
    let server = FixtureServer::start(|request| {
        let navigation = request
            .header("accept")
            .is_some_and(|value| value.contains("text/html"))
            && request.header("sec-fetch-mode") == Some("navigate")
            && request.header("sec-fetch-dest") == Some("document");
        let response = FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("vary", "Accept");
        if navigation {
            response.with_header("set-cookie", "session=example; Path=/")
        } else {
            response
                .without_header("content-type")
                .with_header("content-type", "application/json")
                .with_body(serde_json::json!({"stable": true}).to_string())
        }
    });
    let (ok, report) = probe(&server, json_args(&server));
    assert!(!ok, "should reject a navigation that sets session cookies");
    assert!(
        !verdict(&report, "set-cookie").passed,
        "should inspect HTML navigation cookies"
    );
}

#[test]
fn non_html_navigation_responses_cannot_be_certified() {
    for content_type in [
        None,
        Some("application/json"),
        Some("text/plain"),
        Some("text/html-invalid"),
    ] {
        let server = FixtureServer::start(move |_| {
            let response = FixtureResponse::html("stable")
                .with_header("cache-control", "public, max-age=300")
                .without_header("content-type");
            match content_type {
                Some(value) => response.with_header("content-type", value),
                None => response,
            }
        });
        let (ok, report) = probe(&server, json_args(&server));
        assert!(
            !ok,
            "should reject unexpected navigation representation {content_type:?}"
        );
        assert!(
            !verdict(&report, "content-type").passed,
            "should report representation failure"
        );
    }
}

#[test]
fn revalidation_directives_override_positive_freshness() {
    for (name, value) in [
        ("cache-control", "public, max-age=300, no-cache"),
        ("cache-control", "No-Cache=\"Set-Cookie\""),
        ("surrogate-control", "max-age=300, no-cache"),
        ("pragma", "no-cache"),
    ] {
        let server = FixtureServer::start(move |_| {
            FixtureResponse::html("<html>stable</html>")
                .with_header("cache-control", "public, max-age=300")
                .with_header(name, value)
        });
        let (ok, report) = probe(&server, json_args(&server));
        assert!(!ok, "should reject required revalidation from {name}");
        assert!(
            !verdict(&report, "freshness").passed,
            "should refuse freshness despite positive max-age"
        );
    }
}

#[test]
fn wildcard_vary_refuses_both_stable_and_varying_responses() {
    for varying in [false, true] {
        let server = FixtureServer::start(move |request| {
            let body = if varying {
                request.header("user-agent").unwrap_or("none")
            } else {
                "stable"
            };
            FixtureResponse::html(format!("<html>{body}</html>"))
                .with_header("cache-control", "public, max-age=300")
                .with_header("vary", "*")
        });
        let (ok, report) = probe(&server, json_args(&server));
        assert!(!ok, "should refuse Vary wildcard even with stable bytes");
        assert!(
            !verdict(&report, "vary-coverage").passed,
            "should explain wildcard refusal"
        );
        assert!(
            !axis(&report, "user-agent").covered_by_vary,
            "should not treat wildcard as axis coverage"
        );
    }
}

#[test]
fn undecodable_safety_headers_fail_closed_on_every_sample() {
    for name in [
        "set-cookie",
        "cache-control",
        "surrogate-control",
        "pragma",
        "vary",
        "content-security-policy",
        "age",
        "content-type",
        "x-cache",
    ] {
        for sample in [0, 1, 3] {
            let server = FixtureServer::start(move |request| {
                let response = FixtureResponse::html("<html>stable</html>")
                    .with_header("cache-control", "public, max-age=300");
                if request.request_index == sample {
                    response.with_raw_header(name, b"session=example; extension=caf\xe9")
                } else {
                    response
                }
            });
            let (ok, report) = probe(&server, json_args(&server));
            assert!(
                !ok,
                "should refuse undecodable {name} on sample {sample}: {}",
                report.render_text()
            );
            if name == "set-cookie" {
                assert!(
                    !verdict(&report, "set-cookie").passed,
                    "should preserve cookie presence"
                );
            }
        }
    }
}

#[test]
fn rsc_uses_a_fetch_profile_and_accepts_flight_with_declared_rsc_variation() {
    let server = FixtureServer::start(|request| {
        let is_fetch = request.header("sec-fetch-mode") == Some("cors");
        for name in [
            "accept",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "sec-fetch-site",
        ] {
            assert_eq!(request.header_count(name), 1, "should send one {name}");
        }
        if is_fetch {
            assert_eq!(
                request.header("accept"),
                Some("*/*"),
                "should request a browser fetch representation"
            );
            assert_eq!(
                request.header("sec-fetch-dest"),
                Some("empty"),
                "should use the fetch destination"
            );
            assert_eq!(
                request.header("sec-fetch-site"),
                Some("same-origin"),
                "should describe the same-origin fetch"
            );
            assert!(
                request.header("sec-fetch-user").is_none(),
                "should not label fetches as user navigations"
            );
        } else {
            assert_eq!(
                request.header("sec-fetch-mode"),
                Some("navigate"),
                "should use navigation metadata"
            );
            assert_eq!(
                request.header("sec-fetch-user"),
                Some("?1"),
                "should represent user navigation"
            );
        }
        let response = FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300")
            .with_header("vary", "rsc");
        if request.header("rsc").is_some() {
            assert!(
                is_fetch,
                "should hold the fetch profile for every RSC variant"
            );
            response
                .without_header("content-type")
                .with_header("content-type", "text/x-component")
                .with_body("0:example-flight")
        } else {
            response
        }
    });
    let mut args = json_args(&server);
    args.vary_header = vec!["x-layout".to_owned()];
    let (ok, report) = probe(&server, args);
    assert!(
        ok,
        "should allow independently covered RSC variation: {}",
        report.render_text()
    );
}

#[test]
fn vary_rsc_cannot_excuse_an_accept_negotiated_difference() {
    let server = FixtureServer::start(|request| {
        let body = if request.header("accept") == Some("*/*") {
            "fetch"
        } else {
            "navigation"
        };
        FixtureResponse::html(format!("<html>{body}</html>"))
            .with_header("cache-control", "public, max-age=300")
            .with_header("vary", "rsc")
    });
    let (ok, report) = probe(&server, json_args(&server));
    assert!(!ok, "should not attribute Accept variation to RSC");
    assert!(
        !axis(&report, "fetch-profile").passed(),
        "should retain the independently varied profile"
    );
    assert!(
        !axis(&report, "rsc").differs(),
        "should hold the fetch profile constant while toggling RSC"
    );
}

#[test]
fn plain_fetch_cannot_claim_the_flight_exemption() {
    let server = FixtureServer::start(|request| {
        let response = FixtureResponse::html("<html>stable</html>")
            .with_header("cache-control", "public, max-age=300");
        if request.header("sec-fetch-mode") == Some("cors") {
            response
                .without_header("content-type")
                .with_header("content-type", "text/x-component")
        } else {
            response
        }
    });
    let (_, report) = probe(&server, json_args(&server));
    assert!(
        !verdict(&report, "content-type").passed,
        "should refuse flight without an RSC request"
    );
}

#[test]
fn legacy_prefetch_variation_is_exercised() {
    let server = FixtureServer::start(|request| {
        FixtureResponse::html(if request.header("purpose") == Some("prefetch") {
            "<html>prefetch</html>"
        } else {
            "<html>navigation</html>"
        })
        .with_header("cache-control", "public, max-age=300")
    });
    let (_, report) = probe(&server, json_args(&server));
    assert!(
        !axis(&report, "prefetch").passed(),
        "should detect legacy prefetch variation"
    );
}
