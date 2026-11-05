//! Loopback TCP socket probe and the listener that observes it.
//!
//! The probe makes exactly one TCP `connect` to a loopback `IP:PORT` and
//! reports either `connected` or `denied errno=<n>` with the operating
//! system's real errno. It never synthesizes an errno: a failure without one
//! is an error, not a report. `socket()` success is not evidence (macOS
//! allows `socket(AF_INET)` under `deny default`), so only `connect` against a
//! listener the unconfined control actually reached counts.

use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use error_stack::Report;

use crate::error::ReleaseError;

/// Exit status of a probe whose connect was refused by the operating system.
pub(crate) const DENIED_STATUS: u8 = 3;
/// `boundary-probe` subcommand arguments preceding the address.
const RELEASE_PROBE_ARGUMENTS: [&str; 2] = ["boundary-probe", "connect"];

/// The single-line result of one probe connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Display)]
pub(crate) enum ProbeReport {
    /// The TCP handshake completed.
    #[display("connected")]
    Connected,
    /// `connect` failed with the operating system's `errno`.
    #[display("denied errno={errno}")]
    Denied { errno: i32 },
}

impl ProbeReport {
    /// Parses exactly one report line terminated by a newline.
    #[must_use]
    pub(crate) fn parse_line(output: &[u8]) -> Option<Self> {
        let line = std::str::from_utf8(output.strip_suffix(b"\n")?).ok()?;
        if line == "connected" {
            return Some(Self::Connected);
        }
        let digits = line.strip_prefix("denied errno=")?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok().map(|errno| Self::Denied { errno })
    }
}

fn boundary_error(message: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::Boundary).attach(message.to_string())
}

/// Parses a literal loopback `IP:PORT` with a nonzero port.
///
/// # Errors
///
/// Returns [`ReleaseError::Usage`] for anything other than a literal IPv4 or
/// IPv6 loopback socket address with a nonzero port; host names are never
/// resolved.
pub(crate) fn parse_loopback(address: &str) -> Result<SocketAddr, Report<ReleaseError>> {
    let parsed: SocketAddr = address.parse().map_err(|_| {
        Report::new(ReleaseError::Usage).attach("probe address is not a literal IP:PORT")
    })?;
    if !parsed.ip().is_loopback() || parsed.port() == 0 {
        return Err(Report::new(ReleaseError::Usage)
            .attach("probe address must be loopback with a nonzero port"));
    }
    Ok(parsed)
}

/// Attempts one TCP connect and reports the real outcome.
///
/// # Errors
///
/// Returns [`ReleaseError::Boundary`] when `connect` fails without an
/// operating-system errno; no errno is invented for it.
pub(crate) fn connect_probe(address: SocketAddr) -> Result<ProbeReport, Report<ReleaseError>> {
    match TcpStream::connect(address) {
        Ok(_) => Ok(ProbeReport::Connected),
        Err(error) => match error.raw_os_error() {
            Some(errno) => Ok(ProbeReport::Denied { errno }),
            None => Err(boundary_error(format!(
                "probe connect failed without an errno: {error}"
            ))),
        },
    }
}

/// How a probe program frames its report on stdout.
#[derive(Clone, Copy, Debug)]
enum ProbeOutput {
    /// Stdout is exactly one report line.
    Exact,
    /// Stdout carries a harness preamble, then the sentinel, then the line.
    #[cfg(test)]
    AfterSentinel(&'static [u8]),
}

/// An executable that speaks the probe protocol: the final argument is the
/// loopback address, stdout is the report, exit `0` means connected and
/// [`DENIED_STATUS`] means denied.
#[derive(Clone, Debug)]
pub(crate) struct ProbeProgram {
    program: PathBuf,
    prefix: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    output: ProbeOutput,
}

impl ProbeProgram {
    /// Selects an ordinary CI copy of the source-built producer's fixed probe.
    ///
    /// # Errors
    ///
    /// Returns an error when the staged executable cannot be resolved. This
    /// test-only selector supplies no production tool authentication.
    #[cfg(test)]
    pub(crate) fn from_staged_release_binary(path: &Path) -> Result<Self, Report<ReleaseError>> {
        let program =
            std::fs::canonicalize(path).map_err(|error| boundary_error(error.to_string()))?;
        Ok(Self {
            program,
            prefix: RELEASE_PROBE_ARGUMENTS.iter().map(OsString::from).collect(),
            environment: Vec::new(),
            output: ProbeOutput::Exact,
        })
    }
    /// Selects this release binary's documented `boundary-probe connect`
    /// subcommand.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when the running executable cannot
    /// be located or canonicalized.
    pub(crate) fn release_binary() -> Result<Self, Report<ReleaseError>> {
        let program = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|error| boundary_error(format!("cannot locate release binary: {error}")))?;
        Ok(Self {
            program,
            prefix: RELEASE_PROBE_ARGUMENTS.iter().map(OsString::from).collect(),
            environment: Vec::new(),
            output: ProbeOutput::Exact,
        })
    }

