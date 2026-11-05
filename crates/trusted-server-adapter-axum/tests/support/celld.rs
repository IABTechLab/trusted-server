//! Disposable real celld, selected explicitly by an external-test prerequisite.

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use edgezero_store_celld::settings::{CelldAccess, CelldKvSettings};
use serde_json::json;
use trusted_server_adapter_axum::celld_ec::CelldEcKv;

pub struct CelldFixture {
    root: tempfile::TempDir,
    binary: PathBuf,
    child: Option<Child>,
    pub endpoint: String,
}

impl CelldFixture {
    pub fn new() -> Self {
        let binary = PathBuf::from(std::env::var_os("CELLD_EC_TEST_BINARY").expect(
            "should set CELLD_EC_TEST_BINARY to the built conditional-record celld executable",
        ));
        assert!(
            binary.is_absolute() && binary.is_file(),
            "should supply an existing absolute celld executable path"
        );
        let root = tempfile::tempdir().expect("should create disposable celld directory");
        let policy = json!({"version":1,"listen":"127.0.0.1:0",
            "stores":[{"alias":"identity","namespace":"fixture-ec"}],
            "access":{"mode":"trusted-network","grants":[{"store":"identity","operations":["read","write","delete","list"]}]}});
        std::fs::write(
            root.path().join("policy.json"),
            serde_json::to_vec(&policy).expect("should encode fixture policy"),
        )
        .expect("should write disposable fixture policy");
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
        let mut child = Command::new(&self.binary)
            .env_clear()
            .env("CELLD_TEST_DATA_DIR", self.root.path().join("node-cache"))
            .args([
                "--listen",
                "127.0.0.1:0",
                "--internal-listen",
                "127.0.0.1:0",
                "--store-api-config",
            ])
            .arg(self.root.path().join("policy.json"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("should start disposable real celld");
        let stdout = child.stdout.take().expect("should capture celld readiness");
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some((_, address)) = line.split_once("celld scoped stores listening on ") {
                    let _ = send.send(format!("http://{}", address.trim()));
                }
            }
        });
        self.child = Some(child);
        self.endpoint = receive
            .recv_timeout(Duration::from_secs(30))
            .expect("should observe real celld readiness");
    }
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
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
    pub async fn store(&self) -> CelldEcKv {
        CelldEcKv::prepare(&self.settings(), None)
            .await
            .expect("should prepare typed records against real celld")
    }
}
impl Drop for CelldFixture {
    fn drop(&mut self) {
        self.stop();
    }
}
