//! Google's tag, Tag Manager container and AdSense loader, put on the page
//! from the publisher's own configuration.
//!
//! This is not the Google Tag Manager integration next door. That one takes a
//! tag the page already carries and re-serves it from the publisher's own
//! origin. This one puts the tag on a page that carries none, which is what a
//! publisher needs when their templates forbid a third-party script and every
//! tag has to arrive from the proxy instead.
//!
//! The two work together. When `google-tag-manager` is also selected and it
//! serves the configured tag or container, the loader written here names the
//! first-party path rather than Google's host, so the script the browser
//! fetches is the one the publisher's own origin answers for.
//!
//! Every reader gets the consent mode defaults. The loaders and Tag Manager's
//! noscript iframe reach only a reader whose permissions allow storage on the
//! device, because each of them stores and reads Google's identifiers there.
//!
//! Everything emitted here is copied from Google's own documentation rather
//! than paraphrased, so a reader can hold the two side by side:
//!
//! - The tag snippet and where it goes:
//!   <https://developers.google.com/tag-platform/gtagjs/install>
//! - Consent mode defaults and why they come first:
//!   <https://developers.google.com/tag-platform/security/guides/consent>
//! - Installing a Tag Manager web container, which is the two-part snippet:
//!   <https://support.google.com/tagmanager/answer/14847097>
//! - The AdSense code on every page:
//!   <https://support.google.com/adsense/answer/9274634>
//! - The AdSense loader tag:
//!   <https://support.google.com/adsense/answer/9190028>

use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};

use error_stack::Report;
use regex::Regex;
use serde::{Deserialize, Serialize};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{
    AttributeRewriteAction, IntegrationBuilder, IntegrationRegistration,
};
use trusted_server_core::middleware::{
    AttributeRewrite, AttributeRewriteFn, ElementHandler, HTML_MEDIA_TYPE, Middleware,
    MiddlewareAction, MiddlewareContext, MiddlewarePhase, Preparation,
};
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permissions::{Permission, PermissionSet, PermissionState};
use trusted_server_core::settings::{IntegrationConfig, Settings};
use trusted_server_tag_google_tag_manager as google_tag_manager;
use validator::{Validate, ValidationError};

/// The id this integration's registration carries.
const GOOGLE_TAG_INTEGRATION_ID: &str = "google_tag";

/// The name this module is selected by, in `[tag]`.
pub const MODULE: &str = "tag.google-tag";

/// The page change the module supplies, as an entry names it.
const SUPPLIED_MIDDLEWARE: &[trusted_server_core::integrations::SuppliedMiddleware] = &[
    trusted_server_core::integrations::SuppliedMiddleware::on_html(
        MODULE,
        trusted_server_core::middleware::MiddlewarePhase::Serve,
    ),
];

/// The name a module's builder gives as its source, this crate's package.
const SOURCE: &str = "trusted-server-tag-google-tag";

/// The builder the registry runs when a section selects [`MODULE`].
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(GOOGLE_TAG_INTEGRATION_ID, SOURCE, register, validate)
        .with_module_name(MODULE)
        .with_middleware(SUPPLIED_MIDDLEWARE)
}

/// Google's own address for the tag loader.
///
/// From the install page, which gives the loader as
/// `https://www.googletagmanager.com/gtag/js?id=TAG_ID`.
const GTAG_JS_URL: &str = "https://www.googletagmanager.com/gtag/js";

/// Google's own address for the AdSense loader.
///
/// From the ad unit code page, which gives the loader as
/// `https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js?client=…`.
const ADSENSE_JS_URL: &str = "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js";

/// Google's own address for the Tag Manager container script.
///
/// From the container install page, which builds it in the snippet as
/// `'https://www.googletagmanager.com/gtm.js?id='+i+dl`.
const GTM_JS_URL: &str = "https://www.googletagmanager.com/gtm.js";

/// Google's own address for the Tag Manager `noscript` iframe.
///
/// This one has no first-party route behind it, so it is never substituted.
const GTM_NS_URL: &str = "https://www.googletagmanager.com/ns.html";

/// The path Google serves the tag loader from, and the one a first-party
/// route ends with.
const GTAG_JS_PATH_SUFFIX: &str = "/gtag/js";

/// An AdSense publisher id, `ca-pub-` and sixteen digits.
static ADSENSE_CLIENT_ID_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ca-pub-[0-9]{16}$").expect("the AdSense client id regex should compile")
});

/// Whether a consent type is allowed before the visitor has said anything.
///
/// The names are Google's, from the consent mode guide, and they reach the
/// page as the words `granted` and `denied`.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConsentState {
    /// The default, and the only one allowed before a permission signal has
    /// been read.
    #[default]
    Denied,
    /// Allowed before the visitor has said anything, which only a publisher
    /// who knows their own legal basis may configure.
    Granted,
}

impl ConsentState {
    /// The word Google's consent command expects.
    const fn as_command_value(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::Granted => "granted",
        }
    }
}

/// How long the tag waits for a consent management platform's update before
/// it fires with the defaults, when no setting says otherwise.
const DEFAULT_WAIT_FOR_UPDATE_MS: u32 = 500;

const fn default_wait_for_update() -> u32 {
    DEFAULT_WAIT_FOR_UPDATE_MS
}

/// The four consent types Google requires for its own services, how long the
/// tag waits for a consent management platform before it fires with them,
/// and the regions they apply to.
///
/// All four default to denied. That is Google's own recommended starting
/// point, and no Google identifier may be stored before a permission signal
/// has been read.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = validate_region_codes))]
pub struct ConsentDefaults {
    /// Storage for advertising, such as cookies.
    #[serde(default)]
    pub ad_storage: ConsentState,
    /// Sending user data to Google for advertising.
    #[serde(default)]
    pub ad_user_data: ConsentState,
    /// Personalised advertising.
    #[serde(default)]
    pub ad_personalization: ConsentState,
    /// Storage for analytics, such as cookies.
    #[serde(default)]
    pub analytics_storage: ConsentState,
    /// How long, in milliseconds, the tag waits for a consent management
    /// platform's update before it fires with these defaults, which Google's
    /// consent guide asks for where a platform sets consent on the page. Zero
    /// fires at once.
    #[serde(default = "default_wait_for_update")]
    #[validate(range(max = 60000, message = "must be at most 60000 milliseconds"))]
    pub wait_for_update: u32,
    /// The regions the defaults apply to, as ISO 3166-1 country codes or
    /// ISO 3166-2 region codes such as `GB` or `US-CA`. Empty applies them
    /// everywhere. Where set, every other visitor gets all four denied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub region: Vec<String>,
}

impl Default for ConsentDefaults {
    fn default() -> Self {
        Self {
            ad_storage: ConsentState::default(),
            ad_user_data: ConsentState::default(),
            ad_personalization: ConsentState::default(),
            analytics_storage: ConsentState::default(),
            wait_for_update: DEFAULT_WAIT_FOR_UPDATE_MS,
            region: Vec::new(),
        }
    }
}

/// Refuses a region that is not a country or region code, two capitals with
/// an optional dash and one to three capitals or digits, so the snippet only
/// ever carries codes.
///
/// # Errors
///
/// Returns a [`ValidationError`] naming the first region that is not a code.
fn validate_region_codes(defaults: &ConsentDefaults) -> Result<(), ValidationError> {
    static CODE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[A-Z]{2}(-[A-Z0-9]{1,3})?$").expect("the region code pattern compiles")
    });
    if let Some(region) = defaults.region.iter().find(|code| !CODE.is_match(code)) {
        let mut error = ValidationError::new("google_region_code");
        error.message = Some(
            format!("region `{region}` must be a country or region code such as GB or US-CA")
                .into(),
        );
        return Err(error);
    }
    Ok(())
}

