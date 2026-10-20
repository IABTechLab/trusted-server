//! Final-byte Fastly runtime acceptance through the pinned Viceroy.
//!
//! Before a final Fastly output is accepted, the gate validates the exact
//! bytes as a core Wasm Fastly command and then serves them with the pinned
//! Viceroy against a loopback publisher origin it starts itself. A publisher
//! request must return the origin's page, carrying this run's marker, with
//! exactly one injected Trusted Server script. The gate never invokes Cargo or
//! the Fastly CLI and never rewrites the module: Viceroy reads a private
//! read-only copy, and both the copy and the retained artifact must still
//! hash to the accepted digest afterwards.
//!
//! The settings-free `/health` route only gates readiness. Any rejection after
//! a successful health check (a failed publisher request, an unobserved
//! origin, an early Viceroy exit or a guest failure in its output) carries
//! [`HealthOnlySuccess`], so that outcome stays distinguishable from a
//! runtime that never started.
//!
//! The Viceroy and origin ports are chosen by binding an ephemeral loopback
//! port and releasing it, so another local process could take a port in that
//! window; such a race fails the gate, it cannot make it pass.

mod config;
mod exchange;
mod export;
mod logs;
mod viceroy;

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::compare::{FastlyEqualityChecked, read_artifact};
use super::{RecipeCompleted, Role};
use crate::error::ReleaseError;
use crate::inspect::inspect_wasm;
use crate::process::{BoundedCommand, Cancellation, ProcessLimits, ProcessOutcome};
use crate::source::{ApprovedSource, CommitId};
pub(crate) use exchange::PublisherObservation;
pub(crate) use logs::ViceroyOutput;
#[cfg(test)]
pub(crate) use viceroy::PINNED_VICEROY_VERSION;
pub(crate) use viceroy::SelectedViceroy;

/// Wall-clock bound on the whole Viceroy run, compilation included.
const VICEROY_WALL: Duration = Duration::from_secs(15 * 60);
/// Bound on the time from spawn to a healthy runtime.
const STARTUP_DEADLINE: Duration = Duration::from_secs(10 * 60);
/// Interval between readiness probes.
const READINESS_POLL: Duration = Duration::from_millis(250);
/// Timeout of one readiness probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Timeout of the publisher request.
const PUBLISHER_TIMEOUT: Duration = Duration::from_secs(60);
/// Cap on each captured Viceroy output stream.
const VICEROY_OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;
/// Headers of a top-level browser navigation.
const NAVIGATION_HEADERS: [(&str, &str); 4] = [
    ("Accept", "text/html,application/xhtml+xml"),
    ("Sec-Fetch-Mode", "navigate"),
    ("Sec-Fetch-Dest", "document"),
    ("User-Agent", "trusted-server-release-runtime-gate"),
];

/// Typed reason attached to every [`ReleaseError::Runtime`] report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum RuntimeRejection {
    /// The handed-off artifact is not the accepted, unchanged final output.
    #[display("artifact is not the accepted final output")]
    Artifact,
    /// The Viceroy executable is unusable, unpinned or changed.
    #[display("Viceroy selection failed")]
    Viceroy,
    /// The fixed fixture or its configuration could not be generated.
    #[display("runtime fixture is unusable")]
    Fixture,
    /// Viceroy exited or never answered the health check in time.
    #[display("runtime never became healthy")]
    Startup,
    /// A loopback HTTP exchange failed or did not parse.
    #[display("HTTP exchange failed")]
    Exchange,
    /// The publisher page did not answer 200.
    #[display("publisher page status is not 200")]
    Status,
    /// The Trusted Server script injection is missing or malformed.
    #[display("Trusted Server script injection is missing or malformed")]
    Injection,
    /// The page does not carry this run's origin marker.
    #[display("publisher page did not come from this run's origin")]
    OriginMarker,
    /// The origin fixture never received the publisher request.
    #[display("origin fixture did not observe the publisher request")]
    OriginUnobserved,
    /// The executed copy changed during the run.
    #[display("executed artifact changed during the run")]
    Changed,
    /// Viceroy ended other than by the gate's cancellation.
    #[display("Viceroy did not end by the gate's cancellation")]
    Supervision,
    /// The captured Viceroy output reports a guest trap, panic or failed
    /// request execution.
    #[display("Viceroy output reports a guest failure")]
    GuestFailure,
}

