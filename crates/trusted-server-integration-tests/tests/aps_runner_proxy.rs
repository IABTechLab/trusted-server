#![allow(dead_code, unused_imports)]

mod common;
mod environments;

use common::aps_runner_upstream::{ApsRunnerUpstream, FictionalResponse, ResponseWrite};
use common::runtime::{RuntimeEnvironment, wasm_binary_path};
use environments::spin::SpinRuntime;
use environments::{axum::AxumDevServer, cloudflare::CloudflareWorkers, fastly::FastlyViceroy};
use reqwest::blocking::{Client, Response};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use trusted_server_core::integrations::aps::{
    APS_RUNNER_MAX_RESPONSE_BYTES, APS_RUNNER_ROUTE, APS_RUNNER_UPSTREAM_URL,
};

const SUCCESS_HEADERS: [&str; 5] = [
    "access-control-allow-origin",
    "content-type",
    "cross-origin-resource-policy",
    "referrer-policy",
    "x-content-type-options",
];

// The platform policy owns an exact five-second dispatch-through-final-byte
// deadline. This black-box clock additionally observes downstream request
// dispatch, local error serialization, and response delivery, so retain a
// bounded allowance for work outside the transport window.
const DOWNSTREAM_DEADLINE_OBSERVATION_ALLOWANCE: Duration = Duration::from_millis(250);
const RENDERER_V2_PATH: &str = "/integrations/aps/renderer/v2";
const CLOUDFLARE_READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const RENDERER_V2_CSP: &str = "default-src 'none'; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' http: https:; connect-src http: https:; frame-src data: https:; img-src data: blob: http: https:; media-src blob: http: https:; style-src 'unsafe-inline' http: https:; font-src data: http: https:; worker-src blob: http: https:; frame-ancestors 'self'; form-action https:;";
const RENDERER_V2_DOCUMENT: &str = include_str!(
    "../../trusted-server-core/src/integrations/generated/aps_renderer_bootstrap_v2.html"
);

struct CorpusCase {
    name: &'static str,
    upstream: FictionalResponse,
    expected_status: u16,
    expected_body: Option<Vec<u8>>,
    maximum_elapsed: Option<Duration>,
}

impl CorpusCase {
    fn success(name: &'static str, upstream: FictionalResponse, body: Vec<u8>) -> Self {
        Self {
            name,
            upstream,
            expected_status: 200,
            expected_body: Some(body),
            maximum_elapsed: None,
        }
    }

    fn failure(name: &'static str, upstream: FictionalResponse) -> Self {
        Self {
            name,
            upstream,
            expected_status: 502,
            expected_body: Some(Vec::new()),
            maximum_elapsed: None,
        }
    }

    fn deadline(name: &'static str, upstream: FictionalResponse) -> Self {
        Self {
            maximum_elapsed: Some(
                Duration::from_secs(5) + DOWNSTREAM_DEADLINE_OBSERVATION_ALLOWANCE,
            ),
            ..Self::failure(name, upstream)
        }
    }
}

fn runtime_from_env() -> Box<dyn RuntimeEnvironment> {
    let runtime = std::env::var("APS_RUNNER_PROXY_RUNTIME")
        .expect("should select one APS runner-proxy adapter runtime");
    let environment: Option<Box<dyn RuntimeEnvironment>> = match runtime.as_str() {
        "axum" => Some(Box::new(AxumDevServer)),
        "fastly" => Some(Box::new(FastlyViceroy)),
        "cloudflare" => Some(Box::new(CloudflareWorkers)),
        "spin" => Some(Box::new(SpinRuntime)),
        _ => None,
    };
    environment.expect("should select a known APS runner-proxy adapter runtime")
}

fn fixed(status: &str, headers: &[(&str, &str)], body: impl AsRef<[u8]>) -> FictionalResponse {
    FictionalResponse::fixed(status, headers, body)
}

