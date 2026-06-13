use crate::common::assertions;
use crate::common::ec::{
    assert_json_response, assert_status, batch_sync, batch_sync_no_auth, identify,
    identify_with_headers, is_ec_cookie_expired, normalize_ec_id, BatchMapping, EcTestClient,
};
use crate::common::runtime::{origin_port, TestError, TestResult};
use error_stack::Report;
use error_stack::ResultExt as _;

const INTTEST_API_TOKEN: &str = "integration-test-token-alpha-32-bytes-ok";
const INTTEST2_API_TOKEN: &str = "integration-test-token-bravo-32-bytes-ok";

/// Standard test scenarios applicable to all frontend frameworks.
///
/// Each scenario tests a core trusted-server behavior that should work
/// regardless of the underlying framework serving HTML.
#[derive(Debug, Clone)]
pub enum TestScenario {
    /// Verify `<script>` tag is injected into the HTML response.
    HtmlInjection,

    /// Verify `/static/tsjs=tsjs-unified.min.js` endpoint serves the JS bundle.
    ScriptServing,

    /// Verify origin host URLs in `href`/`src` attributes are rewritten to the proxy host.
    AttributeRewriting,

    /// Verify `/static/tsjs=unknown.js` returns 404, not HTML or a fallback.
    ScriptServingUnknownFile404,
}

/// Framework-specific custom scenarios that test framework-unique behaviors.
#[derive(Debug, Clone)]
pub enum CustomScenario {
    /// Next.js: React Server Components Flight format is preserved.
    NextJsRscFlight,

    /// Next.js: Server Actions POST requests pass through correctly.
    NextJsServerActions,

    /// Next.js: API routes return JSON without HTML injection.
    NextJsApiRoute,

    /// Next.js: Form action URLs are rewritten from origin to proxy.
    NextJsFormAction,

    /// `WordPress`: Admin pages (`/wp-admin/`) receive script injection.
    ///
    /// The trusted server currently injects into ALL HTML responses
    /// regardless of path. This test documents that behavior and guards
    /// against unintended changes.
    WordPressAdminInjection,
}

impl TestScenario {
    /// Execute this scenario against a running runtime.
    ///
    /// # Arguments
    ///
    /// * `base_url` - The base URL of the running runtime (e.g. `http://127.0.0.1:12345`)
    /// * `framework_id` - Identifier for the framework (used in error context)
    ///
    /// # Errors
    ///
    /// Returns a [`TestError`] variant depending on which assertion fails.
    pub fn run(&self, base_url: &str, framework_id: &str) -> TestResult<()> {
        match self {
            Self::HtmlInjection => {
                let resp = reqwest::blocking::get(base_url)
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: HtmlInjection, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach(format!("framework: {framework_id}"))?;

                assertions::assert_unique_script_tag(&html)
                    .attach(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::AttributeRewriting => {
                // Verify that absolute origin URLs in href/src attributes are
                // rewritten to the proxy host. The test fixtures embed links
                // like `http://127.0.0.1:8888/page` which the HTML processor
                // should rewrite to `http://127.0.0.1:{proxy_port}/page`.
                let resp = reqwest::blocking::get(base_url)
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: AttributeRewriting, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach(format!("framework: {framework_id}"))?;

                let origin_host = format!("127.0.0.1:{}", origin_port());

                assertions::assert_attributes_rewritten(&html, &origin_host, base_url)
                    .attach(format!("framework: {framework_id}"))?;

                // Verify URL attributes inside ad-slot elements are also rewritten
                assertions::assert_ad_slot_urls_rewritten(&html, &origin_host, base_url)
                    .attach(format!("framework: {framework_id}"))?;

                // Verify non-URL attributes like data-ad-unit are preserved unchanged
                assertions::assert_data_ad_units_preserved(
                    &html,
                    &["/test/banner", "/test/sidebar"],
                )
                .attach(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::ScriptServing => {
                let url = format!("{base_url}/static/tsjs=tsjs-unified.min.js");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: ScriptServing, framework: {framework_id}"
                    ))?;

                if !resp.status().is_success() {
                    return Err(Report::new(TestError::HttpRequest).attach(format!(
                        "Script serving returned {}, expected 2xx",
                        resp.status()
                    )));
                }

                // Verify content type is JavaScript
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                let body = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach(format!("framework: {framework_id}"))?;

                if !content_type.contains("javascript") {
                    return Err(Report::new(TestError::ResponseParse).attach(format!(
                        "Expected JavaScript content-type, got: {content_type}"
                    )));
                }

                // Verify body is non-empty and contains expected bundle markers
                if body.is_empty() {
                    return Err(
                        Report::new(TestError::ResponseParse).attach("Script bundle body is empty")
                    );
                }

                // The unified bundle should contain the TSJS core initialization
                if !body.contains("trustedserver") && !body.contains("tsjs") {
                    return Err(Report::new(TestError::ResponseParse).attach(
                        "Script bundle does not contain expected trustedserver/tsjs markers",
                    ));
                }

                Ok(())
            }

            Self::ScriptServingUnknownFile404 => {
                let url = format!("{base_url}/static/tsjs=unknown.js");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: ScriptServingUnknownFile404, framework: {framework_id}"
                    ))?;

                let status = resp.status().as_u16();

                if status != 404 {
                    return Err(Report::new(TestError::HttpRequest)
                        .attach(format!("Expected 404 for unknown tsjs file, got {status}")));
                }

                // Response should not be HTML (which would indicate a fallback to origin)
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                if content_type.contains("text/html") {
                    return Err(Report::new(TestError::ResponseParse)
                        .attach("Unknown tsjs file returned HTML instead of a proper 404"));
                }

                Ok(())
            }
        }
    }
}

