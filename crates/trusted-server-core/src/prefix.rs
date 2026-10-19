//! The start of every name the browser holds for a publisher, and of every key
//! the publisher's entries take in a shared store.
//!
//! A deployment selects the module that gives both with `[prefix] module`. Core
//! holds the extension point, [`PrefixModule`], and its own module,
//! [`TrustedServerPrefix`], which gives `ts` and `ts.<domain>` and runs when
//! nothing is selected.
//!
//! The two prefixes answer different questions. A public prefix starts the
//! names the browser holds, being cookies and first-party storage keys, so it
//! carries no domain, because a browser already keeps each site's apart from
//! every other site's. A server key prefix starts every key in a store
//! publishers share, so it carries the publisher's domain, see
//! [`ServerKeyPrefix`].
//!
//! The template cache is the exception. Its surrogate keys, `ts-template` and
//! `ts-template-url-<digest>`, and its store key,
//! `ts-template-cache-v<n>-<digest>`, keep `ts` under every module, so
//! `ts cache purge` names one key whatever the deployment. The store key's
//! digest covers the request's host and the public prefix, so no two
//! publishers share an entry.
//!
//! A module gives the parts, and each use writes its own form from them,
//! because what is allowed differs by use:
//!
//! | Use | Form | `ts` | `_ex`, `www.example.com` |
//! | --- | --- | --- | --- |
//! | cookie | [`PublicPrefix::cookie_name`] | `ts-ec` | `_ex.www.example.com-ec` |
//! | store key | [`ServerKeyPrefix::as_str`] | `ts.example.com` | as its module gives it |
//!
//! The page's client script is told the public prefix on its bundle tag rather
//! than holding a copy, see [`PUBLIC_PREFIX_ATTRIBUTE`]. Its own globals, such
//! as `window.tsjs`, are what page code calls it by and keep their names under
//! every module.

use core::fmt;
use std::sync::Arc;

use crate::module_context::{ModuleCall, ModuleContext, ModuleRequest, Withheld};
use crate::permissions::PermissionSet;
use crate::platform::ServerKeyPrefix;
use crate::settings::Settings;

/// The type folder, and the section, that selects the prefix module.
pub const MODULE_TYPE: &str = "prefix";

/// The name core's own module is called by when it runs.
pub const TRUSTED_SERVER_MODULE: &str = "ts";

/// The head of every name core's own module gives.
const TRUSTED_SERVER_HEAD: &str = "ts";

/// The bundle tag attribute that tells the client script the public prefix,
/// so the script writes the deployment's names and holds no copy of its own.
pub const PUBLIC_PREFIX_ATTRIBUTE: &str = "data-ts-public-prefix";

/// The start of every name the browser holds for a publisher, such as `_ex`,
/// and the `Path` of the cookies it names.
///
/// Made of parts, such as `_ex` and `www.example.com`, joined by `.`. Every
/// part holds only ASCII letters, digits, `_`, `-` and `.`, so the prefix is a
/// valid cookie name and attribute value. The first part starts with a letter
/// or a single `_`, so no cookie name starts with `__`, where browsers give
/// names such as `__Host-` rules of their own and drop a cookie that breaks
/// them.
///
/// The path is `/` unless [`Self::for_request`] or [`Self::under_path`]
/// built the prefix for a request addressed by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicPrefix {
    dotted: String,
    cookie_path: Option<String>,
}

