//! Test-only adapter preserving existing buffered-output regression scenarios.

use std::io;
use std::sync::Arc;

use error_stack::Report;
use futures::future::LocalBoxFuture;

use super::{IntegrationHtmlStreamContext, IntegrationHtmlStreamProcessorFactory};
use crate::streaming_processor::StreamProcessor;
use crate::streaming_processor::finish::{StreamFinishDeadline, StreamFinishInput};

pub use crate::streaming_processor::finish::{
    StreamFinish as HtmlFinalization, StreamFinishError as HtmlFinalizationError,
    StreamOutputWriter as HtmlReplacementWriter,
};

pub trait IntegrationHtmlFinalizerFactory: Send + Sync {
    fn integration_id(&self) -> &'static str;
    fn create(&self, context: IntegrationHtmlStreamContext) -> Box<dyn IntegrationHtmlFinalizer>;
}

pub trait IntegrationHtmlFinalizer {
    fn finalize<'a>(
        &'a mut self,
        input: HtmlFinalizationInput<'a>,
    ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>>;
}

pub struct HtmlFinalizationInput<'a> {
    pub response_status: http::StatusCode,
    pub html: &'a [u8],
    pub replacement: &'a mut HtmlReplacementWriter,
    pub services: &'a crate::platform::RuntimeServices,
    pub deadline: &'a StreamFinishDeadline,
}

pub(super) struct TestFinishFactory(pub Arc<dyn IntegrationHtmlFinalizerFactory>);
struct TestFinishProcessor(Box<dyn IntegrationHtmlFinalizer>);

impl IntegrationHtmlStreamProcessorFactory for TestFinishFactory {
    fn integration_id(&self) -> &'static str {
        self.0.integration_id()
    }
    fn requires_async_finish(&self) -> bool {
        true
    }
    fn buffer_output_until_eof(&self) -> bool {
        true
    }
    fn create(&self, context: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor> {
        Box::new(TestFinishProcessor(self.0.create(context)))
    }
}

impl StreamProcessor for TestFinishProcessor {
    fn process_chunk(&mut self, chunk: &[u8], _: bool) -> io::Result<Vec<u8>> {
        Ok(chunk.to_vec())
    }
    fn requires_async_finish(&self) -> bool {
        true
    }
    fn async_finish<'a>(
        &'a mut self,
        input: StreamFinishInput<'a>,
    ) -> Option<LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>>> {
        Some(self.0.finalize(HtmlFinalizationInput {
            response_status: input.response_status,
            html: input.output,
            replacement: input.replacement,
            services: input.services,
            deadline: input.deadline,
        }))
    }
}