impl CustomScenario {
    /// Execute this custom scenario against a running runtime.
    ///
    /// # Errors
    ///
    /// Returns a [`TestError`] variant depending on which assertion fails.
    pub fn run(&self, base_url: &str, framework_id: &str) -> TestResult<()> {
        match self {
            Self::NextJsRscFlight => {
                // Verify RSC Flight format responses are not corrupted.
                // When the proxy mishandles the RSC header, it returns text/html
                // instead of the expected Flight payload — so we fail on HTML.
                let client = reqwest::blocking::Client::new();
                let resp = client
                    .get(base_url)
                    .header("RSC", "1")
                    .header("Next-Router-State-Tree", "%5B%22%22%5D")
                    .send()
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: NextJsRscFlight, framework: {framework_id}"
                    ))?;

                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                let body = resp.text().change_context(TestError::ResponseParse)?;

                // RSC responses must NOT be HTML — that means the proxy
                // swallowed the RSC header and treated it as a page request
                if content_type.contains("text/html") {
                    return Err(Report::new(TestError::ResponseParse).attach(format!(
                            "RSC request returned text/html instead of Flight payload (content-type: {content_type})"
                        )));
                }

                // If the response is a Flight payload, it must not contain injected scripts
                if body.contains("/static/tsjs=") {
                    return Err(Report::new(TestError::UnexpectedScriptInjection)
                        .attach("Script tag should NOT be injected in RSC Flight responses"));
                }

                Ok(())
            }