fn corpus(runtime_id: &str) -> Vec<CorpusCase> {
    let exact_body = b"/* fictional runner: \xCE\xBB */".to_vec();
    let exact_length = exact_body.len().to_string();
    let cap_body = vec![b'x'; APS_RUNNER_MAX_RESPONSE_BYTES];
    let cap_length = cap_body.len().to_string();
    let one_over = vec![b'y'; APS_RUNNER_MAX_RESPONSE_BYTES + 1];
    let over_declared = (APS_RUNNER_MAX_RESPONSE_BYTES + 1).to_string();
    let slow_headers = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/javascript\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    let mut slow_writes = vec![ResponseWrite::now(slow_headers)];
    for _ in 0..6 {
        slow_writes.push(ResponseWrite::after(
            Duration::from_millis(900),
            b"1\r\nx\r\n".to_vec(),
        ));
    }
    slow_writes.push(ResponseWrite::now(b"0\r\n\r\n".to_vec()));
    let near_deadline_body = vec![b'n'; 24];
    let near_deadline_headers = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\n\r\n",
        near_deadline_body.len()
    )
    .into_bytes();
    let mut near_deadline_writes = vec![ResponseWrite::now(near_deadline_headers)];
    for byte in &near_deadline_body {
        near_deadline_writes.push(ResponseWrite::after(
            Duration::from_millis(195),
            vec![*byte],
        ));
    }
    let over_total_headers = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/javascript\r\nContent-Length: 26\r\n\r\n".to_vec();
    let mut over_total_writes = vec![ResponseWrite::now(over_total_headers)];
    for _ in 0..26 {
        over_total_writes.push(ResponseWrite::after(Duration::from_millis(195), vec![b't']));
    }

    let mut cases = vec![
        CorpusCase::success(
            "byte-preserving JavaScript with identity evidence",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Encoding", "identity"),
                    ("Content-Length", &exact_length),
                    ("Set-Cookie", "must-not-reach-browser=1"),
                    ("X-Fictional-Upstream", "must-be-dropped"),
                ],
                &exact_body,
            ),
            exact_body,
        ),
        CorpusCase::success(
            "missing length and encoding",
            fixed(
                "200 OK",
                &[("Content-Type", "text/javascript; charset=UTF-8")],
                b"ok",
            ),
            b"ok".to_vec(),
        ),
        CorpusCase::failure(
            "non-200 status",
            fixed(
                "204 No Content",
                &[("Content-Type", "application/javascript")],
                [],
            ),
        ),
        CorpusCase::failure(
            "redirect is not followed",
            fixed(
                "302 Found",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Location", "https://example.invalid/runner.js"),
                ],
                b"redirect body",
            ),
        ),
        CorpusCase::failure(
            "missing content type",
            fixed("200 OK", &[], b"ok"),
        ),
        CorpusCase::failure(
            "duplicate content type",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Type", "text/javascript"),
                ],
                b"ok",
            ),
        ),
        CorpusCase::failure(
            "rejected content type",
            fixed("200 OK", &[("Content-Type", "text/plain")], b"ok"),
        ),
        CorpusCase::failure(
            "unknown content type parameter",
            fixed(
                "200 OK",
                &[("Content-Type", "application/javascript; version=1")],
                b"ok",
            ),
        ),
        CorpusCase::failure(
            "listed identity encoding",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Encoding", "identity, gzip"),
                ],
                b"ok",
            ),
        ),
        CorpusCase::failure(
            "non-identity encoding",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Encoding", "gzip"),
                ],
                [
                    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0xcf,
                    0x06, 0x00, 0x47, 0xdd, 0xdc, 0x79, 0x02, 0x00, 0x00, 0x00,
                ],
            ),
        ),
        CorpusCase::failure(
            "duplicate content length",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Length", "2"),
                    ("Content-Length", "2"),
                ],
                b"ok",
            ),
        ),
        CorpusCase::failure(
            "noncanonical content length",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Length", "02"),
                ],
                b"ok",
            ),
        ),
        CorpusCase::failure(
            "declared length mismatch",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Length", "3"),
                ],
                b"ok",
            ),
        ),
        CorpusCase::failure(
            "declared length over cap",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Length", &over_declared),
                ],
                [],
            ),
        ),
        CorpusCase::failure(
            "invalid UTF-8",
            fixed(
                "200 OK",
                &[("Content-Type", "application/javascript")],
                [0xff, 0xfe],
            ),
        ),
        CorpusCase::success(
            "exactly at the body cap",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Length", &cap_length),
                ],
                &cap_body,
            ),
            cap_body,
        ),
        CorpusCase::failure(
            "buffered body one byte over cap",
            fixed(
                "200 OK",
                &[("Content-Type", "application/javascript")],
                &one_over,
            ),
        ),
        CorpusCase::failure(
            "streamed body one byte over cap",
            FictionalResponse::chunked(
                "200 OK",
                &[("Content-Type", "application/javascript")],
                vec![(Duration::ZERO, one_over)],
            ),
        ),
        CorpusCase::success(
            "sub-250ms drip completing after 4.5 seconds preserves the full deadline",
            FictionalResponse::raw(near_deadline_writes),
            near_deadline_body,
        ),
        CorpusCase::deadline(
            "first-byte stall",
            FictionalResponse::raw(vec![ResponseWrite::after(
                Duration::from_millis(5_500),
                b"HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
            )]),
        ),
        CorpusCase::deadline(
            "mid-body stall after partial chunk",
            FictionalResponse::raw(vec![
                ResponseWrite::now(
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/javascript\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n".to_vec(),
                ),
                ResponseWrite::after(Duration::from_millis(5_500), b"0\r\n\r\n".to_vec()),
            ]),
        ),
        CorpusCase::deadline(
            "slow drip exceeds total deadline",
            FictionalResponse::raw(slow_writes),
        ),
        CorpusCase::deadline(
            "sub-250ms drip exceeds the true total deadline",
            FictionalResponse::raw(over_total_writes),
        ),
        CorpusCase::success(
            "late bytes cannot contaminate the next request",
            fixed(
                "200 OK",
                &[
                    ("Content-Type", "application/javascript"),
                    ("Content-Length", "4"),
                ],
                b"next",
            ),
            b"next".to_vec(),
        ),
    ];
    if runtime_id == "cloudflare" {
        cases.insert(
            cases.len() - 1,
            CorpusCase {
                maximum_elapsed: Some(
                    Duration::from_secs(4) + Duration::from_millis(750),
                ),
                ..CorpusCase::failure(
                    "Cloudflare first-byte timeout is distinct from the total timeout",
                    FictionalResponse::raw(vec![ResponseWrite::after(
                        Duration::from_millis(4_500),
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
                    )]),
                )
            },
        );
    }
    cases
}

