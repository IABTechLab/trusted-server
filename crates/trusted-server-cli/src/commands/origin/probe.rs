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

const DESKTOP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// One fetch's result, reduced to what the probe judges.
struct Fetched {
    status: u16,
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
    admission_cookie: Option<&str>,
) -> CliResult<ProbeReport> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to build the Tokio runtime for the probe: {error}"))?;

    crate::tls::install_crypto_provider();

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
            reports.push(
                probe_one(
                    &client,
                    url,
                    repeat,
                    extra_cookies,
                    vary_headers,
                    admission_cookie,
                )
                .await?,
            );
        }
        Ok(ProbeReport { urls: reports })
    })
}

async fn probe_one(
    client: &reqwest::Client,
    url: &str,
    repeat: u32,
    extra_cookies: &[String],
    vary_headers: &[String],
    admission_cookie: Option<&str>,
) -> CliResult<UrlReport> {
    let cookie_jar = cookie_header(extra_cookies, admission_cookie);

    // Baseline: the admission cookie and nothing else, and the left arm of every axis
    // below. It carries that cookie because without it a bot-protected origin answers
    // every arm with a challenge page, and the probe would then compare two challenge
    // pages and report on those instead of on the origin.
    let baseline = fetch(client, url, &[], admission_cookie).await?;

    // A challenge page is not the origin. Judging one produces a confident verdict about
    // content the origin never served — in practice a false FAIL that reads exactly like a
    // real one, which is worse than no answer.
    if baseline.status != 200 {
        return cli_error(format!(
            "{url} answered {} rather than 200, so there is nothing to judge.\n\
             A bot wall or redirect returns a page the origin did not compose, and every \
             verdict below it would describe that page.\n\
             Pass a session cookie that reaches real content with \
             --admission-cookie 'name=value'.",
            baseline.status
        ));
    }

    let (self_identity, repeated) =
        self_identity_axis(client, url, &baseline, repeat, admission_cookie).await?;
    let mut axes = vec![self_identity];
    let mut samples: Vec<(String, Fetched)> = repeated
        .into_iter()
        .enumerate()
        .map(|(index, response)| (format!("self-identity repeat {}", index + 1), response))
        .collect();

    let mut rsc_body = Vec::new();
    for (name, description, value) in [
        (
            "cookie",
            "bare vs. a representative cookie jar",
            cookie_jar.as_str(),
        ),
        (
            "accept-encoding",
            "identity vs. gzip, compared after decoding",
            "gzip",
        ),
        (
            "user-agent",
            "desktop vs. mobile user agent",
            MOBILE_USER_AGENT,
        ),
        ("rsc", "bare vs. an RSC request", "1"),
    ] {
        let (axis, mut response) = compare_axis(
            client,
            url,
            &baseline.body,
            name,
            description,
            Arm {
                headers: &[(name, value)],
            },
            admission_cookie,
        )
        .await?;
        if name == "rsc" {
            rsc_body = std::mem::take(&mut response.body);
        } else {
            response.body = Vec::new();
        }
        axes.push(axis);
        samples.push((name.to_owned(), response));
    }

    // Each configured signal needs its own comparison and Vary declaration. Combining
    // these with RSC lets Vary: rsc hide a difference caused by an unrelated header.
    for name in vary_headers {
        let name = name.to_ascii_lowercase();
        if axes.iter().any(|axis| axis.name == name) {
            continue;
        }
        let description = format!("bare vs. {name}: 1");
        let (mut axis, mut response) = compare_axis(
            client,
            url,
            &baseline.body,
            &name,
            &description,
            Arm {
                headers: &[(name.as_str(), "1")],
            },
            admission_cookie,
        )
        .await?;
        response.body = Vec::new();
        samples.push((name.clone(), response));

        // Some signals only affect flight responses. Hold RSC constant so the
        // configured header still owns its difference and needs its own Vary entry.
        let (rsc_axis, mut response) = compare_axis(
            client,
            url,
            &rsc_body,
            &name,
            &description,
            Arm {
                headers: &[("rsc", "1"), (name.as_str(), "1")],
            },
            admission_cookie,
        )
        .await?;
        if axis.difference.is_none() && rsc_axis.differs() {
            axis.difference = rsc_axis.difference;
            axis.description = format!("RSC request vs. RSC with {name}: 1");
        }
        response.body = Vec::new();
        samples.push((format!("{name} with RSC"), response));
        axes.push(axis);
    }

    let mut baseline = baseline;
    baseline.body = Vec::new();
    samples.insert(0, ("baseline".to_owned(), baseline));
    mark_axes_covered_by_vary(&samples, &mut axes);
    let mut verdicts = judge_headers(&samples[0].1, &axes);
    for (label, sample) in &samples {
        for checked in judge_headers(sample, &axes) {
            if !checked.passed {
                let verdict = verdicts
                    .iter_mut()
                    .find(|verdict| verdict.name == checked.name)
                    .expect("should find every response verdict");
                if verdict.passed {
                    *verdict = VerdictResult {
                        detail: format!("{label}: {}", checked.detail),
                        ..checked
                    };
                }
            }
        }
    }

    if admission_cookie.is_some() {
        verdicts.push(VerdictResult {
            name: "cookieless-coverage".to_owned(),
            passed: false,
            detail: "--admission-cookie was sent on every request; cookieless responses were \
                     not tested. This diagnostic run cannot establish cache safety. Probe \
                     the origin without --admission-cookie before enabling caching."
                .to_owned(),
        });
    }

    Ok(UrlReport {
        url: url.to_owned(),
        axes,
        verdicts,
    })
}

