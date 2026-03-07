use super::FrontendFramework;
use super::scenarios::{CustomScenario, TestScenario};
use crate::common::runtime::TestError;
use testcontainers::core::{ContainerRequest, IntoContainerPort};
use testcontainers::{GenericImage, ImageExt as _};

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

    fn build_container(
        &self,
        origin_port: u16,
    ) -> error_stack::Result<ContainerRequest<GenericImage>, TestError> {
        let container_port = self.container_port();
        let origin_host = format!("127.0.0.1:{origin_port}");
        Ok(GenericImage::new("test-nextjs", "latest")
            .with_exposed_port(container_port.tcp())
            .with_mapped_port(origin_port, container_port.tcp())
            .with_env_var("ORIGIN_HOST", origin_host))
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
            TestScenario::ScriptServing,
            TestScenario::AttributeRewriting,
            TestScenario::ScriptServingUnknownFile404,
        ]
    }

    fn custom_scenarios(&self) -> Vec<CustomScenario> {
        vec![
            CustomScenario::NextJsRscFlight,
            CustomScenario::NextJsServerActions,
            CustomScenario::NextJsApiRoute,
            CustomScenario::NextJsFormAction,
        ]
    }
}
