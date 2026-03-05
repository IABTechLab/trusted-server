use super::FrontendFramework;
use super::scenarios::{CustomScenario, TestScenario};
use crate::common::runtime::TestError;
use testcontainers::GenericImage;

/// WordPress frontend framework for integration testing.
///
/// Uses a pre-built Docker image (`test-wordpress:latest`) that serves
/// a minimal WordPress site with a test theme. The image must be built
/// before running tests:
///
/// ```bash
/// docker build -t test-wordpress:latest \
///   crates/integration-tests/fixtures/frameworks/wordpress/
/// ```
pub struct WordPress;

impl FrontendFramework for WordPress {
    fn id(&self) -> &'static str {
        "wordpress"
    }

    fn build_container(&self) -> error_stack::Result<GenericImage, TestError> {
        Ok(GenericImage::new("test-wordpress", "latest")
            .with_exposed_port(testcontainers::core::IntoContainerPort::tcp(80)))
    }

    fn container_port(&self) -> u16 {
        80
    }

    fn health_check_path(&self) -> &str {
        "/"
    }

    fn standard_scenarios(&self) -> Vec<TestScenario> {
        vec![
            TestScenario::HtmlInjection,
            TestScenario::ScriptServing {
                modules: vec!["core", "prebid"],
            },
            TestScenario::AttributeRewriting,
            TestScenario::GdprSignal,
        ]
    }

    fn custom_scenarios(&self) -> Vec<CustomScenario> {
        vec![CustomScenario::WordPressAdminInjection]
    }
}
