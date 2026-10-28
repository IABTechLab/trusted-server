#[cfg(any(test, feature = "test-utils"))]
pub mod tests {
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource, TcfConsent};
    use crate::ec::module::{EcModuleSelection, HMAC_MODULE_KEY, HOST_SIGNALS_MODULE_KEY};
    use crate::redacted::Redacted;
    use crate::settings::{Ec, EcModuleBlock, HmacModuleConfig, HostSignalsModuleConfig, Settings};

    #[must_use]
    pub fn crate_test_settings_str() -> String {
        r#"
            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [geo]
            # A gdpr-eu country, where every permission requires a signal. This
            # reproduces the prior no-default floor, so existing tests are
            # unaffected by the now-required default.
            # Tests run with no geo module, so single-jurisdiction operation
            # is acknowledged the same way a deployment would.
            assume_single_jurisdiction = true

            [auction]

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            "#
        .to_owned()
    }

    /// The shared fixture TOML with the module `name` selected in `section`
    /// as well, written with the section's type folder left off, for a test
    /// that appends that module's own table.
    #[must_use]
    pub fn crate_test_settings_str_running(section: &str, name: &str) -> String {
        let written = crate::module_name::short_form(section, name);
        if section == "auction" {
            return crate_test_settings_str().replace(
                "[auction]\n",
                &format!("[auction]\nmodules = [\"{written}\"]\n"),
            );
        }
        format!(
            "{}\n[{section}]\nmodules = [\"{written}\"]\n",
            crate_test_settings_str()
        )
    }

    /// The crate test configuration with its whole `[ec]` section replaced by
    /// `ec_section`, which carries its own `[ec]` header and any module
    /// blocks.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML configuration no longer has an `[ec]`
    /// section followed by a `[request_signing]` section.
    #[must_use]
    pub fn crate_test_settings_str_with_ec_section(ec_section: &str) -> String {
        let base = crate_test_settings_str();
        let (before, rest) = base
            .split_once("[ec]")
            .expect("should find the [ec] section in the test settings");
        let (_, after) = rest
            .split_once("[request_signing]")
            .expect("should find the [request_signing] section in the test settings");
        format!("{before}{ec_section}\n\n[request_signing]{after}")
    }

    /// The `[permissions]` table `ts config push` writes into a blob for the
    /// repository's sample policy: its name, its digest and its text.
    #[must_use]
    pub fn sample_permissions() -> crate::settings::PermissionsConfig {
        use crate::permissions::{SAMPLE_POLICY_NAME, SAMPLE_POLICY_TEXT, policy_sha256};

        crate::settings::PermissionsConfig {
            policy: Some(SAMPLE_POLICY_NAME.to_owned()),
            sha256: Some(policy_sha256(SAMPLE_POLICY_TEXT)),
            text: Some(SAMPLE_POLICY_TEXT.to_owned()),
        }
    }

    /// A small valid policy whose digest no other test uses, given a `group`
    /// no other test passes, so no earlier load in the process holds its
    /// rules.
    #[must_use]
    pub fn policy_only_this_test_loads(group: &str) -> String {
        format!(
            "groups:\n  {group}:\n    default: requires_signal\nrules:\n  group: {group}\n  jurisdiction: unknown\n"
        )
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

    /// Selects the built-in HMAC module under `name` with `passphrase`,
    /// replacing whatever Edge Cookie module the settings carried.
    ///
    /// A `name` other than `hmac` is a label, so the block names the
    /// implementation it configures.
    pub fn select_hmac_module(ec: &mut Ec, name: &str, passphrase: &str) {
        let mut block = EcModuleBlock::from(HmacModuleConfig {
            passphrase: Redacted::new(passphrase.to_owned()),
        });
        if name != HMAC_MODULE_KEY {
            block.implementation = Some(HMAC_MODULE_KEY.to_owned());
        }
        ec.module = Some(EcModuleSelection::from(name));
        ec.module_blocks.clear();
        ec.module_blocks.insert(name.to_owned(), block);
    }

    /// Selects the built-in host-signal module under its own name with
    /// `passphrase`, replacing whatever Edge Cookie module the settings
    /// carried.
    pub fn select_host_signals_module(ec: &mut Ec, passphrase: &str) {
        ec.module = Some(EcModuleSelection::from(HOST_SIGNALS_MODULE_KEY));
        ec.module_blocks.clear();
        ec.module_blocks.insert(
            HOST_SIGNALS_MODULE_KEY.to_owned(),
            EcModuleBlock::from(HostSignalsModuleConfig {
                passphrase: Redacted::new(passphrase.to_owned()),
            }),
        );
    }

    /// The passphrase the block `name` holds.
    ///
    /// # Panics
    ///
    /// Panics if `name` has no block, or if its block configures another
    /// module.
    #[must_use]
    #[allow(
        clippy::panic,
        reason = "a fixture names the block it could not find, which `expect` cannot"
    )]
    pub fn hmac_passphrase<'a>(ec: &'a Ec, name: &str) -> &'a str {
        ec.module_blocks
            .get(name)
            .and_then(EcModuleBlock::hmac_settings)
            .unwrap_or_else(|| panic!("settings should configure the hmac module under `{name}`"))
            .passphrase
            .expose()
    }

    /// A valid EC ID in `{64-hex}.{6-alnum}` format for use in tests.
    pub const VALID_SYNTHETIC_ID: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";

    /// The permissions of a reader who allowed storage and withheld
    /// personalised-ad selection: the Edge Cookie stands, partner sharing does
    /// not.
    #[must_use]
    pub fn storage_only_permissions() -> crate::permissions::PermissionState {
        crate::permissions::PermissionState::new(
            [crate::permissions::Permission::StoreOnDevice]
                .into_iter()
                .collect(),
        )
    }

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

