use super::FrontendFramework;
use super::scenarios::{CustomScenario, TestScenario};
use crate::common::runtime::TestError;
use testcontainers::GenericImage;

/// Next.js frontend framework for integration testing.
///
/// Uses a pre-built Docker image (`test-nextjs:latest`) that serves
/// a minimal Next.js 14 app. The image must be built before running tests:
///
/// ```bash
/// docker build -t test-nextjs:latest \
///   crates/integration-tests/fixtures/frameworks/nextjs/
/// ```
pub struct NextJs;

impl FrontendFramework for NextJs {
    fn id(&self) -> &'static str {
        "nextjs"
    }

    fn build_container(&self) -> error_stack::Result<GenericImage, TestError> {
        Ok(GenericImage::new("test-nextjs", "latest")
            .with_exposed_port(testcontainers::core::IntoContainerPort::tcp(3000)))
    }

    fn container_port(&self) -> u16 {
        3000
    }

    fn health_check_path(&self) -> &str {
        "/"
    }

    fn standard_scenarios(&self) -> Vec<TestScenario> {
        vec![
            TestScenario::HtmlInjection,
            TestScenario::ScriptServing {
                modules: vec!["core", "prebid", "lockr"],
            },
            TestScenario::AttributeRewriting,
            TestScenario::GdprSignal,
        ]
    }

    fn custom_scenarios(&self) -> Vec<CustomScenario> {
        vec![
            CustomScenario::NextJsRscFlight,
            CustomScenario::NextJsServerActions,
        ]
    }
}