impl PublicPrefix {
    /// The prefix made of `parts`, in order, or `None` when there are none,
    /// when a part is empty or holds anything else, or when the first part
    /// starts with anything but a letter or a single `_`.
    #[must_use]
    pub fn new<I, S>(parts: I) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let parts: Vec<String> = parts
            .into_iter()
            .map(|part| part.as_ref().to_owned())
            .collect();
        let well_formed = parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        });
        let first_is_a_name = parts.first().is_some_and(|first| {
            first.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && !first.starts_with("__")
        });
        if !well_formed || !first_is_a_name {
            return None;
        }
        Some(Self {
            dotted: parts.join("."),
            cookie_path: None,
        })
    }

    /// The parts joined by `.`, such as `_ex.www.example.com`, which every
    /// cookie name and storage key starts with.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.dotted
    }

    /// The name of the cookie `name`, written `<parts joined by .>-<name>`,
    /// such as `ts-ec` or `_ex-ec`.
    #[must_use]
    pub fn cookie_name(&self, name: &str) -> String {
        format!("{}-{name}", self.dotted)
    }

    /// The same prefix, its cookies written under `path`.
    ///
    /// `None`, an empty path or `/` writes them under `/`. So does a path
    /// that cannot be a cookie's `Path`, logged, so a value written into a
    /// `Set-Cookie` header can never add an attribute.
    #[must_use]
    pub fn under_path(self, path: Option<&str>) -> Self {
        let cookie_path = path.and_then(crate::addressing::cookie_path_for);
        if cookie_path.is_none() && path.is_some_and(|path| !matches!(path, "" | "/")) {
            log::warn!("Writing cookies under / as the removed prefix is not a cookie path");
        }
        Self {
            cookie_path: cookie_path.map(str::to_owned),
            ..self
        }
    }

    /// The prefix for `request`, its cookies written under the prefix
    /// request addressing removed, or under `/` when none was removed or
    /// [`Self::under_path`] keeps `/` for it.
    ///
    /// A browser sends a cookie only on requests under its `Path`, so a
    /// cookie written under one publisher's path is not sent on another
    /// publisher's requests, unless that publisher's path lies under it. A
    /// cookie written under `/` is sent on them all.
    #[must_use]
    pub fn for_request<B>(&self, request: &http::Request<B>) -> Self {
        self.clone().under_path(
            request
                .extensions()
                .get::<crate::addressing::RemovedPathPrefix>()
                .map(crate::addressing::RemovedPathPrefix::as_str),
        )
    }

    /// The `Path` attribute of every cookie written under this prefix.
    #[must_use]
    pub fn cookie_path(&self) -> &str {
        self.cookie_path.as_deref().unwrap_or("/")
    }

    /// Whether `cookie` is in the namespace this prefix starts, which is every
    /// name [`Self::cookie_name`] gives, compared without regard to case.
    #[must_use]
    pub fn owns_cookie_name(&self, cookie: &[u8]) -> bool {
        let length = self.dotted.len();
        cookie
            .get(..length)
            .is_some_and(|head| head.eq_ignore_ascii_case(self.dotted.as_bytes()))
            && cookie.get(length) == Some(&b'-')
    }
}

/// Core's own, `ts`, for a value no request resolved, such as a default
/// Edge Cookie state.
impl Default for PublicPrefix {
    fn default() -> Self {
        TrustedServerPrefix::public()
    }
}

impl fmt::Display for PublicPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.dotted)
    }
}

/// Gives the start of the names the browser holds for a publisher and of the
/// keys its entries take in shared stores.
///
/// Selected with `[prefix] module`, and core's [`TrustedServerPrefix`] runs
/// when nothing is selected. Each method is handed a [`ModuleCall`] carrying
/// the request and the deployment's settings, and
/// nothing else, and an implementation hands its own function to
/// [`ModuleCall::inject`] naming what it needs.
///
/// The domain a server key prefix carries comes from the publisher the request
/// resolved to, being the loaded settings' `publisher.domain`, and never from
/// the raw request.
pub trait PrefixModule: Send + Sync + fmt::Debug {
    /// The start of every cookie name and first-party storage key for the
    /// publisher this call was resolved to.
    ///
    /// # Errors
    ///
    /// [`Withheld`] when the module names a value the call does not carry.
    fn public_prefix(&self, call: ModuleCall<'_>) -> Result<PublicPrefix, Withheld>;

    /// The start of every key the publisher's entries take in a shared store.
    ///
    /// # Errors
    ///
    /// [`Withheld`] when the module names a value the call does not carry.
    fn server_key_prefix(&self, call: ModuleCall<'_>) -> Result<ServerKeyPrefix, Withheld>;
}

/// Core's own prefix module, which gives `ts`, the prefix of the cookie names
/// Trusted Server writes, and `ts.<publisher.domain>`. It runs when `[prefix]`
/// selects nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct TrustedServerPrefix;

impl TrustedServerPrefix {
    /// `ts`.
    #[must_use]
    pub fn public() -> PublicPrefix {
        PublicPrefix {
            dotted: TRUSTED_SERVER_HEAD.to_owned(),
            cookie_path: None,
        }
    }

    fn server_key(&self, settings: &Settings) -> ServerKeyPrefix {
        ServerKeyPrefix::new(TRUSTED_SERVER_HEAD, &settings.publisher.domain)
    }
}

impl PrefixModule for TrustedServerPrefix {
    fn public_prefix(&self, _call: ModuleCall<'_>) -> Result<PublicPrefix, Withheld> {
        Ok(Self::public())
    }

    fn server_key_prefix(&self, call: ModuleCall<'_>) -> Result<ServerKeyPrefix, Withheld> {
        call.inject(self, Self::server_key)
    }
}

/// Both prefixes for one request, as its services carry them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefixes {
    public: PublicPrefix,
    server_key: ServerKeyPrefix,
}

impl Prefixes {
    /// The two prefixes.
    #[must_use]
    pub fn new(public: PublicPrefix, server_key: ServerKeyPrefix) -> Self {
        Self { public, server_key }
    }

