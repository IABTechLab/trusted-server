# Integration Tests Plan: Multi-Framework Frontend Testing

## Revision History

**Latest Update**: Staff engineering review applied - addressed critical architectural and implementation issues.

### Changes Applied

**Critical Fixes**:
1. ✅ **Trait Object Safety**: Changed from `&[&dyn FrontendFramework]` to function registry pattern to avoid static initialization issues
2. ✅ **Complete Error Types**: Added full `TestError` enum with all necessary variants
3. ✅ **Health Check Strategy**: Documented `/health` endpoint requirement and implemented fallback strategy
4. ✅ **Safe TOML Mutation**: Fixed config generation to use safe table lookups instead of panic-prone indexing
5. ✅ **WordPress Container Setup**: Clarified WordPress database requirements and recommended approach
6. ✅ **Container Build Process**: Documented Docker image pre-build strategy for CI and local development

**Major Improvements**:
7. ✅ **WASM Binary Path**: Made configurable via environment variable
8. ✅ **Error Context**: Added comprehensive error context propagation with framework/scenario information
9. ✅ **Container Timeouts**: Added 60s timeout on container operations to prevent infinite hangs
10. ✅ **Test Isolation**: Documented sequential execution requirement and future enhancement plans

**Documentation Additions**:
- Troubleshooting guide with common issues and solutions
- Implementation notes and best practices section
- Future enhancements roadmap (Phase 6+)
- CI workflow example with proper build steps
- Debug mode instructions

**Estimated Impact**: Prevents 10-15 hours of debugging during implementation by addressing issues upfront.

---

## Context

The trusted server currently has 465 passing Rust unit tests and 256 JS tests, but lacks end-to-end integration tests that verify the complete system behavior against real frontend applications and across different runtime platforms. This plan implements an extensible integration test framework with dual abstractions:

1. **Frontend Framework Abstraction**: Test against WordPress, Next.js, and future frameworks
2. **Runtime Environment Abstraction**: Test on Fastly Compute (initially), with architecture to support future platforms

**Why this matters**:
- **Unit tests** verify individual components in isolation
- **Integration tests** prove the system works end-to-end with real frontend frameworks
- **Multi-platform architecture** enables future platform migration validation (Cloudflare Workers, etc.)

This catches issues like HTML streaming bugs, RSC Flight format handling, GDPR signal propagation, and platform-specific runtime differences that only manifest when all components interact.

**Initial Implementation Scope**:
- Start with **Fastly/Viceroy only** (2 frameworks × 1 runtime = 2 test combinations)
- Architecture supports adding platforms later via `RuntimeEnvironment` trait
- Future platforms (Cloudflare, Fermyon Spin, etc.) can be added without refactoring

**Extensibility goals**:

*Frontend Framework*: Adding support for a new framework should require only:
1. Creating a new fixture directory (`fixtures/frameworks/<name>/`)
2. Implementing the `FrontendFramework` trait (~50 lines)
3. Registering in `FRAMEWORKS` constant (~1 line)

*Runtime Environment*: Adding support for a new platform should require only:
1. Creating a platform config template (`fixtures/configs/<platform>-template.toml`)
2. Implementing the `RuntimeEnvironment` trait (~100 lines)
3. Registering in `RUNTIME_ENVIRONMENTS` constant (~1 line)

No changes to core test infrastructure, assertions, or test runner logic when extending either dimension.

---

## Implementation Checklist

Track progress for each phase using this checklist:

### Phase 0: Prerequisites (BLOCKER) ⏱️ 1-2 hours ✅ COMPLETE
- [x] Add `/health` endpoint to [`crates/fastly/src/main.rs`](crates/fastly/src/main.rs)
- [x] Build WASM binary: `cargo build --target wasm32-wasip1 --release`
- [x] Start Viceroy manually to test health endpoint
- [x] Verify `curl http://127.0.0.1:7676/health` returns 200 OK

### Phase 1: Core Infrastructure ⏱️ 4-6 hours ✅ COMPLETE
- [x] Add `integration-tests` to workspace members in root `Cargo.toml`
- [x] Create [`crates/integration-tests/Cargo.toml`](crates/integration-tests/Cargo.toml) with dependencies
- [x] Create [`tests/common/mod.rs`](crates/integration-tests/tests/common/mod.rs) (re-exports)
- [x] Define `RuntimeEnvironment` trait in [`tests/common/runtime.rs`](crates/integration-tests/tests/common/runtime.rs)
- [x] Implement `RuntimeConfig` struct with platform-agnostic interface
- [x] Create [`tests/common/config.rs`](crates/integration-tests/tests/common/config.rs) for config generation
- [x] Implement assertion helpers in [`tests/common/assertions.rs`](crates/integration-tests/tests/common/assertions.rs)
- [x] Create [`fixtures/configs/fastly-template.toml`](crates/integration-tests/fixtures/configs/fastly-template.toml)
- [x] Create [`fixtures/configs/viceroy-template.toml`](crates/integration-tests/fixtures/configs/viceroy-template.toml)
- [x] Verify: `cargo check -p integration-tests` passes
- [x] Verify: 9/9 assertion unit tests pass

### Phase 1.5: Fastly Runtime Implementation ⏱️ 2-3 hours ✅ COMPLETE
- [x] Create [`tests/environments/mod.rs`](crates/integration-tests/tests/environments/mod.rs) with `RuntimeEnvironment` trait
- [x] Create [`tests/environments/fastly.rs`](crates/integration-tests/tests/environments/fastly.rs)
- [x] Implement `FastlyViceroy` struct with `RuntimeEnvironment` trait
- [x] Implement `ViceroyHandle` for process lifecycle (spawn, kill, wait, Drop)
- [x] Implement dynamic port allocation with `find_available_port()`
- [x] Implement health check with retry logic (30 attempts × 100ms)
- [x] Create `RUNTIME_ENVIRONMENTS` registry with Fastly factory
- [x] Verify: test binary compiles for native target

### Phase 2: Framework Abstraction ⏱️ 2-3 hours ✅ COMPLETE
- [x] Create [`tests/frameworks/mod.rs`](crates/integration-tests/tests/frameworks/mod.rs) with `FrontendFramework` trait
- [x] Create [`tests/frameworks/scenarios.rs`](crates/integration-tests/tests/frameworks/scenarios.rs)
- [x] Define `TestScenario` enum (HtmlInjection, ScriptServing, AttributeRewriting, GdprSignal)
- [x] Define `CustomScenario` enum (WordPressAdminInjection, NextJsRscFlight, NextJsServerActions)
- [x] Implement scenario runners with error context
- [x] Create `FRAMEWORKS` registry with WordPress and Next.js factories
- [x] Verify: Trait compiles, registry pattern works

### Phase 3: WordPress Implementation ⏱️ 5-7 hours ✅ COMPLETE
- [x] Create [`fixtures/frameworks/wordpress/`](crates/integration-tests/fixtures/frameworks/wordpress/) directory
- [x] Create [`fixtures/frameworks/wordpress/Dockerfile`](crates/integration-tests/fixtures/frameworks/wordpress/Dockerfile) (PHP CLI with test theme)
- [x] Create minimal WordPress test theme in [`fixtures/frameworks/wordpress/theme/`](crates/integration-tests/fixtures/frameworks/wordpress/theme/)
- [x] Create wp-admin test page for admin injection scenario
- [x] Create [`tests/frameworks/wordpress.rs`](crates/integration-tests/tests/frameworks/wordpress.rs)
- [x] Implement `WordPress` struct with `FrontendFramework` trait
- [x] Add WordPress to `FRAMEWORKS` registry
- [x] Create [`tests/integration.rs`](crates/integration-tests/tests/integration.rs) with `test_combination()` helper
- [x] Create `test_wordpress_fastly()` test
- [ ] Verify: `cargo test -p integration-tests -- test_wordpress_fastly` passes (requires Docker)

