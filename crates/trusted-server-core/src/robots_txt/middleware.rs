//! `/robots.txt` as a middleware of core's own, named `robots_txt` in a
//! `[[fetch]]` entry on the file's path. Its preparation makes the file from
//! the selected contributors, through their holding, and its action replaces
//! the origin's file with it. Where the origin answers with anything else,
//! being no file, a page, an error or a redirect among them, the creating rule
//! runs it on an empty one.

use std::any::Any;
use std::time::Duration;

use error_stack::Report;

use super::{RETRY_AFTER_SECONDS, ROBOTS_TXT_PATH, RobotsTxtConfig, Selected, document, file_for};
use crate::error::TrustedServerError;
use crate::middleware::{
    Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase, Preparation, Refusal,
};
use crate::module_context::{ModuleCall, ModuleRequest, ResolvedRequest};
use crate::platform::RuntimeServices;
use crate::settings::Settings;
use crate::streaming_processor::StreamProcessor;

/// The name an entry writes, and the integration the registry lists the
/// middleware under.
pub const MODULE: &str = "robots_txt";

/// The media type the middleware works on.
pub const MEDIA_TYPE: &str = "text/plain";

/// What a request is told when no file can be made.
const UNAVAILABLE: &str = "robots.txt is temporarily unavailable\n";

/// The middleware, holding the section it was made for and the contributors
/// the registry resolved for it, in the order `modules` names them.
#[derive(Debug)]
pub struct RobotsTxt {
    config: RobotsTxtConfig,
    selected: Vec<Selected>,
}

impl RobotsTxt {
    /// The middleware for `config`, with `selected` the contributors in the
    /// order `modules` names them.
    #[must_use]
    pub(crate) fn new(config: RobotsTxtConfig, selected: Vec<Selected>) -> Self {
        Self { config, selected }
    }

    /// The file for the request, which is the refusal alone or every selected
    /// contributor's held answer assembled, each asked for `addressed_host`.
    /// With no addressed host only the refusal can be made.
    async fn file(
        &self,
        addressed_host: Option<&str>,
        request: ModuleRequest<'_>,
        settings: &Settings,
        services: &RuntimeServices,
    ) -> Result<String, Report<TrustedServerError>> {
        if self.config.refuses_all() {
            return Ok(document::refusal(&self.config.always_allow));
        }
        // The answers are held for the host and served to every crawler of
        // it, so the contributors are handed the request with nothing one
        // crawler put in the address, for the host it was addressed to and
        // never one a forwarded header names. The host is part of the key an
        // answer is held under.
        let Some(host) = addressed_host else {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "robots.txt cannot be made without the host the request was \
                          addressed to"
                    .to_owned(),
            }));
        };
        let request = ResolvedRequest::from_view(&request)
            .without_query()
            .with_host(host);
        file_for(&self.config, &self.selected, &request, services, settings).await
    }
}

impl Middleware for RobotsTxt {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == MEDIA_TYPE
    }

    // The file is the one path, as RFC 9309 section 2.3 gives it, so a longer
    // path the entry's prefix covers keeps the origin's answer.
    fn handles_path(&self, path: &str) -> bool {
        path == ROBOTS_TXT_PATH
    }

    // A crawler takes an error, a redirect, an empty answer, or an origin
    // file the action cannot replace, as permission or as the origin's own
    // rules, so the file is made in place of each.
    fn writes_whole_document(&self) -> bool {
        true
    }

    // The origin is asked for its whole file, so a crawler's validators or
    // range never bring a `304` or a `206` in place of the file.
    fn needs_whole_body(&self) -> bool {
        true
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &MiddlewarePhase::ALL
    }

    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        _context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        Some(Box::pin(async move {
            let file = match call.inject_with(self, call.addressed_host(), Self::file) {
                Ok(file) => file.await,
                Err(withheld) => Err(Report::new(TrustedServerError::Configuration {
                    message: format!("robots.txt cannot be made for this request: {withheld:?}"),
                })),
            };
            match file {
                Ok(file) => Ok(Box::new(file) as Box<dyn Any + Send>),
                Err(report) => {
                    log::error!("Could not assemble robots.txt: {report:?}");
                    Err(Refusal::service_unavailable(
                        Duration::from_secs(u64::from(RETRY_AFTER_SECONDS)),
                        UNAVAILABLE,
                    ))
                }
            }
        }))
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let file = context.take_prepared::<String>(MODULE).unwrap_or_else(|| {
            // Nothing prepared means the preparation did not run. The file
            // then refuses every crawler, because the origin's file, or no
            // file, could allow what the selected rules refuse.
            log::error!("robots.txt was not prepared, so the file refuses every crawler");
            document::refusal(&self.config.always_allow)
        });
        MiddlewareAction {
            stream: Some(Box::new(ReplaceBody { file: Some(file) })),
            ..MiddlewareAction::pass()
        }
    }
}