    /// What `module`, called `name`, gives for the publisher `request` was
    /// resolved to under `settings`.
    ///
    /// # Errors
    ///
    /// [`Withheld`] when the module names a value other than the request and
    /// the settings.
    pub fn resolve(
        module: &dyn PrefixModule,
        name: &'static str,
        settings: &Settings,
        request: ModuleRequest<'_>,
    ) -> Result<Self, Withheld> {
        let context = ModuleContext::new(request).with_settings(settings);
        let call = context.call(name, PermissionSet::none());
        Ok(Self {
            public: module.public_prefix(call)?,
            server_key: module.server_key_prefix(call)?,
        })
    }

    /// What core's own module gives for the publisher `settings` describe.
    #[must_use]
    pub fn trusted_server(settings: &Settings) -> Self {
        Self::new(
            TrustedServerPrefix::public(),
            TrustedServerPrefix.server_key(settings),
        )
    }

    /// The start of every name the browser holds.
    #[must_use]
    pub fn public(&self) -> &PublicPrefix {
        &self.public
    }

    /// The start of every key in a shared store.
    #[must_use]
    pub fn server_key(&self) -> &ServerKeyPrefix {
        &self.server_key
    }
}

/// The prefixes of services built for no request, such as a test's or the
/// operator command line's, being core's own names with no publisher domain.
impl Default for Prefixes {
    fn default() -> Self {
        Self::new(
            TrustedServerPrefix::public(),
            ServerKeyPrefix::new(TRUSTED_SERVER_HEAD, ""),
        )
    }
}

/// The selected prefix module and the name it runs under.
#[derive(Debug, Clone)]
pub struct SelectedPrefixModule {
    name: &'static str,
    module: Arc<dyn PrefixModule>,
}

impl SelectedPrefixModule {
    /// `module`, which runs under `name`.
    #[must_use]
    pub fn new(name: &'static str, module: Arc<dyn PrefixModule>) -> Self {
        Self { name, module }
    }

    /// The name the module runs under.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// What the module gives for the publisher `request` was resolved to.
    ///
    /// # Errors
    ///
    /// [`Withheld`] when the module names a value other than the request and
    /// the settings.
    pub fn prefixes(
        &self,
        settings: &Settings,
        request: ModuleRequest<'_>,
    ) -> Result<Prefixes, Withheld> {
        Prefixes::resolve(&*self.module, self.name, settings, request)
    }
}

/// Core's own module, which runs when nothing is selected.
impl Default for SelectedPrefixModule {
    fn default() -> Self {
        Self::new(TRUSTED_SERVER_MODULE, Arc::new(TrustedServerPrefix))
    }
}

/// Prefixes and checks for a module's own tests of its store keys.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
    use super::{Prefixes, ServerKeyPrefix, TrustedServerPrefix};

    /// The prefixes core's own module gives a publisher on `domain`.
    #[must_use]
    pub fn trusted_server_for(domain: &str) -> Prefixes {
        Prefixes::new(
            TrustedServerPrefix::public(),
            ServerKeyPrefix::new(super::TRUSTED_SERVER_HEAD, domain),
        )
    }