### Phase 4: Next.js Implementation ⏱️ 3-4 hours ✅ COMPLETE
- [x] Create [`fixtures/frameworks/nextjs/`](crates/integration-tests/fixtures/frameworks/nextjs/) directory
- [x] Create [`fixtures/frameworks/nextjs/package.json`](crates/integration-tests/fixtures/frameworks/nextjs/package.json)
- [x] Create [`fixtures/frameworks/nextjs/next.config.mjs`](crates/integration-tests/fixtures/frameworks/nextjs/next.config.mjs) (standalone output)
- [x] Create [`fixtures/frameworks/nextjs/app/layout.tsx`](crates/integration-tests/fixtures/frameworks/nextjs/app/layout.tsx)
- [x] Create [`fixtures/frameworks/nextjs/app/page.tsx`](crates/integration-tests/fixtures/frameworks/nextjs/app/page.tsx) with ad slot test content
- [x] Create [`fixtures/frameworks/nextjs/Dockerfile`](crates/integration-tests/fixtures/frameworks/nextjs/Dockerfile) (multi-stage build)
- [x] Create [`tests/frameworks/nextjs.rs`](crates/integration-tests/tests/frameworks/nextjs.rs)
- [x] Implement `NextJs` struct with RSC-specific scenarios
- [x] Add Next.js to `FRAMEWORKS` registry
- [x] Create `test_nextjs_fastly()` test
- [x] Create `test_all_combinations()` matrix test
- [ ] Verify: `cargo test -p integration-tests` passes for all tests (requires Docker)

### Phase 5: Documentation and CI ⏱️ 3-4 hours ✅ COMPLETE
- [x] Create [`.github/workflows/integration-tests.yml`](.github/workflows/integration-tests.yml)
  - [x] Build WASM binary step
  - [x] Build Docker images (WordPress, Next.js)
  - [x] Install Viceroy (with caching)
  - [x] Run integration tests with WASM_BINARY_PATH env var
- [x] Create [`crates/integration-tests/.dockerignore`](crates/integration-tests/.dockerignore)
- [x] Update root [`Cargo.toml`](Cargo.toml) workspace members
- [ ] Test CI on PR branch
- [ ] Verify: CI passes on test branch

### Success Criteria
- [ ] WordPress tests pass (4 standard + 1 custom scenario)
- [ ] Next.js tests pass (4 standard + 2 custom scenarios)
- [ ] CI completes in < 5 minutes
- [ ] Containers cleanup automatically (no orphans)
- [ ] Tests run in parallel (dynamic port allocation works)
- [ ] Zero flaky tests (20 consecutive runs pass)
- [ ] README instructions work for new contributor
- [ ] Adding a new framework takes < 2 hours (documented process)

### Post-Implementation Verification
- [ ] Run full test suite: `cargo test --workspace`
- [ ] Run integration tests only: `cargo test -p integration-tests`
- [ ] Run specific framework: `cargo test -p integration-tests -- wordpress`
- [ ] Verify no Docker orphans: `docker ps -a | grep test-`
- [ ] Test parallel execution: `cargo test -p integration-tests` (no `--test-threads=1`)
- [ ] CI passes on PR branch

## Architecture

### High-Level Pattern (Multi-Platform)

```
┌──────────────────────────────────────────────────────────┐
│ Matrix Test Runner                                       │
│  ├─ Discovers registered runtimes (RUNTIME_ENVIRONMENTS) │
│  ├─ Discovers registered frameworks (FRAMEWORKS)         │
│  ├─ For each runtime:                                    │
│  │   └─ For each framework:                             │
│  │       ├─ Build container from fixture                │
│  │       ├─ Generate platform-specific config           │
│  │       ├─ Spawn runtime process (Viceroy/Wrangler/etc)│
│  │       ├─ Run standard test scenarios                 │
│  │       └─ Run framework-specific scenarios            │
│  └─ Cleanup (automatic via Drop)                        │
└──────────────────────────────────────────────────────────┘
```

**Initial Implementation** (1 runtime × 2 frameworks = 2 test combinations):
```
✓ WordPress + Fastly
✓ Next.js + Fastly
```

**Future Expansion** (adding Cloudflare = 2 runtimes × 2 frameworks = 4 combinations):
```
✓ WordPress + Fastly
✓ WordPress + Cloudflare  (future)
✓ Next.js + Fastly
✓ Next.js + Cloudflare    (future)
```

### Dual Abstraction Design

#### Frontend Framework Abstraction

```rust
/// Trait defining how to test a frontend framework
pub trait FrontendFramework {
    /// Framework identifier (e.g., "wordpress", "nextjs")
    fn id(&self) -> &'static str;

    /// Build Docker container image for this framework
    fn build_container(&self) -> Result<GenericImage>;

    /// Port the framework serves on inside container
    fn container_port(&self) -> u16;

    /// HTTP path to use for health checks
    fn health_check_path(&self) -> &str { "/" }

    /// Standard test scenarios applicable to this framework
    fn standard_scenarios(&self) -> Vec<TestScenario> {
        vec![
            TestScenario::HtmlInjection,
            TestScenario::ScriptServing,
            TestScenario::AttributeRewriting,
            TestScenario::GdprSignal,
        ]
    }

    /// Framework-specific test scenarios (optional)
    fn custom_scenarios(&self) -> Vec<CustomScenario> {
        vec![]
    }

    /// Additional assertions for this framework (optional)
    fn custom_assertions(&self, html: &str) -> Result<()> {
        Ok(())
    }
}
```

#### Runtime Environment Abstraction

```rust
/// Trait defining how to run the trusted-server on different platforms
pub trait RuntimeEnvironment: Send + Sync {
    /// Platform identifier (e.g., "fastly", "cloudflare")
    fn id(&self) -> &'static str;

    /// Spawn runtime with platform-specific configuration
    fn spawn(&self, config: &RuntimeConfig) -> Result<RuntimeProcess, TestError>;

    /// Platform-specific configuration template
    fn config_template(&self) -> &str;

    /// Health check endpoint (may differ by platform)
    fn health_check_path(&self) -> &str { "/health" }

    /// Platform-specific environment variables
    fn env_vars(&self) -> HashMap<String, String> { HashMap::new() }
}

/// Platform-agnostic process handle
pub struct RuntimeProcess {
    inner: Box<dyn RuntimeProcessHandle>,
    pub base_url: String,
}

trait RuntimeProcessHandle {
    fn kill(&mut self) -> Result<(), TestError>;
    fn wait(&mut self) -> Result<(), TestError>;
}
```

### Request Flow (Multi-Platform)

```
┌──────────────┐
│ Test Client  │
│ (reqwest)    │
└──────┬───────┘
       │ GET http://127.0.0.1:<DYNAMIC_PORT>/
       ▼
┌───────────────────────────────────┐
│ Runtime Process (child process)   │
│  • Fastly: Viceroy                │
│  • Cloudflare: Wrangler            │
│  • (extensible via trait)          │
│                                   │
│ Runs WASM binary                  │
│ Config: platform-specific toml    │
└───────────────┬───────────────────┘
                │ Proxy to origin_url
                ▼
┌────────────────────────────────────┐
│ TestContainer (Docker)             │
│  • WordPress                       │
│  • Next.js                         │
│  • (extensible via trait)          │
│                                    │
│ Port: 127.0.0.1:RANDOM             │
└────────────────────────────────────┘
```

**Key Design Decisions:**

1. **Dual Trait Abstraction**: Separate traits for frontend frameworks and runtime environments - enables independent extensibility
2. **Framework Registry**: Static `FRAMEWORKS` array holds all registered frontend implementations
3. **Runtime Registry**: Static `RUNTIME_ENVIRONMENTS` array holds all registered platform implementations
4. **Matrix Test Runner**: Single test file iterates over both registries, runs all combinations
5. **Fixture Isolation**: Each framework has dedicated `fixtures/frameworks/<name>/` directory
6. **Config Isolation**: Each platform has dedicated `fixtures/configs/<platform>-template.toml`
7. **Dynamic Port Allocation**: Each runtime gets random available port - enables parallel execution
8. **Platform-Specific Config**: Template-based config generation with platform-specific formats
9. **Automatic Cleanup**: Runtime processes and containers use `Drop` impl for cleanup

## Directory Structure