/// Page fixtures shared by core's tests and the tests of a module crate.
#[cfg(any(test, feature = "test-utils"))]
pub mod fixtures {
    /// A publisher page carrying the tags of several vendors, as an origin
    /// would serve it.
    pub const PUBLISHER_PAGE_HTML: &str = include_str!("html_processor.test.html");
}

/// What a test logs on its own thread, for a test of a log line.
///
/// The first use installs a logger for the whole test binary that keeps the
/// records of a thread while it captures and drops every other record.
#[cfg(test)]
pub(crate) mod logs {
    use std::cell::RefCell;
    use std::future::Future;
    use std::sync::Once;

    thread_local! {
        static CAPTURED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    struct ThreadLogger;

    impl log::Log for ThreadLogger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            CAPTURED.with(|captured| captured.borrow().is_some())
        }

        fn log(&self, record: &log::Record<'_>) {
            CAPTURED.with(|captured| {
                if let Some(lines) = captured.borrow_mut().as_mut() {
                    lines.push(format!("{} {}", record.level(), record.args()));
                }
            });
        }

        fn flush(&self) {}
    }

    static LOGGER: ThreadLogger = ThreadLogger;
    static INSTALL: Once = Once::new();

    /// What `future` answers, and each line it logs on this thread at
    /// warning level or above, as the level and then the message, such as
    /// `WARN ...`. A future that runs on another thread logs nothing here.
    pub(crate) async fn logged_by<F: Future>(future: F) -> (F::Output, Vec<String>) {
        INSTALL.call_once(|| {
            log::set_logger(&LOGGER).expect("should be the only logger the tests install");
            log::set_max_level(log::LevelFilter::Warn);
        });
        CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
        let output = future.await;
        let lines = CAPTURED.with(|captured| captured.borrow_mut().take().unwrap_or_default());
        (output, lines)
    }
}