    /// Selects a test-binary helper speaking the same protocol after a
    /// harness preamble ending in `sentinel`.
    #[cfg(test)]
    pub(super) fn test_helper(
        program: PathBuf,
        prefix: Vec<OsString>,
        environment: Vec<(OsString, OsString)>,
        sentinel: &'static [u8],
    ) -> Self {
        Self {
            program,
            prefix,
            environment,
            output: ProbeOutput::AfterSentinel(sentinel),
        }
    }

    /// Copies the executable into `directory` (a tool root) as a private
    /// single-link, read-only file and returns the probe pointing at it.
    ///
    /// Cargo hard-links its build outputs, and tool roots require
    /// `nlink == 1`, so a probe is never run from `target/` directly.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when the copy cannot be created
    /// (including when the destination already exists) or made read-only.
    pub(crate) fn staged(&self, directory: &Path) -> Result<Self, Report<ReleaseError>> {
        let name = self
            .program
            .file_name()
            .ok_or_else(|| boundary_error("probe program has no file name"))?;
        let directory = std::fs::canonicalize(directory)
            .map_err(|error| boundary_error(format!("cannot resolve tool directory: {error}")))?;
        let destination = directory.join(name);
        if destination.exists() {
            return Err(boundary_error("staged probe destination already exists"));
        }
        std::fs::copy(&self.program, &destination)
            .and_then(|_| {
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o555))
            })
            .map_err(|error| boundary_error(format!("cannot stage probe: {error}")))?;
        Ok(Self {
            program: destination,
            ..self.clone()
        })
    }

    /// Returns the probe executable path.
    pub(super) fn program(&self) -> &Path {
        &self.program
    }

    /// Returns the fixed environment the probe needs.
    pub(super) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    /// Returns the argument vector targeting `address`.
    pub(super) fn arguments(&self, address: SocketAddr) -> Vec<OsString> {
        let mut arguments = self.prefix.clone();
        arguments.push(OsString::from(address.to_string()));
        arguments
    }

    /// Parses the probe's stdout under its framing.
    pub(super) fn parse(&self, stdout: &[u8]) -> Option<ProbeReport> {
        match self.output {
            ProbeOutput::Exact => ProbeReport::parse_line(stdout),
            #[cfg(test)]
            ProbeOutput::AfterSentinel(sentinel) => {
                let start = stdout
                    .windows(sentinel.len())
                    .rposition(|window| window == sentinel)?;
                ProbeReport::parse_line(stdout.get(start + sentinel.len()..)?)
            }
        }
    }
}

/// A non-blocking IPv4 loopback listener that counts completed connections.
pub(crate) struct LoopbackListener {
    listener: TcpListener,
    address: SocketAddr,
}

impl LoopbackListener {
    /// Binds an ephemeral port on `127.0.0.1`.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when the listener cannot be bound or
    /// made non-blocking.
    pub(crate) fn bind() -> Result<Self, Report<ReleaseError>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .and_then(|listener| {
                listener.set_nonblocking(true)?;
                Ok(listener)
            })
            .map_err(|error| boundary_error(format!("cannot bind control listener: {error}")))?;
        let address = listener
            .local_addr()
            .map_err(|error| boundary_error(format!("cannot read listener address: {error}")))?;
        Ok(Self { listener, address })
    }

    /// Returns the bound loopback address.
    #[must_use]
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Accepts every queued connection and returns how many there were.
    ///
    /// A probe's handshake completes before it exits, so after its process
    /// is reaped any connection it made is already queued.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::Boundary`] when `accept` fails with anything
    /// other than would-block or interruption.
    pub(crate) fn drain_accepts(&self) -> Result<usize, Report<ReleaseError>> {
        let mut accepted = 0;
        loop {
            match self.listener.accept() {
                Ok(_) => accepted += 1,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(accepted),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(accept_error(&error)),
            }
        }
    }
}

fn accept_error(error: &io::Error) -> Report<ReleaseError> {
    boundary_error(format!(
        "control listener accept failed (errno {:?}): {error}",
        error.raw_os_error()
    ))
}