/// Marks a rejection whose health check succeeded before the publisher
/// request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("health check succeeded but publisher acceptance failed")]
pub(crate) struct HealthOnlySuccess;

/// Builds a runtime rejection carrying `rejection` and a detail line.
fn runtime_error(
    rejection: RuntimeRejection,
    message: impl std::fmt::Display,
) -> Report<ReleaseError> {
    Report::new(ReleaseError::Runtime)
        .attach(rejection)
        .attach(message.to_string())
}

/// Whether the gate serves the publisher origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OriginPresence {
    /// The origin fixture serves the publisher page.
    Served,
    /// The origin port is closed, so only `/health` can succeed. Tests use
    /// it to prove that a health-only success is rejected.
    #[cfg(test)]
    Closed,
}

/// Proof that the retained final Fastly output of an approved source passed
/// core-format validation and a publisher request through the pinned
/// Viceroy.
///
/// It has private fields and only [`accept_fastly_runtime`] constructs it;
/// a bare [`run_gate`] observation is not acceptance.
#[derive(Debug)]
pub(crate) struct FastlyRuntimeAccepted {
    source_commit: CommitId,
    role: Role,
    observed: GateObservation,
}

impl FastlyRuntimeAccepted {
    /// The approved source commit whose fixtures and output were accepted.
    pub(crate) fn source_commit(&self) -> &CommitId {
        &self.source_commit
    }

    /// The build role whose final output was accepted.
    pub(crate) fn role(&self) -> Role {
        self.role
    }

    /// What the gate run observed.
    pub(crate) fn observed(&self) -> &GateObservation {
        &self.observed
    }
}

/// What one successful gate run observed over exact final bytes.
///
/// It has private fields and only [`run_gate`] constructs it.
#[derive(Debug)]
pub(crate) struct GateObservation {
    artifact_sha256: String,
    artifact_size: u64,
    viceroy_version: String,
    viceroy_sha256: String,
    fixture_sha256: String,
    config_sha256: String,
    health_status: u16,
    publisher: PublisherObservation,
    origin_requests: Vec<String>,
    startup: Duration,
    viceroy_outcome: ProcessOutcome,
    viceroy_output: ViceroyOutput,
}

impl GateObservation {
    /// Digests and sanitized stderr tail of the captured Viceroy output.
    pub(crate) fn viceroy_output(&self) -> &ViceroyOutput {
        &self.viceroy_output
    }

    /// SHA-256 of the executed final bytes, unchanged by the run.
    pub(crate) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    /// Size of the executed final bytes.
    pub(crate) fn artifact_size(&self) -> u64 {
        self.artifact_size
    }

    /// The reported Viceroy version.
    pub(crate) fn viceroy_version(&self) -> &str {
        &self.viceroy_version
    }

    /// SHA-256 of the Viceroy executable.
    pub(crate) fn viceroy_sha256(&self) -> &str {
        &self.viceroy_sha256
    }

    /// SHA-256 binding the fixed template, app config and store ID.
    pub(crate) fn fixture_sha256(&self) -> &str {
        &self.fixture_sha256
    }

    /// SHA-256 of the generated Viceroy configuration of this run.
    pub(crate) fn config_sha256(&self) -> &str {
        &self.config_sha256
    }

    /// Status of the readiness health check.
    pub(crate) fn health_status(&self) -> u16 {
        self.health_status
    }

    /// What the accepted publisher response showed.
    pub(crate) fn publisher(&self) -> &PublisherObservation {
        &self.publisher
    }

    /// Request lines the origin fixture received.
    pub(crate) fn origin_requests(&self) -> &[String] {
        &self.origin_requests
    }

    /// Time from spawn to a healthy runtime.
    pub(crate) fn startup(&self) -> Duration {
        self.startup
    }

