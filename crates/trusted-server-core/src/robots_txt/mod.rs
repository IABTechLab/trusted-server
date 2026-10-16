//! A publisher's `robots.txt`, assembled from the rules the selected
//! contributors give.
//!
//! `[robots-txt] modules` names, in order, what makes the file. Two names are
//! core's own and need nothing else: [`REFUSE_ALL`] refuses every crawler, and
//! [`ALLOW_ALL`] allows every crawler everywhere. Any other name is the name a
//! module declared a [`RobotsTxtContributor`] under. Core never names a
//! module here and knows nothing of what any contributor asks or of whom.
//!
//! # Contributors add rules, core writes the file
//!
//! A contributor returns a [`Contribution`], being groups of records, and core
//! writes every byte in [`document`]. That is what lets these hold whatever a
//! contributor says.
//!
//! 1. A refusal cannot be loosened. With `refuse_all` selected the file is the
//!    refusal and nothing else.
//! 2. A path in `always_allow` stays open to every crawler a contribution
//!    refuses the whole site to, `*` included. A narrower rule is kept as
//!    given, so `Disallow: /ads` still closes off `/ads.txt`.
//! 3. The `Sitemap` line, the publisher's own text, and the `X-Robots-Tag`
//!    header on every page agree with the file served.
//! 4. Several contributors combine in the order `modules` gives, and none of
//!    them owns the file.
//! 5. A contributor never writes bytes, so it cannot produce a malformed file.
//! 6. A contributor that fails with no held answer fails the whole file closed
//!    with `503`, because leaving out a contributor's rules would allow by
//!    omission whatever those rules refused. A `404` or an empty file reads to
//!    a crawler as permission to crawl everything, so a file with no rule
//!    fails closed too, and permission is only said with `allow_all`.
//!
//! # Settings
//!
//! `[robots-txt]` holds `modules` and the document's own settings, being
//! `always_allow`, `sitemap`, `top_text` and `bottom_text`. Settings belonging
//! to one contributor go in its own block, `[robots-txt.<name>]`, which that
//! contributor reads and core does not.

mod contributor;
pub mod document;
mod holding;

use std::collections::BTreeMap;
use std::sync::Arc;

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::header::{self, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Deserializer, Serialize};
use validator::{Validate, ValidationError};

pub use contributor::{ALLOW_ALL, REFUSE_ALL, RobotsTxtContributor};
pub use document::{Contribution, Group, Record};

use crate::error::TrustedServerError;
use crate::integrations::IntegrationRegistry;
use crate::module_context::ResolvedRequest;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

/// The path this module answers.
pub const ROBOTS_TXT_PATH: &str = "/robots.txt";

/// How long a browser or an edge cache may hold the answer.
const CLIENT_CACHE_SECONDS: u32 = 3600;

/// How long a crawler is asked to wait when there is no file to give it.
const RETRY_AFTER_SECONDS: u32 = 600;

/// The `X-Robots-Tag` a response carries when `refuse_all` is selected.
///
/// A robots.txt asks a crawler not to fetch. This tells one that has already
/// fetched not to index, and it is the only one of the two that works on a page
/// a crawler learned about from somewhere else, or reached before reading
/// robots.txt, or chose to fetch anyway. They do different jobs and a site that
/// is not ready to be seen needs both.
///
/// Set by the same selection so the two cannot drift apart.
///
/// `nofollow` as well as `noindex`, so a crawler does not walk on from a page
/// it should not have indexed into the rest of a site that is not ready either.
pub const REFUSE_ALL_ROBOTS_TAG: &str = "noindex, nofollow";

/// The type folder of every robots.txt crate, which a name written in
/// `[robots-txt] modules` may leave off.
pub const MODULE_TYPE: &str = "robots-txt";

/// Settings for serving `robots.txt`.
///
/// Mapped from the `[robots-txt]` TOML section. With the section absent the
/// publisher keeps their own file and `/robots.txt` reaches their origin like
/// any other path.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct RobotsTxtConfig {
    /// What makes the file, in order. One name or a list, so
    /// `modules = "refuse_all"` and `modules = ["refuse_all"]` are the same.
    ///
    /// Required, and refused at load when absent or empty. There is no default,
    /// because a default would decide for a publisher what crawlers may do.
    #[serde(default, deserialize_with = "one_or_many")]
    pub modules: Vec<String>,

    /// Paths kept open to every crawler refused the whole site, the page a
    /// refused crawler is sent to for example. A narrower rule, such as
    /// `Disallow: /ads`, still closes off `/ads.txt`. `/robots.txt` needs no
    /// entry, because RFC 9309 section 2.2.2 says it is implicitly allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(custom(function = validate_always_allow))]
    pub always_allow: Vec<String>,

    /// The site's sitemap, added as a `Sitemap:` line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(url)]
    pub sitemap: Option<String>,

    /// The publisher's own text, placed above the contributors' rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_text: Option<String>,

    /// The publisher's own text, placed below the contributors' rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom_text: Option<String>,

    /// Each contributor's own settings, `[robots-txt.<name>]`, keyed by its
    /// name.
    ///
    /// Read by that contributor and by nothing in core, which only checks that
    /// each is a block and belongs to a contributor `modules` names.
    #[serde(flatten)]
    pub contributors: BTreeMap<String, serde_json::Value>,
}

