//! Fetching an origin under varied request signals, and judging the results.

use std::collections::HashMap;
use std::time::Duration;

use crate::commands::origin::report::{
    AxisResult, ProbeReport, UrlReport, VerdictResult, first_difference,
};
use crate::error::{CliResult, cli_error};

/// Cookies a real repeat visitor carries, which is what the cookie axis must send.
///
/// Trusted Server sets `ts-ec` itself, which is why the template cache's cookie gate is
/// very nearly a disable switch — every returning reader trips it.
const TS_COOKIES: &[&str] = &[
    "ts-ec=probe-edge-cookie",
    "euconsent-v2=probe-consent",
    "ts-tester=probe",
];

const DESKTOP_USER_AGENT: &str =
    "FictionalBrowser/123.4 (FictionalOS 10.2; FictionalDesktop) ExampleRenderer/567.8";
const MOBILE_USER_AGENT: &str =
    "FictionalBrowser/123.4 (FictionalPhone; FictionalMobileOS 17.0) ExampleRenderer/567.8";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// One fetch's result, reduced to what the probe judges.
struct Fetched {
    body: Vec<u8>,
    headers: HashMap<String, Vec<String>>,
}

impl Fetched {
    /// Every instance of a header, in arrival order.
    ///
    /// The only accessor on purpose. A first-instance-only variant reads as if it returns
    /// "the" value, which is wrong for any field a proxy can append to: judging a response
    /// on the origin's `Cache-Control` while a later `private` goes unread is a false pass.
    fn all(&self, name: &str) -> &[String] {
        self.headers.get(name).map_or(&[], Vec::as_slice)
    }
}

/// What varies between the two arms of one axis.
struct Arm<'a> {
    headers: &'a [(&'a str, &'a str)],
}

/// Probe every URL and return the combined report.
///
/// # Errors
///
/// Returns an error when the runtime cannot be built or an origin cannot be reached. A
/// *reachable* origin that fails a check is not an error — it is a failing report.
pub(crate) fn probe_urls(
    urls: &[String],
    repeat: u32,
    extra_cookies: &[String],
    vary_headers: &[String],
) -> CliResult<ProbeReport> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to build the Tokio runtime for the probe: {error}"))?;

    install_crypto_provider();

    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // The probe must see exactly what it asked for. A redirect would silently
            // compare two different documents and report them as a difference.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to build the probe HTTP client: {error}"))?;

        let mut reports = Vec::with_capacity(urls.len());
        for url in urls {
            reports.push(probe_one(&client, url, repeat, extra_cookies, vary_headers).await?);
        }
        Ok(ProbeReport { urls: reports })
    })
}

/// Install the process-level rustls provider the HTTP client needs.
///
/// This crate's `reqwest` is built with a `-no-provider` rustls feature on purpose: it
/// already links `aws-lc-rs` through `reqwest` 0.13, and letting `reqwest` 0.12 pull `ring`
/// as well would compile two providers, which makes rustls's default ambiguous and panics
/// the dev proxy. The cost of that choice is that somebody must install the default, and
/// for the probe that is here.
///
/// Idempotent: a second call returns `Err` because one is already installed, which is not
/// a failure.
fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn probe_one(
    client: &reqwest::Client,
    url: &str,
    repeat: u32,
    extra_cookies: &[String],
    vary_headers: &[String],
) -> CliResult<UrlReport> {
    let cookie_jar = cookie_header(extra_cookies);

    // Baseline: bare request, also the left arm of every axis below.
    let baseline = fetch(client, url, &[]).await?;

    let mut axes = Vec::new();
    axes.push(self_identity_axis(client, url, &baseline, repeat).await?);
    axes.push(
        compare_axis(
            client,
            url,
            &baseline,
            "cookie",
            "bare vs. a representative cookie jar",
            Arm {
                headers: &[("cookie", cookie_jar.as_str())],
            },
        )
        .await?,
    );
    axes.push(
        compare_axis(
            client,
            url,
            &baseline,
            "accept-encoding",
            "identity vs. gzip, compared after decoding",
            Arm {
                headers: &[("accept-encoding", "gzip")],
            },
        )
        .await?,
    );
    axes.push(
        compare_axis(
            client,
            url,
            &baseline,
            "user-agent",
            "desktop vs. mobile user agent",
            Arm {
                headers: &[("user-agent", MOBILE_USER_AGENT)],
            },
        )
        .await?,
    );
    axes.push(rsc_axis(client, url, &baseline, vary_headers).await?);

    let verdicts = judge_headers(&baseline, &axes);

    Ok(UrlReport {
        url: url.to_owned(),
        axes,
        verdicts,
    })
}

