//! `ts dev sandbox-probe` — measures sandbox reuse from workload responses.
//!
//! Issues a keep-alive sequence against a locally served Trusted Server over a
//! single connection and reads the counters the Fastly adapter attaches to
//! each response. Reuse is reported only when one instance id serves several
//! *workload* requests with strictly increasing ordinals.
//!
//! The counters endpoint is deliberately not used to establish reuse: a
//! snapshot identifies the sandbox that served the probe, which need not be
//! the one that served the preceding request.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use http_body_util::BodyExt as _;
use hyper_util::rt::TokioIo;

/// Response headers the Fastly adapter attaches when sandbox metrics are on.
const HEADER_INSTANCE: &str = "x-ts-sandbox-instance";
const HEADER_ORDINAL: &str = "x-ts-sandbox-ordinal";
const HEADER_BUILDS: &str = "x-ts-sandbox-builds";
const HEADER_REQUEST_ID: &str = "x-ts-sandbox-request-id";

/// Value the adapter reports when the runtime exposes no instance identity.
const INSTANCE_UNAVAILABLE: &str = "unavailable";

/// Paths that never carry counters because they short-circuit before dispatch.
const NON_WORKLOAD_PATHS: &[&str] = &["/health", "/_ts/debug/sandbox", "/_ts/debug/ja4"];

/// Arguments for `ts dev sandbox-probe`.
#[derive(Debug, clap::Args)]
pub struct SandboxProbeArgs {
    /// Host and port of the locally served instance.
    #[arg(long, default_value = "127.0.0.1:7676")]
    pub authority: String,

    /// Workload path to exercise.
    ///
    /// Required, and it must reach the router. `/health` and the debug probes
    /// short-circuit ahead of the point where counters are attached, so
    /// probing one of those reports nothing.
    #[arg(long)]
    pub path: String,

    /// Number of requests to issue over one connection.
    #[arg(long, default_value_t = 6)]
    pub requests: u32,

    /// Per-request timeout in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    pub timeout_ms: u64,
}

/// One observation taken from a workload response.
#[derive(Debug, Clone)]
struct Observation {
    instance: Option<String>,
    ordinal: Option<u64>,
    builds: Option<u64>,
    request_id: Option<String>,
    status: u16,
    elapsed: Duration,
}

/// Why the request sequence stopped, if it stopped early.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ending {
    /// Every requested iteration completed.
    Completed,
    /// The connection closed. Expected when a bounded sandbox retires, but a
    /// client cannot distinguish that from any other close.
    ConnectionClosed(String),
    /// A transport or body error, which invalidates the run.
    TransportError(String),
}

