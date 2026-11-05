//! Bounded concurrent draining of child pipes.

use std::io::{self, Read, Write as _};
use std::process::Child;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use error_stack::Report;

use super::supervise::{KILL_GRACE, POLL_INTERVAL};
use super::{ProcessFailure, ProcessLimits, Stream, process_error};
use crate::error::ReleaseError;

/// Pipe read size for the drain threads.
const READ_CHUNK: usize = 64 * 1024;

/// Bytes read from one stream before EOF or the cap.
struct Drained {
    bytes: Vec<u8>,
    overflowed: bool,
}

/// Captured output of both streams after every pipe thread finished.
#[derive(Default)]
pub(super) struct DrainedOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) overflowed: Option<Stream>,
    pub(super) stderr_truncated: bool,
}

/// Drain and stdin-writer threads attached to one child.
pub(super) struct Pipes {
    stdout: JoinHandle<io::Result<Drained>>,
    stderr: JoinHandle<io::Result<Drained>>,
    stdin: Option<JoinHandle<io::Result<()>>>,
}

impl Pipes {
    pub(super) fn start(
        child: &mut Child,
        stdin_bytes: Option<Vec<u8>>,
        limits: &ProcessLimits,
    ) -> Option<(Self, Receiver<Stream>)> {
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take()?;
        let stdin = match stdin_bytes {
            Some(bytes) => Some((child.stdin.take()?, bytes)),
            None => None,
        };
        let (sender, overflow) = mpsc::channel();
        let pipes = Self {
            stdout: drain(stdout, Stream::Stdout, limits.stdout_max, sender.clone()),
            stderr: drain(stderr, Stream::Stderr, limits.stderr_max, sender),
            stdin: stdin.map(|(mut pipe, bytes)| {
                thread::spawn(move || match pipe.write_all(&bytes) {
                    // A child that stops reading early is not a pipe failure.
                    Err(source) if source.kind() == io::ErrorKind::BrokenPipe => Ok(()),
                    other => other,
                })
            }),
        };
        Some((pipes, overflow))
    }

    fn finished(&self) -> bool {
        self.stdout.is_finished()
            && self.stderr.is_finished()
            && self.stdin.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Joins every pipe thread within [`KILL_GRACE`].
    ///
    /// Threads still blocked after the grace stay detached: they hold only a
    /// pipe end that the escaped descendant keeps open.
    pub(super) fn join(self) -> Result<DrainedOutput, Report<ReleaseError>> {
        let until = Instant::now() + KILL_GRACE;
        while !self.finished() {
            if Instant::now() >= until {
                return Err(process_error(
                    ProcessFailure::DescendantEscaped,
                    "child pipes stayed open after the process group was gone; \
                     output is rejected",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
        if let Some(stdin) = self.stdin {
            joined(stdin)?;
        }
        let stdout = joined(self.stdout)?;
        let stderr = joined(self.stderr)?;
        let overflowed = if stdout.overflowed {
            Some(Stream::Stdout)
        } else if stderr.overflowed {
            Some(Stream::Stderr)
        } else {
            None
        };
        Ok(DrainedOutput {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            overflowed,
            stderr_truncated: stderr.overflowed,
        })
    }
}

fn joined<T>(handle: JoinHandle<io::Result<T>>) -> Result<T, Report<ReleaseError>> {
    handle
        .join()
        .map_err(|_| process_error(ProcessFailure::Pipe { errno: None }, "pipe thread panicked"))?
        .map_err(|source| {
            process_error(
                ProcessFailure::Pipe {
                    errno: source.raw_os_error(),
                },
                format!("child pipe failed: {source}"),
            )
        })
}

fn drain<R: Read + Send + 'static>(
    mut reader: R,
    stream: Stream,
    limit: u64,
    overflow: Sender<Stream>,
) -> JoinHandle<io::Result<Drained>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = vec![0; READ_CHUNK];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => return Err(source),
            };
            if count == 0 {
                return Ok(Drained {
                    bytes,
                    overflowed: false,
                });
            }
            let captured = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let remaining = usize::try_from(limit.saturating_sub(captured)).unwrap_or(usize::MAX);
            if count > remaining {
                bytes.extend_from_slice(buffer.get(..remaining).unwrap_or_default());
                // Dropping the reader closes the pipe; the supervisor kills the group.
                let _ = overflow.send(stream);
                return Ok(Drained {
                    bytes,
                    overflowed: true,
                });
            }
            bytes.extend_from_slice(buffer.get(..count).unwrap_or_default());
        }
    })
}