/// An origin that is not stable against itself cannot be shared on any axis.
///
/// Runs first, and is reported as its own axis, because a per-request timestamp or CSRF
/// nonce would otherwise surface as a spurious failure on whichever axis happened to run
/// next — sending the operator after the wrong thing.
async fn self_identity_axis(
    client: &reqwest::Client,
    url: &str,
    baseline: &Fetched,
    repeat: u32,
) -> CliResult<AxisResult> {
    for _ in 0..repeat.max(1) {
        let again = fetch(client, url, &[]).await?;
        if let Some(difference) = first_difference(&baseline.body, &again.body) {
            return Ok(AxisResult {
                name: "self-identity".to_owned(),
                description: format!("the same request {} times", repeat.max(1) + 1),
                difference: Some(difference),
            });
        }
    }
    Ok(AxisResult {
        name: "self-identity".to_owned(),
        description: format!("the same request {} times", repeat.max(1) + 1),
        difference: None,
    })
}

/// RSC fetches already flow through the readthrough cache while HTML navigations are
/// passed, so removing the bypass puts both representations under one cache key for the
/// first time. An origin that varies on these without declaring it can serve a flight
/// payload to an HTML navigation.
async fn rsc_axis(
    client: &reqwest::Client,
    url: &str,
    baseline: &Fetched,
    vary_headers: &[String],
) -> CliResult<AxisResult> {
    let mut headers: Vec<(&str, &str)> = vec![("rsc", "1")];
    for name in vary_headers {
        if name.eq_ignore_ascii_case("rsc") || name.eq_ignore_ascii_case("accept-encoding") {
            continue;
        }
        headers.push((name.as_str(), "1"));
    }
    let description = format!(
        "bare vs. rsc plus {}",
        if vary_headers.is_empty() {
            "no configured vary headers".to_owned()
        } else {
            vary_headers.join(", ")
        }
    );

    let varied = fetch(client, url, &headers).await?;
    Ok(AxisResult {
        name: "rsc".to_owned(),
        description,
        difference: first_difference(&baseline.body, &varied.body),
    })
}

async fn compare_axis(
    client: &reqwest::Client,
    url: &str,
    baseline: &Fetched,
    name: &str,
    description: &str,
    arm: Arm<'_>,
) -> CliResult<AxisResult> {
    let varied = fetch(client, url, arm.headers).await?;
    Ok(AxisResult {
        name: name.to_owned(),
        description: description.to_owned(),
        difference: first_difference(&baseline.body, &varied.body),
    })
}

/// The four response-header checks, all blocking.
fn judge_headers(baseline: &Fetched, axes: &[AxisResult]) -> Vec<VerdictResult> {
    vec![
        freshness_verdict(baseline),
        set_cookie_verdict(baseline),
        csp_nonce_verdict(baseline),
        vary_coverage_verdict(baseline, axes),
    ]
}

/// Readthrough has no equivalent of the template cache's `NoPositiveFreshness` refusal, so
/// an origin that declares no freshness would be stored on a platform default instead of
/// being declined.
fn freshness_verdict(baseline: &Fetched) -> VerdictResult {
    // Every instance, not just the first. A field may arrive as several lines — a proxy
    // that appends `Cache-Control: private` after the origin's `public, max-age=300` is
    // the case that matters, and reading only the first line would pass it.
    let cache_control = baseline.all("cache-control").join(", ");
    let surrogate = baseline.all("surrogate-control").join(", ");
    let positive = [&cache_control, &surrogate]
        .iter()
        .any(|value| has_positive_freshness(value));
    let forbids = [&cache_control, &surrogate].iter().any(|value| {
        let lowered = value.to_ascii_lowercase();
        lowered.contains("no-store") || lowered.contains("private")
    });

    VerdictResult {
        name: "freshness".to_owned(),
        passed: positive && !forbids,
        detail: if cache_control.is_empty() && surrogate.is_empty() {
            "origin declared no Cache-Control or Surrogate-Control".to_owned()
        } else {
            format!("cache-control: {cache_control:?}, surrogate-control: {surrogate:?}")
        },
    }
}