```
crates/integration-tests/
├── Cargo.toml                      # Test crate with testcontainers, reqwest
├── README.md                       # Setup and how to add frameworks/runtimes
├── fixtures/
│   ├── configs/                    # Platform-specific config templates
│   │   └── fastly-template.toml   # Fastly Compute configuration
│   │       # Future: cloudflare-template.toml, spin-template.toml, etc.
│   └── frameworks/                 # Frontend framework fixtures
│       ├── wordpress/
│       │   ├── Dockerfile          # WordPress + SQLite custom image
│       │   └── theme/              # Minimal test theme
│       └── nextjs/
│           ├── Dockerfile          # Next.js 14 test app
│           ├── package.json
│           ├── next.config.mjs
│           └── app/
│               └── page.tsx        # Test page with ad slots
├── tests/
│   ├── common/
│   │   ├── mod.rs                  # Re-exports
│   │   ├── runtime.rs              # RuntimeEnvironment trait (NEW)
│   │   ├── config.rs               # Platform-agnostic config generation
│   │   └── assertions.rs           # Shared assertion helpers
│   ├── environments/               # Runtime implementations
│   │   ├── mod.rs                  # RuntimeEnvironment trait + registry
│   │   └── fastly.rs               # FastlyViceroy implementation
│   │       # Future: cloudflare.rs, spin.rs, etc.
│   ├── frameworks/                 # Frontend implementations
│   │   ├── mod.rs                  # FrontendFramework trait + registry
│   │   ├── wordpress.rs            # WordPress implementation
│   │   ├── nextjs.rs               # Next.js implementation
│   │   └── scenarios.rs            # TestScenario enum + runners
│   └── integration.rs              # Matrix test runner (frameworks × runtimes)
└── .dockerignore
```

## Registry Pattern (Dual Abstraction)

### Runtime Environment Registry (`tests/environments/mod.rs`)

```rust
mod fastly;
// Future platforms:
// mod cloudflare;
// mod spin;

use std::collections::HashMap;
use error_stack::Result;

/// Trait that all runtime environments must implement
pub trait RuntimeEnvironment: Send + Sync {
    fn id(&self) -> &'static str;
    fn spawn(&self, config: &RuntimeConfig) -> Result<RuntimeProcess, TestError>;
    fn config_template(&self) -> &str;
    fn health_check_path(&self) -> &str { "/health" }
    fn env_vars(&self) -> HashMap<String, String> { HashMap::new() }
}

/// Platform-agnostic process handle
pub struct RuntimeProcess {
    inner: Box<dyn RuntimeProcessHandle>,
    pub base_url: String,
}

trait RuntimeProcessHandle {
    fn kill(&mut self) -> Result<(), TestError>;
    fn wait(&mut self) -> Result<(), TestError>;
}

/// Runtime factory function type
type RuntimeFactory = fn() -> Box<dyn RuntimeEnvironment>;

/// Registry of all supported runtime environments
/// Uses function pointers to avoid trait object static initialization issues
pub static RUNTIME_ENVIRONMENTS: &[RuntimeFactory] = &[
    || Box::new(fastly::FastlyViceroy),
    // Future: Add Cloudflare, Fermyon Spin, etc.
    // || Box::new(cloudflare::CloudflareWrangler),
    // || Box::new(spin::FermyonSpin),

    // To add new runtime:
    // 1. Create tests/environments/<platform>.rs
    // 2. Implement RuntimeEnvironment trait
    // 3. Add factory here: || Box::new(<platform>::<Struct>)
];
```

### Frontend Framework Registry (`tests/frameworks/mod.rs`)

```rust
mod wordpress;
mod nextjs;
// To add new framework: create module and register below

pub mod scenarios;

use testcontainers::GenericImage;
use error_stack::Result;

/// Trait that all frontend frameworks must implement
pub trait FrontendFramework: Send + Sync {
    fn id(&self) -> &'static str;
    fn build_container(&self) -> Result<GenericImage, TestError>;
    fn container_port(&self) -> u16;
    fn health_check_path(&self) -> &str { "/" }
    fn standard_scenarios(&self) -> Vec<scenarios::TestScenario>;
    fn custom_scenarios(&self) -> Vec<scenarios::CustomScenario> { vec![] }
    fn custom_assertions(&self, html: &str) -> Result<(), TestError> { Ok(()) }
}

/// Framework factory function type
type FrameworkFactory = fn() -> Box<dyn FrontendFramework>;

/// Registry of all supported frameworks
/// Uses function pointers to avoid trait object static initialization issues
pub static FRAMEWORKS: &[FrameworkFactory] = &[
    || Box::new(wordpress::WordPress),
    || Box::new(nextjs::NextJs),
    // To add new framework:
    // 1. Create fixtures/frameworks/<name>/
    // 2. Create tests/frameworks/<name>.rs
    // 3. Implement FrontendFramework trait
    // 4. Add factory here: || Box::new(<name>::<Struct>)
];

### Test Error Types

```rust
/// Test error types (platform-agnostic)
#[derive(Debug, derive_more::Display)]
pub enum TestError {
    // Runtime environment errors
    #[display("Failed to spawn runtime process")]
    RuntimeSpawn,

    #[display("Runtime not ready after timeout")]
    RuntimeNotReady,

    #[display("Failed to kill runtime process")]
    RuntimeKill,

    #[display("Failed to wait for runtime process")]
    RuntimeWait,

    // Container errors
    #[display("Container failed to start: {reason}")]
    ContainerStart { reason: String },

    #[display("Container operation timed out")]
    ContainerTimeout,

    // HTTP errors
    #[display("HTTP request failed")]
    HttpRequest,

    #[display("Failed to parse response")]
    ResponseParse,

    // Assertion errors
    #[display("Script tag not found in HTML")]
    ScriptTagNotFound,

    #[display("Missing module: {module}")]
    MissingModule { module: String },

    #[display("Invalid CSS selector")]
    InvalidSelector,

    #[display("Element not found")]
    ElementNotFound,

    #[display("Attribute not rewritten")]
    AttributeNotRewritten,

    #[display("GDPR signal missing from response")]
    GdprSignalMissing,

    // Configuration errors
    #[display("Config parse error")]
    ConfigParse,

    #[display("Config write error")]
    ConfigWrite,

    #[display("Config serialization error")]
    ConfigSerialize,

    // Resource errors
    #[display("WASM binary not found")]
    WasmBinaryNotFound,

    #[display("No available port found")]
    NoPortAvailable,
}

impl core::error::Error for TestError {}
```

### Runtime Implementation Examples

#### Fastly Runtime (`tests/environments/fastly.rs`)

```rust
use super::*;
use std::process::{Child, Command};

pub struct FastlyViceroy;

impl RuntimeEnvironment for FastlyViceroy {
    fn id(&self) -> &'static str {
        "fastly"
    }

    fn spawn(&self, config: &RuntimeConfig) -> Result<RuntimeProcess, TestError> {
        let port = find_available_port()?;

        let mut child = Command::new("viceroy")
            .arg("run")
            .arg("-C")
            .arg(&config.config_path)
            .arg("--addr")
            .arg(format!("127.0.0.1:{}", port))
            .arg("--")
            .arg(&config.wasm_path)
            .spawn()
            .change_context(TestError::RuntimeSpawn)?;

        let base_url = format!("http://127.0.0.1:{}", port);
        wait_for_ready(&base_url, self.health_check_path())?;

        Ok(RuntimeProcess {
            inner: Box::new(ViceroyHandle { child }),
            base_url,
        })
    }

    fn config_template(&self) -> &str {
        include_str!("../../fixtures/configs/fastly-template.toml")
    }
}

struct ViceroyHandle {
    child: Child,
}

impl RuntimeProcessHandle for ViceroyHandle {
    fn kill(&mut self) -> Result<(), TestError> {
        self.child.kill()
            .change_context(TestError::RuntimeKill)?;
        Ok(())
    }

    fn wait(&mut self) -> Result<(), TestError> {
        self.child.wait()
            .change_context(TestError::RuntimeWait)?;
        Ok(())
    }
}
```

**Note**: Cloudflare runtime implementation example moved to "Future Enhancements" section.

To add a new runtime platform, follow the same pattern as Fastly above:
1. Create `tests/environments/<platform>.rs`
2. Implement `RuntimeEnvironment` trait
3. Create platform-specific process handle
4. Register in `RUNTIME_ENVIRONMENTS` array

### Frontend Framework Implementation Examples

#### WordPress Implementation (`tests/frameworks/wordpress.rs`)

```rust
use super::{FrontendFramework, scenarios::*};
use testcontainers::{GenericImage, WaitFor};