/// Configuration for the Google tag, the Tag Manager container and the
/// AdSense loader.
#[derive(Debug, Clone, Default, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = validate_at_least_one_id))]
pub struct GoogleConfig {
    /// A Google tag id, such as a GA4 measurement id `G-XXXXXXXXXX`.
    ///
    /// Leaving this out means no tag snippet is written, which is what a
    /// publisher running AdSense on its own wants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = validate_analytics_tag_id))]
    pub analytics_tag_id: Option<String>,
    /// A Tag Manager container id, `GTM-` and four to twenty characters.
    ///
    /// A container is a different thing from a tag. A tag id names one
    /// property to measure and is placed with the `gtag.js` snippet, whereas a
    /// container holds whatever tags its owner has configured inside it and is
    /// placed with Tag Manager's own two-part installation code. A publisher
    /// may have both, and setting both writes both.
    ///
    /// Leaving this out means no container is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = validate_container_id))]
    pub container_id: Option<String>,
    /// An AdSense publisher id, `ca-pub-` followed by sixteen digits.
    ///
    /// Leaving this out means no AdSense loader is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = validate_adsense_client_id))]
    pub adsense_client_id: Option<String>,
    /// What each consent type is before the visitor has said anything.
    #[serde(default)]
    #[validate(nested)]
    pub consent_defaults: ConsentDefaults,
}

impl IntegrationConfig for GoogleConfig {}

/// Refuses a configuration naming no tag, no container and no AdSense account.
///
/// Such a configuration selects the integration and then asks it to write
/// nothing, which reads on the page as the integration being broken rather
/// than as the configuration being empty.
///
/// # Errors
///
/// Returns a [`ValidationError`] when all three ids are absent.
fn validate_at_least_one_id(config: &GoogleConfig) -> Result<(), ValidationError> {
    if config.analytics_tag_id.is_none()
        && config.container_id.is_none()
        && config.adsense_client_id.is_none()
    {
        let mut error = ValidationError::new("google_no_ids");
        error.message = Some(
            "[tag.google-tag] needs at least one of analytics_tag_id, container_id and \
             adsense_client_id"
                .into(),
        );
        return Err(error);
    }
    Ok(())
}

/// Refuses a tag id Google does not serve from `gtag/js`.
///
/// The shape is the one the Google Tag Manager integration already accepts.
///
/// # Errors
///
/// Returns a [`ValidationError`] when the id is not a Google tag id.
fn validate_analytics_tag_id(value: &str) -> Result<(), ValidationError> {
    if google_tag_manager::tag_id_is_well_formed(value) {
        return Ok(());
    }
    let mut error = ValidationError::new("google_tag_id");
    error.message =
        Some("analytics_tag_id must be a G-, GT-, GTM-, AW-, DC-, MC- or UA- tag id".into());
    Err(error)
}

/// Refuses a container id Google Tag Manager would not recognise.
///
/// The shape is the one the Google Tag Manager integration already accepts.
///
/// # Errors
///
/// Returns a [`ValidationError`] when the id is not a container id.
fn validate_container_id(value: &str) -> Result<(), ValidationError> {
    if google_tag_manager::container_id_is_well_formed(value) {
        return Ok(());
    }
    let mut error = ValidationError::new("google_container_id");
    error.message = Some("container_id must be GTM- followed by 4 to 20 capitals or digits".into());
    Err(error)
}

/// Refuses an AdSense publisher id that is not `ca-pub-` and sixteen digits.
///
/// # Errors
///
/// Returns a [`ValidationError`] when the id does not match.
fn validate_adsense_client_id(value: &str) -> Result<(), ValidationError> {
    if ADSENSE_CLIENT_ID_PATTERN.is_match(value) {
        return Ok(());
    }
    let mut error = ValidationError::new("google_adsense_client_id");
    error.message = Some("adsense_client_id must be ca-pub- followed by sixteen digits".into());
    Err(error)
}

/// The consent defaults command.
///
/// Copied from <https://developers.google.com/tag-platform/security/guides/consent>.
/// It carries its own `dataLayer` and `gtag` lines because it has to run
/// before anything else, including before the tag snippet that would
/// otherwise define them, and because it is written even when no tag is
/// configured and AdSense alone reads it.
///
/// Where regions are set, the command for them is followed by one with no
/// region, which the guide says sets the default for every visitor no
/// regional command covers. That one denies all four types, because a grant
/// in the settings holds only in the regions they list.
fn consent_defaults_snippet(defaults: &ConsentDefaults) -> String {
    let commands = if defaults.region.is_empty() {
        consent_default_command(defaults, "")
    } else {
        // The regions are validated as codes, so the quotes cannot be escaped.
        let codes: Vec<String> = defaults
            .region
            .iter()
            .map(|code| format!("'{code}'"))
            .collect();
        let region = format!(",\n    'region': [{}]", codes.join(", "));
        let elsewhere = ConsentDefaults {
            wait_for_update: defaults.wait_for_update,
            ..ConsentDefaults::default()
        };
        let regional = consent_default_command(defaults, &region);
        let rest = consent_default_command(&elsewhere, "");
        format!("{regional}\n\n  {rest}")
    };
    format!(
        "<script>\n  \
         window.dataLayer = window.dataLayer || [];\n  \
         function gtag(){{dataLayer.push(arguments);}}\n\n  \
         {commands}\n\
         </script>"
    )
}

/// One `gtag('consent', 'default', …)` command carrying `defaults`, with
/// `region` after the wait, which is empty or the command's `region`
/// parameter.
fn consent_default_command(defaults: &ConsentDefaults, region: &str) -> String {
    format!(
        "gtag('consent', 'default', {{\n    \
         'ad_storage': '{ad_storage}',\n    \
         'ad_user_data': '{ad_user_data}',\n    \
         'ad_personalization': '{ad_personalization}',\n    \
         'analytics_storage': '{analytics_storage}',\n    \
         'wait_for_update': {wait_for_update}{region}\n  \
         }});",
        ad_storage = defaults.ad_storage.as_command_value(),
        ad_user_data = defaults.ad_user_data.as_command_value(),
        ad_personalization = defaults.ad_personalization.as_command_value(),
        analytics_storage = defaults.analytics_storage.as_command_value(),
        wait_for_update = defaults.wait_for_update,
    )
}

/// Google's tag snippet.
///
/// Copied from <https://developers.google.com/tag-platform/gtagjs/install>,
/// which says to place it "immediately after the opening `<head>` HTML tag on
/// every page you want to measure". The middleware writes it in the head
/// immediately before the script bundle's tag, so after whatever the fetch
/// phase wrote there. The only substitutions are the tag id and, where the
/// Google Tag Manager integration serves this tag, the loader address.
fn analytics_snippet(tag_id: &str, src: &str) -> String {
    format!(
        "<!-- Google tag (gtag.js) -->\n\
         <script async src=\"{src}?id={tag_id}\"></script>\n\
         <script>\n  \
         window.dataLayer = window.dataLayer || [];\n  \
         function gtag(){{dataLayer.push(arguments);}}\n  \
         gtag('js', new Date());\n\n  \
         gtag('config', '{tag_id}');\n\
         </script>"
    )
}

/// Tag Manager's container script, the first of its two parts.
///
/// Copied from <https://support.google.com/tagmanager/answer/14847097>, which
/// says to paste it "as high in the `<head>` tag as possible". The only
/// substitutions are the container id and, where this origin serves the same
/// container, the address the snippet builds.
fn container_script_snippet(container_id: &str, gtm_js_src: &str) -> String {
    format!(
        "<!-- Google Tag Manager -->\n\
         <script>(function(w,d,s,l,i){{w[l]=w[l]||[];w[l].push({{'gtm.start':\n\
         new Date().getTime(),event:'gtm.js'}});var f=d.getElementsByTagName(s)[0],\n\
         j=d.createElement(s),dl=l!='dataLayer'?'&l='+l:'';j.async=true;j.src=\n\
         '{gtm_js_src}?id='+i+dl;f.parentNode.insertBefore(j,f);\n\
         }})(window,document,'script','dataLayer','{container_id}');</script>\n\
         <!-- End Google Tag Manager -->"
    )
}

