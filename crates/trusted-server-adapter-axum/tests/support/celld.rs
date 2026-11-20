//! Disposable `EdgeZero` store service on an explicitly selected official celld.

use std::fs::{self, File};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use edgezero_core::key_value_store::KvHandle;
use edgezero_core::record_store::RecordLayout;
use edgezero_core::time::MonotonicClock;
use edgezero_store_celld::settings::{CelldAccess, CelldKvSettings, CelldPreparationLimits};
use edgezero_store_celld::{CelldPreparation, PreparationError};
use serde_json::json;
use trusted_server_core::ec::record_backend::{EcRecordLayout, RecordEcKvStore};

pub struct CelldFixture {
    root: tempfile::TempDir,
    binary: PathBuf,
    child: Option<Child>,
    pub endpoint: String,
}

impl CelldFixture {
    pub fn new() -> Self {
        let binary = PathBuf::from(
            std::env::var_os("CELLD_EC_TEST_BINARY")
                .expect("should set CELLD_EC_TEST_BINARY to the official celld 0.6.3 executable"),
        );
        assert!(
            binary.is_absolute() && binary.is_file(),
            "should supply an existing absolute celld executable path"
        );
        let version = Command::new(&binary)
            .arg("--version")
            .output()
            .expect("should inspect the selected celld executable");
        assert!(version.status.success(), "should read the celld version");
        assert_eq!(
            String::from_utf8(version.stdout)
                .expect("should report a UTF-8 version")
                .trim(),
            "celld 0.6.3",
            "should use the pinned stock runtime"
        );
        let source = PathBuf::from(std::env::var_os("EDGEZERO_CELLD_STORE_SERVICE").expect(
            "should select services/celld-store from the matching EdgeZero dependency revision",
        ));
        assert!(
            source.is_absolute(),
            "should supply an absolute service directory"
        );
        let root = tempfile::tempdir().expect("should create disposable service directory");
        fs::create_dir(root.path().join("src")).expect("should create service source directory");
        fs::copy(
            source.join("src/index.js"),
            root.path().join("src/index.js"),
        )
        .expect("should copy the matching EdgeZero store service");
        let policy = json!({"version":1,
            "stores":[{"alias":"identity","namespace":"fixture-ec"}],
            "access":{"mode":"trusted-network","grants":[{"store":"identity","operations":["read","write","delete","list"]}]}});
        let configuration = json!({
            "name":"edgezero-store", "main":"src/index.js", "no_bundle":true,
            "compatibility_date":"2026-10-01",
            "durable_objects":{"bindings":[{"name":"EDGEZERO_STORE","class_name":"EdgeZeroStore"}]},
            "migrations":[{"tag":"v1","new_sqlite_classes":["EdgeZeroStore"]}],
            "vars":{"EDGEZERO_STORE_POLICY":policy.to_string()}
        });
        fs::write(
            root.path().join("wrangler.json"),
            serde_json::to_vec_pretty(&configuration).expect("should encode service deployment"),
        )
        .expect("should write disposable service deployment");
        let mut fixture = Self {
            root,
            binary,
            child: None,
            endpoint: String::new(),
        };
        fixture.start();
        fixture
    }

    fn start(&mut self) {
        let reservation = TcpListener::bind("127.0.0.1:0").expect("should reserve a local port");
        let address = reservation
            .local_addr()
            .expect("should read the reserved address");
        drop(reservation);
        let log_path = self.root.path().join("celld.log");
        let log = File::create(&log_path).expect("should create the service log");
        let mut command = Command::new(&self.binary);
        command
            .current_dir(self.root.path())
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").expect("should supply fixture tools"),
            )
            .env("HOME", self.root.path().join("home"))
            .env("TMPDIR", std::env::temp_dir())
            .env("NO_COLOR", "1")
            .args(["dev", ".", "--no-watch", "--logs", "--port"])
            .arg(address.port().to_string())
            .stdout(Stdio::from(
                log.try_clone().expect("should clone service log"),
            ))
            .stderr(Stdio::from(log));
        #[cfg(unix)]
        command.process_group(0);
        self.child = Some(
            command
                .spawn()
                .expect("should start official celld with the store service"),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        let expected_address = address.to_string();
        loop {
            let text = fs::read_to_string(&log_path).expect("should read service readiness");
            if text.lines().any(|line| {
                line.split_once("celld listening on ")
                    .and_then(|(_, value)| value.split_whitespace().next())
                    == Some(expected_address.as_str())
            }) {
                self.endpoint = format!("http://{address}");
                return;
            }
            let status = self
                .child
                .as_mut()
                .expect("should retain service ownership")
                .try_wait()
                .expect("should inspect service process");
            assert!(
                status.is_none(),
                "should remain alive until readiness: {status:?}: {text}"
            );
            assert!(
                Instant::now() < deadline,
                "should observe service readiness: {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            {
                let _group = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .status();
            }
            let _killed = child.kill();
            let _reaped = child.wait();
        }
    }

    pub fn restart(&mut self) {
        self.stop();
        self.start();
    }

    pub fn settings(&self) -> CelldKvSettings {
        CelldKvSettings {
            endpoint: self.endpoint.clone(),
            store: "identity".to_owned(),
            access: CelldAccess::TrustedNetwork {
                allow_remote_http: false,
            },
        }
    }

    pub async fn record_handle(&self) -> Result<KvHandle, PreparationError> {
        let mut session =
            CelldPreparation::new(MonotonicClock::default(), CelldPreparationLimits::default())?;
        session
            .prepare_record_store(
                &self.settings(),
                &RecordLayout::new(EcRecordLayout).expect("should declare EC key layout"),
                None,
            )
            .await
    }

    pub async fn store(&self) -> RecordEcKvStore {
        let handle = self
            .record_handle()
            .await
            .expect("should prepare records against the stock celld store service");
        RecordEcKvStore::new("trusted_server_kv".to_owned(), &handle)
            .expect("should acquire the shared EC backend")
    }
}

impl Drop for CelldFixture {
    fn drop(&mut self) {
        self.stop();
    }
}
