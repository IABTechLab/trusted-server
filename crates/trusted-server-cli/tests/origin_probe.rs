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