/// The operator-facing settings template, for tests that check what it
/// documents.
#[cfg(any(test, feature = "test-utils"))]
pub mod template {
    /// The source-controlled template an operator starts from.
    pub const EXAMPLE_TEMPLATE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../trusted-server.example.toml"
    ));

    /// The template with its required secret-store key references replaced by
    /// resolved test values, so parsing it as settings can exercise the
    /// optional blocks a test uncomments.
    #[must_use]
    pub fn template_with_resolved_required_secrets() -> String {
        EXAMPLE_TEMPLATE
            .replace(
                "proxy_secret = \"publisher_proxy_secret\"",
                "proxy_secret = \"unit-test-resolved-publisher-proxy-secret-0001\"",
            )
            .replace(
                "passphrase = \"ec_passphrase\"",
                "passphrase = \"unit-test-resolved-ec-passphrase-secret-0001\"",
            )
    }

    /// Uncomments the contiguous `#`-prefixed block that begins at the line
    /// `# {header}`, leaving the rest of the template untouched. Stops at the
    /// first line that is not a comment, so a blank line ends the block.
    #[must_use]
    pub fn uncomment_block(template: &str, header: &str) -> String {
        let header_line = format!("# {header}");
        let mut out = Vec::new();
        let mut uncommenting = false;

        for line in template.lines() {
            if line == header_line {
                uncommenting = true;
            } else if uncommenting && !line.trim_start().starts_with('#') {
                uncommenting = false;
            }

            if uncommenting {
                let bare = line
                    .strip_prefix("# ")
                    .or_else(|| line.strip_prefix('#'))
                    .unwrap_or(line);
                out.push(bare.to_owned());
            } else {
                out.push(line.to_owned());
            }
        }

        out.join("\n")
    }

    /// Only the block of [`EXAMPLE_TEMPLATE`] that begins at the line
    /// `# {header}`, uncommented, for a test that adds a documented section to
    /// settings of its own. The block ends at the first line that is not a
    /// comment.
    ///
    /// # Panics
    ///
    /// Panics if the template documents no such block.
    #[must_use]
    pub fn documented_block(header: &str) -> String {
        let header_line = format!("# {header}");
        let block: Vec<&str> = EXAMPLE_TEMPLATE
            .lines()
            .skip_while(|line| *line != header_line)
            .take_while(|line| line.trim_start().starts_with('#'))
            .map(|line| {
                line.strip_prefix("# ")
                    .or_else(|| line.strip_prefix('#'))
                    .unwrap_or(line)
            })
            .collect();
        assert!(!block.is_empty(), "the template should document {header}");
        block.join("\n")
    }
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
            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "fixture-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            # The fixture supplies its own geo module through the runtime
            # services, so it selects the host lookup to reach it. Without a
            # selection the jurisdiction is unknown, and a server-side auction
            # fails closed on an unknown jurisdiction.
            [geo]
            module = "platform"

            # What Next.js changes in a page is a middleware, which runs on
            # the pages an entry names it for.
            [[fetch]]
            media_type = "text/html"
            middleware = ["framework.nextjs"]
            "#,
        )
        .expect("should parse Next.js auction fixture settings");
        settings
            .insert_module_config(
                "framework",
                "framework.nextjs",
                &serde_json::json!({
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should select the fixture Next.js integration");
        settings.auction.enabled = true;
        // One `[demand.fixture]` source, which is where an auction provider is
        // configured. `implementation` states the wire format, so there is no
        // separate `protocol`.
        settings.demand = crate::provider_table::ProviderTable::new(
            vec!["fixture".to_owned()],
            std::collections::BTreeMap::from([(
                "fixture".to_owned(),
                serde_json::Map::from_iter([
                    (
                        "implementation".to_owned(),
                        serde_json::json!("auction-protocol.openrtb"),
                    ),
                    (
                        "endpoint".to_owned(),
                        serde_json::json!("https://auction.example.com/bid"),
                    ),
                    ("routing".to_owned(), serde_json::json!("all_eligible")),
                    ("timeout_ms".to_owned(), serde_json::json!(5000)),
                ]),
            )]),
        );
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
        // A loaded domain carries its policy, and the fixture decides under
        // the repository's sample.
        settings.with_permission_policy(crate::permissions::PermissionPolicy::sample())
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

    #[async_trait::async_trait(?Send)]
    impl PlatformGeo for FixtureGeo {
        async fn lookup(
            &self,
            _client_ip: Option<IpAddr>,
            _services: &RuntimeServices,
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
            .permission_policy(crate::permissions::PermissionPolicy::sample())
            .build()
    }
}

/// Loaded permission policies other than the repository's sample.
#[cfg(any(test, feature = "test-utils"))]
pub mod policies {
    use std::sync::Arc;

    use crate::permissions::{PermissionPolicy, PolicyReference, load_policy, policy_sha256};

    /// A policy whose whole world sets every permission without a signal.
    const OPEN_POLICY_TEXT: &str = "groups:\n  open:\n    default: granted\nrules:\n  group: open\n  jurisdiction: non-regulated\n";

    /// The open policy, loaded as a domain that names it would load it, so a
    /// test can tell the policy a request was given from the sample.
    ///
    /// # Panics
    ///
    /// Panics when the open policy does not load.
    #[must_use]
    pub fn open() -> Arc<PermissionPolicy> {
        load_policy(
            PolicyReference {
                policy: "open".to_owned(),
                sha256: policy_sha256(OPEN_POLICY_TEXT),
            },
            OPEN_POLICY_TEXT,
        )
        .expect("should load the open policy")
    }
}

/// A domain whose permission policy does not load, as an adapter's runtime
/// loader meets it, so each adapter can show what its startup router answers.
#[cfg(any(test, feature = "test-utils"))]
pub mod policy_failure {
    use error_stack::Report;

    use crate::config_payload::settings_from_config_blob;
    use crate::error::TrustedServerError;
    use crate::permissions::{SAMPLE_POLICY_TEXT, policy_sha256};
    use crate::platform::{PlatformError, PlatformSecretStore, PolicyOrigin, StoreId, StoreName};
    use crate::settings::{PermissionsConfig, Settings};

    /// The policy the domain's settings name.
    pub const POLICY_NAME: &str = "standard";

    /// Resolves every secret reference to a value long enough to pass the
    /// checks the runtime makes.
    struct ResolvingSecretStore;

    impl PlatformSecretStore for ResolvingSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            Ok(format!("{key}-resolved-for-the-policy-failure-fixture").into_bytes())
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    /// What the runtime loader returns for a blob whose settings name
    /// [`POLICY_NAME`] and its digest but carry no file, as a blob pushed
    /// without the file would.
    ///
    /// # Panics
    ///
    /// Panics if the fixture settings do not parse, or if the blob loads.
    pub fn load_error() -> Report<TrustedServerError> {
        let mut settings = Settings::from_toml(
            r#"
            [publisher]
            domain = "publisher.example"
            cookie_domain = ".publisher.example"
            origin_url = "https://origin.publisher.example"
            proxy_secret = "publisher_proxy_secret"
            "#,
        )
        .expect("should parse the policy failure settings");
        settings.permissions = PermissionsConfig {
            policy: Some(POLICY_NAME.to_owned()),
            sha256: Some(policy_sha256(SAMPLE_POLICY_TEXT)),
            text: None,
        };
        let data = serde_json::to_value(&settings).expect("should serialize the settings");
        let envelope = edgezero_core::blob_envelope::BlobEnvelope::new(
            data,
            "2026-01-01T00:00:00Z".to_owned(),
        );
        let envelope = serde_json::to_string(&envelope).expect("should serialize the envelope");

        settings_from_config_blob(
            &envelope,
            &ResolvingSecretStore,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
        )
        .expect_err("a blob without its policy file should not load")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_fixture_fails_for_its_policy_and_not_for_another_reason() {
            let error = load_error();

            assert!(
                matches!(
                    error.current_context(),
                    TrustedServerError::Configuration { .. }
                ),
                "should be a configuration error: {error:?}"
            );
            assert!(
                format!("{error:?}")
                    .contains("carries no text for the permission policy `standard`"),
                "should fail because the file is missing: {error:?}"
            );
        }
    }
}