/// Only excuse a varying signal when every sampled response declares it.
///
/// Cookie independence and decoded encoding identity are template-cache prerequisites,
/// regardless of Vary. Self-identity varies no request signal at all.
fn mark_axes_covered_by_vary(samples: &[(String, Fetched)], axes: &mut [AxisResult]) {
    for axis in axes {
        if matches!(
            axis.name.as_str(),
            "self-identity" | "cookie" | "accept-encoding"
        ) {
            continue;
        }
        axis.covered_by_vary = samples.iter().all(|(_, response)| {
            response
                .all("vary")
                .iter()
                .flat_map(|value| value.split(','))
                .any(|name| name.trim().eq_ignore_ascii_case(&axis.name) || name.trim() == "*")
        });
    }
}

/// Compare every repeat, keeping the first difference and inspecting all later headers.
async fn self_identity_axis(
    client: &reqwest::Client,
    url: &str,
    baseline: &Fetched,
    repeat: u32,
    admission_cookie: Option<&str>,
) -> CliResult<(AxisResult, Vec<Fetched>)> {
    let mut difference = None;
    let mut samples = Vec::new();
    for _ in 0..repeat.max(1) {
        let mut again = fetch(client, url, &[], admission_cookie).await?;
        if difference.is_none() {
            difference = first_difference(&baseline.body, &again.body);
        }
        // Only response metadata is needed after comparison; do not retain a page body
        // per repeat or per variant.
        again.body = Vec::new();
        samples.push(again);
    }
    Ok((
        AxisResult {
            name: "self-identity".to_owned(),
            description: format!("the same request {} times", repeat.max(1) + 1),
            difference,
            covered_by_vary: false,
        },
        samples,
    ))
}

async fn compare_axis(
    client: &reqwest::Client,
    url: &str,
    baseline_body: &[u8],
    name: &str,
    description: &str,
    arm: Arm<'_>,
    admission_cookie: Option<&str>,
) -> CliResult<(AxisResult, Fetched)> {
    let varied = fetch(client, url, arm.headers, admission_cookie).await?;
    let axis = AxisResult {
        name: name.to_owned(),
        description: description.to_owned(),
        difference: first_difference(baseline_body, &varied.body),
        covered_by_vary: false,
    };
    Ok((axis, varied))
}