/// Tag Manager's `noscript` iframe, the second of its two parts.
///
/// Copied from the same page, which says to paste it "immediately after the
/// opening `<body>` tag". That is why the middleware writes it at the start of
/// `<body>`, because Google places this markup there and nowhere else.
///
/// The address is never substituted, because `ns.html` has no first-party
/// route behind it.
fn container_noscript_snippet(container_id: &str) -> String {
    format!(
        "<!-- Google Tag Manager (noscript) -->\n\
         <noscript><iframe src=\"{GTM_NS_URL}?id={container_id}\"\n\
         height=\"0\" width=\"0\" style=\"display:none;visibility:hidden\"></iframe></noscript>\n\
         <!-- End Google Tag Manager (noscript) -->"
    )
}

/// The AdSense page-level loader.
///
/// Copied from <https://support.google.com/adsense/answer/9190028>. The
/// per-unit `<ins>` markup is not written here, because where a unit goes is
/// a property of the publisher's own page rather than of this configuration.
fn adsense_snippet(client_id: &str) -> String {
    format!(
        "<script async src=\"{ADSENSE_JS_URL}?client={client_id}\" \
         crossorigin=\"anonymous\"></script>"
    )
}

/// Whether `value` is a `gtag/js` loader naming `tag_id`.
///
/// Matching is on the parsed path and the `id` parameter rather than on the
/// whole string, so a loader carrying other parameters, a protocol-relative
/// address or a first-party path is recognised as the same thing.
fn url_is_gtag_js_for(value: &str, tag_id: &str) -> bool {
    let trimmed = value.trim();
    // A protocol-relative or rooted address carries no scheme or host of its
    // own, so it is given one purely to be parsed. Nothing reads it afterwards.
    let absolute = if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.starts_with('/') {
        format!("https://placeholder.invalid{trimmed}")
    } else {
        trimmed.to_owned()
    };
    let Ok(parsed) = url::Url::parse(&absolute) else {
        return false;
    };

    if !parsed.path().ends_with(GTAG_JS_PATH_SUFFIX) {
        return false;
    }

    parsed
        .query_pairs()
        .any(|(name, value)| name == "id" && value == tag_id)
}

/// The integration, holding what it will write.
struct GoogleIntegration {
    config: GoogleConfig,
    /// Where the tag loader points.
    ///
    /// Google's own address, or the first-party path when the Google Tag
    /// Manager module is selected and serves this tag. Settled once at
    /// startup because it depends on configuration rather than on the
    /// request, and because the middleware runs on every document.
    gtag_js_src: Option<String>,
    /// Where the container script's own loader points, decided the same way.
    gtm_js_src: Option<String>,
}

impl GoogleIntegration {
    /// The integration for `config`, with both loader addresses settled.
    const fn new(
        config: GoogleConfig,
        gtag_js_src: Option<String>,
        gtm_js_src: Option<String>,
    ) -> Self {
        Self {
            config,
            gtag_js_src,
            gtm_js_src,
        }
    }
}

impl GoogleIntegration {
    /// The snippets for one reader, in the order Google's own documentation
    /// requires.
    ///
    /// Consent defaults first, because the consent guide says the default
    /// command must come "before any commands that send measurement data
    /// (such as `config` or `event`)". They load nothing, so every reader gets
    /// them. The container next, because Tag Manager asks to be as high in the
    /// head as possible and it may carry tags of its own that send data. Then
    /// the tag snippet, then the AdSense loader. Those three are loaders, so
    /// they are written only where `loaders_allowed`.
    fn page_inserts(&self, loaders_allowed: bool) -> Vec<String> {
        let mut inserts = vec![consent_defaults_snippet(&self.config.consent_defaults)];
        if !loaders_allowed {
            return inserts;
        }

        if let (Some(container_id), Some(src)) = (
            self.config.container_id.as_deref(),
            self.gtm_js_src.as_deref(),
        ) {
            inserts.push(container_script_snippet(container_id, src));
        }

        if let (Some(tag_id), Some(src)) = (
            self.config.analytics_tag_id.as_deref(),
            self.gtag_js_src.as_deref(),
        ) {
            inserts.push(analytics_snippet(tag_id, src));
        }

        if let Some(client_id) = self.config.adsense_client_id.as_deref() {
            inserts.push(adsense_snippet(client_id));
        }

        inserts
    }
}

impl GoogleIntegration {
    /// Tag Manager's `noscript` iframe, and nothing else, where
    /// `loaders_allowed`, because the iframe loads Google's page.
    ///
    /// Google's container install page puts this part immediately after the
    /// opening `<body>` tag, so it is the one piece of this integration's
    /// markup that does not belong in the head.
    fn body_inserts(&self, loaders_allowed: bool) -> Vec<String> {
        if !loaders_allowed {
            return Vec::new();
        }
        self.config
            .container_id
            .as_deref()
            .map(container_noscript_snippet)
            .into_iter()
            .collect()
    }
}

impl GoogleIntegration {
    /// Drops the page's own loader for the configured tag, for every reader.
    ///
    /// Where this module writes its loader, a page also carrying the loader
    /// for the same id would fetch the same library twice, and where the
    /// reader's permissions keep this module's loader off the page, the
    /// page's own would load it anyway, so the element is removed rather than
    /// rewritten. The loader is recognised on its own host and on a
    /// first-party path, which is how it arrives here once the fetch phase's
    /// rewrite has made it first party.
    ///
    /// **An inline `gtag('config', …)` in the publisher's own page is left
    /// alone and will still send a second page view.** The module's guide
    /// says the same.
    fn change_for_src(&self, value: &str) -> AttributeRewriteAction {
        let Some(tag_id) = self.config.analytics_tag_id.as_deref() else {
            return AttributeRewriteAction::Keep;
        };

        if url_is_gtag_js_for(value, tag_id) {
            // The id is the publisher's own configured value, so it is not
            // logged. The message says what happened and not which tag.
            log::debug!(
                "Google: the page carries its own loader for the configured tag, \
                 so it is dropped"
            );
            return AttributeRewriteAction::RemoveElement;
        }

        AttributeRewriteAction::Keep
    }
}

/// What Google's loaders and Tag Manager's noscript iframe need from a reader,
/// being storage on the device, because each of them stores and reads
/// Google's identifiers there.
const LOADERS_REQUIRE: PermissionSet = PermissionSet::none().with(Permission::StoreOnDevice);

/// Whether one reader's permissions allow the loaders, as the middleware's
/// preparation found them.
struct LoadersAllowed(bool);

/// Writes the consent defaults onto every reader's copy, and the loaders and
/// the noscript iframe onto the copy of a reader whose permissions allow
/// storage on the device. Removes the page's own loader for the configured
/// tag for every reader.
///
/// Serve only, because it adds to the page, and the removal belongs with the
/// addition it makes room for.
struct PageTag(Arc<GoogleIntegration>);

impl PageTag {
    /// Whether `permissions` set everything the loaders need.
    fn loaders_allowed(&self, permissions: &PermissionState) -> bool {
        permissions.all_set(self.required_permissions())
    }
}

impl Middleware for PageTag {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == HTML_MEDIA_TYPE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    fn required_permissions(&self) -> PermissionSet {
        LOADERS_REQUIRE
    }

