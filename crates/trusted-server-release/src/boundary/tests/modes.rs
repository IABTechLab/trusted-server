//! Helper child modes run by [`helper_entry`](super::helper_entry).
//!
//! Every mode prints one report line after the sentinel: a success word, or
//! `denied errno=<n>` with the operating system's real errno (exit 3).

use std::io::Write as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::process::Stdio;

use super::*;

/// Prints `line` after the sentinel and returns `code`.
fn report(line: &str, code: i32) -> i32 {
    let mut stdout = std::io::stdout();
    let mut bytes = SENTINEL.to_vec();
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    if stdout
        .write_all(&bytes)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return HELPER_SETUP_FAILED;
    }
    code
}

/// Reports `success`, or the real errno with the denied status.
fn report_errno(success: &str, errno: Option<i32>) -> i32 {
    match errno {
        None => report(success, 0),
        Some(errno) => report(&format!("denied errno={errno}"), i32::from(DENIED_STATUS)),
    }
}

/// Returns the errno of a failed libc call (`rc < 0`).
fn call_errno(rc: libc::c_int) -> Option<i32> {
    (rc < 0).then(|| std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
}

/// Runs `mode` with its `arguments`, returning the exit status.
pub(super) fn run(mode: &str, arguments: &[String]) -> i32 {
    let argument = |index: usize| arguments.get(index).map(String::as_str).unwrap_or_default();
    match mode {
        "connect" => connect(argument(0)),
        "nest" => nest(argument(0), argument(1), argument(2)),
        "write" => report_errno(
            "written",
            std::fs::write(argument(0), b"tampered")
                .err()
                .map(|error| error.raw_os_error().unwrap_or(-1)),
        ),
        "signal" => signal(argument(0), argument(1)),
        "signal-own-child" => signal_own_child(),
        #[cfg(target_os = "linux")]
        "process-memory-read" => process_memory_read(),
        "wait-for-eof" => match std::io::copy(&mut std::io::stdin(), &mut std::io::sink()) {
            Ok(_) => 0,
            Err(_) => HELPER_SETUP_FAILED,
        },
        "sleep" => {
            std::thread::sleep(Duration::from_millis(argument(0).parse().unwrap_or(0)));
            0
        }
        // SAFETY: `setsid` and `setpgid` have no memory-safety preconditions.
        "setsid" => report_errno("detached", call_errno(unsafe { libc::setsid() })),
        // SAFETY: as above.
        "setpgid" => report_errno("detached", call_errno(unsafe { libc::setpgid(0, 0) })),
        "orphan" => leave_child(argument(0), Departure::SameGroup),
        "escape" => leave_child(
            argument(0),
            Departure::NewGroup {
                scrub: argument(1) == "scrub",
            },
        ),
        "link" => report_errno(
            "linked",
            std::fs::hard_link(argument(0), argument(1))
                .err()
                .map(|error| error.raw_os_error().unwrap_or(-1)),
        ),
        "escape-connect" => escape_connect(argument(0)),
        "unix-exchange" => unix_exchange(argument(0)),
        "socket" => socket(argument(0)),
        "umask" => umask(),
        _ => HELPER_SETUP_FAILED,
    }
}

/// Attempts a valid self-memory read, which the confined filter must deny.
#[cfg(target_os = "linux")]
pub(super) fn process_memory_read() -> i32 {
    let remote = [0x5a_u8];
    let mut local = [0_u8];
    let local = libc::iovec {
        iov_base: local.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let remote = libc::iovec {
        iov_base: remote.as_ptr().cast_mut().cast(),
        iov_len: 1,
    };
    // SAFETY: Both one-byte buffers remain live, and their iovecs describe
    // valid memory in this process for the duration of the synchronous call.
    let result = unsafe { libc::process_vm_readv(libc::getpid(), &local, 1, &remote, 1, 0) };
    if result == 1 {
        report("accessed", 0)
    } else if result < 0 {
        report_errno("accessed", std::io::Error::last_os_error().raw_os_error())
    } else {
        HELPER_SETUP_FAILED
    }
}

fn connect(address: &str) -> i32 {
    let Ok(address) = parse_loopback(address) else {
        return HELPER_SETUP_FAILED;
    };
    match connect_probe(address) {
        Ok(ProbeReport::Connected) => report("connected", 0),
        Ok(ProbeReport::Denied { errno }) => report_errno("", Some(errno)),
        Err(_) => HELPER_SETUP_FAILED,
    }
}

/// Like [`helper_environment`] but reusing the inherited nonce.
fn inherited_helper_environment(mode: &str) -> Vec<(OsString, OsString)> {
    let inherited = std::env::var(HELPER_NONCE).unwrap_or_default();
    vec![
        (
            OsString::from(HELPER_SELECTOR),
            OsString::from(format!("{mode}:{inherited}")),
        ),
        (OsString::from(HELPER_NONCE), OsString::from(inherited)),
    ]
}

/// Returns a command re-running this helper in `mode`.
fn self_command(mode: &str, mode_arguments: &[&str]) -> Option<StdCommand> {
    let mut command = StdCommand::new(std::env::current_exe().ok()?);
    command.args(helper_arguments(mode_arguments));
    for (name, value) in inherited_helper_environment(mode) {
        command.env(name, value);
    }
    Some(command)
}

/// Records a marker in `marker_directory`, then connects directly at depth
/// zero or through a child helper at depth `n - 1`.
fn nest(depth: &str, address: &str, marker_directory: &str) -> i32 {
    let Ok(depth) = depth.parse::<u32>() else {
        return HELPER_SETUP_FAILED;
    };
    let marker = Path::new(marker_directory).join(format!("nest-{depth}"));
    if std::fs::write(marker, std::process::id().to_string()).is_err() {
        return HELPER_SETUP_FAILED;
    }
    if depth == 0 {
        return connect(address);
    }
    let next = (depth - 1).to_string();
    let Some(mut child) = self_command("nest", &[&next, address, marker_directory])
        .and_then(|mut command| command.spawn().ok())
    else {
        return HELPER_SETUP_FAILED;
    };
    child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(HELPER_SETUP_FAILED)
}

fn kill_errno(pid: libc::pid_t, signal: libc::c_int) -> Option<i32> {
    // SAFETY: `kill` has no memory-safety preconditions; callers never pass
    // a pid <= 1.
    call_errno(unsafe { libc::kill(pid, signal) })
}

fn signal(pid: &str, signal: &str) -> i32 {
    let (Ok(pid), Ok(signal)) = (pid.parse::<libc::pid_t>(), signal.parse::<libc::c_int>()) else {
        return HELPER_SETUP_FAILED;
    };
    if pid <= 1 {
        return HELPER_SETUP_FAILED;
    }
    report_errno("signalled", kill_errno(pid, signal))
}

fn signal_own_child() -> i32 {
    let Some(mut child) =
        self_command("sleep", &["30000"]).and_then(|mut command| command.spawn().ok())
    else {
        return HELPER_SETUP_FAILED;
    };
    let Ok(pid) = libc::pid_t::try_from(child.id()) else {
        return HELPER_SETUP_FAILED;
    };
    let error = kill_errno(pid, libc::SIGKILL);
    if error.is_some() {
        let _ = child.kill();
    }
    let _ = child.wait();
    report_errno("signalled", error)
}

/// How a helper's child leaves it.
#[derive(Clone, Copy)]
enum Departure {
    /// A daemon-style orphan that stays in the supervised process group.
    SameGroup,
    /// A `posix_spawn(POSIX_SPAWN_SETPGROUP)` child in a new group, keeping
    /// or scrubbing the inherited environment.
    NewGroup { scrub: bool },
}

/// Spawns a sleeping child with null stdio, records its pid and exits
/// without waiting for it.
fn leave_child(pid_file: &str, departure: Departure) -> i32 {
    let Some(mut command) = self_command("sleep", &["30000"]) else {
        return HELPER_SETUP_FAILED;
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Departure::NewGroup { scrub } = departure {
        command.process_group(0);
        if scrub {
            command.env_clear();
            for (name, value) in inherited_helper_environment("sleep") {
                command.env(name, value);
            }
        }
    }
    let Ok(child) = command.spawn() else {
        return HELPER_SETUP_FAILED;
    };
    if std::fs::write(pid_file, child.id().to_string()).is_err() {
        return HELPER_SETUP_FAILED;
    }
    report("spawned", 0)
}

/// Runs `connect` in a `posix_spawn(POSIX_SPAWN_SETPGROUP)` child whose
/// environment keeps only the helper selector, and waits for its report.
fn escape_connect(address: &str) -> i32 {
    let Some(mut command) = self_command("connect", &[address]) else {
        return HELPER_SETUP_FAILED;
    };
    command.process_group(0).env_clear();
    for (name, value) in inherited_helper_environment("connect") {
        command.env(name, value);
    }
    command
        .status()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(HELPER_SETUP_FAILED)
}

/// Binds a Unix socket at `path`, connects to it and echoes four bytes.
fn unix_exchange(path: &str) -> i32 {
    let errno = |error: std::io::Error| Some(error.raw_os_error().unwrap_or(-1));
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) => return report_errno("", errno(error)),
    };
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut bytes = [0_u8; 4];
        std::io::Read::read_exact(&mut stream, &mut bytes)?;
        stream.write_all(&bytes)
    });
    let exchanged = UnixStream::connect(path).and_then(|mut stream| {
        stream.write_all(b"ping")?;
        let mut bytes = [0_u8; 4];
        std::io::Read::read_exact(&mut stream, &mut bytes)?;
        Ok(bytes)
    });
    // The socket file is removed so the run leaves no special file behind
    // in the writable root; the helper `_exit`s, so an unjoined server
    // thread blocked in `accept` cannot hang it.
    let removed = std::fs::remove_file(path).is_ok();
    let bytes = match exchanged {
        Ok(bytes) => bytes,
        Err(error) => return report_errno("", errno(error)),
    };
    match server.join() {
        Ok(Ok(())) if removed && &bytes == b"ping" => report("exchanged", 0),
        _ => HELPER_SETUP_FAILED,
    }
}