pub struct WordPress;

impl FrontendFramework for WordPress {
    fn id(&self) -> &'static str {
        "wordpress"
    }

    fn build_container(&self) -> Result<GenericImage, TestError> {
        // Use wordpress:cli with minimal setup (no MySQL required for basic HTML serving)
        // Alternative: Use pre-configured image with SQLite plugin
        Ok(GenericImage::new("wordpress", "cli")
            .with_exposed_port(8080)
            .with_env_var("WORDPRESS_DEBUG", "0")
            .with_wait_for(WaitFor::message_on_stdout("WordPress Ready")))
    }

    fn container_port(&self) -> u16 {
        8080
    }

    fn standard_scenarios(&self) -> Vec<TestScenario> {
        vec![
            TestScenario::HtmlInjection,
            TestScenario::ScriptServing { modules: vec!["core", "prebid"] },
            TestScenario::AttributeRewriting,
            TestScenario::GdprSignal,
        ]
    }

    fn custom_scenarios(&self) -> Vec<CustomScenario> {
        vec![
            CustomScenario::WordPressAdminInjection,
        ]
    }

    fn custom_assertions(&self, html: &str) -> Result<(), TestError> {
        // Verify WordPress-specific HTML structure preserved
        ensure!(html.contains("wp-content"), "should preserve wp-content paths");
        Ok(())
    }
}
```

**Note**: WordPress requires additional setup for full functionality. For Phase 3, consider:
- **Option A** (Recommended): Use `wordpress:cli` image or pre-built image with SQLite plugin - simpler, faster startup
- **Option B**: Multi-container setup with MySQL - more realistic but adds complexity

### Building Container Images

For frameworks that require custom Docker images (like Next.js), the images must be built before tests run:

**Option 1: Pre-build in CI** (Recommended)
```yaml
# In .github/workflows/test.yml
- name: Build test container images
  run: |
    docker build -t test-nextjs:latest \
      crates/integration-tests/fixtures/frameworks/nextjs/
```

**Option 2: Build on-demand in tests**
```rust
// In tests/frameworks/nextjs.rs
fn build_container(&self) -> Result<GenericImage, TestError> {
    // Check if image exists, build if not
    let image_name = "test-nextjs:latest";

    // Use testcontainers ImageBuild API (if available)
    // or shell out to docker build command
    Ok(GenericImage::new(image_name, "latest")
        .with_exposed_port(3000))
}
```

**Option 3: Use docker-compose** (Future enhancement)
```rust
// Multi-service orchestration for complex setups
```

### Next.js Implementation (`tests/frameworks/nextjs.rs`)

```rust
use super::{FrontendFramework, scenarios::*};
use testcontainers::GenericImage;

pub struct NextJs;

impl FrontendFramework for NextJs {
    fn id(&self) -> &'static str {
        "nextjs"
    }

    fn build_container(&self) -> Result<GenericImage, TestError> {
        // Build from Dockerfile in fixtures/frameworks/nextjs/
        Ok(GenericImage::new("test-nextjs", "latest")
            .with_exposed_port(3000)
            .with_wait_for(WaitFor::message_on_stdout("ready")))
    }

    fn container_port(&self) -> u16 {
        3000
    }

    fn standard_scenarios(&self) -> Vec<TestScenario> {
        vec![
            TestScenario::HtmlInjection,
            TestScenario::ScriptServing { modules: vec!["core", "prebid", "lockr"] },
            TestScenario::AttributeRewriting,
            TestScenario::GdprSignal,
        ]
    }

    fn custom_scenarios(&self) -> Vec<CustomScenario> {
        vec![
            CustomScenario::NextJsRscFlight,  // RSC streaming format
            CustomScenario::NextJsServerActions,
        ]
    }

    fn custom_assertions(&self, html: &str) -> Result<(), TestError> {
        // Verify Next.js hydration markers preserved
        ensure!(html.contains("__NEXT_DATA__"), "should preserve Next.js data");
        Ok(())
    }
}
```

### Test Scenarios (`tests/frameworks/scenarios.rs`)

```rust
/// Standard test scenarios applicable to all frameworks
#[derive(Debug, Clone)]
pub enum TestScenario {
    /// Verify <script> tag injected into <head>
    HtmlInjection,

    /// Verify /static/tsjs= endpoint serves JS bundles
    ScriptServing { modules: Vec<&'static str> },

    /// Verify tsjs-* attributes rewritten correctly
    AttributeRewriting,

    /// Verify GDPR consent signals propagate
    GdprSignal,
}

/// Framework-specific custom scenarios
#[derive(Debug, Clone)]
pub enum CustomScenario {
    WordPressAdminInjection,
    NextJsRscFlight,
    NextJsServerActions,
    LeptosHydration,      // Future
    NuxtSsr,              // Future
}

impl TestScenario {
    pub fn run(&self, viceroy: &ViceroyProcess, framework: &dyn FrontendFramework) -> Result<(), TestError> {
        let framework_id = framework.id();

        match self {
            Self::HtmlInjection => {
                let resp = reqwest::blocking::get(viceroy.base_url())
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: HtmlInjection, framework: {}", framework_id
                    ))?;

                let html = resp.text()
                    .change_context(TestError::ResponseParse)
                    .attach_printable(format!("framework: {}", framework_id))?;

                assert_script_tag_present(&html, &["core"])
                    .attach_printable(format!("framework: {}", framework_id))?;

                Ok(())
            }
            Self::ScriptServing { modules } => {
                let url = format!("{}/static/tsjs={}", viceroy.base_url(), modules.join(","));
                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: ScriptServing, framework: {}, modules: {:?}",
                        framework_id, modules
                    ))?;

                ensure!(
                    resp.status().is_success(),
                    TestError::HttpRequest
                );

                let js = resp.text()
                    .change_context(TestError::ResponseParse)?;

                ensure!(
                    js.contains("window.tsjs"),
                    TestError::ScriptTagNotFound
                );

                Ok(())
            }
            Self::AttributeRewriting => {
                let resp = reqwest::blocking::get(viceroy.base_url())
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: AttributeRewriting, framework: {}", framework_id
                    ))?;

                let html = resp.text()
                    .change_context(TestError::ResponseParse)?;

                assert_attribute_rewritten(&html, "div[data-ad-unit]", "tsjs-")
                    .attach_printable(format!("framework: {}", framework_id))?;

                Ok(())
            }
            Self::GdprSignal => {
                let resp = reqwest::blocking::get(viceroy.base_url())
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: GdprSignal, framework: {}", framework_id
                    ))?;

                assert_gdpr_signal(&resp)
                    .attach_printable(format!("framework: {}", framework_id))?;

                Ok(())
            }
        }
    }
}
```

### Matrix Test Runner (`tests/integration.rs`)

```rust
mod common;
mod environments;
mod frameworks;

use environments::RUNTIME_ENVIRONMENTS;
use frameworks::FRAMEWORKS;
use common::{RuntimeConfig, wasm_binary_path};
use std::time::Duration;
use error_stack::ResultExt;

/// Test all combinations: frameworks × runtimes (matrix testing)
#[test]
fn test_all_combinations() {
    for runtime_factory in RUNTIME_ENVIRONMENTS {
        let runtime = runtime_factory();
        println!("Testing runtime: {}", runtime.id());

        for framework_factory in FRAMEWORKS {
            let framework = framework_factory();
            println!("  Testing framework: {}", framework.id());

            test_combination(runtime.as_ref(), framework.as_ref())
                .expect("combination should pass");
        }
    }
}

