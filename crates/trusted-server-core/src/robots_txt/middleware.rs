//! `/robots.txt` as a middleware of core's own, named `robots_txt` in a
//! `[[fetch]]` or `[[serve]]` entry on the file's path. Its preparation makes
//! the file from the selected contributors, through their holding, and its
//! action replaces whatever the origin sent with that file. Where the origin
//! has no file, the creating rule runs it on an empty one.

use std::any::Any;
use std::time::Duration;

use error_stack::Report;

use super::{RETRY_AFTER_SECONDS, RobotsTxtConfig, Selected, document, file_for};
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
    /// contributor's held answer assembled.
    async fn file(
        &self,
        request: ModuleRequest<'_>,
        settings: &Settings,
        services: &RuntimeServices,
    ) -> Result<String, Report<TrustedServerError>> {
        if self.config.refuses_all() {
            return Ok(document::refusal(&self.config.always_allow));
        }
        // The answers are held and served to every crawler, so the
        // contributors are handed the request with nothing one crawler put in
        // the address.
        let request = ResolvedRequest::from_view(&request).without_query();
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

    fn phases(&self) -> &[MiddlewarePhase] {
        &MiddlewarePhase::ALL
    }

    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        _context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        Some(Box::pin(async move {
            let file = match call.inject(self, Self::file) {
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
        match context.prepared.take::<String>(MODULE) {
            Some(file) => MiddlewareAction {
                stream: Some(Box::new(ReplaceBody { file: Some(file) })),
                ..MiddlewareAction::pass()
            },
            // Nothing prepared means the preparation did not run, which no
            // request can cause. The origin's own file is left as it is,
            // because an empty file would tell crawlers to crawl everything.
            None => {
                log::error!("robots.txt was not prepared, so the origin's file is served as it is");
                MiddlewareAction::pass()
            }
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

    #[test]
    fn with_nothing_prepared_the_origins_file_is_left_as_it_is() {
        let state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = context_with(MiddlewarePhase::Fetch, &state, &prepared);

        let action = middleware().create(&context);

        assert!(action.stream.is_none(), "should pass the body through");
        assert!(action.head_inserts.is_empty());
    }

    #[test]
    fn a_value_of_another_type_under_the_name_is_not_a_file() {
        let state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        prepared.insert(MODULE, Box::new(7_u32));
        let context = context_with(MiddlewarePhase::Serve, &state, &prepared);

        assert!(middleware().create(&context).stream.is_none());
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