fn assert_outbound_request(
    runtime_id: &str,
    case_name: &str,
    request: &common::aps_runner_upstream::ObservedRequest,
) {
    assert_eq!(
        request.request_line, "GET /prebid-creative.js HTTP/1.1",
        "{case_name}: fixed upstream path and method"
    );
    assert_eq!(
        request.header_values("accept-encoding"),
        vec!["identity"],
        "{case_name}: exact identity request; observed {request:?}"
    );
    assert_eq!(
        request.header_values("x-ts-aps-logical-url"),
        vec![APS_RUNNER_UPSTREAM_URL],
        "{case_name}: transport seam must attest the fixed logical URL"
    );
    if matches!(runtime_id, "axum" | "fastly") {
        assert_eq!(
            request.header_values("host"),
            vec!["client.aps.amazon-adsystem.com"],
            "{case_name}: transport must preserve the fixed logical APS authority"
        );
    } else {
        assert_eq!(
            request.header_values("host").len(),
            1,
            "{case_name}: runtime-owned transport authority must be singular"
        );
    }
    for forbidden in [
        "authorization",
        "cookie",
        "forwarded",
        "referer",
        "x-forwarded-for",
        "x-publisher-secret",
    ] {
        assert!(
            request.header_values(forbidden).is_empty(),
            "{case_name}: `{forbidden}` must not reach the fictional upstream"
        );
    }
}