/// Test a specific framework × runtime combination
fn test_combination(
    runtime: &dyn RuntimeEnvironment,
    framework: &dyn FrontendFramework,
) -> Result<(), TestError> {
    // 1. Start frontend container
    let container = framework.build_container()
        .attach_printable(format!(
            "runtime: {}, framework: {}",
            runtime.id(),
            framework.id()
        ))?;

    let container_instance = tokio::time::timeout(
        Duration::from_secs(60),
        async { container.start() }
    )
        .await
        .change_context(TestError::ContainerTimeout)?
        .change_context(TestError::ContainerStart {
            reason: format!("framework: {}", framework.id())
        })?;

    let origin_url = format!(
        "http://127.0.0.1:{}",
        container_instance.get_host_port(framework.container_port())
    );

    // 2. Generate platform-specific config
    let wasm_path = wasm_binary_path();
    let config = RuntimeConfig::new(runtime.config_template())
        .with_origin_url(origin_url)
        .with_integrations(vec!["prebid", "lockr"])
        .with_wasm_path(wasm_path)
        .build()
        .attach_printable(format!(
            "runtime: {}, framework: {}",
            runtime.id(),
            framework.id()
        ))?;

    // 3. Spawn runtime process
    let process = runtime.spawn(&config)
        .attach_printable(format!(
            "runtime: {}, framework: {}",
            runtime.id(),
            framework.id()
        ))?;

    // 4. Run standard scenarios
    for scenario in framework.standard_scenarios() {
        scenario.run(&process.base_url, framework)
            .attach_printable(format!(
                "runtime: {}, framework: {}, scenario: {:?}",
                runtime.id(),
                framework.id(),
                scenario
            ))?;
    }

    // 5. Run custom scenarios
    for scenario in framework.custom_scenarios() {
        scenario.run(&process.base_url, framework)
            .attach_printable(format!(
                "runtime: {}, framework: {}, custom scenario: {:?}",
                runtime.id(),
                framework.id(),
                scenario
            ))?;
    }

    // 6. Cleanup (automatic via Drop)
    Ok(())
}

// Support running specific combinations for faster iteration
#[test]
fn test_wordpress_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework)
        .expect("WordPress on Fastly should work");
}

#[test]
fn test_nextjs_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework)
        .expect("Next.js on Fastly should work");
}

// Future: Add Cloudflare-specific tests when platform support is added
// #[test]
// fn test_wordpress_cloudflare() { ... }
// #[test]
// fn test_nextjs_cloudflare() { ... }
```

## Extensibility Guide

### Adding a New Frontend Framework

**Example workflow** (implementation details in code comments):

**Step 1**: Create fixture directory
```bash
mkdir -p crates/integration-tests/fixtures/frameworks/<name>
# Add Dockerfile, test application code, etc.
```

**Step 2**: Implement `FrontendFramework` trait
```rust
// tests/frameworks/<name>.rs
use super::{FrontendFramework, scenarios::*};

pub struct YourFramework;

impl FrontendFramework for YourFramework {
    fn id(&self) -> &'static str { "<name>" }
    fn build_container(&self) -> Result<GenericImage, TestError> { /* ... */ }
    fn container_port(&self) -> u16 { /* port */ }
    fn standard_scenarios(&self) -> Vec<TestScenario> { /* ... */ }
    // Optional: custom_scenarios, custom_assertions
}
```

**Step 3**: Register in framework registry
```rust
// tests/frameworks/mod.rs
mod <name>;

pub static FRAMEWORKS: &[FrameworkFactory] = &[
    || Box::new(wordpress::WordPress),
    || Box::new(nextjs::NextJs),
    || Box::new(<name>::YourFramework),  // <- Add here
];
```

**Done!** The new framework is automatically tested against ALL registered runtimes.

### Adding a New Runtime Environment

**Example workflow** (implementation details in code comments):

**Step 1**: Create platform config template
```bash
# fixtures/configs/<platform>-template.toml
# Platform-specific configuration format
```

**Step 2**: Implement `RuntimeEnvironment` trait
```rust
// tests/environments/<platform>.rs
use super::*;

pub struct YourRuntime;

impl RuntimeEnvironment for YourRuntime {
    fn id(&self) -> &'static str { "<platform>" }
    fn spawn(&self, config: &RuntimeConfig) -> Result<RuntimeProcess, TestError> {
        // Platform-specific process spawning
        /* ... */
    }
    fn config_template(&self) -> &str {
        include_str!("../../fixtures/configs/<platform>-template.toml")
    }
    // Optional: health_check_path, env_vars
}

struct YourRuntimeHandle {
    child: Child,
}

impl RuntimeProcessHandle for YourRuntimeHandle {
    fn kill(&mut self) -> Result<(), TestError> { /* ... */ }
    fn wait(&mut self) -> Result<(), TestError> { /* ... */ }
}
```

**Step 3**: Register in runtime registry
```rust
// tests/environments/mod.rs
mod <platform>;

pub static RUNTIME_ENVIRONMENTS: &[RuntimeFactory] = &[
    || Box::new(fastly::FastlyViceroy),
    || Box::new(cloudflare::CloudflareWrangler),
    || Box::new(<platform>::YourRuntime),  // <- Add here
];
```

**Done!** The new runtime is automatically tested against ALL registered frameworks.

## Implementation Phases (Multi-Platform)

### Phase 0: Prerequisites (BLOCKER)
**Goal**: Add `/health` endpoint to enable runtime readiness checks

1. Add health check route to `crates/fastly/src/main.rs`:
```rust
(Method::GET, "/health") => Ok(Response::from_status(200).with_body_text_plain("ok"))
```
2. Build and test WASM binary with Viceroy
3. Verify `/health` returns 200 OK

**Verification**: `curl http://127.0.0.1:7676/health` returns "ok"

### Phase 1: Core Infrastructure (Foundation)
**Goal**: Set up test crate structure with dual abstraction support

1. Add `integration-tests` to workspace members in root `Cargo.toml`
2. Create `crates/integration-tests/Cargo.toml` with dependencies
3. Define `RuntimeEnvironment` trait in `tests/common/runtime.rs` (NEW)
4. Implement `RuntimeConfig` with platform-agnostic interface
5. Create platform config templates in `fixtures/configs/`
6. Implement assertion helpers in `tests/common/assertions.rs`

**Verification**: `cargo check -p integration-tests`

### Phase 1.5: Fastly Runtime Implementation
**Goal**: Implement Fastly/Viceroy runtime support

1. Create `tests/environments/fastly.rs`
2. Implement `FastlyViceroy` struct with `RuntimeEnvironment` trait
3. Create `ViceroyHandle` for process lifecycle
4. Implement health check with `/health` endpoint
5. Create `fixtures/configs/fastly-template.toml`

**Verification**: `cargo test -p integration-tests --lib test_fastly_runtime_spawn`

### Phase 2: Framework Abstraction (Extensibility Layer)
**Goal**: Create trait-based framework system

1. Define `FrontendFramework` trait in `tests/frameworks/mod.rs`
2. Define `TestScenario` enum and runners in `tests/frameworks/scenarios.rs`
3. Implement scenario execution logic
4. Create framework registry pattern

**Verification**: Trait compiles and registry pattern works

### Phase 3: WordPress Implementation
**Goal**: Implement first framework and test against Fastly

1. Create WordPress Dockerfile with SQLite plugin
2. Create minimal test theme
3. Implement `WordPress` struct in `tests/frameworks/wordpress.rs`
4. Create matrix test runner in `tests/integration.rs`
5. Test WordPress against Fastly

**Verification**:
```bash
cargo test -p integration-tests -- wordpress_fastly
```

### Phase 4: Next.js Implementation
**Goal**: Prove extensibility works for frameworks

1. Create Next.js 14 minimal app
2. Create Dockerfile
3. Implement `NextJs` struct in `tests/frameworks/nextjs.rs`
4. Add RSC-specific scenarios
5. Test against Fastly

**Verification**:
```bash
cargo test -p integration-tests -- test_all_combinations
cargo test -p integration-tests -- nextjs_fastly
```

### Phase 5: Documentation and CI (Production Ready)
**Goal**: Production-ready CI pipeline and documentation

1. Write comprehensive `README.md` with:
   - How to add new frameworks
   - How to add new runtimes (for future)
   - Troubleshooting guide
2. Create `.github/workflows/integration-tests.yml`:
   - Build WASM binary first
   - Install Viceroy
   - Build container images (WordPress, Next.js)
   - Run integration tests with `WASM_BINARY_PATH` env var