    /// Reads the reader's permissions, which core hands every serve
    /// preparation. A preparation handed none treats the reader as one whose
    /// permissions do not allow the loaders.
    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        _context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        let allowed = matches!(call.inject(self, Self::loaders_allowed), Ok(true));
        Some(Box::pin(async move {
            Ok(Box::new(LoadersAllowed(allowed)) as Box<dyn Any + Send>)
        }))
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        // A document nothing was prepared for gets no loader.
        let loaders_allowed = context
            .take_prepared::<LoadersAllowed>(MODULE)
            .is_some_and(|prepared| prepared.0);
        let mut element_handlers: Vec<Box<dyn ElementHandler>> = Vec::new();
        if self.0.config.analytics_tag_id.is_some() {
            let integration = Arc::clone(&self.0);
            let change: Rc<AttributeRewriteFn> =
                Rc::new(move |matched| integration.change_for_src(matched.value));
            element_handlers.push(Box::new(AttributeRewrite::new("src", change)));
        }
        MiddlewareAction {
            head_inserts: self.0.page_inserts(loaders_allowed),
            body_open_inserts: self.0.body_inserts(loaders_allowed),
            element_handlers,
            ..MiddlewareAction::pass()
        }
    }
}

/// Validates the Google configuration and reports whether a section selects
/// the module.
///
/// # Errors
///
/// Returns an error when the configuration cannot be parsed or fails
/// validation.
pub fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(settings.module_config::<GoogleConfig>(MODULE)?.is_some())
}