/// Runs the probe and prints a report.
///
/// # Errors
/// Returns a message when the path is not a workload route, the async runtime
/// cannot start, or the connection cannot be established.
pub fn run(args: &SandboxProbeArgs) -> Result<(), String> {
    if NON_WORKLOAD_PATHS.contains(&args.path.as_str()) {
        return Err(format!(
            "`{}` short-circuits before counters are attached; choose a workload route",
            args.path
        ));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start async runtime: {e}"))?;

    let (observations, ending) = runtime.block_on(collect(args))?;
    crate::output::info(report(&observations, &ending).trim_end());
    Ok(())
}

/// Issues the keep-alive sequence over a single HTTP/1.1 connection.
async fn collect(args: &SandboxProbeArgs) -> Result<(Vec<Observation>, Ending), String> {
    let stream = tokio::net::TcpStream::connect(&args.authority)
        .await
        .map_err(|e| format!("failed to connect to {}: {e}", args.authority))?;

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| format!("HTTP/1.1 handshake with {} failed: {e}", args.authority))?;

    // The connection task drives the socket; it resolves when the peer closes.
    let connection = tokio::spawn(connection);

    let timeout = Duration::from_millis(args.timeout_ms);
    let mut observations = Vec::with_capacity(args.requests as usize);
    let mut ending = Ending::Completed;

    for _ in 0..args.requests {
        let request = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(&args.path)
            .header(hyper::header::HOST, &args.authority)
            .body(String::new())
            .map_err(|e| format!("failed to build request: {e}"))?;

        let started = Instant::now();

        let response = match tokio::time::timeout(timeout, sender.send_request(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(e)) if e.is_closed() || e.is_incomplete_message() => {
                ending = Ending::ConnectionClosed(e.to_string());
                break;
            }
            Ok(Err(e)) => {
                ending = Ending::TransportError(e.to_string());
                break;
            }
            Err(_) => {
                ending = Ending::TransportError(format!("request timed out after {timeout:?}"));
                break;
            }
        };

        let (parts, body) = response.into_parts();

        // Hyper handles chunk extensions, trailers, and close-delimited
        // bodies. The body must be drained before the next request is sent.
        match tokio::time::timeout(timeout, body.collect()).await {
            Ok(Ok(collected)) => drop(collected.to_bytes()),
            Ok(Err(e)) => {
                ending = Ending::TransportError(format!("body read failed: {e}"));
                break;
            }
            Err(_) => {
                ending = Ending::TransportError(format!("body read timed out after {timeout:?}"));
                break;
            }
        }

        observations.push(observation_from(&parts, started.elapsed()));
    }

    drop(sender);
    if let Ok(Err(e)) = connection.await
        && ending == Ending::Completed
    {
        ending = Ending::ConnectionClosed(e.to_string());
    }

    Ok((observations, ending))
}

/// Extracts the counters from one response.
fn observation_from(parts: &hyper::http::response::Parts, elapsed: Duration) -> Observation {
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };

    Observation {
        instance: header(HEADER_INSTANCE),
        ordinal: header(HEADER_ORDINAL).and_then(|v| v.parse().ok()),
        builds: header(HEADER_BUILDS).and_then(|v| v.parse().ok()),
        request_id: header(HEADER_REQUEST_ID),
        status: parts.status.as_u16(),
        elapsed,
    }
}

/// Renders the report, stating explicitly what was and was not established.
fn report(observations: &[Observation], ending: &Ending) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "responses: {}", observations.len());

    match ending {
        Ending::Completed => {}
        Ending::ConnectionClosed(reason) => {
            let _ = writeln!(
                out,
                "ended: connection closed ({reason}); a client cannot tell sandbox \
                 retirement from any other close"
            );
        }
        Ending::TransportError(reason) => {
            let _ = writeln!(out, "ended: transport error ({reason})");
        }
    }

    if observations.is_empty() {
        let _ = writeln!(out, "reuse: unverified (no responses)");
        return out;
    }

    for (index, observation) in observations.iter().enumerate() {
        let _ = writeln!(
            out,
            "  {:>3}. status={} instance={} ordinal={} builds={} request={} elapsed={:?}",
            index + 1,
            observation.status,
            observation.instance.as_deref().unwrap_or("-"),
            observation
                .ordinal
                .map_or_else(|| "-".to_owned(), |v| v.to_string()),
            observation
                .builds
                .map_or_else(|| "-".to_owned(), |v| v.to_string()),
            observation.request_id.as_deref().unwrap_or("-"),
            observation.elapsed,
        );
    }

    out.push_str(&verdict(observations, ending));
    out
}