    /// How the Viceroy run ended; always the gate's cancellation.
    pub(crate) fn viceroy_outcome(&self) -> ProcessOutcome {
        self.viceroy_outcome
    }
}

/// Runs the runtime gate over the retained final Fastly output that passed
/// the canonical and reproduction equality check.
///
/// # Errors
///
/// Returns [`ReleaseError::Runtime`] with [`RuntimeRejection::Artifact`] when
/// `completed` is not the checked output of `source` or no longer rehashes,
/// [`RuntimeRejection::Fixture`] when `source` does not track the embedded
/// fixtures unchanged, [`ReleaseError::Inspect`] when the bytes are not a
/// valid Fastly core command, and the gate's other rejections from
/// [`run_gate`].
pub(crate) fn accept_fastly_runtime(
    source: &ApprovedSource,
    completed: &RecipeCompleted,
    checked: &FastlyEqualityChecked,
    viceroy: &SelectedViceroy,
) -> Result<FastlyRuntimeAccepted, Report<ReleaseError>> {
    if source.commit() != completed.commit() {
        return Err(runtime_error(
            RuntimeRejection::Artifact,
            "artifact was not built from the supplied approved source",
        ));
    }
    config::require_built_source_fixtures(&|path| source.tracked_sha256(path).map(str::to_owned))?;
    let artifact = completed.artifact();
    if completed.asset() != checked.asset()
        || artifact.sha256() != checked.sha256()
        || artifact.size() != checked.size()
    {
        return Err(runtime_error(
            RuntimeRejection::Artifact,
            "artifact is not the output the equality check accepted",
        ));
    }
    let bytes = read_artifact(completed).map_err(|error| {
        error
            .change_context(ReleaseError::Runtime)
            .attach(RuntimeRejection::Artifact)
    })?;
    let observed = run_gate(&bytes, viceroy, OriginPresence::Served)?;
    completed.rehash_for_handoff().map_err(|error| {
        error
            .change_context(ReleaseError::Runtime)
            .attach(RuntimeRejection::Changed)
    })?;
    Ok(FastlyRuntimeAccepted {
        source_commit: completed.commit().clone(),
        role: completed.role(),
        observed,
    })
}

/// Validates `bytes` and serves them through `viceroy` against the origin.
///
/// # Errors
///
/// Returns [`ReleaseError::Inspect`] for invalid core Wasm, a foreign host
/// ABI, a missing entry ABI or stripped debug sections; otherwise
/// [`ReleaseError::Runtime`] with a [`RuntimeRejection`]. Every rejection
/// after a healthy readiness check also carries [`HealthOnlySuccess`].
pub(super) fn run_gate(
    bytes: &[u8],
    viceroy: &SelectedViceroy,
    origin: OriginPresence,
) -> Result<GateObservation, Report<ReleaseError>> {
    let facts = inspect_wasm(bytes)?;
    facts.require_fastly_entry_abi()?;
    facts.require_debug_sections()?;
    viceroy.reverify()?;
    let private = tempfile::tempdir().map_err(|error| fixture_io("private directory", &error))?;
    let wasm = private.path().join("final.wasm");
    write_private(&wasm, bytes, 0o400)?;
    require_unchanged(&wasm, facts.sha256())?;
    let marker = run_marker(facts.sha256());
    let origin_fixture = match origin {
        OriginPresence::Served => Some(exchange::OriginFixture::start(&marker)?),
        #[cfg(test)]
        OriginPresence::Closed => None,
    };
    let origin_address = match &origin_fixture {
        Some(fixture) => fixture.address(),
        None => free_loopback_address()?,
    };
    let generated = config::viceroy_config(&format!("http://{origin_address}"))?;
    let config_path = private.path().join("viceroy.toml");
    write_private(&config_path, generated.text.as_bytes(), 0o400)?;
    let home = private.path().join("home");
    fs::create_dir(&home).map_err(|error| fixture_io("home directory", &error))?;
    let address = free_loopback_address()?;
    let run = serve(&ServeInputs {
        viceroy,
        wasm: &wasm,
        config: &config_path,
        home: &home,
        working_directory: private.path(),
        address,
        marker: &marker,
    });
    let origin_requests = origin_fixture
        .map(exchange::OriginFixture::stop)
        .unwrap_or_default();
    require_unchanged(&wasm, facts.sha256())?;
    viceroy.reverify()?;
    let served = run?;
    if !origin_requests
        .iter()
        .any(|line| line.starts_with("GET / "))
    {
        return Err(runtime_error(
            RuntimeRejection::OriginUnobserved,
            format!("origin fixture received {origin_requests:?}"),
        )
        .attach(HealthOnlySuccess));
    }
    Ok(GateObservation {
        artifact_sha256: facts.sha256().to_owned(),
        artifact_size: bytes.len() as u64,
        viceroy_version: viceroy.version().to_owned(),
        viceroy_sha256: viceroy.sha256().to_owned(),
        fixture_sha256: generated.fixture_sha256,
        config_sha256: generated.sha256,
        health_status: served.health_status,
        publisher: served.publisher,
        origin_requests,
        startup: served.startup,
        viceroy_outcome: served.outcome,
        viceroy_output: served.output,
    })
}