/// A cached `Set-Cookie` is replayed to every later cookieless reader, which is
/// cross-reader session fixation rather than a staleness bug.
fn set_cookie_verdict(baseline: &Fetched) -> VerdictResult {
    let cookies = baseline.all("set-cookie");
    VerdictResult {
        name: "set-cookie".to_owned(),
        passed: cookies.is_empty(),
        detail: if cookies.is_empty() {
            "origin set no cookies".to_owned()
        } else {
            format!("origin set {} cookie(s) on this response", cookies.len())
        },
    }
}

/// A shared nonce silently defeats the origin's own nonce-based CSP for the cached window.
fn csp_nonce_verdict(baseline: &Fetched) -> VerdictResult {
    let has_nonce = baseline
        .all("content-security-policy")
        .iter()
        .any(|value| value.contains("'nonce-"));
    VerdictResult {
        name: "csp-nonce".to_owned(),
        passed: !has_nonce,
        detail: if has_nonce {
            "Content-Security-Policy carries a per-response nonce".to_owned()
        } else {
            "no per-response CSP nonce".to_owned()
        },
    }
}

/// The platform cache keys on URL plus whatever the origin declares in `Vary`, so an axis
/// that varies and is not declared is cross-served.
fn vary_coverage_verdict(baseline: &Fetched, axes: &[AxisResult]) -> VerdictResult {
    let declared: Vec<String> = baseline
        .all("vary")
        .iter()
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();

    let uncovered: Vec<&str> = axes
        .iter()
        .filter(|axis| !axis.passed())
        .map(|axis| axis.name.as_str())
        // Self-identity is not a request signal, so `Vary` cannot cover it.
        .filter(|name| *name != "self-identity")
        .filter(|name| {
            !declared
                .iter()
                .any(|declared| declared == name || declared == "*")
        })
        .collect();

    VerdictResult {
        name: "vary-coverage".to_owned(),
        passed: uncovered.is_empty(),
        detail: if uncovered.is_empty() {
            format!("declared Vary: {declared:?}")
        } else {
            format!(
                "varies on {} but Vary declares {declared:?}",
                uncovered.join(", ")
            )
        },
    }
}

fn has_positive_freshness(value: &str) -> bool {
    value.to_ascii_lowercase().split(',').any(|directive| {
        let directive = directive.trim();
        for prefix in ["max-age=", "s-maxage="] {
            if let Some(seconds) = directive.strip_prefix(prefix) {
                return seconds
                    .trim_matches('"')
                    .parse::<u64>()
                    .is_ok_and(|s| s > 0);
            }
        }
        false
    })
}

