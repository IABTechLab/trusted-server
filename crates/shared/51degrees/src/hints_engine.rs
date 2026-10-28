//! The server side client hints question, asked on the license key.
//!
//! The question names the three properties the service names its `Accept-CH`
//! in, and a license key answers with exactly those. A resource key would be
//! spent on a call the browser never sees, and answers with everything it
//! carries whatever the call asks for.

use std::sync::Arc;

use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::platform::RuntimeServices;

use crate::client::{CloudPipeline, accept_ch_from_answer};
use crate::engine_cache::AnswerCache;

/// The properties the service names its `Accept-CH` in, and nothing else,
/// because a license key answers with exactly what the call names. Each is
/// read out of the answer by [`accept_ch_from_answer`], which the test
/// `the_values_asked_for_are_the_three_the_answer_is_read_from` holds.
pub const ACCEPT_CH_VALUES: &[&str] = &[
    "device.SetHeaderBrowserAccept-CH",
    "device.SetHeaderHardwareAccept-CH",
    "device.SetHeaderPlatformAccept-CH",
];

/// The base the data endpoint is under, being the configured endpoint without
/// its last path segment. On a multi-tenant service that segment carries the
/// resource key, so a license key call built from this base cannot present
/// it.
#[must_use]
pub fn engine_base(endpoint: &str) -> Option<String> {
    let mut url = url::Url::parse(endpoint).ok()?;
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.pop();
        segments.push("");
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

/// The server side client hints question, a reader of the one answer the
/// deployment's pipeline holds for a request's evidence.
#[derive(Debug, Clone)]
pub struct ClientHintsCall {
    pipeline: Arc<CloudPipeline>,
}

impl ClientHintsCall {
    /// The question on the deployment's pipeline, which [`ACCEPT_CH_VALUES`]
    /// has been registered with.
    #[must_use]
    pub fn on(pipeline: Arc<CloudPipeline>) -> Self {
        Self { pipeline }
    }

    /// The question on a pipeline of its own for `endpoint` and
    /// `license_key`, holding its answers in `cache`, for a test or a
    /// deployment that asks nothing else.
    ///
    /// Returns `None` when the endpoint is not a URL, which startup validation
    /// refuses anyway.
    #[must_use]
    pub fn new(
        endpoint: &str,
        license_key: &str,
        timeout_ms: u32,
        cache: AnswerCache,
    ) -> Option<Self> {
        CloudPipeline::new(endpoint, license_key, timeout_ms, cache)
            .map(|pipeline| Self::on(Arc::new(pipeline.wanting(ACCEPT_CH_VALUES))))
    }

    /// The `Accept-CH` value the service asks for from the request's browser,
    /// or `None` when the request names no browser, the service names no
    /// hints or the call could not be made. A failure is a logged line, never
    /// an error the request carries.
    ///
    /// The evidence is the request's `User-Agent` and `Sec-CH-UA` hints alone,
    /// so where a loading cache module holds the answer, the answer for one
    /// reader serves every reader whose browser sends the same values.
    pub async fn accept_ch(
        &self,
        request_info: &dyn RequestInfo,
        services: &RuntimeServices,
    ) -> Option<String> {
        let evidence = crate::evidence::evidence_for(request_info);
        if evidence.user_agent.trim().is_empty() {
            // The service names hints by the browser, and there is none to
            // name them for.
            return None;
        }
        match self.pipeline.answer(&evidence, services).await {
            Ok(answer) => accept_ch_from_answer(&answer),
            Err(error) => {
                log::warn!("51Degrees client hints: no Accept-CH is sent. {error:?}");
                None
            }
        }
    }
}

/// The hint names the service asked the request's browser for, lower case,
/// as [`hint_names`] gives them.
///
/// The client hints middleware leaves them in the request's state under
/// [`crate::BUILDER_ID`] as it filters a page request, and core copies them
/// into the document state the `[[serve]]` middleware share. A module that
/// wants to know what the browser was asked to send reads them there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceHints(Vec<String>);

impl ServiceHints {
    /// The names the service asked for.
    #[must_use]
    pub const fn new(names: Vec<String>) -> Self {
        Self(names)
    }

    /// The names, in the service's order.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.0
    }
}