3. Add `.dockerignore` for build optimization

**Verification**: CI passes with both frameworks against Fastly

## Critical Files to Create

### 1. `crates/integration-tests/Cargo.toml`

```toml
[package]
name = "integration-tests"
version = "0.1.0"
edition = "2024"
publish = false

[dev-dependencies]
testcontainers = "0.25"
reqwest = { version = "0.12", features = ["blocking"] }
scraper = "0.21"
tempfile = "3.0"
toml = "0.8"
serde = { version = "1.0", features = ["derive"] }
log = "0.4"
error-stack = "0.5"
tokio = { version = "1.0", features = ["time"] }  # For container timeouts
# serial_test = "3.0"  # Optional: for test isolation (Phase 6)
```

### 2. `tests/common/viceroy.rs` - Process Manager

```rust
use std::process::{Child, Command};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::env;
use error_stack::{Result, Report, ResultExt};

pub struct ViceroyProcess {
    child: Child,
    address: String,
}

impl ViceroyProcess {
    pub fn spawn(config_path: &Path, wasm_path: &Path) -> Result<Self, TestError> {
        // Verify WASM binary exists
        ensure!(
            wasm_path.exists(),
            TestError::WasmBinaryNotFound
        );

        // 1. Spawn: viceroy run -C config_path -- wasm_path
        let mut child = Command::new("viceroy")
            .arg("run")
            .arg("-C")
            .arg(config_path)
            .arg("--")
            .arg(wasm_path)
            .spawn()
            .change_context(TestError::ViceroySpawn)?;

        // 2. Wait for Viceroy to be ready on 127.0.0.1:7676
        let address = "http://127.0.0.1:7676".to_string();
        Self::wait_for_ready(&address)?;

        Ok(Self { child, address })
    }

    fn wait_for_ready(address: &str) -> Result<(), TestError> {
        // Try /health endpoint first (requires WASM binary to implement it)
        // Fall back to root path if /health doesn't exist
        for attempt in 0..30 {
            if let Ok(resp) = reqwest::blocking::get(format!("{}/health", address)) {
                if resp.status().is_success() {
                    return Ok(());
                }
            }

            // Fallback: try root path
            if let Ok(resp) = reqwest::blocking::get(address) {
                if resp.status().is_success() || resp.status().as_u16() == 404 {
                    // 404 is fine - means server is responding
                    return Ok(());
                }
            }

            std::thread::sleep(Duration::from_millis(100));
        }
        bail!(TestError::ViceroyNotReady)
    }

    pub fn base_url(&self) -> &str {
        &self.address
    }
}

impl Drop for ViceroyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Get path to WASM binary, respecting environment variable
fn wasm_binary_path() -> PathBuf {
    env::var("WASM_BINARY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/wasm32-wasip1/release/trusted-server-fastly.wasm")
        })
}
```

**IMPORTANT**: For production use, add a `/health` endpoint to the WASM binary:

```rust
// In crates/fastly/src/main.rs
fn route_request(req: Request) -> Result<Response, Report<TrustedServerError>> {
    match req.get_path() {
        "/health" => Ok(Response::from_status(200)
            .with_body_text_plain("ok")),
        // ... existing routes
    }
}
```

### 3. `tests/common/config.rs` - Dynamic Config

```rust
use tempfile::NamedTempFile;
use std::io::Write;
use error_stack::{Result, ResultExt};

pub struct TestConfig {
    origin_url: String,
    integrations: Vec<String>,
}

impl TestConfig {
    pub fn new(container_url: String) -> Self {
        Self {
            origin_url: container_url,
            integrations: vec![],
        }
    }

    pub fn with_integrations(mut self, ids: Vec<String>) -> Self {
        self.integrations = ids;
        self
    }

    pub fn write_to_file(&self) -> Result<NamedTempFile, TestError> {
        // Load template
        let template = include_str!("../../fixtures/base-config.toml");
        let mut config: toml::Value = toml::from_str(template)
            .change_context(TestError::ConfigParse)?;

        // Safe mutation: override publisher.origin_url
        if let Some(publisher) = config.get_mut("publisher") {
            if let Some(publisher_table) = publisher.as_table_mut() {
                publisher_table.insert(
                    "origin_url".to_string(),
                    toml::Value::String(self.origin_url.clone())
                );
            }
        } else {
            return Err(TestError::ConfigParse)
                .attach_printable("missing [publisher] section in base config");
        }

        // Safe mutation: enable integrations
        let integrations_config = config
            .get_mut("integrations")
            .and_then(|v| v.get_mut("config"))
            .and_then(|v| v.as_table_mut())
            .ok_or(TestError::ConfigParse)
            .attach_printable("missing [integrations.config] section")?;

        for integration_id in &self.integrations {
            if let Some(integration_entry) = integrations_config.get_mut(integration_id) {
                // Integration exists in template - update it
                if let Some(table) = integration_entry.as_table_mut() {
                    table.insert("enabled".to_string(), toml::Value::Boolean(true));
                }
            } else {
                // Integration doesn't exist - create new entry
                let mut new_table = toml::map::Map::new();
                new_table.insert("enabled".to_string(), toml::Value::Boolean(true));
                integrations_config.insert(
                    integration_id.clone(),
                    toml::Value::Table(new_table)
                );
            }
        }

        // Write to temp file
        let mut file = NamedTempFile::new()
            .change_context(TestError::ConfigWrite)?;
        let content = toml::to_string_pretty(&config)
            .change_context(TestError::ConfigSerialize)?;
        file.write_all(content.as_bytes())
            .change_context(TestError::ConfigWrite)?;

        Ok(file)
    }
}
```

### 4. `tests/common/assertions.rs` - Shared Helpers

```rust
use scraper::{Html, Selector};
use error_stack::{Result, Report};

pub fn assert_script_tag_present(html: &str, module_ids: &[&str]) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("script[src]").expect("valid selector");

    for element in document.select(&selector) {
        if let Some(src) = element.value().attr("src") {
            if src.starts_with("/static/tsjs=") {
                for module_id in module_ids {
                    ensure!(
                        src.contains(module_id),
                        TestError::MissingModule { module: module_id.to_string() }
                    );
                }
                return Ok(());
            }
        }
    }

    bail!(TestError::ScriptTagNotFound)
}

pub fn assert_attribute_rewritten(html: &str, selector: &str, attr_prefix: &str) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let sel = Selector::parse(selector).change_context(TestError::InvalidSelector)?;

    let element = document
        .select(&sel)
        .next()
        .ok_or(TestError::ElementNotFound)?;

    let has_tsjs_attr = element
        .value()
        .attrs()
        .any(|(name, _)| name.starts_with(attr_prefix));

    ensure!(has_tsjs_attr, TestError::AttributeNotRewritten);
    Ok(())
}

pub fn assert_gdpr_signal(response: &reqwest::blocking::Response) -> Result<(), TestError> {
    let cookies = response.cookies().collect::<Vec<_>>();
    ensure!(
        cookies.iter().any(|c| c.name() == "tsjs_gdpr_consent"),
        TestError::GdprSignalMissing
    );
    Ok(())
}
```

### 5. `crates/integration-tests/README.md`

```markdown
# Integration Tests

End-to-end integration tests for Trusted Server with multiple frontend frameworks.

## Prerequisites

- Docker installed and running
- Rust 1.91+ with wasm32-wasip1 target
- Viceroy installed: `cargo install viceroy`

## Running Tests

```bash
# Build WASM binary first
cargo build --bin trusted-server-fastly --release --target wasm32-wasip1

# Build container images (if needed)
docker build -t test-nextjs:latest \
  crates/integration-tests/fixtures/frameworks/nextjs/

# Run all framework tests (sequential execution required due to shared port)
cargo test -p integration-tests -- --test-threads=1

# Run specific framework
cargo test -p integration-tests --test integration -- wordpress --test-threads=1
cargo test -p integration-tests --test integration -- nextjs --test-threads=1

