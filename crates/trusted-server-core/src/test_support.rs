#[cfg(test)]
pub(crate) mod body_read;
#[cfg(any(test, feature = "test-utils"))]
pub mod logging;
#[cfg(any(test, feature = "test-utils"))]
pub mod scripted_client;

#[cfg(any(test, feature = "test-utils"))]
use std::io::Write as _;

#[cfg(any(test, feature = "test-utils"))]
use edgezero_core::http::Request;

#[cfg(any(test, feature = "test-utils"))]
use crate::platform::HttpClient;
#[cfg(any(test, feature = "test-utils"))]
use crate::{
    error::TrustedServerError,
    redacted::Redacted,
    s3_sigv4::{self, S3Credentials},
};
#[cfg(any(test, feature = "test-utils"))]
use error_stack::Report;
#[cfg(any(test, feature = "test-utils"))]
use http::{HeaderMap, HeaderValue, Method, header};

/// Inserts `client` into `request`'s extensions, where `ctx.http_client()` reads it.
///
/// [`RouterService::oneshot`](edgezero_core::router::RouterService::oneshot) skips the
/// adapter's request conversion, which is what installs a default client, so a `oneshot`
/// route test calls this before dispatch. Pass a client built with
/// [`HttpClient::with_client`], which keeps the default proxy marker, so the adapter's own
/// `.with_proxy_marker(false)` stays observable.
///
/// # Examples
///
/// ```ignore
/// insert_outbound_client(
///     &mut request,
///     HttpClient::with_client(ScriptedHttpClient::forbidding()),
/// );
/// let response = router.oneshot(request).await?;
/// ```
#[cfg(any(test, feature = "test-utils"))]
pub fn insert_outbound_client(request: &mut Request, client: HttpClient) {
    request.extensions_mut().insert(client);
}

/// Returns `bytes` gzip-encoded, for an origin fixture that serves a gzip body.
///
/// # Examples
///
/// ```ignore
/// let body = gzip(b"<html></html>");
/// assert_eq!(&body[..2], &[0x1f, 0x8b]);
/// ```
///
/// # Panics
///
/// Panics if in-memory gzip encoding fails.
#[cfg(any(test, feature = "test-utils"))]
#[must_use]
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .expect("should encode the gzip fixture");
    encoder.finish().expect("should finish the gzip fixture")
}