            Self::NextJsServerActions => {
                // Verify POST requests pass through the proxy to the origin.
                // The minimal Next.js app has no real server actions, so
                // Next.js returns 404 for the unknown action ID. This proves
                // the proxy forwarded the POST to the origin rather than
                // rejecting or mishandling it.
                let client = reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .change_context(TestError::HttpRequest)?;

                let resp = client
                    .post(base_url)
                    .header("Next-Action", "test-action-id")
                    .header("Content-Type", "text/plain;charset=UTF-8")
                    .body("[]")
                    .send()
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: NextJsServerActions, framework: {framework_id}"
                    ))?;

                let status = resp.status().as_u16();
                let body = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach(format!(
                        "scenario: NextJsServerActions, framework: {framework_id}"
                    ))?;

                // Next.js returns 404 for unknown server action IDs.
                // With App Router client components in the layout, Next.js
                // may return a "soft 404" (HTTP 200 with a not-found page).
                // Accept 200, 404, or 405 — all prove the proxy forwarded
                // the POST to the origin rather than rejecting it.
                match status {
                    404 | 405 => {}
                    200 => {
                        // Soft 404: verify the body is a Next.js not-found page
                        if !body.contains("404")
                            && !body.contains("not found")
                            && !body.contains("Not Found")
                        {
                            return Err(Report::new(TestError::UnexpectedContent).attach(format!(
                                "Soft-404 body should contain a 404 indicator; \
                                     framework: {framework_id}, body: {body}"
                            )));
                        }
                    }
                    _ => {
                        return Err(
                            Report::new(TestError::HttpRequest).attach(
                                format!(
                                    "Expected 200/404/405 for unknown server action, got {status}; body: {body}"
                                ),
                            ),
                        );
                    }
                }

                Ok(())
            }

            Self::NextJsApiRoute => {
                // Verify API routes return JSON without HTML injection.
                // The proxy should pass JSON responses through unchanged.
                let url = format!("{base_url}/api/hello");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: NextJsApiRoute, framework: {framework_id}"
                    ))?;

                let status = resp.status().as_u16();
                if status != 200 {
                    return Err(Report::new(TestError::HttpRequest)
                        .attach(format!("Expected 200 for API route, got {status}")));
                }

                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                if !content_type.contains("application/json") {
                    return Err(Report::new(TestError::ResponseParse).attach(format!(
                        "Expected application/json content-type, got: {content_type}"
                    )));
                }

                let body = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach(format!("framework: {framework_id}"))?;

                // JSON responses must not contain HTML injection
                if body.contains("<script") || body.contains("/static/tsjs=") {
                    return Err(Report::new(TestError::ResponseParse).attach(
                            "API route response contains script injection — JSON should pass through unchanged",
                        ));
                }

                // Verify it's valid JSON with expected structure
                let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
                    Report::new(TestError::ResponseParse)
                        .attach(format!("API response is not valid JSON: {e}"))
                })?;

                if json.get("message").is_none() {
                    return Err(Report::new(TestError::ResponseParse)
                        .attach("API response missing expected 'message' field"));
                }

                Ok(())
            }

            Self::NextJsFormAction => {
                // Verify form action URLs are rewritten from origin to proxy.
                let url = format!("{base_url}/contact");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: NextJsFormAction, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach(format!("framework: {framework_id}"))?;

                let origin_host = format!("127.0.0.1:{}", origin_port());

                assertions::assert_form_action_rewritten(
                    &html,
                    "form#contact-form",
                    &origin_host,
                    base_url,
                )
                .attach(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::WordPressAdminInjection => {
                // Verify that /wp-admin/ pages also receive script injection.
                // The trusted server injects into ALL HTML responses regardless
                // of path. This test documents that behavior — if admin-path
                // exclusion is added in the future, this test should be updated
                // to assert NO injection instead.
                let url = format!("{base_url}/wp-admin/");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach(format!(
                        "scenario: WordPressAdminInjection, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach(format!("framework: {framework_id}"))?;

                assertions::assert_script_tag_present(&html).attach(format!(
                    "Admin page should receive injection (current behavior). \
                         framework: {framework_id}"
                ))?;

                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// EC identity lifecycle scenarios
// ---------------------------------------------------------------------------

/// EC identity lifecycle scenarios that test KV-backed stateful behavior.
///
/// These run against the Viceroy runtime directly without a frontend
/// framework container — they exercise EC-specific endpoints
/// (`/_ts/api/v1/identify`, `/_ts/api/v1/batch-sync`).
#[derive(Debug, Clone)]
pub enum EcScenario {
    /// Seeded EC row → batch sync writes partner UID → identify (Bearer auth)
    /// returns the scoped UID.
    FullLifecycle,

    /// Consent withdrawal: GPC header triggers EC cookie deletion for a
    /// seeded EC in the default US-state test geo.
    ConsentWithdrawal,

    /// Identify without EC cookie returns 204.
    IdentifyWithoutEc,

    /// Identify with consent denied returns 403.
    IdentifyConsentDenied,

    /// Batch sync two partners → identify each partner returns its own UID.
    ConcurrentPartnerSyncs,

    /// Batch sync happy path: authenticated request writes UID for a seeded EC.
    BatchSyncHappyPath,

    /// Batch sync auth rejection: no auth → 401, wrong auth → 401.
    BatchSyncAuthRejection,
}

impl EcScenario {
    /// All EC scenarios in order.
    pub fn all() -> Vec<Self> {
        vec![
            Self::FullLifecycle,
            Self::ConsentWithdrawal,
            Self::IdentifyWithoutEc,
            Self::IdentifyConsentDenied,
            Self::ConcurrentPartnerSyncs,
            Self::BatchSyncHappyPath,
            Self::BatchSyncAuthRejection,
        ]
    }

    /// Execute this EC scenario against a running Viceroy instance.
    ///
    /// Each scenario creates its own `EcTestClient` to isolate cookie state.
    ///
    /// # Errors
    ///
    /// Returns [`TestError`] on assertion failures.
    pub fn run(&self, base_url: &str) -> TestResult<()> {
        match self {
            Self::FullLifecycle => ec_full_lifecycle(base_url),
            Self::ConsentWithdrawal => ec_consent_withdrawal(base_url),
            Self::IdentifyWithoutEc => ec_identify_without_ec(base_url),
            Self::IdentifyConsentDenied => ec_identify_consent_denied(base_url),
            Self::ConcurrentPartnerSyncs => ec_concurrent_partner_syncs(base_url),
            Self::BatchSyncHappyPath => ec_batch_sync_happy_path(base_url),
            Self::BatchSyncAuthRejection => ec_batch_sync_auth_rejection(base_url),
        }
    }
}

/// US Privacy signal that explicitly allows storage in the default Viceroy
/// integration-test geo (US-CA).
const ALLOW_US_PRIVACY_COOKIE: &str = "1YNN";
fn allow_ec_generation(client: &EcTestClient) {
    client.set_cookie("us_privacy", ALLOW_US_PRIVACY_COOKIE);
}

fn seeded_ec_id(hex_digit: char, suffix: &str) -> String {
    format!("{}.{suffix}", hex_digit.to_string().repeat(64))
}

fn use_seeded_ec(client: &EcTestClient, ec_id: &str) -> String {
    client.set_cookie("ts-ec", ec_id);
    normalize_ec_id(ec_id)
}

/// Full lifecycle: seeded EC → batch sync → identify (Bearer auth) with scoped UID.
///
/// Uses the `inttest` partner injected by the integration-test build.
fn ec_full_lifecycle(base_url: &str) -> TestResult<()> {
    let client = EcTestClient::new(base_url);
    allow_ec_generation(&client);
    let seeded_ec_id = seeded_ec_id('a', "test01");
    let ec_id = use_seeded_ec(&client, &seeded_ec_id);
    log::info!("EC full lifecycle: using seeded EC ID = {ec_id}");

    // 2. Batch sync writes partner UID (partner "inttest" is injected)
    let mappings = vec![BatchMapping {
        ec_id: ec_id.clone(),
        partner_uid: "user-uid-42".to_owned(),
        timestamp: 1_700_000_000,
    }];
    let resp = batch_sync(&client, INTTEST_API_TOKEN, &mappings)?;
    let json = assert_json_response(resp, 200)
        .attach("EC full lifecycle: batch sync should return 200")?;

    let accepted = json.get("accepted").and_then(serde_json::Value::as_u64);
    if accepted != Some(1) {
        return Err(Report::new(TestError::JsonFieldMismatch {
            field: "accepted".to_owned(),
        })
        .attach(format!(
            "expected accepted=1, got {:?}; body: {json}",
            accepted
        )));
    }

    // 3. Identify with Bearer auth should return the synced UID
    let json = assert_json_response(identify(&client, INTTEST_API_TOKEN)?, 200)
        .attach("EC full lifecycle: identify after batch sync")?;

    let source_domain = json.get("source_domain").and_then(|v| v.as_str());
    if source_domain != Some("inttest.example.com") {
        return Err(Report::new(TestError::JsonFieldMismatch {
            field: "source_domain".to_owned(),
        })
        .attach(format!(
            "expected source_domain 'inttest.example.com', got {:?}; body: {json}",
            source_domain
        )));
    }

    let uid_value = json.get("uid").and_then(|v| v.as_str());
    if uid_value != Some("user-uid-42") {
        return Err(Report::new(TestError::JsonFieldMismatch {
            field: "uid".to_owned(),
        })
        .attach(format!(
            "expected uid 'user-uid-42', got {:?}; body: {json}",
            uid_value
        )));
    }

    log::info!("EC full lifecycle: PASSED");
    Ok(())
}

/// Consent withdrawal: GPC header clears EC cookie.
fn ec_consent_withdrawal(base_url: &str) -> TestResult<()> {
    let client = EcTestClient::new(base_url);
    allow_ec_generation(&client);
    let seeded_ec_id = seeded_ec_id('b', "test02");
    let ec_id = use_seeded_ec(&client, &seeded_ec_id);
    log::info!("EC consent withdrawal: using seeded EC = {ec_id}");

    // GPC overrides the allow cookie in US-CA, so this is an explicit
    // withdrawal and must expire the EC cookie.
    let resp = client.get_with_headers("/", &[("sec-gpc", "1")])?;

    if !is_ec_cookie_expired(&resp) {
        return Err(Report::new(TestError::UnexpectedContent)
            .attach("consent withdrawal should expire ts-ec cookie (expected Max-Age=0)"));
    }
    if client.ec_cookie_value().is_some() {
        return Err(Report::new(TestError::UnexpectedContent)
            .attach("client should stop tracking ts-ec after explicit withdrawal"));
    }

    // 3. With consent still granted and the EC cookie revoked, identify should
    // now report no EC present.
    let resp = identify(&client, INTTEST_API_TOKEN)?;
    assert_status(&resp, 204).attach("identify should return 204 after cookie revocation")?;

    // 4. With GPC still asserted, identify should reflect consent denial.
    let resp = identify_with_headers(&client, INTTEST_API_TOKEN, &[("sec-gpc", "1")])?;
    assert_status(&resp, 403)
        .attach("identify with GPC should return 403 after consent withdrawal")?;

    log::info!("EC consent withdrawal: PASSED");
    Ok(())
}

/// Identify without EC cookie returns 204 No Content.
fn ec_identify_without_ec(base_url: &str) -> TestResult<()> {
    let client = EcTestClient::new(base_url);
    allow_ec_generation(&client);

    let resp = identify(&client, INTTEST_API_TOKEN)?;
    assert_status(&resp, 204)
        .attach("identify without EC cookie should return 204 when consent is granted")?;

    log::info!("EC identify without EC: PASSED");
    Ok(())
}

/// Identify with consent denied returns 403.
fn ec_identify_consent_denied(base_url: &str) -> TestResult<()> {
    let client = EcTestClient::new(base_url);
    allow_ec_generation(&client);
    let seeded_ec_id = seeded_ec_id('c', "test03");
    let _ec_id = use_seeded_ec(&client, &seeded_ec_id);

    // Identify with GPC=1 — in the default US-CA test geo, GPC is an explicit
    // denial that must override the allow cookie. Per spec §11.4, consent is
    // evaluated after Bearer auth, so this must be 403 Forbidden.
    let resp = identify_with_headers(&client, INTTEST_API_TOKEN, &[("sec-gpc", "1")])?;

    let status = resp.status().as_u16();
    if status != 403 {
        return Err(Report::new(TestError::UnexpectedStatusCode {
            expected: 403,
            actual: status,
        })
        .attach("identify with consent denied should return 403"));
    }

    log::info!("EC identify consent denied: PASSED (status={status})");
    Ok(())
}

/// Batch sync two config-based partners → identify each returns its own scoped UID.
fn ec_concurrent_partner_syncs(base_url: &str) -> TestResult<()> {
    let client = EcTestClient::new(base_url);
    allow_ec_generation(&client);
    let seeded_ec_id = seeded_ec_id('d', "test04");
    let ec_id = use_seeded_ec(&client, &seeded_ec_id);
    log::info!("EC concurrent syncs: using seeded EC = {ec_id}");

    // Batch sync both partners injected by the integration-test build.
    let mappings_a = vec![BatchMapping {
        ec_id: ec_id.clone(),
        partner_uid: "uid-a".to_owned(),
        timestamp: 1_700_000_000,
    }];
    let resp = batch_sync(&client, INTTEST_API_TOKEN, &mappings_a)?;
    assert_json_response(resp, 200).attach("batch sync inttest should succeed")?;

    let mappings_b = vec![BatchMapping {
        ec_id: ec_id.clone(),
        partner_uid: "uid-b".to_owned(),
        timestamp: 1_700_000_000,
    }];
    let resp = batch_sync(&client, INTTEST2_API_TOKEN, &mappings_b)?;
    assert_json_response(resp, 200).attach("batch sync inttest2 should succeed")?;

    // Identify as inttest → should see only inttest's UID
    let json = assert_json_response(identify(&client, INTTEST_API_TOKEN)?, 200)
        .attach("identify as inttest after dual sync")?;
    let uid = json.get("uid").and_then(|v| v.as_str());
    if uid != Some("uid-a") {
        return Err(Report::new(TestError::JsonFieldMismatch {
            field: "uid".to_owned(),
        })
        .attach(format!(
            "inttest expected 'uid-a', got {:?}; body: {json}",
            uid
        )));
    }

    // Identify as inttest2 → should see only inttest2's UID
    let json = assert_json_response(identify(&client, INTTEST2_API_TOKEN)?, 200)
        .attach("identify as inttest2 after dual sync")?;
    let uid = json.get("uid").and_then(|v| v.as_str());
    if uid != Some("uid-b") {
        return Err(Report::new(TestError::JsonFieldMismatch {
            field: "uid".to_owned(),
        })
        .attach(format!(
            "inttest2 expected 'uid-b', got {:?}; body: {json}",
            uid
        )));
    }

    log::info!("EC concurrent partner syncs: PASSED");
    Ok(())
}

/// Batch sync happy path: authenticated request writes UID, verify via identify.
///
/// Uses the `inttest` partner injected by the integration-test build.
fn ec_batch_sync_happy_path(base_url: &str) -> TestResult<()> {
    let client = EcTestClient::new(base_url);
    allow_ec_generation(&client);
    let seeded_ec_id = seeded_ec_id('e', "test05");
    let ec_id = use_seeded_ec(&client, &seeded_ec_id);
    log::info!("EC batch sync happy path: using seeded ec_id = {ec_id}");

    // Batch sync writes a UID for this EC ID (partner "inttest" is injected)
    let mappings = vec![BatchMapping {
        ec_id: ec_id.clone(),
        partner_uid: "batch-uid-99".to_owned(),
        timestamp: 1_700_000_000,
    }];
    let resp = batch_sync(&client, INTTEST_API_TOKEN, &mappings)?;
    let json = assert_json_response(resp, 200).attach("batch sync should return 200")?;

    let accepted = json.get("accepted").and_then(serde_json::Value::as_u64);
    if accepted != Some(1) {
        return Err(Report::new(TestError::JsonFieldMismatch {
            field: "accepted".to_owned(),
        })
        .attach(format!(
            "expected accepted=1, got {:?}; body: {json}",
            accepted
        )));
    }

    // Verify via identify (Bearer auth, scoped response)
    let json = assert_json_response(identify(&client, INTTEST_API_TOKEN)?, 200)
        .attach("identify after batch sync")?;

    let uid = json.get("uid").and_then(|v| v.as_str());
    if uid != Some("batch-uid-99") {
        return Err(Report::new(TestError::JsonFieldMismatch {
            field: "uid".to_owned(),
        })
        .attach(format!(
            "expected 'batch-uid-99', got {:?}; body: {json}",
            uid
        )));
    }

    log::info!("EC batch sync happy path: PASSED");
    Ok(())
}

/// Batch sync auth rejection: no auth → 401, wrong auth → 401.
fn ec_batch_sync_auth_rejection(base_url: &str) -> TestResult<()> {
    let client = EcTestClient::new(base_url);

    let dummy_mappings = vec![BatchMapping {
        ec_id: format!("{}.ABC123", "a".repeat(64)),
        partner_uid: "uid-1".to_owned(),
        timestamp: 1_700_000_000,
    }];

    // No auth header
    let resp = batch_sync_no_auth(&client, &dummy_mappings)?;
    assert_status(&resp, 401).attach("batch sync without auth should return 401")?;

    // Wrong bearer token
    let resp = batch_sync(&client, "completely-wrong-key", &dummy_mappings)?;
    assert_status(&resp, 401).attach("batch sync with wrong auth should return 401")?;

    log::info!("EC batch sync auth rejection: PASSED");
    Ok(())
}