# With debug logging
RUST_LOG=debug cargo test -p integration-tests -- --test-threads=1 --nocapture
```

**Note**: Tests must run sequentially (`--test-threads=1`) because they share Viceroy port 7676. Future enhancement will add dynamic port allocation for parallel execution.

## Adding a New Framework

See INTEGRATION_TESTS_PLAN.md section "How to Add a New Framework" for step-by-step guide.

Quick checklist:
1. Create `fixtures/frameworks/<name>/` with Dockerfile and test app
2. Implement `FrontendFramework` trait in `tests/frameworks/<name>.rs`
3. Register in `FRAMEWORKS` array in `tests/frameworks/mod.rs`
4. Run tests!
```

## Fixtures

### `fixtures/base-config.toml`

```toml
[publisher]
id = "test-publisher"
name = "Integration Test Publisher"
origin_url = "PLACEHOLDER"
public_domain_url = "http://127.0.0.1:7676"

[integrations.config]
prebid = { enabled = false }
lockr = { enabled = false }
permutive = { enabled = false }
datadome = { enabled = false }

[storage]
kv_store_name = "test_store"

[gdpr]
enabled = true
```

### `fixtures/frameworks/nextjs/package.json`

```json
{
  "name": "integration-test-nextjs",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "dev": "next dev",
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "^14.0.0",
    "react": "^18.2.0",
    "react-dom": "^18.2.0"
  }
}
```

### `fixtures/frameworks/nextjs/app/page.tsx`

```tsx
export default function Home() {
  return (
    <div>
      <h1>Integration Test Page</h1>
      <div id="ad-slot-1" data-ad-unit="/test/banner"></div>
      <div id="ad-slot-2" data-ad-unit="/test/sidebar"></div>
    </div>
  );
}
```

## Verification

### Local Testing
```bash
# Full suite
cargo test -p integration-tests

# Single framework (faster iteration)
cargo test -p integration-tests --test integration -- wordpress --nocapture

# With logs
RUST_LOG=debug cargo test -p integration-tests
```

### CI Testing

Example workflow job:
```yaml
integration-tests:
  runs-on: ubuntu-latest
  needs: test  # Run after unit tests pass
  steps:
    - uses: actions/checkout@v4

    - name: Install Rust
      uses: dtolnay/rust-toolchain@stable
      with:
        targets: wasm32-wasip1

    - name: Build WASM binary
      run: |
        cargo build --bin trusted-server-fastly --release --target wasm32-wasip1

    - name: Build WordPress test container
      run: |
        docker build -t test-wordpress:latest \
          crates/integration-tests/fixtures/frameworks/wordpress/

    - name: Build Next.js test container
      run: |
        docker build -t test-nextjs:latest \
          crates/integration-tests/fixtures/frameworks/nextjs/

    - name: Install Viceroy
      run: cargo install viceroy --locked

    - name: Run integration tests
      run: cargo test -p integration-tests
      env:
        WASM_BINARY_PATH: target/wasm32-wasip1/release/trusted-server-fastly.wasm
```

- Builds WASM binary as prerequisite
- Pre-builds Docker images for both frameworks
- Installs Viceroy for Fastly runtime
- Runs all framework tests against Fastly
- Fails if any framework test fails

### Success Criteria
- [ ] WordPress tests pass against Fastly (4 standard + 1 custom scenario)
- [ ] Next.js tests pass against Fastly (4 standard + 2 custom scenarios)
- [ ] CI completes in < 5 minutes
- [ ] Containers cleanup automatically (no orphans)
- [ ] Tests run in parallel (dynamic port allocation works)
- [ ] Zero flaky tests (20 consecutive runs pass)
- [ ] README instructions work for new contributor
- [ ] RuntimeEnvironment architecture supports future platforms without refactoring

## Future Framework Support

Planned frameworks to add after initial implementation:

- **Leptos** - Rust frontend framework (SSR + hydration)
- **Nuxt** - Vue.js framework (SSR, streaming)
- **SvelteKit** - Svelte framework (SSR, islands)
- **Qwik** - Resumability-focused framework
- **Astro** - Static site generator with islands

Each should require only:
- Fixture creation (~1 hour)
- Trait implementation (~50 lines)
- Registry addition (~1 line)

## Dependencies

**New Workspace Dependencies:**
- `testcontainers = "0.25"` - Docker container orchestration
- `scraper = "0.21"` - HTML parsing for assertions
- `tempfile = "3.0"` - Temporary config files

**Existing Dependencies (reused):**
- `reqwest` - HTTP client
- `toml` - Config parsing
- `serde` - Serialization
- `error-stack` - Error handling

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Docker not available in CI | High | ubuntu-latest has Docker pre-installed, verify early |
| Port 7676 conflicts | Medium | Add port availability check with retry logic |
| Slow container startup | Medium | Health check polling with 30s timeout, fail fast |
| WASM binary not built | High | Explicit build step in CI before tests, clear error message |
| Framework-specific bugs | Low | Trait abstraction isolates issues to specific impl |
| Flaky tests | High | Proper health checks, deterministic scenarios, retry logic |

## Files to Modify

- `Cargo.toml` (workspace root) - Add `integration-tests` to members
- `.github/workflows/test.yml` - Add integration test job with WASM build and container image builds (or create new `integration-tests.yml`)
- `crates/fastly/src/main.rs` - Add `/health` endpoint for Viceroy readiness checks

## Files to Create (Total: 21 files)

### Core Infrastructure (7 files)
- `crates/integration-tests/Cargo.toml`
- `crates/integration-tests/README.md`
- `crates/integration-tests/tests/common/mod.rs`
- `crates/integration-tests/tests/common/runtime.rs` (RuntimeEnvironment trait)
- `crates/integration-tests/tests/common/config.rs`
- `crates/integration-tests/tests/common/assertions.rs`
- `crates/fastly/src/main.rs` (MODIFY - add `/health` endpoint)

### Environment Abstraction (2 files)
- `crates/integration-tests/tests/environments/mod.rs` (RuntimeEnvironment registry)
- `crates/integration-tests/tests/environments/fastly.rs` (FastlyViceroy implementation)

### Framework System (4 files)
- `crates/integration-tests/tests/frameworks/mod.rs`
- `crates/integration-tests/tests/frameworks/scenarios.rs`
- `crates/integration-tests/tests/frameworks/wordpress.rs`
- `crates/integration-tests/tests/frameworks/nextjs.rs`

### Test Runner (1 file)
- `crates/integration-tests/tests/integration.rs` (Matrix test runner)

### Fixtures (7 files)
- `crates/integration-tests/fixtures/configs/fastly-template.toml`
- `crates/integration-tests/fixtures/frameworks/wordpress/Dockerfile`
- `crates/integration-tests/fixtures/frameworks/wordpress/theme.zip`
- `crates/integration-tests/fixtures/frameworks/nextjs/Dockerfile`
- `crates/integration-tests/fixtures/frameworks/nextjs/package.json`
- `crates/integration-tests/fixtures/frameworks/nextjs/next.config.mjs`
- `crates/integration-tests/fixtures/frameworks/nextjs/app/page.tsx`

## Troubleshooting Guide

### Common Issues

**1. "WASM binary not found"**
```bash
# Solution: Build the WASM binary first
cargo build --bin trusted-server-fastly --release --target wasm32-wasip1

# Or set WASM_BINARY_PATH environment variable
export WASM_BINARY_PATH=/path/to/trusted-server-fastly.wasm
```

**2. "Container image not found: test-nextjs:latest"**
```bash
# Solution: Pre-build the Docker image
docker build -t test-nextjs:latest \
  crates/integration-tests/fixtures/frameworks/nextjs/
```

**3. "Viceroy not ready after 30s"**
- Check if port 7676 is already in use: `lsof -i :7676`
- Verify WASM binary runs: `viceroy run -C fastly.toml -- path/to/binary.wasm`
- Check Viceroy logs for errors
- Ensure `/health` endpoint exists or fallback works

**4. "Address already in use (port 7676)"**
- Kill existing Viceroy process: `pkill viceroy`
- Run tests sequentially (not in parallel)
- Future: Use dynamic port allocation

**5. "Container startup timeout"**
- Increase timeout in `test_framework()` function
- Check Docker daemon is running: `docker ps`
- Verify container image builds correctly: `docker run -it test-nextjs:latest`

