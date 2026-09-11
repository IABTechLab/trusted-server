#[cfg(test)]
pub mod tests {
    use crate::settings::Settings;

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

            [integrations.prebid.bundle]
            adapters = ["exampleBidder"]

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
}

/// Shared Next.js + auction origin fixture.
///
/// Adapters exercise the buffered publisher path against this fixture in their
/// own route tests, and the cross-adapter parity suite reuses it, so all four
/// drive byte-identical input.
#[cfg(any(test, feature = "test-utils"))]
pub mod nextjs_auction {
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use error_stack::Report;

    use crate::geo::GeoInfo;
    use crate::platform::{
        BackendNamingPolicy, ClientInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
        PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
        PlatformPendingRequest, PlatformResponse, PlatformSecretStore, PlatformSelectResult,
        RuntimeServices, StoreId, StoreName, UnavailableKvStore,
    };
    use crate::settings::Settings;

    /// Publisher host the fixture settings serve.
    pub const PUBLISHER_HOST: &str = "test-publisher.example.com";
    /// Upstream host the fixture origin answers for.
    pub const ORIGIN_HOST: &str = "origin.test-publisher.example.com";
    /// Auction endpoint host the fixture bidder answers for.
    pub const AUCTION_HOST: &str = "auction.example.com";

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
                "endpoint": "https://auction.example.com/bid",
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

    /// Upstream that serves [`origin_html`] and one deterministic bid.
    #[derive(Default)]
    pub struct NextJsAuctionOrigin {
        auction_requests: AtomicUsize,
    }

    impl NextJsAuctionOrigin {
        /// Number of auction requests this fixture has answered.
        #[must_use]
        pub fn auction_requests(&self) -> usize {
            self.auction_requests.load(Ordering::SeqCst)
        }

        /// Forget the recorded auction requests.
        pub fn reset_auction_requests(&self) {
            self.auction_requests.store(0, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for NextJsAuctionOrigin {
        async fn send(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            let (content_type, body) = match request.request.uri().host() {
                Some(ORIGIN_HOST) => ("text/html", origin_html()),
                Some(AUCTION_HOST) => {
                    self.auction_requests.fetch_add(1, Ordering::SeqCst);
                    (
                        "application/json",
                        serde_json::json!({
                            "id": "fixture-auction",
                            "seatbid": [{"seat": "example", "bid": [{
                                "id": "fixture-bid", "impid": "fixture-slot", "price": 1.25,
                                "adm": "<div>fixture-creative</div>", "w": 300, "h": 250,
                                "crid": "example-creative", "adomain": ["advertiser.example.com"]
                            }]}]
                        })
                        .to_string(),
                    )
                }
                host => {
                    return Err(Report::new(PlatformError::HttpClient)
                        .attach(format!("unexpected fixture upstream: {host:?}")));
                }
            };
            Ok(PlatformResponse::new(
                http::Response::builder()
                    .status(200)
                    .header("content-type", content_type)
                    .body(edgezero_core::body::Body::from(body))
                    .expect("should build deterministic upstream response"),
            )
            .with_backend_name(request.backend_name))
        }

        async fn send_async(
            &self,
            request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            let backend = request.backend_name.clone();
            Ok(PlatformPendingRequest::new(request).with_backend_name(backend))
        }

        async fn select(
            &self,
            mut pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            let request = pending_requests
                .remove(0)
                .downcast::<PlatformHttpRequest>()
                .expect("should recover fixture pending request");
            Ok(PlatformSelectResult {
                ready: self.send(request).await,
                remaining: pending_requests,
                failed_backend_name: None,
            })
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

    impl PlatformConfigStore for FixtureStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    impl PlatformSecretStore for FixtureStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    impl PlatformBackend for FixtureStore {
        fn naming_policy(&self) -> BackendNamingPolicy {
            BackendNamingPolicy::Axum
        }

        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok("fixture-backend".to_owned())
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Ok("fixture-backend".to_owned())
        }
    }

    /// Runtime services wired to `client`, with every other platform capability
    /// stubbed out.
    #[must_use]
    pub fn services(client: Arc<NextJsAuctionOrigin>) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(FixtureStore))
            .secret_store(Arc::new(FixtureStore))
            .kv_store(Arc::new(UnavailableKvStore))
            .backend(Arc::new(FixtureStore))
            .http_client(client)
            .geo(Arc::new(FixtureGeo))
            .client_info(ClientInfo::default())
            .build()
    }
}