fn one_or_many<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(name) => vec![name],
        OneOrMany::Many(names) => names,
    })
}

fn validate_always_allow(paths: &[String]) -> Result<(), ValidationError> {
    // A path is written into the file as it stands, so it must be a path and
    // must not be able to start a second line.
    if paths
        .iter()
        .any(|path| !path.starts_with('/') || path.chars().any(char::is_whitespace))
    {
        return Err(ValidationError::new("robots_txt_always_allow_not_a_path"));
    }
    Ok(())
}

impl RobotsTxtConfig {
    /// The configuration that refuses every crawler with no exception.
    ///
    /// The starting point for a publisher who wants the refusal with
    /// allowances of their own. Needs no contributor, no key and no network.
    #[must_use]
    pub fn refuse_all() -> Self {
        Self {
            modules: vec![REFUSE_ALL.to_owned()],
            always_allow: Vec::new(),
            sitemap: None,
            top_text: None,
            bottom_text: None,
            contributors: BTreeMap::new(),
        }
    }

    /// Whether `modules` names `id`, by its full name or by its short form,
    /// whichever of the two `id` and the name in `modules` are written in.
    #[must_use]
    pub fn selects(&self, id: &str) -> bool {
        self.modules.iter().any(|selected| {
            crate::module_name::resolve(MODULE_TYPE, selected, &[id]).is_some()
                || crate::module_name::short_form(MODULE_TYPE, selected) == id
        })
    }

    /// Whether the file is the refusal, which it is whenever `refuse_all` is
    /// selected, whatever else is.
    #[must_use]
    pub fn refuses_all(&self) -> bool {
        self.selects(REFUSE_ALL)
    }

    /// The `X-Robots-Tag` every response should carry, or `None` when the
    /// publisher's crawler policy places no blanket rule on pages.
    #[must_use]
    pub fn response_robots_tag(&self) -> Option<&'static str> {
        self.refuses_all().then_some(REFUSE_ALL_ROBOTS_TAG)
    }

    /// The settings block of the contributor named `id`, when there is one.
    #[must_use]
    pub fn contributor_settings(&self, id: &str) -> Option<&serde_json::Value> {
        self.contributors.get(id).or_else(|| {
            self.contributors
                .get(crate::module_name::short_form(MODULE_TYPE, id))
        })
    }

    /// Checks what the type cannot.
    ///
    /// # Errors
    ///
    /// When `modules` names nothing or names one thing twice, when a key is
    /// not a setting, and when a contributor's block belongs to one `modules`
    /// does not name.
    pub fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        if self.modules.is_empty() {
            return Err(Report::new(configuration(
                "[robots-txt] names no module. Name what makes the file, in order: \
                 `refuse_all` to refuse every crawler, `allow_all` to allow every crawler, \
                 or the name of each module whose rules make the file. There is no default, \
                 because a default would decide for the publisher what crawlers may do."
                    .to_owned(),
            )));
        }
        // Compared on the short form, so the full name and the short form of
        // one module count as naming it twice.
        for (index, name) in self.modules.iter().enumerate() {
            let short_name = crate::module_name::short_form(MODULE_TYPE, name);
            if self.modules[..index]
                .iter()
                .any(|earlier| crate::module_name::short_form(MODULE_TYPE, earlier) == short_name)
            {
                return Err(Report::new(configuration(format!(
                    "[robots-txt] modules names `{name}` twice."
                ))));
            }
        }
        for (key, value) in &self.contributors {
            if !value.is_object() {
                return Err(Report::new(configuration(format!(
                    "robots-txt.{key} is not a setting. [robots-txt] holds modules, \
                     always_allow, sitemap, top_text, bottom_text, and a block for each \
                     contributor that has settings of its own."
                ))));
            }
            if !self.selects(key) {
                return Err(Report::new(configuration(format!(
                    "[robots-txt.{key}] configures a contributor that `modules` does not \
                     name, so nothing would read it. Add `{key}` to modules or remove the \
                     block."
                ))));
            }
        }
        Ok(())
    }
}