**6. "Config parse error: missing [integrations.config]"**
- Verify `fixtures/base-config.toml` has correct structure
- Ensure template includes all required sections

**7. "Script tag not found in HTML"**
- Verify origin container is serving HTML correctly
- Check Viceroy proxy configuration
- Inspect response with `--nocapture` flag: `cargo test -- --nocapture`

### Debug Mode

Run tests with full logging:
```bash
RUST_LOG=debug cargo test -p integration-tests -- --nocapture
```

Inspect container logs:
```bash
docker logs <container-id>
```

Attach to running Viceroy:
```bash
# In test, add: std::thread::sleep(Duration::from_secs(300));
# Then: curl http://127.0.0.1:7676/health
```

## Implementation Notes & Best Practices

### Critical Fixes Applied

1. **Trait Object Safety**: Uses function registry (`&[FrameworkFactory]`) instead of `&[&dyn Trait]` to avoid static initialization issues with trait objects returning non-`Sized` types.

2. **Complete Error Types**: `TestError` enum defined with all variants needed for proper error handling and context propagation.

3. **Health Check Strategy**: Viceroy readiness check tries `/health` endpoint first, falls back to root path. Production should implement `/health` endpoint in WASM binary.

4. **Safe TOML Mutation**: Config generation uses safe table lookups with proper error handling instead of direct indexing that could panic.

5. **Container Image Management**: Documents pre-build strategy for custom Docker images (Next.js, Leptos) to avoid "image not found" errors.

6. **WASM Binary Path**: Uses environment variable `WASM_BINARY_PATH` with fallback to relative path for flexibility across environments.

### Error Context Strategy

All fallible operations include:
- `.change_context()` for error type conversion
- `.attach_printable()` for diagnostic context (framework ID, scenario name, etc.)
- This enables precise debugging when tests fail

### Test Isolation

**Current approach**: Tests share port 7676 (Viceroy default)
- Sequential execution required: use `#[serial]` attribute or single-threaded test runner
- Future enhancement: Dynamic port allocation per test

### Container Lifecycle

- Containers start with 60s timeout to prevent indefinite hangs
- Health checks poll for 3 seconds before giving up
- `Drop` implementations ensure cleanup even on panic
- Verify cleanup in CI logs (no orphaned containers)

## Timeline Estimate (Fastly-Only Initial Implementation)

- **Phase 0** (Health endpoint): 1-2 hours (BLOCKER)
- **Phase 1** (Core Infrastructure): 4-6 hours (RuntimeEnvironment trait for future extensibility)
- **Phase 1.5** (Fastly Runtime): 2-3 hours
- **Phase 2** (Framework Abstraction): 2-3 hours
- **Phase 3** (WordPress): 5-7 hours
- **Phase 4** (Next.js): 3-4 hours
- **Phase 5** (Docs + CI): 3-4 hours

**Total**: 20-29 hours for complete implementation with 2 frameworks × 1 runtime

**Future frameworks**: 1-2 hours each (automatically tested on Fastly, and future runtimes when added)
**Future runtimes**: 2-3 hours each (automatically tests all existing frameworks)

## Future Enhancements

### Platform Support: Cloudflare Workers (2-3 hours)

When ready to add Cloudflare Workers support:

**Step 1**: Create Cloudflare runtime implementation
```rust
// tests/environments/cloudflare.rs
use super::*;
use std::process::{Child, Command};

pub struct CloudflareWrangler;

impl RuntimeEnvironment for CloudflareWrangler {
    fn id(&self) -> &'static str { "cloudflare" }

    fn spawn(&self, config: &RuntimeConfig) -> Result<RuntimeProcess, TestError> {
        let port = find_available_port()?;

        let mut child = Command::new("wrangler")
            .arg("dev")
            .arg(&config.wasm_path)
            .arg("--port")
            .arg(port.to_string())
            .arg("--local")
            .env("WASM_BINARY_PATH", &config.wasm_path)
            .spawn()
            .change_context(TestError::RuntimeSpawn)?;

        let base_url = format!("http://127.0.0.1:{}", port);
        wait_for_ready(&base_url, self.health_check_path())?;

        Ok(RuntimeProcess {
            inner: Box::new(WranglerHandle { child }),
            base_url,
        })
    }

    fn config_template(&self) -> &str {
        include_str!("../../fixtures/configs/cloudflare-template.toml")
    }

    fn health_check_path(&self) -> &str {
        "/"  // Cloudflare may use different path
    }
}

struct WranglerHandle {
    child: Child,
}

impl RuntimeProcessHandle for WranglerHandle {
    fn kill(&mut self) -> Result<(), TestError> {
        self.child.kill().change_context(TestError::RuntimeKill)?;
        Ok(())
    }

    fn wait(&mut self) -> Result<(), TestError> {
        self.child.wait().change_context(TestError::RuntimeWait)?;
        Ok(())
    }
}
```

**Step 2**: Create config template
```toml
# fixtures/configs/cloudflare-template.toml
name = "trusted-server-test"
main = "{{WASM_PATH}}"
compatibility_date = "2024-01-01"

[vars]
PUBLISHER_ORIGIN_URL = "{{ORIGIN_URL}}"
```

**Step 3**: Register in runtime registry
```rust
// tests/environments/mod.rs
mod cloudflare;

pub static RUNTIME_ENVIRONMENTS: &[RuntimeFactory] = &[
    || Box::new(fastly::FastlyViceroy),
    || Box::new(cloudflare::CloudflareWrangler),  // <- Add here
];
```

**Step 4**: Add test combinations
```rust
// tests/integration.rs
#[test]
fn test_wordpress_cloudflare() {
    let runtime = environments::cloudflare::CloudflareWrangler;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework)
        .expect("WordPress on Cloudflare should work");
}

#[test]
fn test_nextjs_cloudflare() {
    let runtime = environments::cloudflare::CloudflareWrangler;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework)
        .expect("Next.js on Cloudflare should work");
}
```

**Done!** All existing frameworks (WordPress, Next.js) now automatically test against both Fastly AND Cloudflare.

**Benefits**:
- Validate migration from Fastly → Cloudflare before production
- Catch platform-specific bugs early
- Compare performance across platforms
- Zero changes to framework implementations

---

### Phase 6: Advanced Features (Post-MVP)

**1. Dynamic Port Allocation**
- Eliminate shared port 7676 constraint
- Enable parallel test execution
- Use `TcpListener::bind("127.0.0.1:0")` to find available ports
- Pass port to Viceroy via command-line argument (if supported)

**2. Test Isolation with `serial_test` Crate**
```rust
use serial_test::serial;

#[test]
#[serial]
fn test_wordpress_only() { ... }
```

**3. Stress Testing**
```rust
#[test]
#[ignore]  // Run with --ignored flag
fn test_frameworks_stress() {
    for _ in 0..10 {
        test_all_frameworks();
    }
}
```

**4. Cleanup Verification Tests**
```rust
#[test]
fn verify_no_orphaned_containers() {
    let before = count_docker_containers();
    test_all_frameworks();
    let after = count_docker_containers();
    assert_eq!(before, after, "containers not cleaned up");
}
```

**5. Multi-Container Orchestration**
- Use docker-compose for complex setups (e.g., WordPress + MySQL)
- Support testing against databases, message queues, etc.

**6. Framework Discovery via `inventory` Crate**
```rust
use inventory;

inventory::collect!(FrameworkRegistration);

// Frameworks self-register, no manual registry updates needed
```

**7. Scenario Builder Pattern**
```rust
ScriptServingScenario::builder()
    .modules(vec!["core", "prebid"])
    .expect_globals(vec!["window.tsjs"])
    .build()
```

**8. Performance Benchmarking**
- Track test execution time per framework
- Alert on regressions (e.g., >20% slower)
- CI job outputs performance metrics

**9. Matrix Testing in CI**
```yaml
strategy:
  matrix:
    framework: [wordpress, nextjs, leptos]
steps:
  - run: cargo test -p integration-tests -- ${{ matrix.framework }}
```

**10. Remote Debugging Support**
- Keep Viceroy running after test failure
- Expose debug endpoint for manual inspection
- Capture network traffic for analysis