/// Inputs of one Viceroy serving run.
struct ServeInputs<'a> {
    viceroy: &'a SelectedViceroy,
    wasm: &'a Path,
    config: &'a Path,
    home: &'a Path,
    working_directory: &'a Path,
    address: SocketAddr,
    marker: &'a str,
}

/// What one successful serving run observed.
struct Served {
    health_status: u16,
    publisher: PublisherObservation,
    startup: Duration,
    outcome: ProcessOutcome,
    output: ViceroyOutput,
}

/// Cancels the Viceroy run when dropped, so a panic while exchanging stops
/// Viceroy at once instead of waiting out [`VICEROY_WALL`] in the scope join.
struct CancelOnDrop<'a>(&'a Cancellation);

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Runs Viceroy, waits for health, checks one publisher request and stops it.
fn serve(inputs: &ServeInputs<'_>) -> Result<Served, Report<ReleaseError>> {
    let cancellation = Cancellation::default();
    let command = viceroy_command(inputs, &cancellation)?;
    let started = Instant::now();
    std::thread::scope(|scope| {
        let running = scope.spawn(|| command.run());
        let stop = CancelOnDrop(&cancellation);
        let exchanged = exchange_while_running(inputs, started, &|| running.is_finished());
        drop(stop);
        let result = running
            .join()
            .map_err(|_| {
                runtime_error(RuntimeRejection::Supervision, "Viceroy supervisor panicked")
            })?
            .map_err(|error| {
                error
                    .change_context(ReleaseError::Runtime)
                    .attach(RuntimeRejection::Supervision)
            })?;
        let (publisher, startup) = exchanged.map_err(|error| {
            error.attach(format!(
                "Viceroy {} after {:?}; stderr tail: {}",
                result.outcome(),
                result.elapsed(),
                logs::sanitized_tail(result.stderr())
            ))
        })?;
        if result.outcome() != ProcessOutcome::Cancelled {
            return Err(runtime_error(
                RuntimeRejection::Supervision,
                format!("Viceroy {} before the gate stopped it", result.outcome()),
            )
            .attach(HealthOnlySuccess));
        }
        let output = logs::scan_viceroy_output(result.stdout(), result.stderr())
            .map_err(|error| error.attach(HealthOnlySuccess))?;
        Ok(Served {
            health_status: 200,
            publisher,
            startup,
            outcome: result.outcome(),
            output,
        })
    })
}

