//! Origin fetching, config-derived URL discovery, and response classification.
//!
//! `ts dev audit headers` fetches directly from the publisher origin. When no
//! explicit URLs are given, it fetches the origin root, parses the returned HTML
//! for asset URLs, and audits those. Routes under `/_ts/` are served by the
//! Trusted Server edge — not the origin — so they are never probed here; supply
//! them explicitly if you need to audit them.

use std::path::PathBuf;
use std::time::Duration;

use scraper::{Html, Selector};
use url::Url;

use super::analyze::FetchedResponse;
use super::rules::ResponseHeaders;
use crate::error::{CliResult, cli_error};

/// Caps discovery so one crawl of a large page can't fan out unbounded.
const MAX_DISCOVERED_URLS: usize = 50;

/// Per-request timeout for origin fetches.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Arguments for `ts dev audit headers`.
#[derive(Debug, clap::Args)]
pub struct AuditHeadersArgs {
    /// Explicit URLs to audit. When omitted, URLs are discovered from the
    /// origin root page.
    pub urls: Vec<String>,
    /// Path to `trusted-server.toml`, used to resolve the origin when no URLs
    /// and no `--origin` are given.
    #[arg(long, default_value = "trusted-server.toml")]
    pub config: PathBuf,
    /// Override the origin URL, skipping the config lookup.
    #[arg(long)]
    pub origin: Option<String>,
    /// Emit machine-readable JSON instead of the human table.
    #[arg(long)]
    pub json: bool,
}

/// A single origin response: the cache-relevant headers plus the body (needed
/// only to discover assets from the root HTML page).
#[derive(Debug, Clone)]
pub(crate) struct OriginResponse {
    /// The cache-relevant response headers.
    pub(crate) headers: ResponseHeaders,
    /// The response body as text (empty when not decodable as UTF-8).
    pub(crate) body: String,
}

/// Fetches origin responses. Abstracted so the fetch/discovery logic is
/// unit-testable without real network I/O, mirroring the `audit` command's
/// `AuditCollector`.
pub(crate) trait OriginClient {
    /// Fetches a single URL from the origin.
    fn fetch(&self, url: &Url) -> CliResult<OriginResponse>;
}

/// The production [`OriginClient`], backed by a blocking `reqwest` client.
pub(crate) struct ReqwestOriginClient {
    client: reqwest::blocking::Client,
}

impl ReqwestOriginClient {
    /// Builds a client with a bounded per-request timeout.
    pub(crate) fn new() -> CliResult<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("failed to build HTTP client: {error}"))?;
        Ok(Self { client })
    }
}

impl OriginClient for ReqwestOriginClient {
    fn fetch(&self, url: &Url) -> CliResult<OriginResponse> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .map_err(|error| format!("failed to fetch {url}: {error}"))?;

        let headers = extract_headers(response.headers());
        let body = response.text().unwrap_or_default();

        Ok(OriginResponse { headers, body })
    }
}

/// Pulls the cache-relevant headers out of a `reqwest` header map.
fn extract_headers(headers: &reqwest::header::HeaderMap) -> ResponseHeaders {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    ResponseHeaders {
        content_type: value("content-type"),
        cache_control: value("cache-control"),
        surrogate_control: value("surrogate-control"),
        surrogate_key: value("surrogate-key"),
        vary: value("vary"),
        etag: value("etag"),
    }
}

/// Resolves the target URLs and fetches each, returning classified responses.
pub(crate) fn collect_responses(
    args: &AuditHeadersArgs,
    client: &dyn OriginClient,
) -> CliResult<(String, Vec<FetchedResponse>)> {
    if args.urls.is_empty() {
        collect_via_discovery(args, client)
    } else {
        collect_explicit(&args.urls, client)
    }
}

/// Fetches an explicit list of URLs.
fn collect_explicit(
    urls: &[String],
    client: &dyn OriginClient,
) -> CliResult<(String, Vec<FetchedResponse>)> {
    let mut responses = Vec::with_capacity(urls.len());
    for raw in urls {
        let url = Url::parse(raw).map_err(|error| format!("invalid URL `{raw}`: {error}"))?;
        let response = client.fetch(&url)?;
        responses.push(FetchedResponse {
            url,
            headers: response.headers,
        });
    }
    // With explicit URLs there is no single origin; report the first host.
    let origin = responses
        .first()
        .map(|response| response.url.origin().ascii_serialization())
        .unwrap_or_default();
    Ok((origin, responses))
}

