use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use error_stack::{Report, ResultExt as _};
use tokio::runtime::Handle;

use super::{HarnessError, HarnessResult};

pub(super) fn build_environment(
    source: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    source
        .into_iter()
        .filter(|(name, _)| {
            let Some(name) = name.to_str() else {
                return false;
            };
            name != "CARGO_BUILD_TARGET"
                && (matches!(
                    name,
                    "PATH"
                        | "RUSTUP_HOME"
                        | "RUSTUP_TOOLCHAIN"
                        | "CARGO_HOME"
                        | "CARGO_TARGET_DIR"
                        | "TMPDIR"
                        | "WASM_BINDGEN_BIN"
                        | "WASM_OPT_BIN"
                        | "ESBUILD_BIN"
                        | "CARGO_NET_OFFLINE"
                        | "RUSTFLAGS"
                        | "CARGO_ENCODED_RUSTFLAGS"
                        | "RUSTDOCFLAGS"
                        | "CARGO_INCREMENTAL"
                        | "RUSTC"
                        | "RUSTDOC"
                        | "RUSTC_WRAPPER"
                        | "RUSTC_WORKSPACE_WRAPPER"
                        | "TSJS_PREBUILT_DIR"
                ) || name.starts_with("CARGO_PROFILE_")
                    || name.starts_with("CARGO_BUILD_")
                    || name.starts_with("CARGO_TARGET_"))
        })
        .collect()
}

#[derive(Clone, Default)]
pub(super) struct Cancellation {
    cancelled: Arc<AtomicBool>,
    groups: Arc<Mutex<BTreeSet<u32>>>,
}

impl Cancellation {
    pub(super) fn check(&self) -> HarnessResult<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(Report::new(HarnessError::Cancelled))
        } else {
            Ok(())
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        for id in self
            .groups
            .lock()
            .expect("should lock owned process groups")
            .iter()
        {
            signal_group(*id, libc::SIGTERM);
        }
    }

    pub(super) fn watch(&self, handle: &Handle) -> HarnessResult<()> {
        let _entered = handle.enter();
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .change_context(HarnessError::Runtime)?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .change_context(HarnessError::Runtime)?;
        // Cargo owns the test process. Detect its disappearance as well as
        // direct signals so terminating the shell entry point leaves no runtimes.
        let parent = unsafe { libc::getppid() };
        let cancellation = self.clone();
        handle.spawn(async move {
            loop {
                tokio::select! {
                    _ = term.recv() => break,
                    _ = interrupt.recv() => break,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if unsafe { libc::getppid() } != parent { break; }
                    }
                }
            }
            cancellation.cancel();
            tokio::time::sleep(Duration::from_secs(5)).await;
            for id in cancellation
                .groups
                .lock()
                .expect("should lock owned process groups")
                .iter()
            {
                signal_group(*id, libc::SIGKILL);
            }
        });
        Ok(())
    }
}

fn signal_group(id: u32, signal: i32) {
    if let Ok(id) = libc::pid_t::try_from(id) {
        // Every registered ID belongs to a child started in its own process group.
        unsafe {
            libc::killpg(id, signal);
        }
    }
}

pub(super) struct ManagedChild {
    child: Child,
    cancellation: Cancellation,
}

impl ManagedChild {
    pub(super) fn spawn(command: &mut Command, cancellation: &Cancellation) -> HarnessResult<Self> {
        cancellation.check()?;
        command.process_group(0);
        command.stdin(Stdio::null());
        let child = command.spawn().change_context(HarnessError::Runtime)?;
        cancellation
            .groups
            .lock()
            .expect("should lock owned process groups")
            .insert(child.id());
        Ok(Self {
            child,
            cancellation: cancellation.clone(),
        })
    }

    pub(super) fn status(&mut self) -> HarnessResult<Option<ExitStatus>> {
        self.cancellation.check()?;
        self.child.try_wait().change_context(HarnessError::Runtime)
    }