fn assert_success(case_name: &str, response: &Response) {
    assert_eq!(
        response.headers()["content-type"],
        "application/javascript; charset=utf-8",
        "{case_name}"
    );
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "*",
        "{case_name}"
    );
    assert_eq!(
        response.headers()["cross-origin-resource-policy"],
        "cross-origin",
        "{case_name}"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(!response.headers().contains_key("set-cookie"));
    assert!(!response.headers().contains_key("x-fictional-upstream"));
    assert!(!response.headers().contains_key("x-geo-info-available"));
    let semantic_headers: BTreeSet<&str> = response
        .headers()
        .keys()
        .map(reqwest::header::HeaderName::as_str)
        .filter(|name| {
            !matches!(
                *name,
                "connection" | "content-length" | "date" | "server" | "transfer-encoding"
            )
        })
        .collect();
    assert_eq!(
        semantic_headers,
        BTreeSet::from(SUCCESS_HEADERS),
        "{case_name}: successful proxy application headers must be exact"
    );
}

fn exact_renderer_method_response(response: Response) -> bool {
    if response.status().as_u16() != 405
        || response.headers().get_all("allow").iter().count() != 1
        || response
            .headers()
            .get("allow")
            .is_none_or(|value| value != "GET")
        || response.headers().get_all("cache-control").iter().count() != 1
        || response
            .headers()
            .get("cache-control")
            .is_none_or(|value| value != "no-store")
    {
        return false;
    }
    let semantic_headers: BTreeSet<&str> = response
        .headers()
        .keys()
        .map(reqwest::header::HeaderName::as_str)
        .filter(|name| {
            !matches!(
                *name,
                "connection" | "content-length" | "date" | "server" | "transfer-encoding"
            )
        })
        .collect();
    semantic_headers == BTreeSet::from(["allow", "cache-control"])
        && response.bytes().is_ok_and(|body| body.is_empty())
}

fn assert_exact_local_failure(response: Response, status: u16, allow_get: bool) {
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(response.headers()["cache-control"], "no-store");
    if allow_get {
        assert_eq!(response.headers()["allow"], "GET");
    } else {
        assert!(!response.headers().contains_key("allow"));
    }
    let semantic_headers: BTreeSet<&str> = response
        .headers()
        .keys()
        .map(reqwest::header::HeaderName::as_str)
        .filter(|name| {
            !matches!(
                *name,
                "connection" | "content-length" | "date" | "server" | "transfer-encoding"
            )
        })
        .collect();
    let expected = if allow_get {
        BTreeSet::from(["allow", "cache-control"])
    } else {
        BTreeSet::from(["cache-control"])
    };
    assert_eq!(semantic_headers, expected);
    assert!(
        response
            .bytes()
            .expect("local failure body should be readable")
            .is_empty()
    );
}

