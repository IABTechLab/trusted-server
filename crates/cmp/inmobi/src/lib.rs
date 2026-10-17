//! `InMobi` Choice, a TCF consent prompt loaded from the publisher's own
//! account, as a consent management platform module.
//!
//! The prompt is the first thing a reader sees and no advertising may run
//! before they answer it. Its tag is this module's middleware, written where
//! a `[[fetch]]` entry names [`MODULE`], before any vendor's tag. The
//! middleware writes the IAB TCF stub and `InMobi`'s GPP stub ahead of
//! the deferred Choice loader, so a call to `__tcfapi` or `__gpp` made before
//! the prompt arrives is answered or queued rather than thrown, and each stub
//! steps aside where a page already has its interface. The prompt writes the
//! reader's answer where the TCF permission signal reads it, so the answer
//! reaches the server on the next request.

use std::sync::Arc;

use error_stack::Report;
use serde::Deserialize;
use validator::Validate;

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};
use trusted_server_core::middleware::{
    HTML_MEDIA_TYPE, Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase, PhaseEntry,
};
use trusted_server_core::settings::{IntegrationConfig, Settings};

/// The id the prompt's registration carries.
const INMOBI_INTEGRATION_ID: &str = "inmobi";

/// The name `[cmp]` selects the prompt by and a `[[fetch]]` entry names its
/// middleware by, from this crate's folder. Its settings are the table `[cmp.inmobi]`.
pub const MODULE: &str = "cmp.inmobi";

/// Hosts a Choice tag may be served from.
const ALLOWED_HOSTS: [&str; 2] = ["cmp.inmobi.com", "cdn.inmobi.com"];

/// The IAB TCF v2 stub, written ahead of the loader.
const TCF_STUB_SCRIPT: &str = include_str!("tcf_stub.js");

/// `InMobi`'s GPP stub, written after the TCF stub and ahead of the loader.
const GPP_STUB_SCRIPT: &str = include_str!("gpp_stub.js");

/// The stub as the head carries it, without the file's line ending, which a
/// checkout may write as either form.
fn tcf_stub() -> String {
    format!("<script>{}</script>", TCF_STUB_SCRIPT.trim_end())
}

/// The GPP stub as the head carries it, told the CMP id. The page carries the
/// same bytes whichever line ending a checkout gave the file.
fn gpp_stub(cmp_id: u16) -> String {
    let script = GPP_STUB_SCRIPT.trim_end().replace("\r\n", "\n");
    format!("<script>{script}({cmp_id});</script>")
}

/// Configuration for the `InMobi` Choice consent prompt.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct InMobiChoiceConfig {
    /// The account's Choice loader, which is per publisher and per site.
    ///
    /// Checked by [`validate_script_url`] rather than only by the type,
    /// because this becomes a `<script src>` on every page.
    pub script_url: String,

    /// The IAB registered CMP identifier, which is 10 for `InMobi`, from 1 to
    /// [`MAX_CMP_ID`].
    ///
    /// The GPP stub the middleware writes answers `ping` with it while the
    /// prompt loads, as `InMobi`'s own tag does.
    #[serde(default = "default_cmp_id", deserialize_with = "deserialize_cmp_id")]
    pub cmp_id: u16,
}

/// The largest CMP id, because a TC string holds the id in 12 bits.
const MAX_CMP_ID: u16 = 4095;

fn default_cmp_id() -> u16 {
    10
}

/// Reads `cmp_id` as a whole number from 1 to [`MAX_CMP_ID`], or refuses it
/// with a message naming the setting, which a type's own parse error does
/// not. Zero is what a stub reports before it knows its id, so it is no id.
fn deserialize_cmp_id<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    let id = i64::deserialize(deserializer).map_err(|error| {
        D::Error::custom(format!(
            "`cmp_id` is not a whole number from 1 to {MAX_CMP_ID}: {error}"
        ))
    })?;
    u16::try_from(id)
        .ok()
        .filter(|id| (1..=MAX_CMP_ID).contains(id))
        .ok_or_else(|| {
            D::Error::custom(format!(
                "`cmp_id` is {id}, and a CMP id is a whole number from 1 to {MAX_CMP_ID}"
            ))
        })
}

impl IntegrationConfig for InMobiChoiceConfig {}