/// Recomputes the S3 `SigV4` `Authorization` value for a request an origin received.
///
/// An origin stub passes the method, the `Host`, request target, and `x-amz-date` it
/// received, and the route's region and credentials. The result equals the request's
/// `Authorization` only when Trusted Server signed exactly that `Host`, path, and query.
///
/// # Errors
///
/// Returns [`TrustedServerError::Proxy`] when
/// `amz_date` is not a `SigV4` timestamp or the received values cannot be signed.
#[cfg(any(test, feature = "test-utils"))]
pub fn s3_sigv4_authorization(
    method: &Method,
    host: &str,
    request_target: &str,
    amz_date: &str,
    region: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<String, Report<TrustedServerError>> {
    let invalid = |message: String| Report::new(TrustedServerError::Proxy { message });
    let signed_at = chrono::NaiveDateTime::parse_from_str(amz_date, "%Y%m%dT%H%M%SZ")
        .map_err(|_| invalid(format!("invalid x-amz-date `{amz_date}`")))?
        .and_utc();
    let url = url::Url::parse(&format!("https://{host}{request_target}"))
        .map_err(|_| invalid(format!("invalid received target `{host}{request_target}`")))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        HeaderValue::from_str(host)
            .map_err(|_| invalid(format!("invalid received Host `{host}`")))?,
    );
    s3_sigv4::sign_headers(
        method,
        &url,
        &mut headers,
        region,
        &S3Credentials {
            access_key_id: access_key_id.to_owned(),
            secret_access_key: Redacted::new(secret_access_key.to_owned()),
            session_token: None,
        },
        std::time::SystemTime::from(signed_at),
    )?;
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| invalid("signer set no Authorization header".to_owned()))
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::error::EdgeError;
    use edgezero_core::http::RequestParts;
    use edgezero_core::router::{PreDispatchHook, RouterService};
    use http::{Request, Response};

    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource, TcfConsent};
    use crate::settings::Settings;

    /// Run `edit` against the head of `request` and return its result.
    ///
    /// The body is left in place. This lets tests apply a head-only helper,
    /// such as forwarder preparation or basic auth, to a whole request.
    pub fn with_request_parts<T>(
        request: &mut Request<Body>,
        edit: impl FnOnce(&mut RequestParts) -> T,
    ) -> T {
        let (mut parts, body) = std::mem::take(request).into_parts();
        let result = edit(&mut parts);
        *request = Request::from_parts(parts, body);
        result
    }

    /// Drive `hook` through a router and report the outcome.
    ///
    /// The route matches every path for the request's method.
    ///
    /// # Errors
    ///
    /// Returns the router's response when the hook answers instead of
    /// continuing. `Ok` carries the request the route received, with the hook's
    /// head and the buffered body bytes.
    ///
    /// # Panics
    ///
    /// Panics if the router cannot render its response.
    pub async fn run_pre_dispatch_hook<H: PreDispatchHook>(
        hook: H,
        request: Request<Body>,
    ) -> Result<Request<Body>, Response<Body>> {
        let received: Arc<Mutex<Option<(RequestParts, Bytes)>>> = Arc::default();
        let sink = Arc::clone(&received);
        let record = move |context: RequestContext| {
            let sink = Arc::clone(&sink);
            async move {
                let body = context.body_bytes(usize::MAX).await?;
                let (parts, _) = context.into_request()?.into_parts();
                *sink.lock().expect("should lock the recorded request") = Some((parts, body));
                Ok::<_, EdgeError>(Response::new(Body::empty()))
            }
        };
        let method = request.method().clone();
        let router = RouterService::builder()
            .pre_dispatch_hook(hook)
            .route("/", method.clone(), record.clone())
            .route("/{*rest}", method, record)
            .build();
        let response = router
            .oneshot(request)
            .await
            .expect("should render the router response");
        let recorded = received
            .lock()
            .expect("should lock the recorded request")
            .take();
        match recorded {
            Some((parts, body)) => Ok(Request::from_parts(parts, Body::from(body))),
            None => Err(response),
        }
    }

    #[must_use]
    pub fn crate_test_settings_str() -> String {
        r#"
            [[handlers]]
            path = "^/secure"
            username = "user"
            password = "pass"

            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [integrations.prebid]
            enabled = true
            external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"

            [integrations.prebid.bundle.modules]
            bidder = ["exampleBidderBidAdapter"]

            [integrations.nextjs]
            enabled = false
            rewrite_attributes = ["href", "link", "url"]

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            [request_signing]
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#
        .to_owned()
    }

    #[must_use]
    /// Creates test settings from embedded TOML configuration.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML configuration is invalid.
    pub fn create_test_settings() -> Settings {
        let toml_str = crate_test_settings_str();
        let mut settings = Settings::from_toml(&toml_str).expect("Invalid config");
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
    }

    /// A valid EC ID in `{64-hex}.{6-alnum}` format for use in tests.
    pub const VALID_SYNTHETIC_ID: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";

    /// GDPR consent granting TCF Purpose 1 (storage/EC) but denying Purpose 4
    /// (personalized ads), so EC is allowed while EID persistence is not.
    #[must_use]
    pub fn purpose_one_only_consent() -> ConsentContext {
        ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            gdpr_applies: true,
            tcf: Some(TcfConsent {
                version: 2,
                cmp_id: 1,
                cmp_version: 1,
                consent_screen: 0,
                consent_language: "EN".to_owned(),
                vendor_list_version: 1,
                tcf_policy_version: 4,
                created_ds: 0,
                last_updated_ds: 0,
                // Purpose 1 (index 0) granted; Purpose 4 (index 3) denied.
                purpose_consents: {
                    let mut purposes = vec![false; 24];
                    purposes[0] = true;
                    purposes
                },
                purpose_legitimate_interests: vec![false; 24],
                vendor_consents: Vec::new(),
                vendor_legitimate_interests: Vec::new(),
                special_feature_opt_ins: vec![false; 12],
            }),
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        }
    }
}

/// Shared Next.js + auction origin fixture.
///
/// Adapters exercise the buffered publisher path against this fixture in their
/// own route tests, and the cross-adapter parity suite reuses it, so all four
/// drive byte-identical input.
#[cfg(any(test, feature = "test-utils"))]
pub mod nextjs_auction {
    use std::io::Read as _;
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bytes::Bytes;
    use error_stack::Report;
    use flate2::read::GzDecoder;
    use http::{Method, StatusCode};

