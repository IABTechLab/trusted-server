pub mod nextjs;
pub mod scenarios;
pub mod wordpress;

use crate::common::runtime::TestError;
use scenarios::{CustomScenario, TestScenario};
use testcontainers::core::ContainerRequest;
use testcontainers::GenericImage;

/// Trait defining how to test a frontend framework.
///
/// Each framework provides a Docker container image and declares which
/// test scenarios apply to it. The matrix test runner uses this trait
/// to test every framework against every runtime environment.
///
/// # Adding a new framework
///
/// 1. Create `fixtures/frameworks/<name>/` with a Dockerfile
/// 2. Create `tests/frameworks/<name>.rs` implementing this trait
/// 3. Register in [`FRAMEWORKS`] below
pub trait FrontendFramework: Send + Sync {
    /// Framework identifier (e.g. "wordpress", "nextjs").
    fn id(&self) -> &'static str;

    /// Build a Docker container request mapped to the given origin port.
    ///
    /// The `origin_port` is the fixed host port that the WASM binary
    /// expects the origin to be running on (baked in at build time).
    ///
    /// # Errors
    ///
    /// Returns [`TestError::ContainerStart`] if the image cannot be created.
    fn build_container(
        &self,
        origin_port: u16,
    ) -> error_stack::Result<ContainerRequest<GenericImage>, TestError>;

    /// Port the framework serves on inside the container.
    fn container_port(&self) -> u16;

    /// HTTP path to use for container health checks.
    fn health_check_path(&self) -> &str {
        "/"
    }

    /// Standard test scenarios applicable to this framework.
    fn standard_scenarios(&self) -> Vec<TestScenario>;

    /// Framework-specific test scenarios (optional).
    fn custom_scenarios(&self) -> Vec<CustomScenario> {
        vec![]
    }
}

/// Framework factory function type.
type FrameworkFactory = fn() -> Box<dyn FrontendFramework>;

/// Registry of all supported frontend frameworks.
///
/// Uses function pointers to avoid trait object static initialization issues.
///
/// # Adding a new framework
///
/// 1. Create `fixtures/frameworks/<name>/`
/// 2. Create `tests/frameworks/<name>.rs`
/// 3. Implement [`FrontendFramework`] trait
/// 4. Add factory here: `|| Box::new(<name>::<Struct>)`
pub static FRAMEWORKS: &[FrameworkFactory] = &[
    || Box::new(wordpress::WordPress),
    || Box::new(nextjs::NextJs),
];