fn wait_for_cloudflare_renderer_readiness(client: &Client, base_url: &str) {
    let deadline = Instant::now() + CLOUDFLARE_READINESS_TIMEOUT;
    let mut consecutive = 0_u8;
    while consecutive < 2 {
        let exact = client
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND should be valid"),
                format!("{base_url}{RENDERER_V2_PATH}"),
            )
            .send()
            .is_ok_and(exact_renderer_method_response);
        consecutive = if exact { consecutive + 1 } else { 0 };
        if consecutive == 2 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Cloudflare renderer route did not produce two consecutive exact readiness responses"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "requires a feature-gated adapter artifact and its local runtime"]
fn actual_adapter_proxy_corpus() {
    let _ = env_logger::try_init();
    let fixture = ApsRunnerUpstream::start().expect("should start fictional APS upstream");
    let runtime = runtime_from_env();
    let runtime_id = runtime.id();
    let process = runtime
        .spawn_aps_runner_proxy(&wasm_binary_path(), &fixture.endpoint_url())
        .expect("should spawn APS runner proxy artifact");
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // This is only a downstream dead-test guard. Deadline corpus cases
        // retain their independent five-second transport assertion plus the
        // bounded black-box observation allowance above.
        // Leave enough headroom for an 8 MiB boundary response through local
        // wasm runtimes on a loaded CI worker.
        .timeout(Duration::from_secs(30))
        .build()
        .expect("should build downstream client");

    if runtime_id == "cloudflare" {
        wait_for_cloudflare_renderer_readiness(&client, &process.base_url);
    }

    // Independent corpus assertion: readiness never substitutes for endpoint acceptance.
    let response = client
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND should be valid"),
            format!("{}{}", process.base_url, RENDERER_V2_PATH),
        )
        .header("authorization", "Bearer must-not-reach-publisher")
        .send()
        .expect("PROPFIND reserved request should complete");
    assert_exact_local_failure(response, 405, true);

    let renderer = client
        .get(format!("{}{}", process.base_url, RENDERER_V2_PATH))
        .send()
        .expect("renderer request should complete");
    assert_eq!(renderer.status().as_u16(), 200);
    assert_eq!(
        renderer.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert_eq!(
        renderer.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(renderer.headers()["x-content-type-options"], "nosniff");
    assert_eq!(renderer.headers()["referrer-policy"], "no-referrer");
    assert_eq!(
        renderer.headers()["content-security-policy"],
        RENDERER_V2_CSP
    );
    assert!(!renderer.headers().contains_key("x-frame-options"));
    let semantic_headers: BTreeSet<&str> = renderer
        .headers()
        .keys()
        .map(reqwest::header::HeaderName::as_str)
        .filter(|name| {
            !matches!(
                *name,
                "connection" | "content-length" | "date" | "server" | "transfer-encoding"
            )
        })
        .collect();
    assert_eq!(
        semantic_headers,
        BTreeSet::from([
            "cache-control",
            "content-security-policy",
            "content-type",
            "referrer-policy",
            "x-content-type-options",
        ])
    );
    assert_eq!(
        renderer
            .bytes()
            .expect("renderer body should be readable")
            .as_ref(),
        RENDERER_V2_DOCUMENT.as_bytes()
    );

    for path in [
        "/integrations/aps/renderer/v1",
        "/integrations/aps/runner/v1.js",
    ] {
        let response = client
            .get(format!("{}{path}", process.base_url))
            .send()
            .expect("unknown APS route request should complete");
        assert_exact_local_failure(response, 404, false);
    }
    fixture.assert_no_proxy_observation(0, Duration::from_millis(150));

    for (observation_index, case) in corpus(runtime_id).into_iter().enumerate() {
        fixture.enqueue(case.upstream);
        let started = Instant::now();
        let response = client
            .get(format!("{}{}", process.base_url, APS_RUNNER_ROUTE))
            .header("authorization", "Bearer must-not-leave-downstream")
            .header("cookie", "must-not-leave-downstream=1")
            .header("x-forwarded-for", "203.0.113.19")
            .header("x-publisher-secret", "must-not-leave-downstream")
            .send()
            .expect("should receive an APS runner-proxy downstream response");
        let elapsed = started.elapsed();
        assert_eq!(
            response.status().as_u16(),
            case.expected_status,
            "{}",
            case.name
        );
        if case.expected_status == 200 {
            assert_success(case.name, &response);
        } else {
            assert_eq!(
                response.headers()["cache-control"],
                "no-store",
                "{}",
                case.name
            );
        }
        let body = response
            .bytes()
            .expect("should read the APS runner-proxy downstream response body");
        if let Some(expected) = case.expected_body {
            assert_eq!(body.as_ref(), expected, "{}", case.name);
        }
        if let Some(maximum) = case.maximum_elapsed {
            assert!(
                elapsed <= maximum,
                "{}: deadline returned after {elapsed:?}, expected <= {maximum:?}",
                case.name
            );
        }
        let observed = fixture
            .wait_for_observation(observation_index)
            .expect("should observe the APS runner-proxy upstream request");
        assert_outbound_request(runtime_id, case.name, &observed);
    }
}