    use crate::geo::GeoInfo;
    use crate::platform::{
        ClientInfo, PlatformConfigStore, PlatformError, PlatformGeo, PlatformSecretStore,
        RuntimeServices, StoreId, StoreName, UnavailableKvStore,
    };
    use crate::settings::Settings;
    use crate::test_support::scripted_client::{ScriptedHttpClient, ScriptedReply, TestClock};

    /// Publisher host the fixture settings serve.
    pub const PUBLISHER_HOST: &str = "test-publisher.example.com";
    /// Upstream host the fixture origin answers for.
    pub const ORIGIN_HOST: &str = "origin.test-publisher.example.com";
    /// Bid endpoint of the fixture auction provider, where its scripted bidder answers.
    pub const AUCTION_ENDPOINT: &str = "https://auction.example.com/bid";

    /// Settings that enable the Next.js integration and a single auction provider.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML is invalid.
    #[must_use]
    pub fn settings() -> Settings {
        let mut settings = Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "fixture-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should parse Next.js auction fixture settings");
        settings
            .integrations
            .insert_config(
                "nextjs",
                &serde_json::json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should enable the fixture Next.js integration");
        settings.auction.enabled = true;
        settings.auction.mediator = None;
        settings.auction.providers = serde_json::from_value(serde_json::json!({
            "fixture": {
                "protocol": "openrtb-2.6",
                "endpoint": AUCTION_ENDPOINT,
                "routing": "all_eligible",
                "timeout_ms": 5000
            }
        }))
        .expect("should configure the fixture auction provider");
        settings.creative_opportunities = Some(
            toml::from_str(
                r#"
            gam_network_id = "12345"
            [[slot]]
            id = "fixture-slot"
            page_patterns = ["/article"]
            formats = [{ width = 300, height = 250 }]
        "#,
            )
            .expect("should parse fixture creative opportunities"),
        );
        settings
    }

    /// Flight payload content the fixture must carry once its origin URL has been
    /// rewritten to the proxy host.
    const REWRITTEN_FLIGHT_CONTENT: &str =
        r#"{"url":"http://test-publisher.example.com/app","text":"</body>"}"#;

    /// The complete rewritten Flight payload, with the `T` length recomputed for
    /// the shortened URL.
    ///
    /// The fixture deliberately splits the URL across two scripts, so this never
    /// appears contiguously in the HTML: a caller that cannot parse the DOM must
    /// assert on [`expected_rewritten_flight_header`] instead.
    #[must_use]
    pub fn expected_rewritten_flight_payload() -> String {
        format!(
            "1:T{:x},{REWRITTEN_FLIGHT_CONTENT}",
            REWRITTEN_FLIGHT_CONTENT.len()
        )
    }

    /// The `id:Tlength,` header of the rewritten payload.
    ///
    /// The declared length shrinks when the origin URL is replaced by the shorter
    /// proxy URL, so this header changes if rewriting silently stops happening.
    #[must_use]
    pub fn expected_rewritten_flight_header() -> String {
        format!("1:T{:x},", REWRITTEN_FLIGHT_CONTENT.len())
    }

    /// Origin HTML whose Flight payload spans two scripts and contains a literal
    /// `</body>` inside RSC data, so a parser-blind body seam would fire early.
    ///
    /// # Panics
    ///
    /// Panics if the embedded fixture content cannot be split.
    #[must_use]
    pub fn origin_html() -> String {
        let content = r#"{"url":"https://origin.test-publisher.example.com/app","text":"</body>"}"#;
        let split = content.find("/app").expect("should locate content split");
        let first = serde_json::json!(format!("1:T{:x},{}", content.len(), &content[..split]));
        let second = serde_json::json!(&content[split..]);
        format!(
            "<html><head></head><body><p>prefix</p><script>self.__next_f.push([1,{first}])</script><script>window.between=true</script><script>self.__next_f.push([1,{second}])</script><p>suffix</p></body></html>"
        )
    }

