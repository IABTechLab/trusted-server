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
//!    refusal and nothing else, and the origin cannot loosen it either,
//!    because the file takes the place of whatever the origin answers to a
//!    `GET` or `HEAD` that names its host.
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
//!    a crawler as permission to crawl everything.
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
pub mod middleware;

use std::collections::BTreeMap;
use std::sync::Arc;

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::Response;
use http::header::{self, HeaderValue};
use serde::{Deserialize, Deserializer, Serialize};
use validator::{Validate, ValidationError};

pub use contributor::{ALLOW_ALL, REFUSE_ALL, RobotsTxtContributor};
pub use document::{Contribution, Group, Record};

use crate::error::TrustedServerError;
use crate::module_context::ResolvedRequest;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

/// The path this module answers.
pub const ROBOTS_TXT_PATH: &str = "/robots.txt";

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
    /// allowances of their own. Needs no contributor and no key. The origin is
    /// still asked, and the file takes the place of whatever it answers.
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

/// One entry of `modules` as the registry resolved it: the name a
/// contributor was declared under and the contributor, or `allow_all` and
/// none.
pub(crate) type Selected = (&'static str, Option<Arc<dyn RobotsTxtContributor>>);

/// The entries of `config.modules` resolved against `contributors`, the
/// contributors a deployment runs by the names they were declared under.
///
/// # Errors
///
/// When an entry names a contributor `contributors` does not carry, which
/// the registry refuses before it asks, so this is a wiring fault.
pub(crate) fn select(
    config: &RobotsTxtConfig,
    contributors: &[(&'static str, Arc<dyn RobotsTxtContributor>)],
) -> Result<Vec<Selected>, Report<TrustedServerError>> {
    let mut selected = Vec::with_capacity(config.modules.len());
    for written in &config.modules {
        if written == REFUSE_ALL {
            // The refusal needs no contributor, and the middleware answers it
            // before asking any.
            continue;
        }
        if written == ALLOW_ALL {
            selected.push((ALLOW_ALL, None));
            continue;
        }
        let names: Vec<&str> = contributors.iter().map(|(name, _)| *name).collect();
        let Some((declared, contributor)) =
            crate::module_name::resolve(MODULE_TYPE, written, &names)
                .and_then(|declared| contributors.iter().find(|(name, _)| *name == declared))
        else {
            return Err(Report::new(configuration(format!(
                "[robots-txt] modules names `{written}`, which no module in this deployment supplies"
            ))));
        };
        selected.push((declared, Some(Arc::clone(contributor))));
    }
    Ok(selected)
}

/// The file for `config`, from the contributions of `selected` in order,
/// each asked for `request`.
pub(crate) async fn file_for(
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
    Ok(document::assemble(config, &contributions))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::holding::tests::{
        ECHOED, Echoing, MemoryKvStore, Scripted, request, robots_request, rules, services_with,
        settings,
    };
    use super::*;
    use crate::integrations::{IntegrationBuilder, IntegrationRegistration, IntegrationRegistry};
    use crate::middleware::Middleware;
    use crate::module_context::{ModuleContext, ModuleRequest};
    use http::StatusCode;

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

    /// What the middleware prepares for `settings`, with the services given,
    /// through the registry that made it from `[robots-txt]`, as the handler
    /// runs it for `/robots.txt` in the fetch phase.
    async fn prepared_by(
        settings: &Settings,
        extra: &[IntegrationBuilder],
        services: &RuntimeServices,
    ) -> Result<Option<String>, crate::middleware::Refusal> {
        use crate::middleware::{MiddlewarePhase, Prepared};

        let registry = IntegrationRegistry::with_registrations(settings, extra)
            .expect("should build the registry");
        let chain = registry.middleware_chain(
            &settings.fetch,
            MiddlewarePhase::Fetch,
            middleware::MEDIA_TYPE,
            ROBOTS_TXT_PATH,
        );
        assert_eq!(
            chain.ids(),
            [middleware::MODULE],
            "the entry names the middleware"
        );
        let request = robots_request();
        let module_context =
            ModuleContext::new(ModuleRequest::of(&request, services.client_info()))
                .with_settings(settings)
                .with_services(services);
        let state = crate::integrations::IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = crate::middleware::MiddlewareContext {
            prepared: &prepared,
            ..crate::middleware::test_support::context(MiddlewarePhase::Fetch, &state)
        };
        chain.prepare(&module_context, &context).await?;
        Ok(prepared.take::<String>(MiddlewarePhase::Fetch, middleware::MODULE))
    }

    /// `settings` with `config` as its `[robots-txt]` section and the entry
    /// that places the middleware on the file.
    fn settings_placing(config: RobotsTxtConfig) -> Settings {
        let mut settings = settings();
        settings.robots_txt = Some(config);
        settings.fetch =
            crate::middleware::PhaseEntries::new(vec![crate::middleware::PhaseEntry {
                media_type: middleware::MEDIA_TYPE.to_owned(),
                path: Some(ROBOTS_TXT_PATH.to_owned()),
                middleware: vec![middleware::MODULE.to_owned()],
            }]);
        settings
    }

    /// Leaving out the rules of a contributor that failed would allow by
    /// omission what those rules refused, so the whole file fails closed.
    #[tokio::test]
    async fn with_nothing_held_a_failure_refuses_the_request() {
        let failing: Arc<dyn RobotsTxtContributor> =
            Arc::new(Scripted::new(vec![Err("cannot answer".to_owned())]));
        let config = parsed(serde_json::json!({ "modules": ["failing", "allow_all"] }));
        let middleware =
            middleware::RobotsTxt::new(config, vec![("failing", Some(failing)), (ALLOW_ALL, None)]);
        let settings = settings();
        let services = services_with(Arc::default(), Arc::default());
        let request = robots_request();
        let module_context =
            ModuleContext::new(ModuleRequest::of(&request, services.client_info()))
                .with_settings(&settings)
                .with_services(&services);
        let state = crate::integrations::IntegrationDocumentState::default();
        let context = crate::middleware::test_support::context(
            crate::middleware::MiddlewarePhase::Fetch,
            &state,
        );

        let refusal = middleware
            .prepare(
                module_context.call(
                    middleware::MODULE,
                    crate::permissions::PermissionSet::none(),
                ),
                &context,
            )
            .expect("the middleware prepares")
            .await
            .expect_err("a crawler reads an empty or allow everything file as permission");

        assert_eq!(refusal.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            refusal.retry_after,
            Some(std::time::Duration::from_secs(u64::from(
                RETRY_AFTER_SECONDS
            )))
        );
        assert_eq!(refusal.message, "robots.txt is temporarily unavailable\n");
    }

    /// The refusal asks nothing of anything, and dominates whatever else is
    /// selected, because a refusal cannot be loosened.
    #[tokio::test]
    async fn refusing_every_crawler_asks_no_contributor_and_reads_no_store() {
        let mut refusing = parsed(serde_json::json!({ "modules": ["allow_all", "refuse_all"] }));
        refusing.always_allow = vec!["/ads.txt".to_owned()];
        let settings = settings_placing(refusing);
        let store = Arc::new(MemoryKvStore::default());

        let file = prepared_by(
            &settings,
            &[],
            &services_with(Arc::default(), Arc::clone(&store)),
        )
        .await
        .expect("the refusal answers without asking")
        .expect("the refusal is prepared");

        assert_eq!(
            file, "User-agent: *\nAllow: /ads.txt\nDisallow: /\n",
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

    /// The registry makes the middleware only for settings that carry the
    /// section, so a publisher keeping their own file is left alone.
    #[test]
    fn the_middleware_exists_only_with_the_section() {
        let without = IntegrationRegistry::new(&settings()).expect("should build the registry");
        assert!(
            !without.middleware_ids().contains(&middleware::MODULE),
            "{:?}",
            without.middleware_ids()
        );

        let with = IntegrationRegistry::new(&settings_placing(RobotsTxtConfig::refuse_all()))
            .expect("should build the registry");
        assert!(
            with.middleware_ids().contains(&middleware::MODULE),
            "{:?}",
            with.middleware_ids()
        );
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

    /// The middleware asks a contributor for the request it is answering,
    /// being its host and path, without the query and without the reader's
    /// evidence.
    #[tokio::test]
    async fn the_middleware_asks_the_contributors_for_the_request_it_answers() {
        let mut settings = settings_selecting_echo();
        settings.fetch = settings_placing(RobotsTxtConfig::refuse_all()).fetch;

        let file = prepared_by(
            &settings,
            &[echo_builder()],
            &services_with(Arc::default(), Arc::default()),
        )
        .await
        .expect("should answer")
        .expect("the file is prepared");

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
}