    pub(super) fn wait(&mut self) -> HarnessResult<ExitStatus> {
        loop {
            if let Some(status) = self.status()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let id = self.child.id();
        signal_group(id, libc::SIGTERM);
        let until = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
        signal_group(id, libc::SIGKILL);
        let _ = self.child.wait();
        self.cancellation
            .groups
            .lock()
            .expect("should lock owned process groups")
            .remove(&id);
    }
}

pub(super) fn logged_command(
    command: &mut Command,
    log: &Path,
    cancellation: &Cancellation,
) -> HarnessResult<()> {
    let output = File::create(log).change_context(HarnessError::Io)?;
    command
        .stdout(Stdio::from(
            output.try_clone().change_context(HarnessError::Io)?,
        ))
        .stderr(Stdio::from(output));
    let status = ManagedChild::spawn(command, cancellation)?.wait()?;
    if !status.success() {
        return Err(Report::new(HarnessError::Runtime).attach(format!(
            "command exits with {status}; see {}",
            log.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn compiler_settings_are_preserved_without_operator_settings() {
        let source = [
            ("PATH", "/tmp/example-bin"),
            ("RUSTFLAGS", "-D warnings"),
            ("RUSTUP_TOOLCHAIN", "1.98.1"),
            ("CARGO_PROFILE_DEV_DEBUG", "0"),
            ("CARGO_INCREMENTAL", "0"),
            ("CARGO_TARGET_DIR", "/tmp/example-target"),
            ("CARGO_BUILD_TARGET", "wasm32-wasip1"),
            ("TRUSTED_SERVER__PUBLISHER__DOMAIN", "example.com"),
            ("TSJS_SKIP_BUILD", "1"),
        ];
        let selected = build_environment(
            source.map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        for name in [
            "PATH",
            "RUSTFLAGS",
            "RUSTUP_TOOLCHAIN",
            "CARGO_PROFILE_DEV_DEBUG",
            "CARGO_INCREMENTAL",
            "CARGO_TARGET_DIR",
        ] {
            assert!(
                selected.contains_key(&OsString::from(name)),
                "should preserve {name}"
            );
        }
        for name in [
            "CARGO_BUILD_TARGET",
            "TRUSTED_SERVER__PUBLISHER__DOMAIN",
            "TSJS_SKIP_BUILD",
        ] {
            assert!(
                !selected.contains_key(&OsString::from(name)),
                "should isolate {name}"
            );
        }
    }

    #[test]
    fn owned_process_is_reaped_on_drop() {
        let cancellation = Cancellation::default();
        let mut command = Command::new("sleep");
        command.arg("300");
        let child =
            ManagedChild::spawn(&mut command, &cancellation).expect("should launch an owned child");
        let id = child.child.id();
        drop(child);
        assert_eq!(
            unsafe { libc::kill(libc::pid_t::try_from(id).expect("should fit pid"), 0) },
            -1,
            "should reap the terminated process"
        );
        assert!(
            cancellation
                .groups
                .lock()
                .expect("should lock groups")
                .is_empty(),
            "should release ownership"
        );
    }

    #[test]
    fn managed_child_closes_stdin() {
        let cancellation = Cancellation::default();
        let mut command = Command::new("sh");
        command
            .args(["-c", "if read value; then exit 1; fi"])
            .stdin(Stdio::piped());
        let mut child = ManagedChild::spawn(&mut command, &cancellation)
            .expect("should launch a child with closed stdin");
        assert!(
            child.child.stdin.is_none(),
            "should replace inherited input with null"
        );
        assert!(
            child.wait().expect("should observe EOF on stdin").success(),
            "should complete without reading terminal input"
        );
    }

    #[test]
    fn completed_process_is_safe_to_drop() {
        let cancellation = Cancellation::default();
        let child = ManagedChild::spawn(&mut Command::new("true"), &cancellation)
            .expect("should launch a completed command");
        let mut child = child;
        assert!(
            child.wait().expect("should wait for completion").success(),
            "should complete normally"
        );
        drop(child);
        assert!(
            cancellation
                .groups
                .lock()
                .expect("should lock groups")
                .is_empty(),
            "should release completed ownership"
        );
    }

    #[test]
    fn sigterm_cleans_owned_descendants() {
        signal_cleanup(libc::SIGTERM);
    }

    #[test]
    fn sigint_cleans_owned_descendants() {
        signal_cleanup(libc::SIGINT);
    }

    fn signal_cleanup(signal: i32) {
        let directory = tempfile::tempdir().expect("should create probe workspace");
        let cancellation = Cancellation::default();
        let executable = std::env::current_exe().expect("should locate Rust probe executable");
        let mut command = Command::new(executable);
        command
            .args([
                "--exact",
                "process::tests::cleanup_owner_probe",
                "--ignored",
            ])
            .env("HTML_EOF_PROBE_DIR", directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut parent = ManagedChild::spawn(&mut command, &cancellation)
            .expect("should launch the owner probe");
        let ready = directory.path().join("owner.ready");
        let until = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                parent
                    .status()
                    .expect("should inspect the owner probe")
                    .is_none(),
                "should keep the probe alive until ready"
            );
            assert!(Instant::now() < until, "should start the cleanup probe");
            thread::sleep(Duration::from_millis(20));
        }
        let leaf: libc::pid_t = fs::read_to_string(&ready)
            .expect("should read owned PID")
            .parse()
            .expect("should parse owned PID");
        assert_eq!(
            unsafe {
                libc::kill(
                    libc::pid_t::try_from(parent.child.id()).expect("should fit pid"),
                    signal,
                )
            },
            0,
            "should signal the owner"
        );
        let until = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = parent.status().expect("should inspect cleanup progress") {
                break status;
            }
            assert!(
                Instant::now() < until,
                "should finish cancellation cleanup within its deadline"
            );
            thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "should complete cancellation cleanup");
        assert!(
            directory.path().join("cleaned").exists(),
            "should unwind owned-process guards"
        );
        assert_eq!(
            unsafe { libc::kill(leaf, 0) },
            -1,
            "should reap the stubborn descendant"
        );
    }

    #[test]
    #[ignore = "internal subprocess probe, selected only by cleanup tests"]
    fn cleanup_leaf_probe() {
        let directory = PathBuf::from(
            std::env::var_os("HTML_EOF_PROBE_DIR").expect("should supply probe workspace"),
        );
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
        fs::write(directory.join("leaf.ready"), "ready")
            .expect("should report installed signal handler");
        thread::sleep(Duration::from_secs(300));
    }

    #[test]
    #[ignore = "internal subprocess probe, selected only by cleanup tests"]
    fn cleanup_owner_probe() {
        let directory = PathBuf::from(
            std::env::var_os("HTML_EOF_PROBE_DIR").expect("should supply probe workspace"),
        );
        let runtime = tokio::runtime::Runtime::new().expect("should create signal runtime");
        let cancellation = Cancellation::default();
        cancellation
            .watch(runtime.handle())
            .expect("should watch cancellation");
        let mut command = Command::new(std::env::current_exe().expect("should locate leaf probe"));
        command
            .args(["--exact", "process::tests::cleanup_leaf_probe", "--ignored"])
            .env("HTML_EOF_PROBE_DIR", &directory)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut leaf = ManagedChild::spawn(&mut command, &cancellation)
            .expect("should spawn the stubborn descendant");
        let until = Instant::now() + Duration::from_secs(5);
        while !directory.join("leaf.ready").exists() {
            assert!(
                Instant::now() < until,
                "should start the stubborn descendant"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let staged = directory.join("owner.ready.tmp");
        fs::write(&staged, leaf.child.id().to_string()).expect("should stage owned descendant");
        fs::rename(&staged, directory.join("owner.ready")).expect("should report owned descendant");
        let result = leaf
            .wait()
            .expect_err("should interrupt an active owned command");
        assert!(
            matches!(result.current_context(), HarnessError::Cancelled),
            "should report cancellation"
        );
        drop(leaf);
        fs::write(directory.join("cleaned"), "cleaned").expect("should report completed cleanup");
    }
}