fn configuration(message: String) -> TrustedServerError {
    TrustedServerError::Configuration { message }
}

/// Puts [`REFUSE_ALL_ROBOTS_TAG`] on an outgoing response when `refuse_all` is
/// selected, in place of any `X-Robots-Tag` it carried, and does nothing
/// otherwise. Called once where an adapter finalizes a response.
pub fn apply_response_robots_tag(settings: &Settings, response: &mut Response<EdgeBody>) {
    if settings
        .robots_txt
        .as_ref()
        .and_then(RobotsTxtConfig::response_robots_tag)
        .is_none()
    {
        return;
    }
    response.headers_mut().insert(
        header::HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static(REFUSE_ALL_ROBOTS_TAG),
    );
}

/// Answers `GET` and `HEAD /robots.txt` for a publisher whose settings carry
/// a `[robots-txt]` section. A `HEAD` is answered with the headers of the
/// `GET` and no body.
///
/// # Errors
///
/// When the settings carry no `[robots-txt]` section. An adapter registers
/// this route only when the section is present, so that is a wiring fault and
/// not something a request can cause.
pub async fn handle_robots_txt(
    settings: &Settings,
    services: &RuntimeServices,
    registry: &IntegrationRegistry,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let response = answer(settings, services, registry, &req).await?;
    Ok(if req.method() == Method::HEAD {
        response.map(|_| EdgeBody::empty())
    } else {
        response
    })
}

async fn answer(
    settings: &Settings,
    services: &RuntimeServices,
    registry: &IntegrationRegistry,
    req: &Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let config = settings.robots_txt.as_ref().ok_or_else(|| {
        Report::new(configuration(
            "the robots.txt route is registered but there is no [robots-txt] section".to_owned(),
        ))
    })?;
    // A refusal answers without asking anything, before any contributor,
    // held answer or network is involved. That is the point of it, because a
    // site that is not ready to be crawled can then say so with nothing
    // provisioned at all.
    if config.refuses_all() {
        return Ok(respond(Ok(document::refusal(&config.always_allow))));
    }
    let mut selected: Vec<Selected> = Vec::new();
    for id in &config.modules {
        if id == ALLOW_ALL {
            selected.push((ALLOW_ALL, None));
            continue;
        }
        let Some((declared, contributor)) = registry.robots_txt_contributor(id) else {
            // Unreachable, the registry refuses this name when it is built.
            return Ok(respond(Err(Report::new(configuration(format!(
                "[robots-txt] modules names `{id}`, which no module in this deployment supplies"
            ))))));
        };
        selected.push((declared, Some(contributor)));
    }
    // The answers are held and served to every crawler of the host, so the
    // contributors are handed the request with nothing one crawler put in
    // the address, for the host it was addressed to and never one a
    // forwarded header names. That host is part of the key an answer is
    // held under.
    let request = ResolvedRequest::of(req, services.client_info())
        .without_query()
        .with_host(crate::http_util::addressed_host(req));
    Ok(respond(
        file_for(config, &selected, &request, services, settings).await,
    ))
}

