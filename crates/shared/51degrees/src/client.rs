//! The one call to the cloud service for a request's evidence, and the
//! answer it gives.
//!
//! The call is a form POST to the service's `json` endpoint, made over the
//! platform's HTTP client. It presents the license key, never the resource
//! key. A resource key is restricted to the domains it was created for,
//! checked against the `Referer` or `Origin` header, neither of which a
//! server to server call sends, so it answers `401`. A license key has no
//! such restriction and answers with only the properties the call names.
//!
//! Answers load through the loading cache in [`crate::engine_cache`], each
//! whole call being the load, so where a deployment links a loading cache
//! module a question many requests ask at once is asked once. A stock build
//! links none, so there every call asks the service. The evidence is the
//! `User-Agent` and the `Sec-CH-UA` hints alone, so one held answer serves
//! every reader whose browser sends the same values. Failures are not held.

use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use trusted_server_core::platform::{
    LoadError, LoadedValue, Loader, PlatformBackendSpec, PlatformError, PlatformHttpRequest,
    RuntimeServices,
};

use crate::BACKEND_DISCRIMINATOR;
use crate::engine_cache::{AnswerCache, answer_key};
use crate::hints_engine::engine_base;

/// The evidence one cloud call sends, as the fields [`evidence_fields`] gives.
/// The call's key in the loading cache is built from the same fields the call
/// sends, so the key cannot drift from the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// The visitor's `User-Agent`.
    pub user_agent: String,
    /// The `Sec-CH-UA` hints the browser sent, by lower case header name and
    /// in name order. Different hints are a different question, so they are
    /// held as a different entry.
    pub client_hints: Vec<(String, String)>,
}

/// A decoded cloud response, the answer to one call's question.
#[derive(Debug, Clone)]
pub struct CloudAnswer {
    body: serde_json::Value,
}

impl CloudAnswer {
    /// Wraps a decoded response.
    #[must_use]
    pub const fn new(body: serde_json::Value) -> Self {
        Self { body }
    }

    /// The whole decoded response.
    #[must_use]
    pub const fn body(&self) -> &serde_json::Value {
        &self.body
    }

    /// One element of the response, such as `device`.
    ///
    /// `None` when the credential is not entitled to that product, which is
    /// ordinary, and the module reading it resolves nothing rather than
    /// erroring.
    #[must_use]
    pub fn element(&self, name: &str) -> Option<&serde_json::Value> {
        self.body.get(name)
    }
}

/// The form fields the call sends for `evidence`, under the bare header names
/// the service reads.
///
/// No usage is sent, so no identifier comes back. An empty value is left out,
/// because sending one asks about an empty `User-Agent`.
#[must_use]
pub fn evidence_fields(evidence: &Evidence) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    if !evidence.user_agent.trim().is_empty() {
        fields.push(("user-agent".to_owned(), evidence.user_agent.clone()));
    }
    for (name, value) in &evidence.client_hints {
        if !value.is_empty() {
            fields.push((name.clone(), value.clone()));
        }
    }
    fields
}

/// The file the service answers data requests at, under the endpoint's base.
const DATA_ENDPOINT: &str = "json";

/// Cap on the body read back from the service, so a wrong endpoint cannot grow
/// the instance's heap without bound.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Calls the cloud service. The 51Degrees modules share one, and each
/// registers the properties it reads, so one call for a request carries them
/// all.
///
/// Each answer loads through the loading cache, so where a deployment links a
/// loading cache module a question many requests ask at once is asked once.
#[derive(Debug, Clone)]
pub struct CloudPipeline {
    /// The base the data endpoint is under, being the configured endpoint
    /// without its last path segment, which is the segment a multi-tenant
    /// endpoint carries the resource key in, so this call cannot present it.
    base: String,
    /// The license key the call authenticates with. Never a resource key.
    license_key: String,
    timeout: Duration,
    /// Where the answers are held, and for which publisher.
    cache: AnswerCache,
    /// Every property a module of this deployment reads from an answer,
    /// registered with [`wanting`](Self::wanting) when the deployment starts,
    /// so one call carries them all.
    properties: Vec<&'static str>,
}