/// Refuse a script URL that should never reach a reader's browser, being one
/// that is empty, is not a URL, is not `https`, is not on an `InMobi` Choice host
/// or carries a user name or password.
///
/// The value becomes a `<script src>` on every page, and a settings document
/// reaches the service from a config store rather than from a person reading
/// it, so the URL is checked rather than trusted. It is parsed and its host
/// compared with an allow list rather than matched against a pattern, so a
/// host that merely contains an allowed name is refused on what it is rather
/// than on what it looks like.
fn validate_script_url(url: &str) -> Result<(), Report<TrustedServerError>> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(Report::new(TrustedServerError::Settings {
            message: "`script_url` in [cmp.inmobi] is empty".to_owned(),
        }));
    }
    let parsed = url::Url::parse(trimmed).map_err(|error| {
        Report::new(TrustedServerError::Settings {
            message: format!("`script_url` in [cmp.inmobi] is not a URL: {error}"),
        })
    })?;
    if parsed.scheme() != "https" {
        return Err(Report::new(TrustedServerError::Settings {
            message: format!(
                "`script_url` in [cmp.inmobi] is {:?}, not https",
                parsed.scheme()
            ),
        }));
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if !ALLOWED_HOSTS.contains(&host.as_str()) {
        return Err(Report::new(TrustedServerError::Settings {
            message: format!(
                "`script_url` in [cmp.inmobi] points at {host:?}, which is not \
                 an InMobi Choice host. Allowed: {}",
                ALLOWED_HOSTS.join(", ")
            ),
        }));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Report::new(TrustedServerError::Settings {
            message: "`script_url` in [cmp.inmobi] carries a user name or password, \
                      which every reader's browser would be sent"
                .to_owned(),
        }));
    }
    Ok(())
}

/// Refuse a document that selects the prompt but leaves a page served as
/// `text/html` without it.
///
/// A response takes the first entry that covers it, so every `text/html`
/// entry up to the first that covers every path has to name the prompt, and
/// such an entry has to exist. A path covers every page when the home page,
/// `/`, begins with it.
fn require_an_entry(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    let refusal = |gap: String| {
        Report::new(TrustedServerError::Settings {
            message: format!("[cmp] selects `inmobi`, but {gap}"),
        })
    };
    let names_the_prompt = |entry: &PhaseEntry| entry.middleware.iter().any(|name| name == MODULE);
    let entries = settings
        .fetch
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.media_type == HTML_MEDIA_TYPE);
    if !entries.clone().any(|(_, entry)| names_the_prompt(entry)) {
        return Err(refusal(format!(
            "no [[fetch]] entry for \"{HTML_MEDIA_TYPE}\" names `{MODULE}`, so the consent \
             prompt would never be written. Name it in that entry, before any middleware \
             that writes a vendor's tag"
        )));
    }
    let mut paths: Vec<String> = Vec::new();
    for (index, entry) in entries {
        let path = entry.path.as_deref().filter(|path| !"/".starts_with(path));
        if !names_the_prompt(entry) {
            let pages = match path {
                Some(path) => format!("the pages under `{path}`"),
                None if paths.is_empty() => "every page".to_owned(),
                None => format!("every page outside {}", paths.join(", ")),
            };
            return Err(refusal(format!(
                "[[fetch]] entry {} covers {pages} and does not name `{MODULE}`, so those \
                 pages would be served with no consent prompt. Name it in that entry, before \
                 any middleware that writes a vendor's tag",
                index + 1
            )));
        }
        match path {
            Some(path) => paths.push(format!("`{path}`")),
            None => return Ok(()),
        }
    }
    Err(refusal(format!(
        "the [[fetch]] entries for \"{HTML_MEDIA_TYPE}\" cover only the pages under {}, so \
         every other page, `/` among them, would be served with no consent prompt. Name \
         `{MODULE}` in an entry with no path as well, after the entries with one",
        paths.join(", ")
    )))
}

/// Validates the `InMobi` Choice configuration and reports whether a section
/// selects the module.
///
/// # Errors
///
/// Returns an error when the configuration cannot be parsed, fails
/// validation, carries a script URL that should not be served, or names the
/// prompt in no entry.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<InMobiChoiceConfig>(MODULE)? else {
        return Ok(false);
    };
    validate_script_url(&config.script_url)?;
    require_an_entry(settings)?;
    Ok(true)
}