/// Response checks applied to every sample, all blocking.
fn judge_headers(response: &Fetched, axes: &[AxisResult]) -> Vec<VerdictResult> {
    vec![
        VerdictResult {
            name: "status".to_owned(),
            passed: response.status == 200,
            detail: format!("response status: {}", response.status),
        },
        fronting_cache_verdict(response),
        freshness_verdict(response),
        set_cookie_verdict(response),
        csp_nonce_verdict(response),
        vary_coverage_verdict(response, axes),
    ]
}

/// Every axis compares two responses. A cache between this tool and the origin can answer
/// both from one stored object, so all five axes read identical and the report goes green
/// on an origin that personalizes freely on a miss. That is the one failure that invalidates
/// the whole run at once, so it is judged before anything else.
///
/// Detected rather than defeated. Busting the cache would need a query parameter or a
/// `no-cache` request header, and both change what the origin is asked for — the first
/// changes the cache key and the page identity, the second can change the origin's own
/// caching behaviour and with it the freshness verdict. Perturbing the measurement to
/// rescue it would make a green result mean less, not more. Probe the origin directly.
fn fronting_cache_verdict(baseline: &Fetched) -> VerdictResult {
    // Even Age: 0 can be a fresh cache hit. Any Age field makes direct-origin
    // evidence uncertain; malformed values must not turn that uncertainty into a pass.
    let ages = baseline.all("age");
    let served_from_cache = !ages.is_empty();

    const HIT_INDICATORS: &[&str] = &["x-cache", "cf-cache-status", "x-cache-status"];
    let vendor_hit = HIT_INDICATORS.iter().find(|name| {
        baseline
            .all(name)
            .iter()
            .any(|value| value.to_ascii_lowercase().contains("hit"))
    });

    let detail = match (served_from_cache, vendor_hit) {
        (true, _) => format!(
            "a cache may have answered this request (age: {}), so every axis may be comparing one \
             stored object with itself",
            ages.join(", ")
        ),
        (false, Some(name)) => format!(
            "a cache answered this request ({name} reports a hit), so every axis may be \
             comparing one stored object with itself"
        ),
        (false, None) => "no cache reported serving this response".to_owned(),
    };

    VerdictResult {
        name: "fronting-cache".to_owned(),
        passed: !served_from_cache && vendor_hit.is_none(),
        detail,
    }
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
        // The raw observation, not `passed()`: `passed()` forgives a declared signal, and
        // this verdict is what decides whether it is declared.
        .filter(|axis| axis.differs())
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

/// The cookie arm's jar: the admission cookie plus the cookies a repeat visitor carries.
///
/// The admission cookie is included so this arm differs from the baseline by the *added*
/// cookies only. Without it the axis would also be varying whether the request is admitted
/// at all, which is not a question about personalization.
fn cookie_header(extra: &[String], admission_cookie: Option<&str>) -> String {
    let mut parts: Vec<String> = admission_cookie.into_iter().map(str::to_owned).collect();
    parts.extend(TS_COOKIES.iter().map(|pair| (*pair).to_owned()));
    parts.extend(extra.iter().cloned());
    parts.join("; ")
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
    admission_cookie: Option<&str>,
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
    // Seeded before the arm's own headers so an arm that sets `cookie` replaces it rather
    // than duplicating it — every arm must be admitted, but only the cookie arm varies
    // what else it carries.
    if let Some(cookie) = admission_cookie {
        resolved.push(("cookie", cookie));
    }
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
    let status = response.status().as_u16();

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
        status,
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
            status: 200,
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
            covered_by_vary: false,
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
        let header = cookie_header(&["publisher_session=1".to_owned()], None);
        assert!(header.contains("ts-ec="), "TS sets its own identity cookie");
        assert!(header.contains("publisher_session=1"));
    }

    #[test]
    fn the_cookie_arm_keeps_the_admission_cookie() {
        // Otherwise the cookie axis would vary two things at once: the added cookies, and
        // whether the request is admitted past the bot wall at all.
        let header = cookie_header(&[], Some("datadome=abc"));
        assert!(header.contains("datadome=abc"));
        assert!(header.contains("ts-ec="));
    }
}