/// Drops the body it is given and writes the file in its place.
struct ReplaceBody {
    file: Option<String>,
}

impl StreamProcessor for ReplaceBody {
    fn process_chunk(&mut self, _chunk: &[u8], is_last: bool) -> Result<Vec<u8>, std::io::Error> {
        Ok(if is_last {
            self.file.take().unwrap_or_default().into_bytes()
        } else {
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationDocumentState;
    use crate::middleware::test_support::context;
    use crate::middleware::{MiddlewareChain, Prepared};

    fn middleware() -> RobotsTxt {
        RobotsTxt::new(RobotsTxtConfig::refuse_all(), Vec::new())
    }

    /// What the middleware is told in `phase`, with `prepared` as what its
    /// preparation left.
    fn context_with<'a>(
        phase: MiddlewarePhase,
        state: &'a IntegrationDocumentState,
        prepared: &'a Prepared,
    ) -> MiddlewareContext<'a> {
        MiddlewareContext {
            prepared,
            ..context(phase, state)
        }
    }

    #[test]
    fn the_middleware_works_on_text_in_either_phase() {
        let middleware = middleware();

        assert!(middleware.handles_media_type(MEDIA_TYPE));
        assert!(!middleware.handles_media_type(crate::middleware::HTML_MEDIA_TYPE));
        assert_eq!(middleware.phases(), &MiddlewarePhase::ALL);
        assert_eq!(middleware.middleware_id(), MODULE);
    }

    #[test]
    fn the_prepared_file_replaces_whatever_the_origin_sent() {
        let state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        prepared.insert(
            MiddlewarePhase::Fetch,
            MODULE,
            Box::new(String::from("User-agent: *\nDisallow: /\n")),
        );
        let context = context_with(MiddlewarePhase::Fetch, &state, &prepared);

        let mut processor = middleware()
            .create(&context)
            .stream
            .expect("a prepared file should be written as a stream processor");
        let mut output = processor
            .process_chunk(b"User-agent: *\nAllow: /\n", false)
            .expect("should take the origin's file");
        output.extend(
            processor
                .process_chunk(b"", true)
                .expect("should write the file at the end"),
        );

        assert_eq!(output, b"User-agent: *\nDisallow: /\n");
    }

    /// What `middleware` writes in place of an origin's file in `phase`, with
    /// `prepared` as what its preparation left.
    fn written(middleware: &RobotsTxt, phase: MiddlewarePhase, prepared: &Prepared) -> String {
        let state = IntegrationDocumentState::default();
        let mut processor = middleware
            .create(&context_with(phase, &state, prepared))
            .stream
            .expect("the middleware should write a file whatever was prepared");
        let output = processor
            .process_chunk(b"User-agent: *\nAllow: /\n", true)
            .expect("should write the file");
        String::from_utf8(output).expect("the file is text")
    }

    /// Nothing prepared means the preparation did not run, so the file
    /// refuses every crawler, whatever `modules` selects, rather than letting
    /// the origin's file through.
    #[test]
    fn with_nothing_prepared_the_file_refuses_every_crawler() {
        let allowing: RobotsTxtConfig = serde_json::from_value(serde_json::json!({
            "modules": "allow_all",
            "always_allow": ["/ads.txt"],
        }))
        .expect("should read the robots.txt settings");
        let middleware = RobotsTxt::new(allowing, vec![(crate::robots_txt::ALLOW_ALL, None)]);

        assert_eq!(
            written(&middleware, MiddlewarePhase::Fetch, &Prepared::default()),
            "User-agent: *\nAllow: /ads.txt\nDisallow: /\n"
        );
    }

    #[test]
    fn a_value_of_another_type_under_the_name_is_not_a_file() {
        let prepared = Prepared::default();
        prepared.insert(MiddlewarePhase::Serve, MODULE, Box::new(7_u32));

        assert_eq!(
            written(&middleware(), MiddlewarePhase::Serve, &prepared),
            "User-agent: *\nDisallow: /\n",
            "the file refuses every crawler, as with nothing prepared"
        );
    }

    #[test]
    fn a_chain_plans_the_middleware_on_text() {
        let chain = MiddlewareChain::new(
            MiddlewarePhase::Fetch,
            MEDIA_TYPE,
            vec![std::sync::Arc::new(middleware()) as std::sync::Arc<dyn Middleware>],
        );

        assert_eq!(chain.ids(), [MODULE]);
    }
}