/// Registers the `InMobi` Choice prompt when a section selects it.
///
/// # Errors
///
/// Returns an error when the configuration cannot be parsed, carries a
/// script URL that should not be served, or names the prompt in no entry.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<InMobiChoiceConfig>(MODULE)? else {
        return Ok(None);
    };
    validate_script_url(&config.script_url)?;
    require_an_entry(settings)?;
    Ok(Some(
        IntegrationRegistration::builder(INMOBI_INTEGRATION_ID)
            // No browser module. The two stubs and the Choice script are the
            // whole client side.
            .without_js()
            .with_middleware(Arc::new(Choice::new(&config.script_url, config.cmp_id)))
            .build(),
    ))
}

/// The builder a deployment installs to offer this prompt.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(
        INMOBI_INTEGRATION_ID,
        env!("CARGO_PKG_NAME"),
        register,
        validate,
    )
    .with_module_name(MODULE)
}

/// The Choice consent prompt's tag, being the TCF stub, the GPP stub and the
/// loader.
#[derive(Debug)]
pub struct Choice {
    tag: Arc<str>,
}

impl Choice {
    /// The prompt for an account's script URL, which the caller has checked
    /// is `https` on an `InMobi` host, with the CMP id the GPP stub answers
    /// `ping` with.
    #[must_use]
    pub(crate) fn new(script_url: &str, cmp_id: u16) -> Self {
        Self {
            tag: Arc::from(format!(
                "{}{}<script defer src=\"{}\"></script>",
                tcf_stub(),
                gpp_stub(cmp_id),
                escape_attribute(script_url.trim())
            )),
        }
    }
}

impl Middleware for Choice {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        MiddlewareAction {
            head_inserts: vec![self.tag.to_string()],
            ..MiddlewareAction::pass()
        }
    }

    /// The whole tag, because the stubs are compiled into this crate and no
    /// setting changes when they do.
    fn template_key(&self) -> &str {
        &self.tag
    }
}