impl CloudPipeline {
    /// Creates a pipeline for one endpoint and license key, holding its
    /// answers in `cache` and asking for nothing until a module registers
    /// what it reads.
    ///
    /// Returns `None` when the endpoint is not a URL, which startup validation
    /// refuses anyway. There is no constructor taking a resource key, so a
    /// server side call cannot present one.
    #[must_use]
    pub fn new(
        endpoint: &str,
        license_key: &str,
        timeout_ms: u32,
        cache: AnswerCache,
    ) -> Option<Self> {
        Some(Self {
            base: engine_base(endpoint)?,
            license_key: license_key.to_owned(),
            timeout: Duration::from_millis(u64::from(timeout_ms)),
            cache,
            properties: Vec::new(),
        })
    }

    /// Registers the properties a module reads from every answer, so the one
    /// call a request makes carries them. Called for each module when the
    /// deployment starts, and a property registered twice is asked for once.
    #[must_use]
    pub fn wanting(mut self, properties: &[&'static str]) -> Self {
        for property in properties {
            if !self.properties.contains(property) {
                self.properties.push(property);
            }
        }
        self
    }

    /// Every property the registered modules read, in registration order.
    #[must_use]
    pub fn properties(&self) -> &[&'static str] {
        &self.properties
    }

    /// The answer for a request's evidence, held from an earlier call or
    /// asked of the service.
    ///
    /// # Errors
    ///
    /// When the service cannot be reached, refuses the call, or answers with
    /// something that is not JSON.
    pub async fn answer(
        &self,
        evidence: &Evidence,
        services: &RuntimeServices,
    ) -> Result<CloudAnswer, Report<PlatformError>> {
        let bytes = self
            .call_bytes(
                &self.properties,
                evidence_fields(evidence),
                crate::engine_cache::ANSWER_LIFETIME,
                services,
            )
            .await?;
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<CloudAnswer, Report<PlatformError>> {
        let body: serde_json::Value = serde_json::from_slice(bytes)
            .change_context(PlatformError::HttpClient)
            .attach("the held answer is not JSON")?;
        Ok(CloudAnswer::new(body))
    }

    /// The bytes held for this question, or the service's answer, which is
    /// then held for `lifetime`.
    async fn call_bytes(
        &self,
        values: &[&str],
        evidence: Vec<(String, String)>,
        lifetime: Duration,
        services: &RuntimeServices,
    ) -> Result<bytes::Bytes, Report<PlatformError>> {
        let key = answer_key(&self.base, &self.license_key, values, &evidence);
        let pipeline = self.clone();
        let values: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        let call_services = services.clone();
        let load: Loader = Box::new(move || {
            Box::pin(async move {
                let json = pipeline
                    .ask_service(values, evidence, &call_services)
                    .await
                    .map_err(|report| LoadError::new(format!("{report:?}")))?;
                Ok(LoadedValue::new(json.into_bytes()).with_lifetime(lifetime))
            })
        });
        self.cache
            .held(services, key, load)
            .await
            .map_err(|error| Report::new(PlatformError::HttpClient).attach(error.to_string()))
    }

    /// Sends one data request and returns the answer's JSON text.
    ///
    /// The license key and the property list lead the form, as the published
    /// engines send them, and the evidence follows under the bare names the
    /// service reads.
    async fn ask_service(
        &self,
        values: Vec<String>,
        evidence: Vec<(String, String)>,
        services: &RuntimeServices,
    ) -> Result<String, Report<PlatformError>> {
        let mut form = vec![
            ("license".to_owned(), self.license_key.clone()),
            ("values".to_owned(), values.join(",")),
        ];
        form.extend(evidence);
        let outbound = http::Request::builder()
            .method(http::Method::POST)
            .uri(format!("{}{DATA_ENDPOINT}", self.base))
            .header(http::header::ACCEPT, "application/json")
            .header(
                http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(edgezero_core::body::Body::from(
                form_urlencoded_body(&form).into_bytes(),
            ))
            .change_context(PlatformError::HttpClient)
            .attach("could not build the cloud request")?;

        let backend = services
            .backend()
            .ensure(&backend_spec(&self.base, self.timeout)?)
            .change_context(PlatformError::HttpClient)
            .attach("could not resolve a backend for the cloud endpoint")?;
        let response = services
            .http_client()
            .send(PlatformHttpRequest::new(outbound, backend))
            .await
            .change_context(PlatformError::HttpClient)
            .attach("the cloud service did not answer")?;

        let status = response.response.status();
        let bytes = response
            .response
            .into_body()
            .into_bytes_bounded(MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| {
                Report::new(PlatformError::HttpClient)
                    .attach("could not read the cloud service's answer")
            })?;
        let json = String::from_utf8_lossy(&bytes).into_owned();
        if !status.is_success() {
            return Err(Report::new(PlatformError::HttpClient).attach(format!(
                "the cloud service answered {status}: {}",
                excerpt(&json)
            )));
        }
        // Checked before it is held, so what is held is always an answer. The
        // service refuses a call with a list of errors and a success status,
        // so that list is read as the refusal it is.
        let body: serde_json::Value = serde_json::from_str(&json)
            .change_context(PlatformError::HttpClient)
            .attach("the cloud service did not return JSON")?;
        if let Some(errors) = body.get("errors").and_then(serde_json::Value::as_array)
            && !errors.is_empty()
        {
            return Err(Report::new(PlatformError::HttpClient).attach(format!(
                "the cloud service refused the call: {}",
                excerpt(&serde_json::Value::Array(errors.clone()).to_string())
            )));
        }
        Ok(json)
    }
}

/// The start of `text`, for a message about an answer that was not one.
fn excerpt(text: &str) -> &str {
    let end = text
        .char_indices()
        .nth(200)
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

/// The backend the data endpoint under `base` is reached through, with the
/// deployment's own timeout on the connection.
fn backend_spec(
    base: &str,
    timeout: Duration,
) -> Result<PlatformBackendSpec, Report<PlatformError>> {
    let url = url::Url::parse(base)
        .change_context(PlatformError::HttpClient)
        .attach("the cloud endpoint is not a URL")?;
    let host = url
        .host_str()
        .ok_or_else(|| {
            Report::new(PlatformError::HttpClient).attach("the cloud endpoint names no host")
        })?
        .to_owned();
    Ok(PlatformBackendSpec {
        scheme: url.scheme().to_owned(),
        host,
        port: url.port(),
        host_header_override: None,
        certificate_check: true,
        first_byte_timeout: timeout,
        between_bytes_timeout: timeout,
        discriminator: Some(BACKEND_DISCRIMINATOR.to_owned()),
    })
}

/// The url-encoded form body the data endpoint expects.
fn form_urlencoded_body(form: &[(String, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in form {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

/// The properties in which the service names the hints it wants, one per
/// product, so the header follows the service.
const ACCEPT_CH_PROPERTIES: &[&str] = &[
    "setheaderbrowseraccept-ch",
    "setheaderhardwareaccept-ch",
    "setheaderplatformaccept-ch",
];

/// The `Accept-CH` value the service asked for, or `None` when it named none.
/// The three properties overlap, so their union is taken in order of first
/// appearance, which keeps the header stable between requests.
#[must_use]
pub fn accept_ch_from_answer(answer: &CloudAnswer) -> Option<String> {
    let device = answer.element("device")?;
    let mut hints: Vec<&str> = Vec::new();
    for property in ACCEPT_CH_PROPERTIES {
        let Some(value) = device.get(*property).and_then(serde_json::Value::as_str) else {
            continue;
        };
        for hint in value.split(',').map(str::trim) {
            if hint.is_empty() || hint.eq_ignore_ascii_case(crate::UNKNOWN) {
                continue;
            }
            if !hints.iter().any(|seen| seen.eq_ignore_ascii_case(hint)) {
                hints.push(hint);
            }
        }
    }
    (!hints.is_empty()).then(|| hints.join(", "))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_services::{
        MemoryKvStore, MemoryValueCache, ScriptedAnswer, ScriptedHttpClient, services_with,
        services_with_value_cache,
    };

    /// What the data endpoint answers.
    const DATA: &str = r#"{"device":{"ismobile":false,"browsername":"Chrome"}}"#;

    const PROPERTIES: &[&str] = &["device.IsMobile", "device.BrowserName"];

    fn evidence() -> Evidence {
        Evidence {
            user_agent: "Mozilla/5.0".to_owned(),
            client_hints: Vec::new(),
        }
    }

    fn pipeline() -> CloudPipeline {
        CloudPipeline::new(
            "https://cloud.example.com/api/v4/a-public-resource-key.json",
            "a-secret-license-key",
            500,
            crate::test_services::answer_cache(),
        )
        .expect("should build")
        .wanting(PROPERTIES)
    }

    fn browser_name(answer: &CloudAnswer) -> Option<&str> {
        answer
            .element("device")
            .and_then(|device| device["browsername"].as_str())
    }

    // The credential.

    /// A server side call presents the license key and never the public
    /// resource key, which answers `401` to a call that sends no `Referer`
    /// or `Origin`.
    #[tokio::test]
    async fn the_call_presents_the_license_key_and_never_the_resource_key() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        let answer = pipeline()
            .answer(&evidence(), &services)
            .await
            .expect("the call should succeed");

        let sent = client.sent();
        let data = sent.last().expect("the data request should have been sent");
        assert!(
            data.body.contains("license=a-secret-license-key"),
            "got {}",
            data.body
        );
        assert!(
            !data.body.contains("resource="),
            "a resource key is public by design and a call the browser never sees has no \
             business spending it: {}",
            data.body
        );
        assert!(
            !sent.iter().any(|request| {
                request.uri.contains("a-public-resource-key")
                    || request.body.contains("a-public-resource-key")
            }),
            "and the key in the configured endpoint path must not reach the address or the \
             body either: {sent:?}"
        );
        assert_eq!(browser_name(&answer), Some("Chrome"));
    }

    // What is asked, and where.

    /// The data request is a form POST to the `json` file under the
    /// endpoint's base, as the published engines send it.
    #[tokio::test]
    async fn the_call_is_a_form_post_to_the_data_endpoint() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        pipeline()
            .answer(&evidence(), &services)
            .await
            .expect("the call should succeed");

        let sent = client.sent();
        assert_eq!(sent.len(), 1, "one data call and no discovery");
        assert_eq!(sent[0].uri, "https://cloud.example.com/api/v4/json");
        assert_eq!(sent[0].method, "POST");
        assert!(
            sent[0].headers.iter().any(|(name, value)| {
                name == "content-type" && value == "application/x-www-form-urlencoded"
            }),
            "{:?}",
            sent[0].headers
        );
    }

    /// The `User-Agent` and each hint reach the service under the bare
    /// header names it reads.
    #[tokio::test]
    async fn the_evidence_reaches_the_service_under_the_bare_names_it_reads() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));
        let with_hint = Evidence {
            client_hints: vec![("sec-ch-ua-mobile".to_owned(), "?0".to_owned())],
            ..evidence()
        };

        pipeline()
            .answer(&with_hint, &services)
            .await
            .expect("the call should succeed");

        let sent = client.sent();
        let body = &sent.last().expect("the data request").body;
        assert!(
            body.contains("user-agent=Mozilla%2F5.0") && body.contains("sec-ch-ua-mobile=%3F0"),
            "the bare evidence names are what the service reads, got {body}"
        );
    }

    /// An empty value is not evidence, and sending one asks the service to
    /// describe an empty `User-Agent`.
    #[test]
    fn an_empty_value_is_left_out_of_the_evidence() {
        let hints_only = Evidence {
            user_agent: String::new(),
            client_hints: vec![
                ("sec-ch-ua-mobile".to_owned(), "?0".to_owned()),
                ("sec-ch-ua-model".to_owned(), String::new()),
            ],
        };

        assert_eq!(
            evidence_fields(&hints_only),
            vec![("sec-ch-ua-mobile".to_owned(), "?0".to_owned())]
        );
    }

    /// A server side call never asks for an identifier, whatever the visitor
    /// has answered, because an identifier is created only in the browser.
    #[test]
    fn a_server_side_call_never_asks_for_an_identifier() {
        assert!(
            evidence_fields(&evidence())
                .iter()
                .all(|(key, _)| !key.ends_with("usage")),
            "no usage may reach a server side call",
        );
    }

    /// A license key names nothing of its own, so the call names every
    /// property the modules registered, and nothing else comes back.
    #[tokio::test]
    async fn a_server_side_call_names_every_property_it_wants() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        pipeline()
            .answer(&evidence(), &services)
            .await
            .expect("the call should succeed");

        let sent = client.sent();
        let body = &sent.last().expect("the data request").body;
        let asked: Vec<String> = body
            .split('&')
            .filter_map(|field| field.strip_prefix("values="))
            .flat_map(|list| list.split("%2C"))
            .map(str::to_owned)
            .collect();

        assert_eq!(
            asked,
            PROPERTIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
            "a license key names nothing of its own, so a call that names nothing gets \
             nothing: {body}"
        );
    }

    #[test]
    fn a_property_registered_twice_is_asked_for_once() {
        let pipeline = pipeline().wanting(&["device.IsMobile", "device.HardwareName"]);

        assert_eq!(
            pipeline.properties(),
            &[
                "device.IsMobile",
                "device.BrowserName",
                "device.HardwareName"
            ]
        );
    }

    // The cache, counted.

    /// The cache reduces calls. Two instances share only the key-value store,
    /// which holds the answers on a host with no value cache, and the second
    /// asks the service nothing.
    #[tokio::test]
    async fn a_second_instance_with_the_same_evidence_asks_the_cloud_nothing() {
        let store = Arc::new(MemoryKvStore::default());
        let first = Arc::new(ScriptedHttpClient::new(&[DATA]));
        pipeline()
            .answer(&evidence(), &services_with(first.clone(), store.clone()))
            .await
            .expect("the first call should succeed");
        assert_eq!(
            first.sent().len(),
            1,
            "the first instance pays for the call"
        );

        let second = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let answer = pipeline()
            .answer(&evidence(), &services_with(second.clone(), store))
            .await
            .expect("the second call should succeed");

        assert_eq!(
            second.sent().len(),
            0,
            "the answer was paid for by the first instance, so a second one with the same \
             evidence has nothing to ask"
        );
        assert_eq!(
            browser_name(&answer),
            Some("Chrome"),
            "and the answer still comes back"
        );
    }

    /// With a host value cache and no key-value store, a second lookup with
    /// the same evidence asks the cloud nothing.
    #[tokio::test]
    async fn the_host_value_cache_answers_a_second_lookup_without_the_store() {
        let cache = Arc::new(MemoryValueCache::default());
        let first = Arc::new(ScriptedHttpClient::new(&[DATA]));
        pipeline()
            .answer(
                &evidence(),
                &services_with_value_cache(first.clone(), cache.clone()),
            )
            .await
            .expect("the first lookup should succeed");
        assert_eq!(first.sent().len(), 1, "the first lookup pays for the call");
        assert_eq!(cache.len(), 1, "the answer is held");

        let second = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let answer = pipeline()
            .answer(
                &evidence(),
                &services_with_value_cache(second.clone(), cache),
            )
            .await
            .expect("the second lookup should succeed");

        assert_eq!(
            second.sent().len(),
            0,
            "the second lookup with the same evidence is answered from the value cache"
        );
        assert_eq!(browser_name(&answer), Some("Chrome"));
    }

    /// Evidence that differs from a question already answered is a question of
    /// its own, so it costs a call.
    #[tokio::test]
    async fn different_evidence_still_costs_a_call() {
        for (case, changed, why) in [
            (
                "a different User-Agent",
                Evidence {
                    user_agent: "curl/8".to_owned(),
                    ..evidence()
                },
                "the answer depends on the User-Agent",
            ),
            (
                "a hint the browser sent",
                Evidence {
                    client_hints: vec![("sec-ch-ua-mobile".to_owned(), "?1".to_owned())],
                    ..evidence()
                },
                "a hint changes the question",
            ),
        ] {
            let store = Arc::new(MemoryKvStore::default());
            let first = Arc::new(ScriptedHttpClient::new(&[DATA]));
            pipeline()
                .answer(&evidence(), &services_with(first, store.clone()))
                .await
                .expect("the first call should succeed");

            let second = Arc::new(ScriptedHttpClient::new(&[DATA]));
            pipeline()
                .answer(&changed, &services_with(second.clone(), store))
                .await
                .expect("the second call should succeed");

            assert_eq!(
                second.sent().len(),
                1,
                "{case} costs one call, because {why}"
            );
        }
    }

    /// Two requests asking the same question at the same moment, on two
    /// threads, make one call to the service between them, and both get the
    /// answer. The first asks while the second waits for it.
    #[test]
    fn two_requests_asking_at_once_make_one_call_between_them() {
        let client =
            Arc::new(ScriptedHttpClient::new(&[DATA]).with_delay(Duration::from_millis(200)));
        // One instance, so one loading cache, as a native server's requests
        // share.
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));
        let start = std::sync::Barrier::new(2);

        let names: Vec<Option<String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("should build a runtime");
                        start.wait();
                        let answer = runtime
                            .block_on(pipeline().answer(&evidence(), &services))
                            .expect("each request should be answered");
                        browser_name(&answer).map(str::to_owned)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("a request should not panic"))
                .collect()
        });

        assert_eq!(client.sent().len(), 1, "one call between both requests");
        assert_eq!(names, vec![Some("Chrome".to_owned()); 2]);
    }

    /// An answer is held in the store for an hour, so the store is told the
    /// time left, which is a moment less by the time the answer is written.
    #[tokio::test]
    async fn each_answer_is_held_for_its_own_lifetime() {
        const HOUR: Duration = Duration::from_secs(60 * 60);
        let store = Arc::new(MemoryKvStore::default());
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));

        pipeline()
            .answer(&evidence(), &services_with(client, store.clone()))
            .await
            .expect("the call should succeed");

        let prefix = format!(
            "ts.publisher.example/{}/answer-",
            crate::engine_cache::CACHE
        );
        let held = store
            .keys()
            .into_iter()
            .find(|key| key.starts_with(&prefix))
            .and_then(|key| store.lifetime(&key));
        assert!(
            held.is_some_and(|held| held <= HOUR && HOUR - held < Duration::from_secs(1)),
            "{held:?}, among {:?}",
            store.keys()
        );
    }

    // Failure.

    /// A failed lookup stays a failure, and nothing is held for the next
    /// request to read back.
    #[tokio::test]
    async fn a_service_that_answers_nonsense_is_an_error_and_not_an_empty_answer() {
        let client = Arc::new(ScriptedHttpClient::new(&["not json at all"]));
        let services = services_with(client, Arc::new(MemoryKvStore::default()));

        assert!(
            pipeline().answer(&evidence(), &services).await.is_err(),
            "a failed lookup is not the same answer as a visitor the service knows nothing of"
        );
    }

    /// The service refuses a call it cannot serve with a status, or with a
    /// list of errors and a success status, and either is a failure that
    /// names what the service said rather than an answer.
    #[tokio::test]
    async fn a_refusal_is_an_error_naming_what_the_service_said() {
        for (case, answer, said) in [
            (
                "a status",
                ScriptedAnswer::new(400, r#"{"errors":["license key not valid"]}"#),
                "answered 400",
            ),
            (
                "a list of errors with a success status",
                ScriptedAnswer::new(200, r#"{"errors":["license key not valid"]}"#),
                "refused the call",
            ),
        ] {
            let client = Arc::new(ScriptedHttpClient::answering([answer]));
            let services = services_with(client, Arc::new(MemoryKvStore::default()));

            let message = format!(
                "{:?}",
                pipeline()
                    .answer(&evidence(), &services)
                    .await
                    .expect_err(case)
            );

            assert!(
                message.contains(said) && message.contains("license key not valid"),
                "{case}: {message}"
            );
        }
    }

    #[test]
    fn an_endpoint_that_is_not_a_url_yields_no_pipeline() {
        assert!(
            CloudPipeline::new(
                "not-a-url",
                "license",
                500,
                crate::test_services::answer_cache()
            )
            .is_none(),
            "startup validation refuses one anyway, so this is the belt"
        );
    }

    #[test]
    fn the_form_body_is_url_encoded() {
        let body = form_urlencoded_body(&[
            ("license".to_owned(), "a key".to_owned()),
            (
                "values".to_owned(),
                "device.SetHeaderBrowserAccept-CH".to_owned(),
            ),
        ]);

        assert_eq!(
            body, "license=a+key&values=device.SetHeaderBrowserAccept-CH",
            "the data endpoint reads a form body, and an unencoded space ends the field"
        );
    }

    // What the service asks the browser for.

    /// The `Accept-CH` value is the union of the three properties in order of
    /// first appearance, so the header is stable between requests and
    /// comparable in a log. `Unknown` is how the service spells absent, so it
    /// is never asked for, and an answer naming no hints gives no value,
    /// because an empty header would tell a browser to stop sending the hints
    /// it already sends.
    #[test]
    fn the_accept_ch_value_is_the_union_of_the_hints_the_service_named() {
        for (case, body, expected) in [
            (
                "three overlapping properties, as the live service answers",
                serde_json::json!({"device": {
                    "setheaderbrowseraccept-ch":
                        "Sec-CH-UA,Sec-CH-UA-Full-Version-List,Sec-CH-UA-Mobile,Sec-CH-UA-Platform",
                    "setheaderhardwareaccept-ch": "Sec-CH-UA-Model,Sec-CH-UA-Mobile",
                    "setheaderplatformaccept-ch": "Sec-CH-UA-Platform,Sec-CH-UA-Platform-Version",
                }}),
                Some(
                    "Sec-CH-UA, Sec-CH-UA-Full-Version-List, Sec-CH-UA-Mobile, \
                     Sec-CH-UA-Platform, Sec-CH-UA-Model, Sec-CH-UA-Platform-Version",
                ),
            ),
            (
                "one property spelled Unknown",
                serde_json::json!({"device": {
                    "setheaderbrowseraccept-ch": "Unknown",
                    "setheaderhardwareaccept-ch": "Sec-CH-UA-Model",
                }}),
                Some("Sec-CH-UA-Model"),
            ),
            (
                "a device section naming no hints",
                serde_json::json!({"device": {}}),
                None,
            ),
            ("no device section", serde_json::json!({}), None),
        ] {
            assert_eq!(
                accept_ch_from_answer(&CloudAnswer::new(body)).as_deref(),
                expected,
                "{case}"
            );
        }
    }
}