/// Register the Google tag integration when a section selects it.
///
/// # Errors
///
/// Returns an error when the Google configuration, or the Google Tag Manager
/// configuration consulted for first-party delivery, cannot be parsed or
/// fails validation.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<GoogleConfig>(MODULE)? else {
        return Ok(None);
    };

    // Where each loader points is decided once, here, because it depends on
    // another module's configuration rather than on the request.
    let gtag_js_src = match config.analytics_tag_id.as_deref() {
        Some(tag_id) => Some(
            google_tag_manager::first_party_gtag_js_path(settings, tag_id)?
                .unwrap_or_else(|| GTAG_JS_URL.to_owned()),
        ),
        None => None,
    };

    let gtm_js_src = match config.container_id.as_deref() {
        Some(container_id) => Some(
            google_tag_manager::first_party_gtm_js_path(settings, container_id)?
                .unwrap_or_else(|| GTM_JS_URL.to_owned()),
        ),
        None => None,
    };

    let integration = Arc::new(GoogleIntegration::new(config, gtag_js_src, gtm_js_src));

    Ok(Some(
        IntegrationRegistration::builder(GOOGLE_TAG_INTEGRATION_ID)
            .with_middleware(Arc::new(PageTag(integration)))
            .build(),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use trusted_server_core::html_processor::HtmlProcessorConfig;
    use trusted_server_core::html_processor::test_support::{
        ORIGIN_HOST, REQUEST_HOST, REQUEST_PATH, create_page_processor, place_on_every_page,
    };
    use trusted_server_core::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use trusted_server_core::middleware::test_support as middleware_test_support;
    use trusted_server_core::middleware::{MiddlewareChain, MiddlewarePlan, Prepared};
    use trusted_server_core::module_context::ModuleContext;
    use trusted_server_core::module_context::test_support::request;
    use trusted_server_core::permissions::{ConsentSignal, PermissionMaps};
    use trusted_server_core::streaming_processor::StreamProcessor as _;
    use trusted_server_core::test_support::tests::{
        create_test_settings, storage_only_permissions,
    };

    /// An invented tag id of the shape Google's own pages use.
    const TEST_TAG_ID: &str = "G-XXXXXXXXXX";
    /// An invented AdSense account of the shape Google's own pages use.
    const TEST_ADSENSE_CLIENT_ID: &str = "ca-pub-0000000000000000";
    /// An invented container id.
    const TEST_CONTAINER_ID: &str = "GTM-EXAMPLE1";

    /// Google's snippet as the install page gives it.
    const GOOGLE_PUBLISHED_TAG_SNIPPET: &str = "<!-- Google tag (gtag.js) -->\n\
         <script async src=\"https://www.googletagmanager.com/gtag/js?id=TAG_ID\"></script>\n\
         <script>\n  \
         window.dataLayer = window.dataLayer || [];\n  \
         function gtag(){dataLayer.push(arguments);}\n  \
         gtag('js', new Date());\n\n  \
         gtag('config', 'TAG_ID');\n\
         </script>";

    /// Google's AdSense loader exactly as
    /// <https://support.google.com/adsense/answer/9190028> gives it.
    const GOOGLE_PUBLISHED_ADSENSE_LOADER: &str = concat!(
        "<script async src=\"https://pagead2.googlesyndication.com/pagead/js/",
        "adsbygoogle.js?client=CLIENT_ID\" crossorigin=\"anonymous\"></script>"
    );
    /// Tag Manager's container script exactly as
    /// <https://support.google.com/tagmanager/answer/14847097> gives it.
    const GOOGLE_PUBLISHED_CONTAINER_SCRIPT: &str = "<!-- Google Tag Manager -->\n\
         <script>(function(w,d,s,l,i){w[l]=w[l]||[];w[l].push({'gtm.start':\n\
         new Date().getTime(),event:'gtm.js'});var f=d.getElementsByTagName(s)[0],\n\
         j=d.createElement(s),dl=l!='dataLayer'?'&l='+l:'';j.async=true;j.src=\n\
         'https://www.googletagmanager.com/gtm.js?id='+i+dl;f.parentNode.insertBefore(j,f);\n\
         })(window,document,'script','dataLayer','CONTAINER_ID');</script>\n\
         <!-- End Google Tag Manager -->";

    /// Tag Manager's `noscript` part, from the same page.
    const GOOGLE_PUBLISHED_CONTAINER_NOSCRIPT: &str = "<!-- Google Tag Manager (noscript) -->\n\
         <noscript><iframe src=\"https://www.googletagmanager.com/ns.html?id=CONTAINER_ID\"\n\
         height=\"0\" width=\"0\" style=\"display:none;visibility:hidden\"></iframe></noscript>\n\
         <!-- End Google Tag Manager (noscript) -->";

    /// An invented publisher's head carrying its own tag manager, so the
    /// insertion is checked against a head shaped the way a content
    /// management system writes one.
    const HEAD_WITH_TAG_MANAGER: &str = include_str!("fixtures/head-with-tag-manager.html");
    /// An invented publisher's head that already loads the configured tag.
    const HEAD_WITH_ITS_OWN_TAG: &str = include_str!("fixtures/head-with-its-own-tag.html");

    /// Settings selecting the `google-tag` module with `config`.
    fn settings_with_google(config: &serde_json::Value) -> Settings {
        let mut settings = create_test_settings();
        settings
            .insert_module_config("tag", MODULE, config)
            .expect("should write the google integration config");
        settings
    }

    /// Settings selecting both `google-tag` and `google-tag-manager`.
    fn settings_with_google_and_tag_manager(
        google: &serde_json::Value,
        tag_manager: &serde_json::Value,
    ) -> Settings {
        let mut settings = settings_with_google(google);
        settings
            .insert_module_config("tag", google_tag_manager::MODULE, tag_manager)
            .expect("should write the google_tag_manager config");
        settings
    }

    /// The registration a named google integration produces for `settings`.
    fn registration_for(settings: &Settings) -> IntegrationRegistration {
        register(settings)
            .expect("should read the google config")
            .expect("a named google integration should register")
    }

    /// The refusal `register` answers `settings` with.
    ///
    /// `IntegrationRegistration` holds trait objects and has no `Debug`, so
    /// the error is taken with a pattern rather than with `expect_err`.
    fn refusal_for(settings: &Settings) -> String {
        let Err(error) = register(settings) else {
            panic!("should refuse the configuration");
        };
        format!("{error:?}")
    }

    /// The middleware a named module registers for `settings`.
    fn page_tag_for(settings: &Settings) -> Arc<dyn Middleware> {
        Arc::clone(
            registration_for(settings)
                .middleware
                .first()
                .expect("the module should register its middleware"),
        )
    }

    /// What the middleware plans for one reader's copy, after the serve
    /// phase's preparation has run with `module_context`, as core prepares
    /// and plans every reader's copy.
    fn plan_with(settings: &Settings, module_context: &ModuleContext<'_>) -> MiddlewarePlan {
        let chain = MiddlewareChain::new(
            MiddlewarePhase::Serve,
            HTML_MEDIA_TYPE,
            vec![page_tag_for(settings)],
        );
        let document_state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = MiddlewareContext {
            prepared: &prepared,
            ..middleware_test_support::context(MiddlewarePhase::Serve, &document_state)
        };
        futures::executor::block_on(chain.prepare(module_context, &context))
            .expect("should prepare the middleware");
        chain.plan(&context).expect("should plan the middleware")
    }

    /// What the middleware plans for a reader whose request resolved to
    /// `permissions`.
    fn plan_for(settings: &Settings, permissions: &PermissionState) -> MiddlewarePlan {
        plan_with(
            settings,
            &ModuleContext::new(request(REQUEST_PATH)).with_permissions(permissions),
        )
    }

    /// The head snippets the middleware writes for `settings`, for a reader
    /// who allows storage on the device.
    fn head_inserts_for(settings: &Settings) -> Vec<String> {
        plan_for(settings, &storage_only_permissions()).head_inserts
    }

    /// `html` after the serve phase has run the middleware for `settings`,
    /// for a reader who allows storage on the device.
    fn process(settings: &Settings, html: &str) -> String {
        process_for(settings, html, &[], &storage_only_permissions())
    }

    /// The page a reader whose request resolved to `permissions` receives of
    /// `html`, with this module's middleware named in a `[[serve]]` entry and
    /// the middleware in `fetch` named in a `[[fetch]]` entry. Both phases are
    /// prepared and then run on one pass, as core does for a page it does not
    /// store.
    fn process_for(
        settings: &Settings,
        html: &str,
        fetch: &[&str],
        permissions: &PermissionState,
    ) -> String {
        let mut placed = settings.clone();
        if !fetch.is_empty() {
            place_on_every_page(&mut placed, MiddlewarePhase::Fetch, fetch);
        }
        place_on_every_page(&mut placed, MiddlewarePhase::Serve, &[MODULE]);
        let registry = IntegrationRegistry::with_registrations(
            &placed,
            &[builder(), google_tag_manager::builder()],
        )
        .expect("should build a registry with the google modules");

        let prepared = Arc::new(Prepared::default());
        let module_context = ModuleContext::new(request(REQUEST_PATH))
            .with_settings(&placed)
            .with_permissions(permissions);
        for phase in MiddlewarePhase::ALL {
            let chain = registry.middleware_chain(
                placed.phase_entries(phase),
                phase,
                HTML_MEDIA_TYPE,
                REQUEST_PATH,
            );
            let document_state = IntegrationDocumentState::default();
            let context = MiddlewareContext {
                prepared: &prepared,
                ..middleware_test_support::context(phase, &document_state)
            };
            futures::executor::block_on(chain.prepare(&module_context, &context))
                .expect("should prepare the page's middleware");
        }

        let config = HtmlProcessorConfig::from_settings(
            &placed,
            &registry,
            ORIGIN_HOST,
            REQUEST_HOST,
            "https",
        )
        .with_prepared(prepared);
        let page = create_page_processor(&placed, &registry, config)
            .process_chunk(html.as_bytes(), true)
            .expect("should process the page");
        String::from_utf8(page).expect("should leave the page UTF-8")
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            super::MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }

    #[test]
    fn an_unnamed_integration_does_not_register() {
        // Arrange
        let settings = create_test_settings();

        // Act
        let registration = register(&settings).expect("should read an unnamed integration");

        // Assert
        assert!(
            registration.is_none(),
            "a google integration nothing names should not register"
        );
    }

    #[test]
    fn a_config_naming_no_id_is_refused() {
        // Arrange
        let settings = settings_with_google(&json!({}));

        // Act
        let refusal = refusal_for(&settings);

        // Assert
        assert!(
            refusal.contains("analytics_tag_id")
                && refusal.contains("container_id")
                && refusal.contains("adsense_client_id"),
            "the refusal should name all three settings, one of which is needed: {refusal}"
        );
    }

    #[test]
    fn an_id_of_the_wrong_shape_is_refused_by_name() {
        // Arrange
        // A tag id with a prefix Google does not serve, a container id in the
        // wrong case, and an AdSense id missing its `ca-` prefix.
        let cases = [
            ("analytics_tag_id", "GA-XXXXXXXXXX"),
            ("container_id", "GTM-lowercase"),
            ("adsense_client_id", "pub-0000000000000000"),
        ];

        for (setting, value) in cases {
            let settings = settings_with_google(&json!({ setting: value }));

            // Act
            let refusal = refusal_for(&settings);

            // Assert
            assert!(
                refusal.contains(setting),
                "the refusal should name `{setting}` as the setting at fault: {refusal}"
            );
        }
    }

    #[test]
    fn an_unknown_setting_is_refused() {
        // Arrange
        let settings = settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "analytics_tag": "G-OTHER",
        }));

        // Act
        let refusal = refusal_for(&settings);

        // Assert
        assert!(
            refusal.contains("analytics_tag") || refusal.contains("unknown field"),
            "the refusal should name the unknown setting: {refusal}"
        );
    }

    /// The value `trusted-server.example.toml` gives `setting` in its
    /// `[tag.google-tag]` table.
    fn example_value(setting: &str) -> String {
        let prefix = format!("# {setting} = \"");
        trusted_server_core::test_support::template::EXAMPLE_TEMPLATE
            .lines()
            .skip_while(|line| *line != "# [tag.google-tag]")
            .take_while(|line| line.starts_with('#'))
            .find_map(|line| line.strip_prefix(prefix.as_str())?.strip_suffix('"'))
            .unwrap_or_else(|| panic!("the example should give `{setting}` a value"))
            .to_owned()
    }

    #[test]
    fn the_example_s_placeholder_ids_are_refused() {
        // Arrange, Act, Assert
        // A table uncommented with its placeholders left in should be refused,
        // rather than write a tag or a container that is not the publisher's.
        for setting in ["analytics_tag_id", "container_id"] {
            let placeholder = example_value(setting);
            let refusal = refusal_for(&settings_with_google(&json!({ setting: placeholder })));
            assert!(
                refusal.contains(setting),
                "the example's `{placeholder}` should be refused for `{setting}`: {refusal}"
            );
        }
    }

    #[test]
    fn the_tag_snippet_is_googles_own_with_the_id_substituted() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));
        let expected = GOOGLE_PUBLISHED_TAG_SNIPPET.replace("TAG_ID", TEST_TAG_ID);

        // Act
        let inserts = head_inserts_for(&settings);

        // Assert
        let written = inserts
            .iter()
            .find(|insert| insert.starts_with("<!-- Google tag"))
            .expect("should write the tag snippet");
        assert_eq!(
            written, &expected,
            "the snippet should be Google's own, changed only where the id goes"
        );
    }

    #[test]
    fn consent_defaults_come_before_the_config_command() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        let consent = written
            .find("gtag('consent', 'default'")
            .expect("should write the consent defaults");
        let config = written
            .find("gtag('config'")
            .expect("should write the config command");
        assert!(
            consent < config,
            "Google's consent guide requires the default before any command that sends \
             measurement data"
        );
    }

    #[test]
    fn every_consent_type_defaults_to_denied() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        for consent_type in [
            "ad_storage",
            "ad_user_data",
            "ad_personalization",
            "analytics_storage",
        ] {
            assert!(
                written.contains(&format!("'{consent_type}': 'denied'")),
                "{consent_type} should be denied until a signal has been read"
            );
        }
    }

    #[test]
    fn the_tag_waits_for_the_platform_s_update_before_it_fires() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains("'wait_for_update': 500"),
            "the defaults should tell the tag to wait for a platform's update: {written}"
        );
        assert!(
            !written.contains("'region'"),
            "defaults scoped to no region should name none: {written}"
        );
    }

    #[test]
    fn a_publisher_may_scope_the_defaults_to_regions_and_set_the_wait() {
        // Arrange
        let settings = settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "consent_defaults": { "wait_for_update": 2000, "region": ["GB", "US-CA"] },
        }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(written.contains("'wait_for_update': 2000"), "{written}");
        assert!(
            written.contains("'region': ['GB', 'US-CA']"),
            "the defaults should name the regions as Google's command does: {written}"
        );
    }

    #[test]
    fn a_region_setting_leaves_every_other_visitor_with_all_four_denied() {
        // Arrange
        // A grant scoped to one region, which reaches visitors there and no
        // one else.
        let settings = settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "consent_defaults": {
                "analytics_storage": "granted",
                "wait_for_update": 2000,
                "region": ["US"],
            },
        }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        // Google's guide gives the regional command first and then the one
        // with no region, which covers every visitor the first does not.
        let commands: Vec<&str> = written
            .split("gtag('consent', 'default', {")
            .skip(1)
            .collect();
        assert_eq!(
            commands.len(),
            2,
            "should write two default commands: {written}"
        );
        assert!(
            commands[0].contains("'region': ['US']")
                && commands[0].contains("'analytics_storage': 'granted'"),
            "the regional command should carry the grant: {written}"
        );
        assert!(
            !commands[1].contains("'region'"),
            "the second command should name no region: {written}"
        );
        for consent_type in [
            "ad_storage",
            "ad_user_data",
            "ad_personalization",
            "analytics_storage",
        ] {
            assert!(
                commands[1].contains(&format!("'{consent_type}': 'denied'")),
                "every other visitor should have {consent_type} denied: {written}"
            );
        }
        assert!(
            commands[1].contains("'wait_for_update': 2000"),
            "every other visitor should wait as long: {written}"
        );
    }

    #[test]
    fn a_region_that_is_not_a_code_is_refused_by_name() {
        // Arrange
        let settings = settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "consent_defaults": { "region": ["GB", "england"] },
        }));

        // Act
        let refusal = refusal_for(&settings);

        // Assert
        assert!(
            refusal.contains("england"),
            "the refusal should name the region that is not a code: {refusal}"
        );
    }

    #[test]
    fn a_publisher_may_grant_a_consent_type_up_front() {
        // Arrange
        let settings = settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "consent_defaults": { "analytics_storage": "granted" },
        }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains("'analytics_storage': 'granted'"),
            "a configured grant should reach the page"
        );
        assert!(
            written.contains("'ad_storage': 'denied'"),
            "the types left alone should stay denied"
        );
    }

    #[test]
    fn the_adsense_loader_is_googles_own_with_the_account_substituted() {
        // Arrange
        let settings =
            settings_with_google(&json!({ "adsense_client_id": TEST_ADSENSE_CLIENT_ID }));

        // Act
        let inserts = head_inserts_for(&settings);

        // Assert
        let written = inserts
            .iter()
            .find(|insert| insert.contains("adsbygoogle.js"))
            .expect("should write the AdSense loader");
        assert_eq!(
            written,
            &GOOGLE_PUBLISHED_ADSENSE_LOADER.replace("CLIENT_ID", TEST_ADSENSE_CLIENT_ID),
            "the loader should be the one the AdSense ad unit page gives"
        );
    }

    #[test]
    fn each_id_on_its_own_writes_the_consent_defaults_and_its_own_snippet() {
        // Arrange
        // Each case names one id, the address only its snippet carries, and
        // the addresses the other two snippets would have carried.
        let cases = [
            (
                json!({ "analytics_tag_id": TEST_TAG_ID }),
                "gtag/js",
                ["gtm.js", "adsbygoogle"],
            ),
            (
                json!({ "container_id": TEST_CONTAINER_ID }),
                "gtm.js",
                ["gtag/js", "adsbygoogle"],
            ),
            (
                json!({ "adsense_client_id": TEST_ADSENSE_CLIENT_ID }),
                "adsbygoogle",
                ["gtag/js", "gtm.js"],
            ),
        ];

        for (config, written, not_written) in cases {
            let settings = settings_with_google(&config);

            // Act
            let inserts = head_inserts_for(&settings);

            // Assert
            assert_eq!(
                inserts.len(),
                2,
                "the consent defaults and one snippet should be written for {config}"
            );
            let head = inserts.join("");
            assert!(
                head.contains(written),
                "the snippet for {config} should be written"
            );
            for other in not_written {
                assert!(
                    !head.contains(other),
                    "nothing naming `{other}` should be written for {config}"
                );
            }
        }
    }

    #[test]
    fn the_loader_names_googles_host_when_tag_manager_is_not_enabled() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains(&format!(
                "src=\"https://www.googletagmanager.com/gtag/js?id={TEST_TAG_ID}\""
            )),
            "with nothing serving the tag first party, the loader is Google's own address"
        );
    }

    #[test]
    fn the_loader_names_the_first_party_path_when_tag_manager_serves_the_tag() {
        // Arrange
        let settings = settings_with_google_and_tag_manager(
            &json!({ "analytics_tag_id": TEST_TAG_ID }),
            &json!({
                "container_id": "GTM-ABCD1234",
                "allowed_tag_ids": [TEST_TAG_ID],
            }),
        );

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains(&format!(
                "src=\"/integrations/google_tag_manager/gtag/js?id={TEST_TAG_ID}\""
            )),
            "the tag this origin serves should be fetched from this origin: {written}"
        );
    }

    #[test]
    fn the_loader_names_googles_host_when_tag_manager_would_not_serve_the_tag() {
        // Arrange
        // The tag is not the container and is not on the allowed list, so a
        // request for it is redirected upstream rather than proxied.
        let settings = settings_with_google_and_tag_manager(
            &json!({ "analytics_tag_id": TEST_TAG_ID }),
            &json!({ "container_id": "GTM-ABCD1234" }),
        );

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains("src=\"https://www.googletagmanager.com/gtag/js?"),
            "naming a first-party path for a tag that is not served would only add a redirect"
        );
    }

    #[test]
    fn a_gtag_loader_is_recognised_however_it_is_written() {
        // Arrange, Act, Assert
        for value in [
            format!("https://www.googletagmanager.com/gtag/js?id={TEST_TAG_ID}"),
            format!("//www.googletagmanager.com/gtag/js?id={TEST_TAG_ID}"),
            format!("/integrations/google_tag_manager/gtag/js?id={TEST_TAG_ID}"),
            format!("https://www.googletagmanager.com/gtag/js?l=dataLayer&id={TEST_TAG_ID}"),
        ] {
            assert!(
                url_is_gtag_js_for(&value, TEST_TAG_ID),
                "should recognise `{value}` as a loader for the configured tag"
            );
        }

        for value in [
            "https://www.googletagmanager.com/gtag/js?id=G-SOMETHINGELSE",
            "https://www.googletagmanager.com/gtm.js?id=GTM-EXAMPLE1",
            "https://www.example.com/app.js",
            "not a url at all",
        ] {
            assert!(
                !url_is_gtag_js_for(value, TEST_TAG_ID),
                "should leave `{value}` alone"
            );
        }
    }

    #[test]
    fn the_snippets_land_in_the_head_once() {
        // Arrange
        let settings = settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "adsense_client_id": TEST_ADSENSE_CLIENT_ID,
        }));
        let html = "<html><head><title>Example Publisher</title></head><body></body></html>";

        // Act
        let processed = process(&settings, html);

        // Assert
        assert_eq!(
            processed.matches("gtag('consent', 'default'").count(),
            1,
            "the consent defaults should be written once"
        );
        assert_eq!(
            processed.matches("gtag/js?id=").count(),
            1,
            "the tag loader should be written once"
        );
        assert_eq!(
            processed.matches("adsbygoogle.js?client=").count(),
            1,
            "the AdSense loader should be written once"
        );

        let consent = processed
            .find("gtag('consent', 'default'")
            .expect("should write the consent defaults");
        let title = processed
            .find("<title>")
            .expect("should keep the page's own head content");
        assert!(
            consent < title,
            "the snippets should come before the page's own head content"
        );
    }

    #[test]
    fn the_snippet_reaches_a_real_shaped_head_before_the_publishers_own_scripts() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));

        // Act
        let processed = process(&settings, HEAD_WITH_TAG_MANAGER);

        // Assert
        let ours = processed
            .find("gtag('consent', 'default'")
            .expect("should write the consent defaults");
        let theirs = processed
            .find("<!-- Google Tag Manager -->")
            .expect("should keep the publisher's own tag manager");
        assert!(
            ours < theirs,
            "the snippet should come before the publisher's own scripts"
        );
        assert_eq!(
            processed.matches("gtag('config'").count(),
            1,
            "the tag should be configured once"
        );
    }

    #[test]
    fn a_page_already_loading_the_same_tag_ends_with_one_loader() {
        // Arrange
        // A page carrying its own loader for the tag this integration writes
        // would otherwise fetch the same library twice.
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));

        // Act
        let processed = process(&settings, HEAD_WITH_ITS_OWN_TAG);

        // Assert
        assert_eq!(
            processed.matches("gtag/js?id=").count(),
            1,
            "the page should end with one loader for the tag, not two: {processed}"
        );
    }

    #[test]
    fn a_loader_the_fetch_rewrite_made_first_party_is_removed_as_well() {
        // Arrange
        // By the time the middleware runs, a fetch phase rewrite may already have
        // pointed the page's own loader at the first-party path.
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));
        let html = format!(
            "<html><head><script async src=\"/integrations/google_tag_manager/gtag/js?id=\
             {TEST_TAG_ID}\"></script></head><body></body></html>"
        );

        // Act
        let processed = process(&settings, &html);

        // Assert
        assert_eq!(
            processed.matches("gtag/js?id=").count(),
            1,
            "the page should end with one loader for the tag, not two: {processed}"
        );
    }

    #[test]
    fn a_page_through_both_modules_ends_with_one_first_party_loader() {
        // Arrange
        // The Tag Manager module serves the configured tag, so its fetch
        // middleware points the page's own loader at the first-party path, and
        // this module's serve middleware then removes that loader and writes
        // its own, which names the same path.
        let settings = settings_with_google_and_tag_manager(
            &json!({ "analytics_tag_id": TEST_TAG_ID }),
            &json!({
                "container_id": "GTM-ABCD1234",
                "allowed_tag_ids": [TEST_TAG_ID],
            }),
        );

        // Act
        let processed = process_for(
            &settings,
            HEAD_WITH_ITS_OWN_TAG,
            &[google_tag_manager::MODULE],
            &storage_only_permissions(),
        );

        // Assert
        assert_eq!(
            processed.matches("gtag/js?id=").count(),
            1,
            "the page should end with one loader for the tag: {processed}"
        );
        assert!(
            processed.contains(&format!(
                "src=\"/integrations/google_tag_manager/gtag/js?id={TEST_TAG_ID}\""
            )),
            "the loader should name the first-party path: {processed}"
        );
        assert!(
            !processed.contains("www.googletagmanager.com/gtag/js"),
            "nothing should load the tag from Google's host: {processed}"
        );
    }

    #[test]
    fn a_page_loading_another_tag_keeps_it() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));
        let html = "<html><head><title>Example Publisher</title>\
             <script async src=\"https://www.googletagmanager.com/gtag/js?id=G-OTHERTAG\">\
             </script></head><body></body></html>";

        // Act
        let processed = process(&settings, html);

        // Assert
        assert!(
            processed.contains("id=G-OTHERTAG"),
            "a second property the publisher measures is theirs to keep"
        );
        assert!(
            processed.contains(&format!("id={TEST_TAG_ID}")),
            "the configured tag should still be written"
        );
    }

    #[test]
    fn the_container_script_is_googles_own_with_the_id_substituted() {
        // Arrange
        let settings = settings_with_google(&json!({ "container_id": TEST_CONTAINER_ID }));
        let expected = GOOGLE_PUBLISHED_CONTAINER_SCRIPT.replace("CONTAINER_ID", TEST_CONTAINER_ID);

        // Act
        let inserts = head_inserts_for(&settings);

        // Assert
        let written = inserts
            .iter()
            .find(|insert| insert.starts_with("<!-- Google Tag Manager -->"))
            .expect("should write the container script");
        assert_eq!(
            written, &expected,
            "the container script should be Google's own, changed only where the id goes"
        );
    }

    #[test]
    fn the_container_noscript_is_googles_own_with_the_id_substituted() {
        // Arrange
        let settings = settings_with_google(&json!({ "container_id": TEST_CONTAINER_ID }));
        let expected =
            GOOGLE_PUBLISHED_CONTAINER_NOSCRIPT.replace("CONTAINER_ID", TEST_CONTAINER_ID);

        // Act
        let processed = process(&settings, "<html><head></head><body></body></html>");

        // Assert
        assert!(
            processed.contains(&format!("<body>{expected}")),
            "the noscript part should be Google's own, changed only where the id goes, and \
             written immediately after the opening body tag: {processed}"
        );
    }

    #[test]
    fn consent_defaults_come_before_the_container() {
        // Arrange
        let settings = settings_with_google(&json!({ "container_id": TEST_CONTAINER_ID }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        let consent = written
            .find("gtag('consent', 'default'")
            .expect("should write the consent defaults");
        let container = written
            .find("<!-- Google Tag Manager -->")
            .expect("should write the container script");
        assert!(
            consent < container,
            "a container may carry tags that send measurement data, so the defaults come first"
        );
    }

    #[test]
    fn a_container_and_a_tag_are_both_written_container_first() {
        // Arrange
        let settings = settings_with_google(&json!({
            "container_id": TEST_CONTAINER_ID,
            "analytics_tag_id": TEST_TAG_ID,
        }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        let container = written
            .find("<!-- Google Tag Manager -->")
            .expect("should write the container script");
        let tag = written
            .find("<!-- Google tag (gtag.js) -->")
            .expect("should write the tag snippet");
        assert!(
            container < tag,
            "Tag Manager asks to be as high in the head as possible"
        );
    }

    #[test]
    fn the_container_loader_names_googles_host_when_tag_manager_is_not_enabled() {
        // Arrange
        let settings = settings_with_google(&json!({ "container_id": TEST_CONTAINER_ID }));

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains("'https://www.googletagmanager.com/gtm.js?id='+i+dl"),
            "with nothing serving the container first party, the snippet is Google's own"
        );
    }

    #[test]
    fn the_container_loader_names_the_first_party_path_for_the_same_container() {
        // Arrange
        let settings = settings_with_google_and_tag_manager(
            &json!({ "container_id": TEST_CONTAINER_ID }),
            &json!({ "container_id": TEST_CONTAINER_ID }),
        );

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains("'/integrations/google_tag_manager/gtm.js?id='+i+dl"),
            "the container this origin serves should be fetched from this origin: {written}"
        );
    }

    #[test]
    fn the_container_loader_names_googles_host_for_a_different_container() {
        // Arrange
        // That endpoint always answers with the container it is configured
        // for, so a first-party path would serve the wrong one.
        let settings = settings_with_google_and_tag_manager(
            &json!({ "container_id": TEST_CONTAINER_ID }),
            &json!({ "container_id": "GTM-OTHER123" }),
        );

        // Act
        let written = head_inserts_for(&settings).join("");

        // Assert
        assert!(
            written.contains("'https://www.googletagmanager.com/gtm.js?id='+i+dl"),
            "a container this origin does not serve stays with Google"
        );
    }

    #[test]
    fn the_noscript_lands_immediately_after_the_opening_body_tag_once() {
        // Arrange
        let settings = settings_with_google(&json!({ "container_id": TEST_CONTAINER_ID }));
        let html = "<html><head><title>Example Publisher</title></head>\
             <body><h1>Headline</h1></body></html>";

        // Act
        let processed = process(&settings, html);

        // Assert
        assert_eq!(
            processed.matches("ns.html?id=").count(),
            1,
            "the noscript iframe should be written once"
        );
        let body_open = processed.find("<body>").expect("should keep the body tag");
        let noscript = processed
            .find("<!-- Google Tag Manager (noscript) -->")
            .expect("should write the noscript part");
        let headline = processed
            .find("<h1>")
            .expect("should keep the page content");
        assert!(
            body_open < noscript && noscript < headline,
            "Google's install page puts this part immediately after the opening body tag: \
             {processed}"
        );
    }

    #[test]
    fn a_second_body_does_not_get_a_second_noscript() {
        // Arrange
        // Template fragments sometimes carry more than one body element.
        let settings = settings_with_google(&json!({ "container_id": TEST_CONTAINER_ID }));
        let html = "<html><head><title>Example Publisher</title></head>\
             <body><h1>Headline</h1></body><body>Second</body></html>";

        // Act
        let processed = process(&settings, html);

        // Assert
        assert_eq!(
            processed.matches("ns.html?id=").count(),
            1,
            "the noscript iframe should be written once per document"
        );
    }

    #[test]
    fn a_page_with_no_container_configured_is_left_alone_at_the_body() {
        // Arrange
        let settings = settings_with_google(&json!({ "analytics_tag_id": TEST_TAG_ID }));
        let html = "<html><head><title>Example Publisher</title></head>\
             <body><h1>Headline</h1></body></html>";

        // Act
        let processed = process(&settings, html);

        // Assert
        assert!(
            !processed.contains("noscript"),
            "nothing should be written at the body when no container is configured"
        );
    }

    #[test]
    fn a_page_with_no_head_gets_the_noscript_and_none_of_the_head_snippets() {
        // Arrange
        // Core writes the script bundle's tag at the start of the head, and
        // the head snippets go straight before that tag, so a document with no
        // head has nowhere to take them.
        let settings = settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "container_id": TEST_CONTAINER_ID,
        }));
        let html = "<html><body><h1>Headline</h1></body></html>";

        // Act
        let processed = process(&settings, html);

        // Assert
        for snippet in ["gtag('consent'", "gtag/js", "gtm.js"] {
            assert!(
                !processed.contains(snippet),
                "nothing naming `{snippet}` should be written without a head: {processed}"
            );
        }
        assert_eq!(
            processed.matches("ns.html?id=").count(),
            1,
            "the noscript iframe should still be written once: {processed}"
        );
        assert!(
            processed.contains("<body><!-- Google Tag Manager (noscript) -->"),
            "the noscript iframe should follow the opening body tag: {processed}"
        );
    }

    /// Settings naming all three ids, so every loader would be written.
    fn settings_with_every_id() -> Settings {
        settings_with_google(&json!({
            "analytics_tag_id": TEST_TAG_ID,
            "container_id": TEST_CONTAINER_ID,
            "adsense_client_id": TEST_ADSENSE_CLIENT_ID,
        }))
    }

    #[test]
    fn a_reader_who_has_not_allowed_storage_gets_the_consent_defaults_alone() {
        // Arrange
        // Every permission but storage on the device, which each loader and
        // the noscript iframe need.
        let settings = settings_with_every_id();
        let all_but_storage: PermissionSet = Permission::all()
            .filter(|permission| *permission != Permission::StoreOnDevice)
            .collect();

        // Act
        let plan = plan_for(&settings, &PermissionState::new(all_but_storage));

        // Assert
        assert_eq!(
            plan.head_inserts,
            vec![consent_defaults_snippet(&ConsentDefaults::default())],
            "the consent defaults should be the only head snippet"
        );
        assert!(
            plan.body_open_inserts.is_empty(),
            "no noscript iframe should be written: {:?}",
            plan.body_open_inserts
        );
    }

    #[test]
    fn a_reader_who_allows_storage_gets_every_configured_loader() {
        // Arrange
        let settings = settings_with_every_id();

        // Act
        let plan = plan_for(&settings, &storage_only_permissions());

        // Assert
        let head = plan.head_inserts.join("");
        for loader in ["gtm.js", "gtag/js", "adsbygoogle.js"] {
            assert!(
                head.contains(loader),
                "the head should name `{loader}`: {head}"
            );
        }
        assert_eq!(
            plan.body_open_inserts,
            vec![container_noscript_snippet(TEST_CONTAINER_ID)],
            "the noscript iframe should be written"
        );
    }

    #[test]
    fn no_loader_is_written_where_no_permission_state_reached_the_middleware() {
        // Arrange
        // A preparation whose context carries no permission state, and a
        // document nothing was prepared for.
        let settings = settings_with_every_id();
        let document_state = IntegrationDocumentState::default();

        // Act
        let unresolved = plan_with(&settings, &ModuleContext::new(request(REQUEST_PATH)));
        let context = middleware_test_support::context(MiddlewarePhase::Serve, &document_state);
        let unprepared = page_tag_for(&settings).create(&context);

        // Assert
        for (case, head, body) in [
            (
                "no permission state",
                unresolved.head_inserts,
                unresolved.body_open_inserts,
            ),
            (
                "nothing prepared",
                unprepared.head_inserts,
                unprepared.body_open_inserts,
            ),
        ] {
            assert_eq!(
                head,
                vec![consent_defaults_snippet(&ConsentDefaults::default())],
                "{case}: the consent defaults should be the only head snippet"
            );
            assert!(body.is_empty(), "{case}: no noscript iframe: {body:?}");
        }
    }

    #[test]
    fn a_page_for_a_reader_who_has_not_agreed_carries_no_google_loader() {
        // Arrange
        // Under the repository's sample policy, a reader in Germany whose
        // request carries no signal is still waiting to allow storage on the
        // device, and a reader in California who sends an opt-out, such as
        // Global Privacy Control, has it revoked. The page carries its own
        // loader for the configured tag as well.
        let settings = settings_with_every_id();
        let policy = PermissionMaps::sample();
        let readers = [
            ("not yet agreed", policy.baseline(Some("DE"), None)),
            (
                "opted out",
                policy.resolve_with(Some("US"), Some("CA"), |permission, _| {
                    if policy.signals().opt_out_revokes(permission) {
                        ConsentSignal::Revoke
                    } else {
                        ConsentSignal::Neutral
                    }
                }),
            ),
        ];

        for (reader, permissions) in readers {
            // Act
            let processed = process_for(&settings, HEAD_WITH_ITS_OWN_TAG, &[], &permissions);

            // Assert
            assert_eq!(
                processed.matches("gtag('consent', 'default'").count(),
                1,
                "{reader}: the consent defaults should be written: {processed}"
            );
            for loader in ["gtag/js", "gtm.js", "adsbygoogle.js", "ns.html"] {
                assert!(
                    !processed.contains(loader),
                    "{reader}: nothing naming `{loader}` should reach the page: {processed}"
                );
            }
        }
    }

    /// The builder describes the middleware the registration supplies, so a
    /// tool that writes a configuration places what the module registers.
    #[test]
    fn the_builder_describes_the_middleware_the_module_registers() {
        let settings = trusted_server_core::html_processor::test_support::page_settings(
            include_str!("fixtures/page-change.settings.toml"),
        );
        trusted_server_core::test_support::middleware::assert_described_middleware_is_registered(
            &settings,
            &[super::builder()],
            super::builder().id(),
        );
    }
}