/// Decides whether the observations establish reuse.
///
/// Deliberately conservative: only strictly increasing ordinals on one
/// instance count. A repeated or decreasing ordinal under one instance id
/// means the attribution is unreliable — the id is not identifying what it
/// claims to — so no conclusion is drawn either way. Counting bare repeats
/// would instead report those runs as reuse.
fn verdict(observations: &[Observation], ending: &Ending) -> String {
    if let Ending::TransportError(reason) = ending {
        return format!("reuse: unverified (transport error: {reason})\n");
    }

    if observations.iter().all(|o| o.instance.is_none()) {
        return "reuse: unverified (no counters; enable debug.sandbox_metrics_enabled)\n"
            .to_owned();
    }

    if observations
        .iter()
        .any(|o| o.instance.as_deref() == Some(INSTANCE_UNAVAILABLE))
    {
        return "reuse: unverified (runtime reported no instance identity)\n".to_owned();
    }

    // Partial attribution means some responses cannot be placed in a sandbox,
    // so neither reuse nor its absence can be concluded.
    if observations
        .iter()
        .any(|o| o.instance.is_none() || o.ordinal.is_none())
    {
        return "reuse: unverified (incomplete attribution: some responses carried no \
                instance id or ordinal)\n"
            .to_owned();
    }

    let mut by_instance: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for observation in observations {
        if let (Some(instance), Some(ordinal)) =
            (observation.instance.as_deref(), observation.ordinal)
        {
            by_instance.entry(instance).or_default().push(ordinal);
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "distinct instances: {}", by_instance.len());

    // Ordinals from one sandbox must arrive strictly increasing. Anything else
    // means the ids are not the identities they claim to be.
    let anomalous: Vec<&str> = by_instance
        .iter()
        .filter(|(_, ordinals)| !ordinals.windows(2).all(|pair| pair[0] < pair[1]))
        .map(|(instance, _)| *instance)
        .collect();
    if !anomalous.is_empty() {
        let _ = writeln!(
            out,
            "reuse: unverified (instance(s) {} reported repeated or decreasing ordinals)",
            anomalous.join(", ")
        );
        return out;
    }

    let reused = by_instance
        .values()
        .filter(|ordinals| ordinals.len() > 1)
        .count();
    let max_per_instance = by_instance.values().map(Vec::len).max().unwrap_or_default();
    let max_builds = observations
        .iter()
        .filter_map(|o| o.builds)
        .max()
        .unwrap_or_default();

    let _ = writeln!(out, "max requests per instance: {max_per_instance}");
    let _ = writeln!(out, "max builds per instance: {max_builds}");

    if reused == 0 {
        let _ = writeln!(
            out,
            "reuse: not observed (every response came from a distinct instance)"
        );
    } else {
        let _ = writeln!(
            out,
            "reuse: observed ({reused} instance(s) served more than one workload request)"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(
        instance: Option<&str>,
        ordinal: Option<u64>,
        builds: Option<u64>,
    ) -> Observation {
        Observation {
            instance: instance.map(str::to_owned),
            ordinal,
            builds,
            request_id: Some("req".to_owned()),
            status: 200,
            elapsed: Duration::from_millis(1),
        }
    }

    #[test]
    fn absent_counters_report_unverified_not_negative() {
        let observations = vec![observation(None, None, None), observation(None, None, None)];

        let verdict = verdict(&observations, &Ending::Completed);

        assert!(
            verdict.contains("unverified"),
            "missing counters are not evidence that reuse failed"
        );
        assert!(
            !verdict.contains("reusable-sandbox"),
            "counters are feature-independent, so the hint must not name the feature: {verdict}"
        );
    }

    #[test]
    fn an_unavailable_instance_id_reports_unverified() {
        let observations = vec![
            observation(Some(INSTANCE_UNAVAILABLE), Some(1), Some(1)),
            observation(Some(INSTANCE_UNAVAILABLE), Some(2), Some(1)),
        ];

        assert!(
            verdict(&observations, &Ending::Completed).contains("unverified"),
            "a runtime with no identity cannot establish reuse"
        );
    }

    #[test]
    fn repeated_ordinals_on_one_instance_are_not_reuse() {
        // Unreliable attribution: one id reporting ordinal 1 twice. A bare
        // repeat count would call this reuse.
        let observations = vec![
            observation(Some("a"), Some(1), Some(1)),
            observation(Some("a"), Some(1), Some(1)),
        ];

        let verdict = verdict(&observations, &Ending::Completed);

        assert!(
            verdict.contains("unverified"),
            "duplicate ordinals must not read as reuse, got: {verdict}"
        );
        assert!(
            verdict.contains("repeated or decreasing"),
            "should name the anomaly, got: {verdict}"
        );
    }

    #[test]
    fn decreasing_ordinals_are_not_reuse() {
        let observations = vec![
            observation(Some("a"), Some(3), Some(1)),
            observation(Some("a"), Some(2), Some(1)),
        ];

        assert!(
            verdict(&observations, &Ending::Completed).contains("unverified"),
            "ordinals going backwards mean the identity is untrustworthy"
        );
    }

    #[test]
    fn incomplete_attribution_reports_unverified() {
        let observations = vec![
            observation(Some("a"), Some(1), Some(1)),
            observation(None, None, None),
        ];

        let verdict = verdict(&observations, &Ending::Completed);

        assert!(
            verdict.contains("incomplete attribution"),
            "a response that cannot be placed in a sandbox blocks a verdict, got: {verdict}"
        );
    }

    #[test]
    fn a_transport_error_invalidates_the_run() {
        let observations = vec![
            observation(Some("a"), Some(1), Some(1)),
            observation(Some("a"), Some(2), Some(1)),
        ];

        let verdict = verdict(&observations, &Ending::TransportError("reset".to_owned()));

        assert!(
            verdict.contains("unverified"),
            "a transport error makes the run unreliable, got: {verdict}"
        );
    }

    #[test]
    fn distinct_instances_report_reuse_not_observed() {
        let observations = vec![
            observation(Some("a"), Some(1), Some(1)),
            observation(Some("b"), Some(1), Some(1)),
        ];

        let verdict = verdict(&observations, &Ending::Completed);

        assert!(
            verdict.contains("not observed"),
            "one request per instance is arm A, got: {verdict}"
        );
        assert!(
            verdict.contains("distinct instances: 2"),
            "should report the instance count, got: {verdict}"
        );
    }

    #[test]
    fn increasing_ordinals_on_one_instance_report_reuse() {
        let observations = vec![
            observation(Some("a"), Some(1), Some(1)),
            observation(Some("a"), Some(2), Some(1)),
            observation(Some("a"), Some(3), Some(1)),
        ];

        let verdict = verdict(&observations, &Ending::Completed);

        assert!(
            verdict.contains("reuse: observed"),
            "three increasing ordinals on one instance is reuse, got: {verdict}"
        );
        assert!(
            verdict.contains("max requests per instance: 3"),
            "should report the depth reached, got: {verdict}"
        );
        assert!(
            verdict.contains("max builds per instance: 1"),
            "one build across three requests is the arm-C signal, got: {verdict}"
        );
    }

    #[test]
    fn a_closed_connection_is_reported_without_being_read_as_retirement() {
        let observations = vec![
            observation(Some("a"), Some(1), Some(1)),
            observation(Some("a"), Some(2), Some(1)),
        ];

        let report = report(
            &observations,
            &Ending::ConnectionClosed("closed".to_owned()),
        );

        assert!(
            report.contains("cannot tell sandbox"),
            "a close is ambiguous and must be reported as such, got: {report}"
        );
        assert!(
            report.contains("reuse: observed"),
            "a close does not invalidate observations already collected, got: {report}"
        );
    }

    #[test]
    fn non_workload_paths_are_rejected_before_connecting() {
        for path in NON_WORKLOAD_PATHS {
            let args = SandboxProbeArgs {
                authority: "127.0.0.1:7676".to_owned(),
                path: (*path).to_owned(),
                requests: 2,
                timeout_ms: 100,
            };

            let error = run(&args).expect_err("should refuse a short-circuiting path");

            assert!(
                error.contains("short-circuits"),
                "should explain why `{path}` cannot measure reuse, got: {error}"
            );
        }
    }

    #[test]
    fn an_empty_run_is_unverified() {
        assert!(
            report(&[], &Ending::Completed).contains("unverified"),
            "no responses cannot establish anything"
        );
    }
}