    /// Isolated Next.js/GTM settings without auction dispatch.
    ///
    /// # Panics
    ///
    /// Panics if the static GTM configuration is invalid.
    #[must_use]
    pub fn script_composition_settings() -> Settings {
        let mut settings = settings();
        settings.auction.enabled = false;
        settings.creative_opportunities = None;
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true, "container_id": "GTM-MIX1"
                }),
            )
            .expect("should enable fixture GTM");
        settings
    }

    const SCRIPT_COMPOSITION_BOOTSTRAP: &str =
        "(self.__next_f=self.__next_f||[]).push([0]);self.__next_f.push([2,null])";

    /// HTML with standard bootstrap controls and a mixed Flight T record,
    /// crossing parser-internal boundaries with same-delimiter query escapes.
    #[must_use]
    pub fn script_composition_html() -> String {
        let content = serde_json::json!({
            "url": format!("https://{ORIGIN_HOST}/app"),
            "tag": "http://www.googletagmanager.com/gtm.js?q=\"x\"&v=é",
            "ga": "//www.google-analytics.com/g/collect?v=2",
            "text": "é😀</body>", "padding": "_".repeat(11000),
        })
        .to_string();
        let payload = serde_json::json!(format!("1:T{:x},{content}", content.len()));
        format!(
            "<html><head></head><body><script>{SCRIPT_COMPOSITION_BOOTSTRAP}</script><script>self.__next_f.push([1,{payload}])</script><p>script-composition-suffix</p></body></html>"
        )
    }

    /// Assert fully consumed adapter output, decoded JSON and recomputed T length.
    ///
    /// # Panics
    ///
    /// Panics if response coding, Flight syntax, rewrites, or completion are wrong.
    pub fn assert_script_composition_response(bytes: &[u8], gzip: bool) {
        let mut decoded = Vec::new();
        if gzip {
            GzDecoder::new(bytes)
                .read_to_end(&mut decoded)
                .expect("should decode complete gzip response");
        } else {
            decoded.extend_from_slice(bytes);
        }
        let html = String::from_utf8(decoded).expect("should emit UTF-8 HTML");
        assert!(
            html.contains("<p>script-composition-suffix</p>"),
            "should deliver suffix sentinel"
        );
        assert!(html.ends_with("</body></html>"), "should finish HTML");
        assert!(
            !html.contains("__ts_rsc_"),
            "should resolve every captured payload"
        );
        assert!(
            html.contains(SCRIPT_COMPOSITION_BOOTSTRAP),
            "should preserve inert bootstrap controls"
        );
        assert_eq!(
            html.matches("self.__next_f.push([1,").count(),
            1,
            "should emit Flight once"
        );
        let json = html
            .rsplit("self.__next_f.push(")
            .next()
            .expect("should keep push")
            .split(")</script>")
            .next()
            .expect("should close push");
        let push: serde_json::Value =
            serde_json::from_str(json).expect("should retain valid push JSON");
        let payload = push[1].as_str().expect("should retain payload");
        let (header, body) = payload.split_once(',').expect("should retain T record");
        assert_eq!(
            usize::from_str_radix(&header[3..], 16).expect("should parse hex length"),
            body.len(),
            "should recount decoded T bytes"
        );
        let data: serde_json::Value =
            serde_json::from_str(body).expect("should preserve valid model JSON");
        assert!(
            data["url"]
                .as_str()
                .expect("should retain URL")
                .contains(&format!("{PUBLISHER_HOST}/app")),
            "should rewrite origin"
        );
        assert_eq!(
            data["tag"], "/integrations/google_tag_manager/gtm.js?q=\"x\"&v=é",
            "should preserve query quotes and Unicode while routing the GTM tag"
        );
        assert_eq!(
            data["ga"], "/integrations/google_tag_manager/g/collect?v=2",
            "should route the GA collect endpoint"
        );
        assert_eq!(
            data["text"], "é😀</body>",
            "should preserve Unicode and literal closing-tag text"
        );
        assert_eq!(
            data["padding"],
            "_".repeat(11000),
            "should preserve all model padding"
        );
    }

    /// Publisher origin that serves [`origin_html`], with a scripted bidder that
    /// answers every bid request at [`AUCTION_ENDPOINT`] with one deterministic bid.
    ///
    /// Both answer through one scripted client: every GET to [`ORIGIN_HOST`] gets the
    /// page, whatever its path. [`services`] sends the publisher origin and the
    /// auction through it and reads time from the clock its replies arrive on.
    pub struct NextJsAuctionOrigin {
        clock: TestClock,
        client: ScriptedHttpClient,
        auction_requests: Arc<AtomicUsize>,
    }

    impl Default for NextJsAuctionOrigin {
        fn default() -> Self {
            Self::serving(Bytes::from(origin_html()), false, false)
        }
    }

    /// Queues the fixture bid for the next bid request `bidder` receives.
    fn queue_fixture_bid(bidder: &ScriptedHttpClient) {
        let bid = serde_json::json!({
            "id": "fixture-auction",
            "seatbid": [{"seat": "example", "bid": [{
                "id": "fixture-bid", "impid": "fixture-slot", "price": 1.25,
                "adm": "<div>fixture-creative</div>", "w": 300, "h": 250,
                "crid": "example-creative", "adomain": ["advertiser.example.com"]
            }]}]
        });
        bidder.respond_to(
            Method::POST,
            AUCTION_ENDPOINT,
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(bid.to_string()),
        );
    }

    /// The origin's answer to one page request: `html`, gzip-coded when `gzip`, and
    /// delivered in 97-byte chunks when `chunked`.
    fn origin_reply(html: &Bytes, gzip: bool, chunked: bool) -> ScriptedReply {
        let mut reply = ScriptedReply::status(StatusCode::OK).header("content-type", "text/html");
        if gzip {
            reply = reply.header("content-encoding", "gzip");
        }
        if chunked {
            reply.chunks(html.chunks(97).map(Bytes::copy_from_slice).collect())
        } else {
            reply.body(html.clone())
        }
    }

    impl NextJsAuctionOrigin {
        /// Serve canned HTML instead of the default fixture, optionally gzip-encoded
        /// and delivered in small chunks. The default fixture remains unchanged.
        ///
        /// # Panics
        ///
        /// Panics if in-memory gzip encoding fails.
        #[must_use]
        pub fn with_html_response(html: &str, gzip: bool, chunked: bool) -> Self {
            let bytes = if gzip {
                super::gzip(html.as_bytes())
            } else {
                html.as_bytes().to_vec()
            };
            Self::serving(Bytes::from(bytes), gzip, chunked)
        }

        fn serving(html: Bytes, gzip: bool, chunked: bool) -> Self {
            let clock = TestClock::new();
            let auction_requests = Arc::new(AtomicUsize::new(0));
            let script = ScriptedHttpClient::new(&clock);
            let rearm = script.clone();
            let counted = Arc::clone(&auction_requests);
            // The scripted client spends each reply on one request, so each request
            // queues its own answer as it is recorded, before its reply is taken.
            let client = script.with_request_observer(move |request| {
                if request.method == Method::POST && request.uri == AUCTION_ENDPOINT {
                    counted.fetch_add(1, Ordering::SeqCst);
                    queue_fixture_bid(&rearm);
                } else if request.method == Method::GET && request.uri.host() == Some(ORIGIN_HOST) {
                    rearm.respond_to(
                        Method::GET,
                        &request.uri.to_string(),
                        origin_reply(&html, gzip, chunked),
                    );
                }
            });
            queue_fixture_bid(&client);
            Self {
                clock,
                client,
                auction_requests,
            }
        }

        /// Number of bid requests the fixture bidder has received.
        #[must_use]
        pub fn auction_requests(&self) -> usize {
            self.auction_requests.load(Ordering::SeqCst)
        }

        /// Forget the recorded bid requests.
        pub fn reset_auction_requests(&self) {
            self.auction_requests.store(0, Ordering::SeqCst);
        }
    }

    struct FixtureGeo;

    impl PlatformGeo for FixtureGeo {
        fn lookup(
            &self,
            _client_ip: Option<IpAddr>,
        ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            Ok(Some(GeoInfo {
                country: "AU".to_owned(),
                city: "Example City".to_owned(),
                continent: "Oceania".to_owned(),
                latitude: 0.0,
                longitude: 0.0,
                metro_code: 0,
                region: None,
                asn: None,
            }))
        }
    }

    struct FixtureStore;

    #[async_trait::async_trait(?Send)]
    impl PlatformConfigStore for FixtureStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore))
        }

        async fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore))
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _key: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore))
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformSecretStore for FixtureStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore))
        }

        async fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore))
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _name: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore))
        }
    }

    /// Runtime services wired to `client`, with its scripted origin and bidder as the
    /// outbound client and every other platform capability stubbed out.
    #[must_use]
    pub fn services(client: &NextJsAuctionOrigin) -> RuntimeServices {
        services_with_outbound_client(&client.client, &client.clock)
    }

    /// Fixture runtime services whose outbound requests go to `client`, on `clock`.
    #[must_use]
    pub fn services_with_outbound_client(
        client: &ScriptedHttpClient,
        clock: &TestClock,
    ) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(FixtureStore))
            .secret_store(Arc::new(FixtureStore))
            .kv_store(Arc::new(UnavailableKvStore))
            .outbound_client(client.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(FixtureGeo))
            .client_info(ClientInfo::default())
            .build()
    }
}

#[cfg(feature = "test-utils")]
pub(crate) mod async_html_finalization;