/// Escapes a value for an HTML double quoted attribute.
///
/// The URL is checked before it reaches here, so this is the second of two
/// checks, because a URL on the right host can still carry a quote in its
/// query that would end the attribute early.
fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::html_processor::test_support::{
        assert_page_is_recorded, page_settings,
    };
    use trusted_server_core::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use trusted_server_core::middleware::test_support::context;
    use trusted_server_core::middleware::{MiddlewareChain, PhaseEntries};

    const REAL: &str = "https://cmp.inmobi.com/choice/EXAMPLEacct00/www.publisher.example/choice.js?tag_version=V3";

    /// `InMobi`'s registered CMP id.
    const CMP_ID: u16 = 10;

    /// A document selecting the prompt, named in a fetch entry.
    fn settings_with(url: &str) -> Settings {
        page_settings(&format!(
            "[cmp]\nmodule = \"inmobi\"\n\n[cmp.inmobi]\nscript_url = {}\n\n\
             [[fetch]]\nmedia_type = \"text/html\"\nmiddleware = [\"cmp.inmobi\"]\n",
            serde_json::to_string(url).expect("a string")
        ))
    }

    /// A document selecting the prompt at [`REAL`], with `rest` written after
    /// `script_url`, being more of `[cmp.inmobi]` and then the entries.
    fn settings_with_rest(rest: &str) -> Settings {
        page_settings(&format!(
            "[cmp]\nmodule = \"inmobi\"\n\n[cmp.inmobi]\nscript_url = \"{REAL}\"\n{rest}"
        ))
    }

    /// A `[[fetch]]` entry for `text/html`, covering the pages under `path`
    /// where one is given, that names `middleware` in order.
    fn html_entry(path: Option<&str>, middleware: &[&str]) -> String {
        let path = path
            .map(|path| format!("path = \"{path}\"\n"))
            .unwrap_or_default();
        format!(
            "\n[[fetch]]\nmedia_type = \"text/html\"\n{path}middleware = {}\n",
            serde_json::to_string(middleware).expect("a list")
        )
    }

    /// Whether a page asked for at `path` gets the prompt from `settings`'
    /// entries.
    fn gets_the_prompt(settings: &Settings, path: &str) -> bool {
        settings
            .fetch
            .for_response(HTML_MEDIA_TYPE, path)
            .iter()
            .any(|name| name == MODULE)
    }

    /// Checks that `validate` and `register` both refuse `settings`, each
    /// with a message that says every one of `reasons`.
    fn assert_refused(settings: &Settings, reasons: &[&str]) {
        for (call, refusal) in [
            ("validate", validate(settings).err()),
            ("register", register(settings).err()),
        ] {
            let message = refusal
                .unwrap_or_else(|| panic!("{call} should refuse the document"))
                .current_context()
                .to_string();
            for reason in reasons {
                assert!(
                    message.contains(reason),
                    "{call} should say {reason:?}, and said {message:?}"
                );
            }
        }
    }

    /// The head markup `middleware` write in `phase`, in the order written.
    fn head_markup(phase: MiddlewarePhase, middleware: Vec<Arc<dyn Middleware>>) -> Vec<String> {
        let document_state = IntegrationDocumentState::default();
        MiddlewareChain::new(
            phase,
            trusted_server_core::middleware::HTML_MEDIA_TYPE,
            middleware,
        )
        .plan(&context(phase, &document_state))
        .expect("the chain should plan")
        .head_inserts
    }

    /// The one piece of head markup the prompt writes in `phase`.
    fn tag_in(phase: MiddlewarePhase, prompt: Choice) -> String {
        let mut markup = head_markup(phase, vec![Arc::new(prompt)]);
        assert_eq!(
            markup.len(),
            1,
            "{phase}: the prompt writes one piece: {markup:?}"
        );
        markup.remove(0)
    }

    /// A stand-in for a vendor middleware that writes its own tag into the
    /// head, such as a tag manager.
    #[derive(Debug)]
    struct VendorTag;

    impl Middleware for VendorTag {
        fn middleware_id(&self) -> &'static str {
            "vendor.tag"
        }

        fn phases(&self) -> &[MiddlewarePhase] {
            &MiddlewarePhase::ALL
        }

        fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
            MiddlewareAction {
                head_inserts: vec![
                    "<script src=\"https://vendor.example/tag.js\"></script>".to_owned(),
                ],
                ..MiddlewareAction::pass()
            }
        }
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }

    #[test]
    fn an_unselected_module_does_not_register() {
        let settings = page_settings("");
        assert!(
            register(&settings)
                .expect("should read a document that selects nothing")
                .is_none()
        );
        assert!(!validate(&settings).expect("should read a document that selects nothing"));
    }

    #[test]
    fn the_prompt_runs_in_the_fetch_phase_alone() {
        assert_eq!(
            Choice::new(REAL, CMP_ID).phases(),
            [MiddlewarePhase::Fetch],
            "the tag is the same for every reader, so it belongs in the stored page"
        );
    }

    #[test]
    fn the_prompt_is_offered_as_the_modules_middleware() {
        let registration = register(&settings_with(REAL))
            .expect("should parse")
            .expect("should register");

        assert!(
            registration
                .middleware
                .iter()
                .any(|middleware| middleware.middleware_id() == MODULE),
            "the prompt should be offered for an entry to write"
        );
        assert!(
            registration.js_disabled,
            "the stubs and the Choice script are the whole client side"
        );
    }

    /// Selected but named in no entry, the prompt would never be written.
    #[test]
    fn a_document_naming_the_prompt_in_no_entry_is_refused() {
        let settings = page_settings(&format!(
            "[cmp]\nmodule = \"inmobi\"\n\n[cmp.inmobi]\nscript_url = \"{REAL}\"\n"
        ));

        let refused = validate(&settings).expect_err("should refuse a prompt no entry names");
        assert!(
            refused
                .current_context()
                .to_string()
                .contains("no [[fetch]] entry for \"text/html\" names `cmp.inmobi`"),
            "{refused:?}"
        );
        assert!(register(&settings).is_err());
    }

    /// Only an entry for `text/html` writes the prompt into a page, so an
    /// entry for another media type naming it does not place it.
    #[test]
    fn an_entry_for_another_media_type_does_not_place_the_prompt() {
        let mut settings = settings_with(REAL);
        settings.fetch = PhaseEntries::new(vec![PhaseEntry {
            media_type: "application/javascript".to_owned(),
            path: None,
            middleware: vec![MODULE.to_owned()],
        }]);

        let refused = validate(&settings).expect_err("should refuse a prompt no page entry names");
        assert!(
            refused
                .current_context()
                .to_string()
                .contains("no [[fetch]] entry for \"text/html\" names `cmp.inmobi`"),
            "{refused:?}"
        );
        assert!(register(&settings).is_err());
    }

    /// A prompt entry with a path, and no entry for the rest of the site,
    /// leaves every other page with no prompt, so it is refused naming the
    /// pages it leaves out.
    #[test]
    fn a_prompt_entry_limited_to_a_path_is_refused_naming_the_pages_left_out() {
        let settings = settings_with_rest(&html_entry(Some("/news/"), &[MODULE]));
        assert!(
            gets_the_prompt(&settings, "/news/today") && !gets_the_prompt(&settings, "/"),
            "the entry puts the prompt on the pages under /news/ alone"
        );

        assert_refused(
            &settings,
            &[
                "cover only the pages under `/news/`",
                "`/` among them",
                "with no consent prompt",
            ],
        );
    }

    /// A response takes the first entry that covers it, so an earlier entry
    /// for part of the site that does not name the prompt leaves those pages
    /// without it, whatever a later entry names.
    #[test]
    fn an_earlier_entry_without_the_prompt_is_refused_naming_its_pages() {
        let settings = settings_with_rest(&format!(
            "{}{}",
            html_entry(Some("/news/"), &["vendor.tag"]),
            html_entry(None, &[MODULE, "vendor.tag"])
        ));
        assert!(
            !gets_the_prompt(&settings, "/news/today") && gets_the_prompt(&settings, "/"),
            "the pages under /news/ get the vendor's tag and no prompt"
        );

        assert_refused(
            &settings,
            &[
                "[[fetch]] entry 1 covers the pages under `/news/` and does not name \
                 `cmp.inmobi`",
                "with no consent prompt",
            ],
        );
    }

    /// Entries that each name the prompt, the last covering every path, put
    /// it on every page and are accepted, and so is one entry for `/`.
    #[test]
    fn entries_that_put_the_prompt_on_every_page_are_accepted() {
        for entries in [
            format!(
                "{}{}",
                html_entry(Some("/news/"), &[MODULE, "vendor.tag"]),
                html_entry(None, &[MODULE])
            ),
            html_entry(Some("/"), &[MODULE]),
        ] {
            let settings = settings_with_rest(&entries);
            for path in ["/", "/news/today", "/sport/"] {
                assert!(
                    gets_the_prompt(&settings, path),
                    "{path} should get the prompt from {entries}"
                );
            }
            assert!(
                validate(&settings).expect("should accept the entries"),
                "{entries}"
            );
            assert!(
                register(&settings)
                    .expect("should accept the entries")
                    .is_some(),
                "{entries}"
            );
        }
    }

    /// A script URL that should never reach a reader's browser is refused at
    /// load and at registration, with a message naming the section and the
    /// reason.
    #[test]
    fn a_script_url_that_should_never_be_served_is_refused_for_its_reason() {
        // (case, script URL, the reason the refusal must give)
        let cases = [
            ("an empty URL", "   ", "is empty"),
            (
                "a value that is not a URL",
                "cmp.inmobi.com/choice.js",
                "is not a URL",
            ),
            (
                "plain http, which whoever carries the page can replace",
                "http://cmp.inmobi.com/choice/a/b/choice.js",
                "is \"http\", not https",
            ),
            (
                "a host that is not InMobi's",
                "https://evil.example.com/choice.js",
                "points at \"evil.example.com\", which is not an InMobi Choice host",
            ),
            (
                "a host that merely contains an allowed name",
                "https://cmp.inmobi.com.evil.example/choice.js",
                "points at \"cmp.inmobi.com.evil.example\", which is not an InMobi",
            ),
            (
                "an allowed name before an `@`, which makes it a user name",
                "https://cmp.inmobi.com@evil.example/choice.js",
                "points at \"evil.example\", which is not an InMobi",
            ),
            (
                "a user name and password on an allowed host",
                "https://user:secret@cmp.inmobi.com/choice/a/b/choice.js",
                "carries a user name or password",
            ),
            (
                "a user name alone on an allowed host",
                "https://user@cmp.inmobi.com/choice/a/b/choice.js",
                "carries a user name or password",
            ),
            (
                "a password alone on an allowed host",
                "https://:secret@cmp.inmobi.com/choice/a/b/choice.js",
                "carries a user name or password",
            ),
        ];
        for (case, url, reason) in cases {
            let settings = settings_with(url);
            for (call, refusal) in [
                ("validate", validate(&settings).err()),
                ("register", register(&settings).err()),
            ] {
                let message = refusal
                    .unwrap_or_else(|| panic!("{case}: {call} should refuse {url:?}"))
                    .current_context()
                    .to_string();
                assert!(
                    message.contains("`script_url` in [cmp.inmobi]") && message.contains(reason),
                    "{case}: {call} should say {reason:?}, and said {message:?}"
                );
            }
        }
    }

    #[test]
    fn the_cmp_id_defaults_to_inmobis_registered_number() {
        let config = settings_with(REAL)
            .module_config::<InMobiChoiceConfig>(MODULE)
            .expect("should read the table")
            .expect("should select the module");
        assert_eq!(config.cmp_id, CMP_ID);
    }

    /// The CMP id in `[cmp.inmobi]` is the one the GPP stub is told.
    #[test]
    fn the_configured_cmp_id_reaches_the_gpp_stub() {
        let settings = page_settings(&format!(
            "[cmp]\nmodule = \"inmobi\"\n\n[cmp.inmobi]\nscript_url = \"{REAL}\"\ncmp_id = 42\n\n\
             [[fetch]]\nmedia_type = \"text/html\"\nmiddleware = [\"cmp.inmobi\"]\n"
        ));
        let registration = register(&settings)
            .expect("should parse")
            .expect("should register");

        let markup = head_markup(MiddlewarePhase::Fetch, registration.middleware).join("");

        assert!(markup.contains("})(42);</script>"), "{markup}");
        assert!(!markup.contains("})(10);</script>"), "{markup}");
    }

    /// A CMP id is a whole number the IAB assigned, which a TC string holds
    /// in 12 bits, so any other value is refused when the settings load, with
    /// a message naming the setting and never its value.
    #[test]
    fn a_cmp_id_outside_1_to_4095_is_refused_naming_the_setting() {
        // The test settings name the template's placeholder origin, which the
        // load refuses before any module's own check, so a real one stands in.
        let with_cmp_id = |value: &str| {
            let mut settings = settings_with_rest(&format!(
                "cmp_id = {value}\n{}",
                html_entry(None, &[MODULE])
            ));
            settings.publisher.origin_url = "https://origin.publisher.example".to_owned();
            settings
        };
        let load = |settings: &Settings| {
            trusted_server_core::config::validate_settings_for_runtime_with(
                settings,
                &[super::builder()],
            )
        };

        // (case, the value as written)
        let cases = [
            ("zero, which a stub reports before it has an id", "0"),
            ("past the 12 bits a TC string holds", "4096"),
            ("past what the field could hold", "70000"),
            ("below zero", "-1"),
            ("a string", "\"10\""),
            ("a fraction", "10.5"),
        ];
        for (case, value) in cases {
            let settings = with_cmp_id(value);
            for (call, refusal) in [
                ("loading the settings", load(&settings).err()),
                ("validate", validate(&settings).err()),
                ("register", register(&settings).err()),
            ] {
                let message = refusal
                    .unwrap_or_else(|| panic!("{case}: {call} should refuse cmp_id = {value}"))
                    .current_context()
                    .to_string();
                assert!(
                    message.contains("[cmp.inmobi]") && message.contains("`cmp_id`"),
                    "{case}: {call} should name the table and the setting, and said {message:?}"
                );
                assert!(
                    !message.contains(value),
                    "{case}: {call} should not quote the value, and said {message:?}"
                );
            }
        }

        for (value, id) in [("1", 1), ("4095", 4095)] {
            let settings = with_cmp_id(value);
            load(&settings).expect("should load a CMP id in range");
            let config = settings
                .module_config::<InMobiChoiceConfig>(MODULE)
                .expect("should read the table")
                .expect("should select the module");
            assert_eq!(config.cmp_id, id);
        }
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        let settings = page_settings(&format!(
            "[cmp]\nmodule = \"inmobi\"\n\n[cmp.inmobi]\nscript_url = \"{REAL}\"\ntypo = true\n"
        ));
        assert!(
            settings
                .module_config::<InMobiChoiceConfig>(MODULE)
                .is_err()
        );
    }

    /// The tag is written once, deferred and carrying the account's URL.
    #[test]
    fn the_tag_is_deferred_and_carries_the_url() {
        let tag = tag_in(MiddlewarePhase::Fetch, Choice::new(REAL, CMP_ID));

        let loader = "<script defer src=\"https://cmp.inmobi.com/choice/EXAMPLEacct00/www.publisher.example/choice.js?tag_version=V3\"></script>";
        assert_eq!(tag.matches(loader).count(), 1, "{tag}");
        assert!(tag.ends_with(loader), "the loader comes last: {tag}");
    }

    /// The prompt's loader calls `__tcfapi` and `__gpp` as soon as it runs,
    /// so each stub has to be ahead of the loader, once, the TCF stub first.
    #[test]
    fn the_stubs_go_ahead_of_the_loader_once_each_with_the_tcf_stub_first() {
        let tag = tag_in(MiddlewarePhase::Fetch, Choice::new(REAL, 42));

        let tcf = tcf_stub();
        let gpp = gpp_stub(42);
        assert!(gpp.ends_with("})(42);</script>"), "{gpp}");
        assert_eq!(tag.matches(&tcf).count(), 1, "{tag}");
        assert_eq!(tag.matches(&gpp).count(), 1, "{tag}");
        let position = |needle: &str| {
            tag.find(needle)
                .unwrap_or_else(|| panic!("{needle} belongs in the tag: {tag}"))
        };
        assert!(
            position(&tcf) < position(&gpp) && position(&gpp) < position("cmp.inmobi.com"),
            "the order is TCF stub, GPP stub, loader: {tag}"
        );
    }

    /// The stubs are compiled into this crate and no setting changes when
    /// they do, so they have to be in the text the registered middleware
    /// keys a stored page by, or a page stored before a change to a stub
    /// keeps the old stub until it expires.
    #[test]
    fn the_stubs_are_in_what_keys_a_stored_page() {
        let settings = settings_with(REAL);
        let registry = IntegrationRegistry::with_registrations(&settings, &[super::builder()])
            .expect("should build the registry");

        let keys = registry.middleware_template_keys();
        let (_, key) = keys
            .iter()
            .find(|(name, _)| *name == MODULE)
            .expect("the prompt's middleware should be registered");
        assert!(key.contains(&tcf_stub()), "{key}");
        assert!(key.contains(&gpp_stub(CMP_ID)), "{key}");
    }

    #[test]
    fn ampersands_and_quotes_in_the_url_are_escaped() {
        let tag = tag_in(
            MiddlewarePhase::Fetch,
            Choice::new(
                "https://cmp.inmobi.com/choice/abc/site/choice.js?a=1&b=\"2\"",
                CMP_ID,
            ),
        );

        assert!(tag.contains("a=1&amp;b=&quot;2&quot;"), "{tag}");
    }

    /// The entry's order is the head's order, which is why the prompt must be
    /// named before any vendor's tag.
    #[test]
    fn a_vendor_tag_named_after_the_prompt_is_written_after_it() {
        let prompt_first = head_markup(
            MiddlewarePhase::Fetch,
            vec![Arc::new(Choice::new(REAL, CMP_ID)), Arc::new(VendorTag)],
        )
        .join("");
        let vendor_first = head_markup(
            MiddlewarePhase::Fetch,
            vec![Arc::new(VendorTag), Arc::new(Choice::new(REAL, CMP_ID))],
        )
        .join("");

        let position = |page: &str, needle: &str| {
            page.find(needle)
                .unwrap_or_else(|| panic!("{needle} should be written: {page}"))
        };
        assert!(
            position(&prompt_first, "cmp.inmobi.com") < position(&prompt_first, "vendor.example"),
            "named first, the prompt should load first: {prompt_first}"
        );
        assert!(
            position(&vendor_first, "vendor.example") < position(&vendor_first, "cmp.inmobi.com"),
            "named second, the prompt loads after the vendor's tag, so the order \
             in the entry is what keeps it first: {vendor_first}"
        );
    }

    /// The page a reader receives with this module running, kept as a file
    /// so that changing how the page change is made can be shown to leave
    /// the page as it was.
    #[test]
    fn the_page_a_reader_receives_is_the_recorded_one() {
        assert_page_is_recorded(
            include_str!("fixtures/page-change.settings.toml"),
            &[super::builder()],
            include_str!("fixtures/page-change.input.html"),
            include_str!("fixtures/page-change.recorded.html"),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/fixtures/page-change.recorded.html"
            ),
        );
    }
}