/// The hint names an `Accept-CH` value lists, lower case, in its order and
/// each once.
///
/// Lower case because the `Delegate-CH` content and a request's header names
/// are written that way, whatever case the service answers in.
#[must_use]
pub fn hint_names(accept_ch: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in accept_ch.split(',') {
        let name = name.trim().to_ascii_lowercase();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_services::{MemoryKvStore, ScriptedHttpClient, services_with};
    use trusted_server_core::evidence::OwnedRequestInfo;

    /// What the data endpoint answers for a license key naming the three.
    const DATA: &str = r#"{"device":{
        "setheaderbrowseraccept-ch":"Sec-CH-UA,Sec-CH-UA-Platform",
        "setheaderhardwareaccept-ch":"Sec-CH-UA-Model",
        "setheaderplatformaccept-ch":"Sec-CH-UA-Platform-Version"}}"#;

    fn chrome() -> OwnedRequestInfo {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static("Mozilla/5.0"),
        );
        OwnedRequestInfo::new("2.125.160.216".to_owned(), headers)
    }

    fn call() -> ClientHintsCall {
        ClientHintsCall::new(
            "https://cloud.example.com/api/v4/a-public-resource-key.json",
            "a-secret-license-key",
            500,
            crate::test_services::answer_cache(),
        )
        .expect("should build")
    }

    /// A server side call presents the license key and never the public
    /// resource key.
    #[tokio::test]
    async fn the_call_presents_the_license_key_and_never_the_resource_key() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        let accept_ch = call().accept_ch(&chrome(), &services).await;

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
            !sent
                .iter()
                .any(|request| request.uri.contains("a-public-resource-key")),
            "and the key in the configured endpoint path must not reach the address either: \
             {sent:?}"
        );
        assert_eq!(
            accept_ch.as_deref(),
            Some("Sec-CH-UA, Sec-CH-UA-Platform, Sec-CH-UA-Model, Sec-CH-UA-Platform-Version"),
        );
    }

    /// A license key names no properties of its own, so the call names them:
    /// the three the hints are read from, among everything else the
    /// deployment's modules registered, because one call carries them all.
    #[tokio::test]
    async fn the_call_names_the_three_properties_among_the_deployments() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        call().accept_ch(&chrome(), &services).await;

        let sent = client.sent();
        let body = &sent.last().expect("the data request").body;
        // One field carrying a comma separated list, as the service's `values`
        // parameter expects.
        let asked: Vec<String> = body
            .split('&')
            .filter_map(|field| field.strip_prefix("values="))
            .flat_map(|list| list.split("%2C"))
            .map(str::to_owned)
            .collect();

        for value in ACCEPT_CH_VALUES {
            assert!(
                asked.iter().any(|name| name == value),
                "{value} in {asked:?}"
            );
        }
        assert_eq!(sent.len(), 1, "one data call");
    }

    /// A reader from `address`, on Chrome, carrying the `Sec-CH-UA` hints
    /// Chrome sends unasked, cookies, and a header that is not a hint.
    fn reader(address: &str, cookies: &'static str) -> OwnedRequestInfo {
        let mut headers = http::HeaderMap::new();
        for (name, value) in [
            ("user-agent", "Mozilla/5.0"),
            ("sec-ch-ua", "\"Chromium\";v=\"140\""),
            ("sec-ch-ua-mobile", "?0"),
            ("sec-ch-ua-platform", "\"Windows\""),
            ("cookie", cookies),
            ("accept-language", "en-GB"),
        ] {
            headers.insert(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_static(value),
            );
        }
        OwnedRequestInfo::new(address.to_owned(), headers)
    }

    /// The request the client builds carries the `User-Agent` and the
    /// `Sec-CH-UA` hints, and neither the reader's address nor a cookie nor
    /// any other header.
    #[tokio::test]
    async fn no_address_and_no_cookie_reaches_the_request_the_client_builds() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        call()
            .accept_ch(
                &reader("2.125.160.216", "51D_ScreenPixelsWidth=1080; ts-ec=an-id"),
                &services,
            )
            .await;

        let sent = client.sent();
        assert_eq!(sent.len(), 1);
        let body = &sent[0].body;
        for field in [
            "user-agent=",
            "sec-ch-ua=",
            "sec-ch-ua-mobile=",
            "sec-ch-ua-platform=",
        ] {
            assert!(body.contains(field), "{field} in {body}");
        }
        let request = format!("{} {} {:?}", sent[0].uri, body, sent[0].headers);
        for absent in [
            "client-ip",
            "2.125.160.216",
            "cookie",
            "51D_",
            "1080",
            "ts-ec",
            "an-id",
            "accept-language",
            "en-GB",
        ] {
            assert!(
                !request
                    .to_ascii_lowercase()
                    .contains(&absent.to_ascii_lowercase()),
                "{absent} reached the service: {request}"
            );
        }
    }

    /// Two readers on the same browser share one answer and one call,
    /// whatever their addresses and cookies.
    #[tokio::test]
    async fn one_answer_serves_every_reader_on_the_same_browser() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));

        let first = call()
            .accept_ch(&reader("2.125.160.216", "ts-ec=one"), &services)
            .await;
        let second = call()
            .accept_ch(&reader("192.0.2.1", "ts-ec=two"), &services)
            .await;

        assert_eq!(
            client.sent().len(),
            1,
            "the second reader reads the held answer"
        );
        assert!(first.is_some());
        assert_eq!(first, second);
    }

    /// A request with no `User-Agent` names no browser for the service to
    /// name hints for, so nothing is asked and nothing is sent.
    #[tokio::test]
    async fn a_request_with_no_user_agent_asks_nothing() {
        let client = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let services = services_with(client.clone(), Arc::new(MemoryKvStore::default()));
        let request = OwnedRequestInfo::new("2.125.160.216".to_owned(), http::HeaderMap::new());

        assert!(call().accept_ch(&request, &services).await.is_none());
        assert!(client.sent().is_empty());
    }

    /// The answer is held, so a second instance sharing only the store asks
    /// the cloud nothing.
    #[tokio::test]
    async fn a_second_instance_asks_the_cloud_nothing_at_all() {
        let store = Arc::new(MemoryKvStore::default());
        let first = Arc::new(ScriptedHttpClient::new(&[DATA]));
        call()
            .accept_ch(&chrome(), &services_with(first.clone(), store.clone()))
            .await;
        assert_eq!(first.sent().len(), 1, "the first call pays for the answer");

        // A separate instance, sharing only the store.
        let second = Arc::new(ScriptedHttpClient::new(&[DATA]));
        let accept_ch = call()
            .accept_ch(&chrome(), &services_with(second.clone(), store))
            .await;

        assert_eq!(
            second.sent().len(),
            0,
            "the answer was paid for by the first instance, so a second one with the same \
             evidence has nothing to ask"
        );
        assert!(accept_ch.is_some(), "and the answer still comes back");
    }

    /// A service that cannot be reached is no header, never a failed request.
    #[tokio::test]
    async fn an_unreachable_service_yields_no_header_and_no_error() {
        let client = Arc::new(ScriptedHttpClient::new(&["not json at all"]));
        let services = services_with(client, Arc::new(MemoryKvStore::default()));

        assert!(
            call().accept_ch(&chrome(), &services).await.is_none(),
            "a visitor whose browser was not asked for its hints is still served a page"
        );
    }

    /// The base is the endpoint without its last path segment, which a
    /// multi-tenant endpoint puts the public key in, and without a query or
    /// fragment, because the data file is added to it and anything left on
    /// the configured endpoint would be sent twice.
    #[test]
    fn the_base_is_the_endpoint_without_its_last_segment() {
        for (case, endpoint, base) in [
            (
                "a resource key in the last segment",
                "https://cloud.example.com/api/v4/AQS5HKcy.json",
                Some("https://cloud.example.com/api/v4/"),
            ),
            (
                "a self-hosted container's endpoint, whose last segment is `json`",
                "http://127.0.0.1:8080/api/v4/json",
                Some("http://127.0.0.1:8080/api/v4/"),
            ),
            (
                "a query string",
                "https://cloud.example.com/api/v4/key.json?resource=public",
                Some("https://cloud.example.com/api/v4/"),
            ),
            (
                "a fragment",
                "https://cloud.example.com/api/v4/key.json#top",
                Some("https://cloud.example.com/api/v4/"),
            ),
            ("not a URL", "not-a-url", None),
        ] {
            assert_eq!(engine_base(endpoint).as_deref(), base, "{case}");
        }
    }

    #[test]
    fn an_endpoint_that_is_not_a_url_yields_no_call() {
        assert!(
            ClientHintsCall::new(
                "not-a-url",
                "license",
                500,
                crate::test_services::answer_cache()
            )
            .is_none()
        );
    }

    /// Every value the call names is read out of the answer, each with a hint
    /// of its own, so a value added here that the reader does not read fails.
    /// The reader lowercases, because the service answers in lower case
    /// element and property names.
    #[test]
    fn the_values_asked_for_are_the_three_the_answer_is_read_from() {
        let mut device = serde_json::Map::new();
        let mut hints = Vec::new();
        for (index, value) in ACCEPT_CH_VALUES.iter().enumerate() {
            let (element, property) = value
                .split_once('.')
                .expect("every value names its component");
            assert_eq!(element, "device", "{value} is read out of `device`");
            let hint = format!("Sec-CH-Test-{index}");
            device.insert(property.to_lowercase(), hint.clone().into());
            hints.push(hint);
        }
        let answer = crate::client::CloudAnswer::new(serde_json::json!({ "device": device }));

        assert_eq!(
            accept_ch_from_answer(&answer),
            Some(hints.join(", ")),
            "what is asked for has to be what is read, or the call buys nothing"
        );
    }

    /// The `Accept-CH` the live service answers a publisher's license key
    /// with.
    const SERVICE_ACCEPT_CH: &str = "Sec-CH-UA, Sec-CH-UA-Full-Version-List, Sec-CH-UA-Mobile, \
                                     Sec-CH-UA-Platform, Sec-CH-UA-Model, Sec-CH-UA-Platform-Version";

    #[test]
    fn the_names_are_the_service_list_lower_case_in_its_order() {
        assert_eq!(
            hint_names(SERVICE_ACCEPT_CH),
            vec![
                "sec-ch-ua".to_owned(),
                "sec-ch-ua-full-version-list".to_owned(),
                "sec-ch-ua-mobile".to_owned(),
                "sec-ch-ua-platform".to_owned(),
                "sec-ch-ua-model".to_owned(),
                "sec-ch-ua-platform-version".to_owned(),
            ]
        );
    }

    #[test]
    fn a_repeated_or_empty_name_is_written_once_or_not_at_all() {
        assert_eq!(
            hint_names(" Sec-CH-UA-Model ,, sec-ch-ua-model, Sec-CH-UA-Arch,"),
            vec!["sec-ch-ua-model".to_owned(), "sec-ch-ua-arch".to_owned()]
        );
        assert!(hint_names("").is_empty());
    }
}