/// Resolves the origin, fetches its root, and audits the assets discovered in
/// the returned HTML alongside the root itself.
fn collect_via_discovery(
    args: &AuditHeadersArgs,
    client: &dyn OriginClient,
) -> CliResult<(String, Vec<FetchedResponse>)> {
    let origin = resolve_origin(args)?;
    let root =
        Url::parse(&origin).map_err(|error| format!("invalid origin `{origin}`: {error}"))?;

    let root_response = client.fetch(&root)?;
    let discovered = discover_asset_urls(&root, &root_response.body);

    let mut responses = Vec::with_capacity(discovered.len() + 1);
    responses.push(FetchedResponse {
        url: root.clone(),
        headers: root_response.headers,
    });

    for url in discovered {
        // A single asset fetch failing should not abort the whole audit.
        match client.fetch(&url) {
            Ok(response) => responses.push(FetchedResponse {
                url,
                headers: response.headers,
            }),
            Err(error) => log::warn!("skipping {url}: {error}"),
        }
    }

    Ok((origin, responses))
}

/// Resolves the origin from `--origin`, else from the config's
/// `publisher.origin_url`.
fn resolve_origin(args: &AuditHeadersArgs) -> CliResult<String> {
    if let Some(origin) = &args.origin {
        return Ok(origin.clone());
    }

    let text = std::fs::read_to_string(&args.config).map_err(|error| {
        format!(
            "failed to read config {}: {error}; pass --origin to audit without a config",
            args.config.display()
        )
    })?;
    let parsed: toml::Value = toml::from_str(&text)
        .map_err(|error| format!("failed to parse {}: {error}", args.config.display()))?;

    parsed
        .get("publisher")
        .and_then(|publisher| publisher.get("origin_url"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or(())
        .or_else(|()| {
            cli_error(format!(
                "publisher.origin_url not found in {}",
                args.config.display()
            ))
        })
}

/// Extracts asset URLs from HTML: `<script src>`, `<img src>`,
/// `<link rel=stylesheet href>`, and `<link rel=icon href>`, resolved against
/// the origin and de-duplicated. Falls back to `/favicon.ico`.
fn discover_asset_urls(base: &Url, html: &str) -> Vec<Url> {
    let document = Html::parse_document(html);
    let mut urls = Vec::new();

    let push_resolved = |raw: &str, urls: &mut Vec<Url>| {
        if urls.len() >= MAX_DISCOVERED_URLS {
            return;
        }
        if let Ok(resolved) = base.join(raw)
            && !urls.contains(&resolved)
        {
            urls.push(resolved);
        }
    };

    for (selector, attribute) in asset_selectors() {
        let Ok(selector) = Selector::parse(selector) else {
            continue;
        };
        for element in document.select(&selector) {
            if let Some(value) = element.value().attr(attribute) {
                push_resolved(value, &mut urls);
            }
        }
    }

    // Ensure at least the favicon is checked when the page links none.
    if let Ok(favicon) = base.join("/favicon.ico")
        && !urls.contains(&favicon)
        && urls.len() < MAX_DISCOVERED_URLS
    {
        urls.push(favicon);
    }

    urls
}

/// The (CSS selector, attribute) pairs discovery scans for asset URLs.
fn asset_selectors() -> [(&'static str, &'static str); 4] {
    [
        ("script[src]", "src"),
        ("img[src]", "src"),
        ("link[rel~=stylesheet][href]", "href"),
        ("link[rel~=icon][href]", "href"),
    ]
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    /// An in-memory [`OriginClient`] for tests: maps URL to `(content_type, body)`.
    struct FakeClient {
        responses: HashMap<String, OriginResponse>,
        fetched: RefCell<Vec<String>>,
    }

    impl FakeClient {
        fn new() -> Self {
            Self {
                responses: HashMap::new(),
                fetched: RefCell::new(Vec::new()),
            }
        }

        fn with(mut self, url: &str, content_type: &str, body: &str) -> Self {
            self.responses.insert(
                url.to_owned(),
                OriginResponse {
                    headers: ResponseHeaders {
                        content_type: Some(content_type.to_owned()),
                        cache_control: Some("no-store".to_owned()),
                        ..ResponseHeaders::default()
                    },
                    body: body.to_owned(),
                },
            );
            self
        }
    }

    impl OriginClient for FakeClient {
        fn fetch(&self, url: &Url) -> CliResult<OriginResponse> {
            self.fetched.borrow_mut().push(url.to_string());
            self.responses
                .get(url.as_str())
                .cloned()
                .ok_or(())
                .or_else(|()| cli_error(format!("no fake response for {url}")))
        }
    }

    fn args(origin: &str) -> AuditHeadersArgs {
        AuditHeadersArgs {
            urls: Vec::new(),
            config: PathBuf::from("trusted-server.toml"),
            origin: Some(origin.to_owned()),
            json: false,
        }
    }

    #[test]
    fn discovers_assets_from_root_html() {
        let base = Url::parse("https://origin.example").expect("should parse base");
        let html = r#"
            <html><head>
              <link rel="stylesheet" href="/style.css">
              <link rel="icon" href="/favicon.ico">
              <script src="https://origin.example/app.js"></script>
            </head><body>
              <img src="/logo.png">
            </body></html>
        "#;
        let urls = discover_asset_urls(&base, html);
        assert!(
            urls.contains(&Url::parse("https://origin.example/app.js").unwrap()),
            "should discover script src"
        );
        assert!(
            urls.contains(&Url::parse("https://origin.example/style.css").unwrap()),
            "should discover stylesheet href"
        );
        assert!(
            urls.contains(&Url::parse("https://origin.example/logo.png").unwrap()),
            "should discover img src"
        );
    }

    #[test]
    fn discovery_fetches_root_and_assets_but_never_ts_routes() {
        let html = r#"<html><body><img src="/logo.png"></body></html>"#;
        let client = FakeClient::new()
            .with("https://origin.example/", "text/html", html)
            .with("https://origin.example/logo.png", "image/png", "")
            .with("https://origin.example/favicon.ico", "image/x-icon", "");

        let (origin, responses) =
            collect_responses(&args("https://origin.example"), &client).expect("should collect");

        assert_eq!(origin, "https://origin.example");
        let fetched = client.fetched.borrow();
        assert!(
            fetched.iter().any(|url| url == "https://origin.example/"),
            "should fetch the root"
        );
        assert!(
            fetched
                .iter()
                .any(|url| url == "https://origin.example/logo.png"),
            "should fetch the discovered image"
        );
        assert!(
            !fetched.iter().any(|url| url.contains("/_ts/")),
            "should never probe edge-only /_ts/ routes against the origin"
        );
        assert!(
            !responses.is_empty(),
            "should return at least the root response"
        );
    }

    #[test]
    fn explicit_urls_skip_discovery() {
        let client = FakeClient::new().with("https://origin.example/rtb", "application/json", "");
        let explicit = AuditHeadersArgs {
            urls: vec!["https://origin.example/rtb".to_owned()],
            config: PathBuf::from("trusted-server.toml"),
            origin: None,
            json: false,
        };
        let (_, responses) = collect_responses(&explicit, &client).expect("should collect");
        assert_eq!(responses.len(), 1, "should fetch only the explicit URL");
        assert_eq!(
            client.fetched.borrow().len(),
            1,
            "should not fetch a root page for explicit URLs"
        );
    }

    #[test]
    fn invalid_explicit_url_is_rejected() {
        let client = FakeClient::new();
        let explicit = AuditHeadersArgs {
            urls: vec!["not a url".to_owned()],
            config: PathBuf::from("trusted-server.toml"),
            origin: None,
            json: false,
        };
        let error = collect_responses(&explicit, &client).expect_err("should reject invalid URL");
        assert!(error.contains("invalid URL"), "should explain the failure");
    }
}