fn cookie_header(extra: &[String]) -> String {
    let mut parts: Vec<String> = TS_COOKIES.iter().map(|pair| (*pair).to_owned()).collect();
    parts.extend(extra.iter().cloned());
    parts.join("; ")
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
) -> CliResult<Fetched> {
    // Resolved into one map before the request is built, because `RequestBuilder::header`
    // *appends*. Layering an arm's override on top of a default would send the header
    // twice, and an origin that reads the first instance would never see the override —
    // silently turning the user-agent and accept-encoding axes into no-ops that pass.
    let mut resolved: Vec<(&str, &str)> = vec![
        ("user-agent", DESKTOP_USER_AGENT),
        // Identity unless an arm overrides it, so the encoding axis is the only thing that
        // changes what the origin may compress.
        ("accept-encoding", "identity"),
    ];
    for (name, value) in headers {
        match resolved
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        {
            Some(slot) => slot.1 = value,
            None => resolved.push((*name, *value)),
        }
    }

    let mut request = client.get(url);
    for (name, value) in &resolved {
        request = request.header(*name, *value);
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return cli_error(format!("could not reach {url}: {error}")),
    };

    let mut collected: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in response.headers() {
        let Ok(value) = value.to_str() else { continue };
        collected
            .entry(name.as_str().to_ascii_lowercase())
            .or_default()
            .push(value.to_owned());
    }

    let body = match response.bytes().await {
        Ok(bytes) => bytes.to_vec(),
        Err(error) => return cli_error(format!("could not read the body of {url}: {error}")),
    };

    Ok(Fetched {
        body,
        headers: collected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetched(headers: &[(&str, &str)]) -> Fetched {
        let mut collected: HashMap<String, Vec<String>> = HashMap::new();
        for (name, value) in headers {
            collected
                .entry((*name).to_owned())
                .or_default()
                .push((*value).to_owned());
        }
        Fetched {
            body: b"<html></html>".to_vec(),
            headers: collected,
        }
    }

    fn failing_axis(name: &str) -> AxisResult {
        AxisResult {
            name: name.to_owned(),
            description: String::new(),
            difference: Some(crate::commands::origin::report::Difference {
                offset: 0,
                left: "a".to_owned(),
                right: "b".to_owned(),
            }),
        }
    }

    #[test]
    fn absent_cache_control_fails_freshness() {
        assert!(
            !freshness_verdict(&fetched(&[])).passed,
            "readthrough would store this on a platform default where the template cache \
             declines it"
        );
    }

    #[test]
    fn positive_max_age_passes_freshness() {
        assert!(freshness_verdict(&fetched(&[("cache-control", "public, max-age=300")])).passed);
        assert!(freshness_verdict(&fetched(&[("surrogate-control", "max-age=60")])).passed);
    }

    #[test]
    fn zero_max_age_is_not_positive_freshness() {
        assert!(!freshness_verdict(&fetched(&[("cache-control", "max-age=0")])).passed);
    }

    #[test]
    fn private_or_no_store_fails_freshness_even_with_a_max_age() {
        assert!(
            !freshness_verdict(&fetched(&[("cache-control", "private, max-age=300")])).passed,
            "an origin that marks HTML private must not be declared shareable"
        );
        assert!(!freshness_verdict(&fetched(&[("cache-control", "no-store, max-age=300")])).passed);
    }

    #[test]
    fn set_cookie_fails_its_verdict() {
        assert!(set_cookie_verdict(&fetched(&[])).passed);
        assert!(
            !set_cookie_verdict(&fetched(&[("set-cookie", "sid=1")])).passed,
            "a cached Set-Cookie is replayed to every later cookieless reader"
        );
    }

    #[test]
    fn csp_nonce_fails_its_verdict() {
        assert!(
            csp_nonce_verdict(&fetched(&[(
                "content-security-policy",
                "default-src 'self'"
            )]))
            .passed
        );
        assert!(
            !csp_nonce_verdict(&fetched(&[(
                "content-security-policy",
                "script-src 'nonce-abc123'"
            )]))
            .passed
        );
    }

    #[test]
    fn a_varying_axis_must_be_declared_in_vary() {
        let axes = vec![failing_axis("user-agent")];
        assert!(
            !vary_coverage_verdict(&fetched(&[]), &axes).passed,
            "varying on User-Agent without declaring it is cross-served"
        );
        assert!(
            vary_coverage_verdict(&fetched(&[("vary", "User-Agent")]), &axes).passed,
            "a declared axis is keyed by the platform cache and is therefore safe"
        );
    }

    #[test]
    fn vary_star_covers_everything() {
        let axes = vec![failing_axis("user-agent"), failing_axis("cookie")];
        assert!(vary_coverage_verdict(&fetched(&[("vary", "*")]), &axes).passed);
    }

    #[test]
    fn self_identity_failure_is_not_blamed_on_vary() {
        let axes = vec![failing_axis("self-identity")];
        assert!(
            vary_coverage_verdict(&fetched(&[]), &axes).passed,
            "an unstable origin is a self-identity failure; Vary cannot express it and the \
             operator must not be sent looking for a header"
        );
    }

    #[test]
    fn cookie_header_carries_the_cookies_a_repeat_visitor_has() {
        let header = cookie_header(&["publisher_session=1".to_owned()]);
        assert!(header.contains("ts-ec="), "TS sets its own identity cookie");
        assert!(header.contains("publisher_session=1"));
    }
}
