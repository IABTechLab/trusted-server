//! The extension point a module uses to add rules to `/robots.txt`.

use core::time::Duration;

use error_stack::Report;

use super::Contribution;
use crate::error::TrustedServerError;
use crate::module_context::ModuleCall;
use crate::permissions::PermissionSet;

/// The `[robots-txt] modules` entry that refuses every crawler.
///
/// Core's own, and it names no vendor. Selecting it makes the file the refusal
/// and nothing else, whatever else is selected, and puts
/// [`REFUSE_ALL_ROBOTS_TAG`](super::REFUSE_ALL_ROBOTS_TAG) on every response.
pub const REFUSE_ALL: &str = "refuse_all";

/// The `[robots-txt] modules` entry that allows every crawler everywhere.
///
/// Core's own, so permission is something the document says rather than
/// something that follows from a lookup coming back empty.
pub const ALLOW_ALL: &str = "allow_all";

/// Something that adds rules to `/robots.txt`.
///
/// A module supplies one through
/// [`IntegrationRegistrationBuilder::with_robots_txt_contributor`](crate::integrations::IntegrationRegistrationBuilder::with_robots_txt_contributor),
/// and `[robots-txt] modules` selects it by the name it was declared under.
/// Core holds what a contributor returns, assembles the file from every
/// selected contributor in the order `modules` names them, and writes the
/// bytes. The contributor never does, so it cannot produce a malformed file.
///
/// `Send + Sync` so the registry can share it between requests, while the
/// future it returns is pinned to one thread, because the host SDKs produce
/// `!Send` futures on wasm32.
#[async_trait::async_trait(?Send)]
pub trait RobotsTxtContributor: Send + Sync + core::fmt::Debug {
    /// The longest one answer may be held before this contributor is asked
    /// again.
    ///
    /// A ceiling, not a schedule. Each contributor's answer is held and
    /// refreshed on its own, and the file is assembled from whatever each one
    /// holds at the time, so two contributors never have to agree on anything.
    fn refresh(&self) -> Duration;

    /// A value that changes whenever this contributor's answer would, such as a
    /// hash of its settings.
    ///
    /// It keys the held answers, so an answer given for different settings is
    /// never served for these. Core hashes it into the key an answer is held
    /// under, so nothing of it is written to the store.
    fn fingerprint(&self) -> String;

    /// The permissions this contributor declares, which decide whether a
    /// gated value is passed to it. None by default.
    fn required_permissions(&self) -> PermissionSet {
        PermissionSet::none()
    }

    /// This contributor's rules.
    ///
    /// Core holds the answer for the publisher and the host the request was
    /// addressed to, and serves it to every crawler of that host. So the call
    /// carries the request the answer was asked for, being its method, path,
    /// host and scheme without its query, with the host never taken from a
    /// forwarded header, and the request's services with no client
    /// information and no host signals, and nothing about who asked. An
    /// implementation hands its own function to [`ModuleCall::inject`],
    /// naming what it needs. Its answer must be the same for every request
    /// for that host, because it is served for all of them.
    ///
    /// A contribution with no groups is an answer, and is held as one, but a
    /// file in which nothing holds a rule fails closed with `503`. An error
    /// is a failure, and core serves this contributor's held copy instead.
    /// With no held copy the whole file fails closed with `503`.
    ///
    /// # Errors
    ///
    /// When this contributor cannot give its rules now.
    async fn contribute(
        &self,
        call: ModuleCall<'_>,
    ) -> Result<Contribution, Report<TrustedServerError>>;
}