    /// Checks that the keys `key_for` makes for a publisher's domain follow
    /// the publisher: two publishers never share one, `www.example.com` and
    /// `example.com` do, and `clients.example.com` and `public.example.com`
    /// do not.
    ///
    /// # Panics
    ///
    /// When any of the three does not hold, naming it.
    pub fn assert_keys_follow_the_publisher(key_for: impl Fn(&str) -> String) {
        assert_ne!(
            key_for("first.example"),
            key_for("second.example"),
            "two publishers should never share a key"
        );
        assert_eq!(
            key_for("www.example.com"),
            key_for("example.com"),
            "a publisher's www. and bare names should share a key"
        );
        assert_ne!(
            key_for("clients.example.com"),
            key_for("public.example.com"),
            "two subdomains other than www. should not share a key"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_context::test_support::request;

    #[test]
    fn cookies_are_under_the_root_with_no_removed_prefix() {
        assert_eq!(PublicPrefix::default().cookie_path(), "/");
        for path in ["", "/"] {
            assert_eq!(
                PublicPrefix::default().under_path(Some(path)),
                PublicPrefix::default(),
                "`{path}` should count as no removed prefix"
            );
        }
    }

    #[test]
    fn cookies_are_under_the_prefix_the_request_had_removed() {
        let mut addressed = http::Request::new(());
        addressed
            .extensions_mut()
            .insert(crate::addressing::RemovedPathPrefix::new(
                "/publisher-a.example".to_owned(),
            ));
        let public = PublicPrefix::default();

        assert_eq!(
            public.for_request(&addressed).cookie_path(),
            "/publisher-a.example"
        );
        assert_eq!(
            public.for_request(&http::Request::new(())).cookie_path(),
            "/"
        );
    }

    #[test]
    fn a_prefix_that_is_not_a_cookie_path_keeps_the_root() {
        for path in ["/a; Domain=example.com", "a", "/a b", "/\u{e9}"] {
            assert_eq!(
                PublicPrefix::default().under_path(Some(path)).cookie_path(),
                "/",
                "{path}"
            );
        }
    }

    fn settings_for(domain: &str) -> Settings {
        let mut settings = Settings::default();
        settings.publisher.domain = domain.to_owned();
        settings
    }

    #[test]
    fn no_module_selected_gives_exactly_ts() {
        let prefixes = SelectedPrefixModule::default()
            .prefixes(&settings_for("www.example.com"), request("/"))
            .expect("core's module should name only what the call carries");

        assert_eq!(prefixes.public().as_str(), "ts");
        assert_eq!(prefixes.server_key().as_str(), "ts.example.com");
        for (name, cookie) in [("ec", "ts-ec"), ("ecr", "ts-ecr"), ("tester", "ts-tester")] {
            assert_eq!(prefixes.public().cookie_name(name), cookie);
        }
    }

    #[test]
    fn each_use_writes_its_own_form_from_the_parts() {
        let prefix =
            PublicPrefix::new(["_ex", "www.example.com"]).expect("should accept the parts");

        assert_eq!(prefix.as_str(), "_ex.www.example.com");
        assert_eq!(prefix.cookie_name("ec"), "_ex.www.example.com-ec");
    }

    #[test]
    fn a_part_that_could_leave_a_name_is_refused() {
        for parts in [
            vec![],
            vec![""],
            vec!["9ex"],
            vec!["-ex"],
            vec![".ex"],
            vec!["__ex"],
            vec!["__Host"],
            vec!["_ex", ""],
            vec!["_ex", "a b"],
            vec!["_ex", "a;b"],
            vec!["_ex", "a=b"],
            vec!["_ex", "a\"b"],
            vec!["_ex", "a/b"],
        ] {
            assert!(PublicPrefix::new(parts.clone()).is_none(), "{parts:?}");
        }
    }

    #[test]
    fn a_prefix_owns_the_cookie_names_it_gives_and_no_others() {
        let prefix = TrustedServerPrefix::public();

        for owned in ["ts-ec", "TS-ECR", "ts-anything", "ts-"] {
            assert!(prefix.owns_cookie_name(owned.as_bytes()), "{owned}");
        }
        for other in ["ts", "tsx-ec", "_ex-ec", "sharedId"] {
            assert!(!prefix.owns_cookie_name(other.as_bytes()), "{other}");
        }
    }

    /// The client script reads the prefix from the attribute the server
    /// writes, on the bundle tag, so both names are the same in the two
    /// places.
    #[test]
    fn the_client_script_reads_the_attribute_the_server_writes() {
        let script = include_str!("../../trusted-server-js/lib/src/shared/public_prefix.ts");

        assert!(
            script.contains(&format!(
                "export const PUBLIC_PREFIX_ATTRIBUTE = '{PUBLIC_PREFIX_ATTRIBUTE}'"
            )),
            "the client script should read the attribute the server writes"
        );
        assert!(
            script.contains("const BUNDLE_TAG_ID = 'trustedserver-js'")
                && crate::tsjs::tsjs_script_tag_with_attributes(&[], &[])
                    .contains(r#"id="trustedserver-js""#),
            "the client script should read the tag the server writes"
        );
    }

    #[test]
    fn the_default_carries_no_publisher_domain() {
        let prefixes = Prefixes::default();

        assert_eq!(prefixes.public().as_str(), "ts");
        assert_eq!(prefixes.server_key().domain(), "");
    }
}