/// Builds the fixed Viceroy command line.
fn viceroy_command(
    inputs: &ServeInputs<'_>,
    cancellation: &Cancellation,
) -> Result<BoundedCommand, Report<ReleaseError>> {
    let arguments = [
        OsString::from("serve"),
        OsString::from("--addr"),
        OsString::from(inputs.address.to_string()),
        OsString::from("--config"),
        inputs.config.as_os_str().to_owned(),
        OsString::from("--unknown-import-behavior"),
        OsString::from("link-error"),
        inputs.wasm.as_os_str().to_owned(),
    ];
    BoundedCommand::new(
        inputs.viceroy.path(),
        inputs.working_directory,
        ProcessLimits {
            wall: VICEROY_WALL,
            stdout_max: VICEROY_OUTPUT_LIMIT,
            stderr_max: VICEROY_OUTPUT_LIMIT,
        },
    )
    .and_then(|command| {
        command
            .arguments(arguments)
            .environment("HOME", inputs.home)
    })
    .map(|command| command.cancellation(cancellation.clone()))
    .map_err(|error| {
        error
            .change_context(ReleaseError::Runtime)
            .attach(RuntimeRejection::Supervision)
    })
}

/// Waits for health, then sends and checks the publisher request.
fn exchange_while_running(
    inputs: &ServeInputs<'_>,
    started: Instant,
    exited: &dyn Fn() -> bool,
) -> Result<(PublisherObservation, Duration), Report<ReleaseError>> {
    loop {
        if exited() {
            return Err(runtime_error(
                RuntimeRejection::Startup,
                "Viceroy exited before answering the health check",
            ));
        }
        if started.elapsed() > STARTUP_DEADLINE {
            return Err(runtime_error(
                RuntimeRejection::Startup,
                "Viceroy did not answer the health check before the startup deadline",
            ));
        }
        if exchange::get(inputs.address, "/health", &[], PROBE_TIMEOUT)
            .is_ok_and(|response| exchange::is_health_ok(&response))
        {
            break;
        }
        std::thread::sleep(READINESS_POLL);
    }
    let startup = started.elapsed();
    exchange::get(inputs.address, "/", &NAVIGATION_HEADERS, PUBLISHER_TIMEOUT)
        .and_then(|response| exchange::check_publisher_response(&response, inputs.marker))
        .map(|publisher| (publisher, startup))
        .map_err(|error| error.attach(HealthOnlySuccess))
}

fn fixture_io(what: &str, error: &std::io::Error) -> Report<ReleaseError> {
    runtime_error(
        RuntimeRejection::Fixture,
        format!("cannot create the {what}: {error}"),
    )
}

/// Writes a new private file with `mode` and syncs it.
fn write_private(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Report<ReleaseError>> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .map_err(|error| fixture_io("private file", &error))
}

/// Requires the file at `path` to hash to `sha256`.
fn require_unchanged(path: &Path, sha256: &str) -> Result<(), Report<ReleaseError>> {
    let bytes = fs::read(path).map_err(|error| {
        runtime_error(
            RuntimeRejection::Changed,
            format!("cannot reread the copy: {error}"),
        )
    })?;
    if hex::encode(Sha256::digest(&bytes)) != sha256 {
        return Err(runtime_error(
            RuntimeRejection::Changed,
            "executed copy no longer hashes to the accepted digest",
        ));
    }
    Ok(())
}

/// Returns a loopback address whose port was free a moment ago.
fn free_loopback_address() -> Result<SocketAddr, Report<ReleaseError>> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map_err(|error| fixture_io("loopback port", &error))
}

/// Returns a marker unique to this run, so a cached or foreign page fails.
fn run_marker(artifact_sha256: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let seed = format!("{artifact_sha256}:{nanos}:{}", std::process::id());
    let digest = hex::encode(Sha256::digest(seed.as_bytes()));
    format!("trusted-server-release-origin-{}", &digest[..24])
}

#[cfg(test)]
pub(super) mod tests_support {
    //! Narrow access to the gate's pure parts for the recipe tests.

    pub(in crate::recipe) use super::config::{
        app_config_data, canonical, config_store_id, envelope_json, require_built_source_fixtures,
        viceroy_config,
    };
    pub(in crate::recipe) use super::exchange::{
        HttpResponse, OriginFixture, check_publisher_response, get, is_health_ok, origin_page,
        parse_response,
    };
    pub(in crate::recipe) use super::logs::{sanitized_tail, scan_viceroy_output};
    pub(in crate::recipe) use super::viceroy::parse_version_output;
}