/// Maps a fixture domain name to `(domain, type)`.
fn socket_kind(domain: &str) -> Option<(libc::c_int, libc::c_int)> {
    match domain {
        "unix" => Some((libc::AF_UNIX, libc::SOCK_STREAM)),
        "inet" => Some((libc::AF_INET, libc::SOCK_STREAM)),
        "inet6" => Some((libc::AF_INET6, libc::SOCK_STREAM)),
        #[cfg(target_os = "linux")]
        "netlink" => Some((libc::AF_NETLINK, libc::SOCK_RAW)),
        #[cfg(target_os = "linux")]
        "packet" => Some((libc::AF_PACKET, libc::SOCK_RAW)),
        _ => None,
    }
}

/// Creates (and closes) one socket of `domain`.
fn socket(domain: &str) -> i32 {
    let Some((domain, kind)) = socket_kind(domain) else {
        return HELPER_SETUP_FAILED;
    };
    // SAFETY: `socket` has no memory-safety preconditions.
    let fd = unsafe { libc::socket(domain, kind, 0) };
    let errno = call_errno(fd);
    if errno.is_none() {
        // SAFETY: `fd` is a descriptor this helper just created.
        unsafe {
            libc::close(fd);
        }
    }
    report_errno("created", errno)
}

/// Reports this process's umask as three octal digits.
///
/// Reading requires setting; the previous value is restored at once, and the
/// helper is single-threaded.
fn umask() -> i32 {
    // SAFETY: `umask` has no memory-safety preconditions and cannot fail; the
    // second call restores the value the first call returned.
    let current = unsafe {
        let current = libc::umask(0);
        libc::umask(current);
        current
    };
    report(&format!("{current:03o}"), 0)
}