/// One entry of `modules` as the handler resolved it: the name a contributor
/// was declared under and the contributor, or `allow_all` and none.
type Selected = (&'static str, Option<Arc<dyn RobotsTxtContributor>>);

/// The file for `config`, from the contributions of `selected` in order,
/// each asked for `request` and held for the publisher `settings` describe and
/// the request's host.
///
/// Fails, and the file is answered `503`, when a contributor fails with no
/// answer of its kept, and when the file would hold no rule.
async fn file_for(
    config: &RobotsTxtConfig,
    selected: &[Selected],
    request: &ResolvedRequest,
    services: &RuntimeServices,
    settings: &Settings,
) -> Result<String, Report<TrustedServerError>> {
    let mut contributions = Vec::with_capacity(selected.len());
    for (id, contributor) in selected {
        contributions.push(match contributor {
            None => document::allow_all(),
            Some(contributor) => {
                holding::contribution_for(id, contributor, request, services, settings).await?
            }
        });
    }
    let file = document::assemble(config, &contributions);
    // Permission to crawl everything is said with `allow_all`. A file with no
    // rule says it too, by leaving everything out, so it fails closed.
    if !document::holds_a_rule(&file) {
        return Err(Report::new(configuration(
            "[robots-txt] made a file with no rule, which a crawler reads as permission to \
             crawl everything. Select `allow_all` to allow every crawler."
                .to_owned(),
        )));
    }
    Ok(file)
}

fn respond(file: Result<String, Report<TrustedServerError>>) -> Response<EdgeBody> {
    match file {
        Ok(file) => Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )
            .header(
                header::CACHE_CONTROL,
                format!("public, max-age={CLIENT_CACHE_SECONDS}"),
            )
            .body(EdgeBody::from(file.into_bytes()))
            .expect("should build the robots.txt response"),
        Err(report) => {
            log::error!("Could not assemble robots.txt: {report:?}");
            Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                )
                .header(header::RETRY_AFTER, RETRY_AFTER_SECONDS.to_string())
                .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
                .body(EdgeBody::from(
                    b"robots.txt is temporarily unavailable\n".to_vec(),
                ))
                .expect("should build the robots.txt failure response")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::holding::tests::{
        ECHOED, Echoing, MemoryKvStore, PUBLISHER, Scripted, request, robots_request, rules,
        services_with, settings,
    };
    use super::*;
    use crate::integrations::{IntegrationBuilder, IntegrationRegistration};

    fn parsed(document: serde_json::Value) -> RobotsTxtConfig {
        serde_json::from_value(document).expect("should read the robots.txt settings")
    }

    #[test]
    fn a_single_module_reads_as_a_list_of_one() {
        let config = parsed(serde_json::json!({ "modules": "refuse_all" }));

        assert_eq!(config.modules, vec!["refuse_all"]);
        assert!(config.refuses_all());
        config
            .prepare_runtime()
            .expect("a single module written as a string must load");
    }

    #[test]
    fn a_list_of_modules_keeps_its_order() {
        let config = parsed(serde_json::json!({
            "modules": ["a_module", "allow_all"],
            "a_module": { "anything": "the module reads" },
        }));

        assert_eq!(config.modules, vec!["a_module", "allow_all"]);
        config.prepare_runtime().expect("should load");
    }

    #[test]
    fn no_module_is_refused_rather_than_defaulted() {
        let error = parsed(serde_json::json!({ "sitemap": "https://example.com/sitemap.xml" }))
            .prepare_runtime()
            .expect_err("there is no default");
        let message = format!("{error:?}");

        assert!(message.contains("refuse_all"), "{message}");
        assert!(message.contains("allow_all"), "{message}");
    }

    #[test]
    fn an_empty_list_is_refused_the_same_way() {
        assert!(
            parsed(serde_json::json!({ "modules": [] }))
                .prepare_runtime()
                .is_err()
        );
    }

    #[test]
    fn a_name_given_twice_is_refused() {
        assert!(
            parsed(serde_json::json!({ "modules": ["allow_all", "allow_all"] }))
                .prepare_runtime()
                .is_err()
        );
    }

    /// A section may write a contributor's full name or leave the type folder
    /// off, so both spellings select the block, and naming both is naming the
    /// contributor twice.
    #[test]
    fn a_contributor_named_in_full_selects_its_block() {
        parsed(serde_json::json!({
            "modules": "robots-txt.a_module",
            "a_module": { "anything": "the module reads" },
        }))
        .prepare_runtime()
        .expect("the full name should select the block written at the short name");

        let error = parsed(serde_json::json!({
            "modules": ["a_module", "robots-txt.a_module"],
            "a_module": { "anything": "the module reads" },
        }))
        .prepare_runtime()
        .expect_err("one contributor named in two spellings is named twice");
        assert!(format!("{error:?}").contains("twice"), "{error:?}");
    }

    #[test]
    fn a_key_that_is_not_a_setting_is_refused() {
        let message = format!(
            "{:?}",
            parsed(serde_json::json!({ "modules": "allow_all", "sitmap": "a typo" }))
                .prepare_runtime()
                .expect_err("a misspelled setting must not load")
        );

        assert!(message.contains("is not a setting"), "{message}");
    }

    #[test]
    fn a_block_for_a_contributor_not_selected_is_refused() {
        let message = format!(
            "{:?}",
            parsed(serde_json::json!({ "modules": "allow_all", "a_module": {} }))
                .prepare_runtime()
                .expect_err("nothing would read the block")
        );

        assert!(message.contains("does not name"), "{message}");
    }

    #[test]
    fn a_path_kept_open_must_be_a_path_on_one_line() {
        for path in ["ads.txt", "/ads.txt\nDisallow: /", "/a path"] {
            let config = parsed(serde_json::json!({
                "modules": "refuse_all",
                "always_allow": [path],
            }));

            assert!(config.validate().is_err(), "should refuse {path:?}");
        }
        parsed(serde_json::json!({ "modules": "refuse_all", "always_allow": ["/ads.txt"] }))
            .validate()
            .expect("should accept a path");
    }

    #[test]
    fn refusing_sets_the_response_header_and_a_contributor_does_not() {
        assert_eq!(
            RobotsTxtConfig::refuse_all().response_robots_tag(),
            Some(REFUSE_ALL_ROBOTS_TAG)
        );
        assert_eq!(
            parsed(serde_json::json!({ "modules": "allow_all" })).response_robots_tag(),
            None
        );
    }

    #[test]
    fn the_header_replaces_one_the_origin_set() {
        let mut settings = settings();
        settings.robots_txt = Some(RobotsTxtConfig::refuse_all());
        let mut response = Response::builder()
            .header("x-robots-tag", "all")
            .body(EdgeBody::empty())
            .expect("should build a response");

        apply_response_robots_tag(&settings, &mut response);

        let tags: Vec<_> = response.headers().get_all("x-robots-tag").iter().collect();
        assert_eq!(tags, vec![REFUSE_ALL_ROBOTS_TAG]);
    }

    #[test]
    fn a_response_is_left_alone_when_nothing_refuses_every_crawler() {
        let mut allowing = settings();
        allowing.robots_txt = Some(parsed(serde_json::json!({ "modules": "allow_all" })));
        let mut without_a_section = settings();
        without_a_section.robots_txt = None;

        for settings in [allowing, without_a_section] {
            let mut response = Response::builder()
                .header("x-robots-tag", "noarchive")
                .body(EdgeBody::empty())
                .expect("should build a response");

            apply_response_robots_tag(&settings, &mut response);

            let tags: Vec<_> = response.headers().get_all("x-robots-tag").iter().collect();
            assert_eq!(tags, vec!["noarchive"]);
        }
    }

    #[tokio::test]
    async fn contributions_are_assembled_in_the_order_selected() {
        let first: Arc<dyn RobotsTxtContributor> = Arc::new(Scripted::new(vec![Ok(rules(
            "User-Agent: FirstBot\nDisallow: /",
        ))]));
        let config = parsed(serde_json::json!({
            "modules": ["first", "allow_all"],
            "sitemap": "https://publisher.example.com/sitemap.xml",
        }));

        let file = file_for(
            &config,
            &[("first", Some(first)), (ALLOW_ALL, None)],
            &request(),
            &services_with(Arc::default(), Arc::default()),
            &settings(),
        )
        .await
        .expect("should assemble the file");

        assert_eq!(
            file,
            "User-Agent: FirstBot\nDisallow: /\n\nUser-Agent: *\nAllow: /\n\n\
             Sitemap: https://publisher.example.com/sitemap.xml\n"
        );
    }

    #[tokio::test]
    async fn with_nothing_held_a_failure_is_service_unavailable() {
        let failing: Arc<dyn RobotsTxtContributor> =
            Arc::new(Scripted::new(vec![Err("cannot answer".to_owned())]));
        let config = parsed(serde_json::json!({ "modules": ["failing", "allow_all"] }));

        let response = respond(
            file_for(
                &config,
                &[("failing", Some(failing)), (ALLOW_ALL, None)],
                &request(),
                &services_with(Arc::default(), Arc::default()),
                &settings(),
            )
            .await,
        );

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a crawler reads an empty or allow everything file as permission"
        );
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("600")),
            "a crawler should be told to come back in ten minutes"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store")),
            "a failure must not be held in place of the file"
        );
    }

    /// The refusal asks nothing of anything, and dominates whatever else is
    /// selected, because a refusal cannot be loosened.
    #[tokio::test]
    async fn refusing_every_crawler_asks_no_contributor_and_reads_no_store() {
        let mut settings = settings();
        let mut refusing = parsed(serde_json::json!({ "modules": ["allow_all", "refuse_all"] }));
        refusing.always_allow = vec!["/ads.txt".to_owned()];
        settings.robots_txt = Some(refusing);
        let store = Arc::new(MemoryKvStore::default());
        let registry = IntegrationRegistry::new(&settings).expect("should build the registry");

        let response = handle_robots_txt(
            &settings,
            &services_with(Arc::default(), Arc::clone(&store)),
            &registry,
            robots_request(),
        )
        .await
        .expect("the refusal answers without asking");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("public, max-age=3600"))
        );
        let body = response
            .into_body()
            .into_bytes_bounded(1024)
            .await
            .expect("should read the body");
        assert_eq!(
            body.as_ref(),
            b"User-agent: *\nAllow: /ads.txt\nDisallow: /\n",
            "the refusal, whatever else is selected"
        );
        assert!(
            store
                .entries
                .lock()
                .expect("should lock the test store")
                .is_empty(),
            "nothing is held for a refusal"
        );
    }

    /// A crawler that asks with `HEAD` is told what a `GET` would be told and
    /// is sent no file.
    #[tokio::test]
    async fn a_head_is_answered_with_the_headers_and_no_body() {
        let mut settings = settings();
        settings.robots_txt = Some(RobotsTxtConfig::refuse_all());
        let registry = IntegrationRegistry::new(&settings).expect("should build the registry");
        let mut request = robots_request();
        *request.method_mut() = Method::HEAD;

        let response = handle_robots_txt(
            &settings,
            &services_with(Arc::default(), Arc::default()),
            &registry,
            request,
        )
        .await
        .expect("should answer");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("text/plain; charset=utf-8"))
        );
        let body = response
            .into_body()
            .into_bytes_bounded(16)
            .await
            .expect("should read the body");
        assert!(body.is_empty(), "a HEAD carries no body");
    }

    /// The id of the builder that registers the echoing contributor.
    const ECHO_BUILDER: &str = "robots-txt-echo";

    /// The module name that builder is selected by.
    const ECHO_BUILDER_MODULE: &str = "testing.robots-txt-echo";

    fn register_echo(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder(ECHO_BUILDER)
                .with_robots_txt_contributor("robots-txt.echo", Arc::new(Echoing))
                .build(),
        ))
    }

    fn accept(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        Ok(true)
    }

    fn settings_selecting_echo() -> Settings {
        let mut settings = settings();
        settings.robots_txt = Some(parsed(serde_json::json!({ "modules": ["echo"] })));
        settings
            .insert_module_config("testing", ECHO_BUILDER_MODULE, &serde_json::json!({}))
            .expect("should select the test builder");
        settings
    }

    fn echo_builder() -> IntegrationBuilder {
        IntegrationBuilder::new(ECHO_BUILDER, ECHO_BUILDER, register_echo, accept)
            .with_module_name(ECHO_BUILDER_MODULE)
    }

    /// The handler asks a contributor for the request it is answering, being
    /// its host and path, without the query and without the reader's
    /// evidence.
    #[tokio::test]
    async fn the_handler_asks_the_contributors_for_the_request_it_answers() {
        let settings = settings_selecting_echo();
        let registry = IntegrationRegistry::with_registrations(&settings, &[echo_builder()])
            .expect("should build the registry");

        let response = handle_robots_txt(
            &settings,
            &services_with(Arc::default(), Arc::default()),
            &registry,
            robots_request(),
        )
        .await
        .expect("should answer");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .into_bytes_bounded(4096)
            .await
            .expect("should read the body");
        let file = String::from_utf8(body.to_vec()).expect("the file is text");
        let config = settings
            .robots_txt
            .as_ref()
            .expect("the settings carry a [robots-txt] section");
        assert_eq!(file, document::assemble(config, &[rules(ECHOED)]));
    }

    /// A name nothing supplies is refused where the registry is built, so the
    /// file does not fail on the first request for it.
    #[test]
    fn a_selection_nothing_supplies_is_refused_when_the_registry_is_built() {
        let mut settings = settings();
        settings.robots_txt = Some(parsed(serde_json::json!({ "modules": ["nobody"] })));

        let error = IntegrationRegistry::new(&settings)
            .err()
            .expect("should refuse a contributor nothing supplies");
        let message = format!("{error:?}");

        assert!(message.contains("`nobody`"), "{message}");
        assert!(message.contains("refuse_all"), "{message}");
        assert!(message.contains("allow_all"), "{message}");
    }

    /// The body of a response, as text.
    async fn text_of(response: Response<EdgeBody>) -> String {
        let body = response
            .into_body()
            .into_bytes_bounded(4096)
            .await
            .expect("should read the body");
        String::from_utf8(body.to_vec()).expect("the file is text")
    }

    /// A crawler's request for the file, addressed to `host`, carrying
    /// `headers` as well.
    fn robots_request_to(host: &str, headers: &[(&str, &str)]) -> Request<EdgeBody> {
        let mut builder = Request::builder()
            .uri(ROBOTS_TXT_PATH)
            .header(header::HOST, host);
        for &(name, value) in headers {
            builder = builder.header(name, value);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build the request")
    }

    /// One deployment may serve several hosts, and each is answered with
    /// what its contributors gave for that host, never with an answer held
    /// for another.
    #[tokio::test]
    async fn each_host_is_answered_for_itself() {
        let settings = settings_selecting_echo();
        let registry = IntegrationRegistry::with_registrations(&settings, &[echo_builder()])
            .expect("should build the registry");
        let services = services_with(Arc::default(), Arc::default());

        for host in [PUBLISHER, "other.publisher.example"] {
            let response = handle_robots_txt(
                &settings,
                &services,
                &registry,
                robots_request_to(host, &[]),
            )
            .await
            .expect("should answer");
            let file = text_of(response).await;

            assert!(
                file.contains(&format!("Disallow: /{host}/robots.txt?")),
                "{host} should be answered for itself: {file}"
            );
        }
    }

    /// `Forwarded` and `X-Forwarded-Host` name whatever host a client likes,
    /// so a contributor is asked for the host the request was addressed to,
    /// and an answer made for a claimed host is never held for the real one.
    #[tokio::test]
    async fn a_forwarded_host_is_never_the_host_a_contributor_is_asked_for() {
        let settings = settings_selecting_echo();
        let registry = IntegrationRegistry::with_registrations(&settings, &[echo_builder()])
            .expect("should build the registry");
        let services = services_with(Arc::default(), Arc::default());
        let claiming = robots_request_to(
            PUBLISHER,
            &[
                ("forwarded", "host=elsewhere.example"),
                ("x-forwarded-host", "elsewhere.example"),
            ],
        );

        let claimed = handle_robots_txt(&settings, &services, &registry, claiming)
            .await
            .expect("should answer");
        let after = handle_robots_txt(
            &settings,
            &services,
            &registry,
            robots_request_to(PUBLISHER, &[]),
        )
        .await
        .expect("should answer");

        for file in [text_of(claimed).await, text_of(after).await] {
            assert!(
                !file.contains("elsewhere.example"),
                "a claimed host should never reach the file: {file}"
            );
            assert!(
                file.contains(&format!("Disallow: /{PUBLISHER}/robots.txt?")),
                "the file should be made for the host addressed: {file}"
            );
        }
    }

    /// A contributor with nothing to add, which answers a comment and no
    /// group.
    #[derive(Debug)]
    struct Silent;

    #[async_trait::async_trait(?Send)]
    impl RobotsTxtContributor for Silent {
        fn refresh(&self) -> core::time::Duration {
            core::time::Duration::from_secs(60)
        }

        fn fingerprint(&self) -> String {
            "silent".to_owned()
        }

        async fn contribute(
            &self,
            _call: crate::module_context::ModuleCall<'_>,
        ) -> Result<Contribution, Report<TrustedServerError>> {
            Ok(Contribution {
                preamble: vec![Record::Comment(" nothing to add".to_owned())],
                groups: Vec::new(),
            })
        }
    }

    /// The id of the builder that registers the silent contributor.
    const SILENT_BUILDER: &str = "robots-txt-silent";

    /// The module name that builder is selected by.
    const SILENT_BUILDER_MODULE: &str = "testing.robots-txt-silent";

    fn register_silent(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder(SILENT_BUILDER)
                .with_robots_txt_contributor("robots-txt.silent", Arc::new(Silent))
                .build(),
        ))
    }

    fn silent_builder() -> IntegrationBuilder {
        IntegrationBuilder::new(SILENT_BUILDER, SILENT_BUILDER, register_silent, accept)
            .with_module_name(SILENT_BUILDER_MODULE)
    }

    /// The test settings with `robots_txt` as the section and the test
    /// builder named `module` selected.
    fn settings_selecting(robots_txt: serde_json::Value, module: &str) -> Settings {
        let mut settings = settings();
        settings.robots_txt = Some(parsed(robots_txt));
        settings
            .insert_module_config("testing", module, &serde_json::json!({}))
            .expect("should select the test builder");
        settings
    }

    /// A file with no rule reads to a crawler as permission to crawl
    /// everything, which is only ever said with `allow_all`, so the file
    /// fails closed when its sources give no rule.
    #[tokio::test]
    async fn a_file_with_no_rule_is_service_unavailable() {
        let settings = settings_selecting(
            serde_json::json!({
                "modules": ["silent"],
                "sitemap": "https://www.publisher.example/sitemap.xml",
            }),
            SILENT_BUILDER_MODULE,
        );
        let registry = IntegrationRegistry::with_registrations(&settings, &[silent_builder()])
            .expect("should build the registry");

        let response = handle_robots_txt(
            &settings,
            &services_with(Arc::default(), Arc::default()),
            &registry,
            robots_request(),
        )
        .await
        .expect("should answer");

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a file with no rule should never be served"
        );
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("600"))
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );
    }

    /// The publisher's own text says what the publisher wants, so a file
    /// whose only rules are the publisher's is served.
    #[tokio::test]
    async fn the_publishers_own_rules_are_served_when_no_contributor_adds_any() {
        let settings = settings_selecting(
            serde_json::json!({
                "modules": ["silent"],
                "top_text": "User-agent: *\nDisallow: /private/",
            }),
            SILENT_BUILDER_MODULE,
        );
        let registry = IntegrationRegistry::with_registrations(&settings, &[silent_builder()])
            .expect("should build the registry");

        let response = handle_robots_txt(
            &settings,
            &services_with(Arc::default(), Arc::default()),
            &registry,
            robots_request(),
        )
        .await
        .expect("should answer");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            text_of(response).await,
            "User-agent: *\nDisallow: /private/\n\n# nothing to add\n"
        );
    }

    /// A contributor whose rule says what it could read of the connection
    /// that asked.
    #[derive(Debug)]
    struct ReadsTheConnection;

    impl ReadsTheConnection {
        fn rules(&self, services: &RuntimeServices) -> Contribution {
            let client = services.client_info();
            Contribution::parse(&format!(
                "User-Agent: ConnectionBot\nDisallow: /ip-{}/tls-{}/ja4-{}/edge-{}/signals-{}",
                client.client_ip.is_some(),
                client.tls_protocol.is_some(),
                client.tls_ja4.is_some(),
                client.server_hostname.is_some(),
                services.host_signals().is_some(),
            ))
        }
    }

    #[async_trait::async_trait(?Send)]
    impl RobotsTxtContributor for ReadsTheConnection {
        fn refresh(&self) -> core::time::Duration {
            core::time::Duration::from_secs(60)
        }

        fn fingerprint(&self) -> String {
            "connection".to_owned()
        }

        async fn contribute(
            &self,
            call: crate::module_context::ModuleCall<'_>,
        ) -> Result<Contribution, Report<TrustedServerError>> {
            Ok(call.inject(self, Self::rules)?)
        }
    }

    /// The signals a host computes from a crawler's connection.
    #[derive(Debug)]
    struct CrawlerSignals;

    impl crate::evidence::HostSignals for CrawlerSignals {
        fn ja4(&self) -> Option<&str> {
            Some("t13d1516h2_8daaf6152771_02713d6af862")
        }

        fn h2(&self) -> Option<&str> {
            Some("1:65536;2:0;4:6291456")
        }
    }

    /// The id of the builder that registers the connection reading
    /// contributor.
    const CONNECTION_BUILDER: &str = "robots-txt-connection";

    /// The module name that builder is selected by.
    const CONNECTION_BUILDER_MODULE: &str = "testing.robots-txt-connection";

    fn register_connection(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder(CONNECTION_BUILDER)
                .with_robots_txt_contributor("robots-txt.connection", Arc::new(ReadsTheConnection))
                .build(),
        ))
    }

    /// The answer is served to every crawler, so the services a contributor
    /// is handed carry nothing of the connection that asked: no address, no
    /// TLS details, no edge server and no host signals.
    #[tokio::test]
    async fn a_contributor_is_handed_nothing_of_the_connection_that_asked() {
        let settings = settings_selecting(
            serde_json::json!({ "modules": ["connection"] }),
            CONNECTION_BUILDER_MODULE,
        );
        let builder = IntegrationBuilder::new(
            CONNECTION_BUILDER,
            CONNECTION_BUILDER,
            register_connection,
            accept,
        )
        .with_module_name(CONNECTION_BUILDER_MODULE);
        let registry = IntegrationRegistry::with_registrations(&settings, &[builder])
            .expect("should build the registry");
        let connection = crate::platform::ClientInfo {
            client_ip: Some("203.0.113.7".parse().expect("should parse the address")),
            tls_protocol: Some("TLSv1.3".to_owned()),
            tls_ja4: Some("t13d1516h2_8daaf6152771_02713d6af862".to_owned()),
            server_hostname: Some("cache-edge-1".to_owned()),
            ..crate::platform::ClientInfo::default()
        };
        let services = RuntimeServices {
            host_signals: Some(Arc::new(CrawlerSignals)),
            ..services_with(Arc::default(), Arc::default()).with_client_info(connection)
        };

        let response = handle_robots_txt(&settings, &services, &registry, robots_request())
            .await
            .expect("should answer");
        let file = text_of(response).await;

        assert!(
            file.contains("Disallow: /ip-false/tls-false/ja4-false/edge-false/signals-false"),
            "the contributor should read nothing of the connection: {file}"
        );
    }
}
