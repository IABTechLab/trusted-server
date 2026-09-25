//! CA command tests use disposable HOME, XDG paths, CA files, and NSS databases only.
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            root: tempfile::tempdir().expect("should create isolated HOME"),
        };
        assert!(fixture.run("path").status.success());
        fixture
    }

    fn ca(&self) -> PathBuf {
        self.root.path().join("ca")
    }
    fn db(&self) -> PathBuf {
        self.root.path().join("data/pki/nssdb")
    }
    fn journal(&self) -> PathBuf {
        self.ca().join("managed-nss-trust.json")
    }

    fn initialize_nss(&self) {
        fs::create_dir_all(self.db()).expect("should create isolated NSS directory");
        let out = Command::new("certutil")
            .args(["-N", "--empty-password", "-d"])
            .arg(format!("sql:{}", self.db().display()))
            .output()
            .expect("should initialize isolated NSS database");
        assert!(out.status.success(), "{out:?}");
    }

    fn import_certificate(&self, nickname: &str, certificate: &Path) {
        let out = Command::new("certutil")
            .args(["-A", "-n", nickname, "-t", "C,,", "-d"])
            .arg(format!("sql:{}", self.db().display()))
            .arg("-i")
            .arg(certificate)
            .output()
            .expect("should import fixture certificate");
        assert!(out.status.success(), "{out:?}");
    }

    fn inspect_certificate(&self, nickname: &str) -> Vec<u8> {
        let out = Command::new("certutil")
            .args(["-L", "-n", nickname, "-d"])
            .arg(format!("sql:{}", self.db().display()))
            .output()
            .expect("should inspect fixture certificate and trust flags");
        assert!(out.status.success(), "{out:?}");
        out.stdout
    }

    fn unrelated_certificate(&self) -> PathBuf {
        let cert = rcgen::generate_simple_self_signed(vec!["unrelated.example.com".into()])
            .expect("should generate unrelated certificate")
            .cert;
        let path = self.root.path().join("unrelated.pem");
        fs::write(&path, cert.pem()).expect("should write unrelated certificate");
        path
    }

    fn command(&self, action: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ts"));
        cmd.env("HOME", self.root.path())
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .args(["dev", "proxy", "--ca-dir"])
            .arg(self.ca())
            .args(["ca", action]);
        cmd
    }

    fn run(&self, action: &str) -> Output {
        self.command(action)
            .output()
            .expect("should run isolated CA command")
    }

    fn record(&self) {
        let pem = fs::read(self.ca().join("ca-cert.pem")).expect("should read certificate");
        let der = rustls_pemfile::certs(&mut pem.as_slice())
            .next()
            .expect("should contain certificate")
            .expect("should parse certificate");
        fs::create_dir_all(self.db()).expect("should create isolated database directory");
        fs::write(self.root.path().join("cert.der"), der.as_ref()).expect("should write DER");
        let record = serde_json::json!([{"database": self.db(), "nickname": "ts-dev-proxy-0123456789abcdef", "certificate": der.as_ref()}]);
        fs::write(
            self.journal(),
            serde_json::to_vec(&record).expect("should serialize record"),
        )
        .expect("should persist record");
    }

    fn fake_tool(&self, body: &str) -> PathBuf {
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin).expect("should create tools directory");
        let path = bin.join("certutil");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("should write tool");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("should make executable");
        bin
    }

    fn assert_rotation_fails_unchanged(&self, path: &Path) -> Output {
        let key = fs::read(self.ca().join("ca-key.pem")).expect("should read key");
        let cert = fs::read(self.ca().join("ca-cert.pem")).expect("should read cert");
        let record = fs::read(self.journal()).expect("should read journal");
        let out = self
            .command("regenerate")
            .env("PATH", path)
            .output()
            .expect("should run regenerate");
        assert!(!out.status.success(), "rotation must fail: {out:?}");
        assert_eq!(
            key,
            fs::read(self.ca().join("ca-key.pem")).expect("should retain key")
        );
        assert_eq!(
            cert,
            fs::read(self.ca().join("ca-cert.pem")).expect("should retain certificate")
        );
        assert_eq!(
            record,
            fs::read(self.journal()).expect("should retain journal")
        );
        out
    }
}

#[test]
fn failed_queries_missing_tool_and_failed_deletion_prevent_rotation() {
    let fixture = Fixture::new();
    fixture.record();
    fixture.assert_rotation_fails_unchanged(&fixture.root.path().join("no-tools"));
    let bin = fixture.fake_tool("echo 'database locked or unreadable' >&2; exit 1");
    fixture.assert_rotation_fails_unchanged(&bin);
    // Successful listing and export, but deletion fails. Neither CA file may change.
    let bin = fixture.fake_tool("case \"$1:$2\" in\n-L:-d) echo 'ts-dev-proxy-0123456789abcdef C,,';;\n-L:-n) /bin/cat \"$HOME/cert.der\";;\n*) exit 1;;\nesac");
    fixture.assert_rotation_fails_unchanged(&bin);
    let out = fixture
        .command("uninstall")
        .env("PATH", &bin)
        .output()
        .expect("should run uninstall");
    assert!(
        !out.status.success(),
        "explicit uninstall must report failure"
    );
}

#[test]
fn identity_conflict_and_malformed_journal_prevent_rotation() {
    let fixture = Fixture::new();
    fixture.record();
    let bin = fixture.fake_tool("case \"$1:$2\" in\n-L:-d) echo 'ts-dev-proxy-0123456789abcdef C,,';;\n-L:-n) echo wrong-certificate;;\n*) exit 99;;\nesac");
    fixture.assert_rotation_fails_unchanged(&bin);
    fs::write(fixture.journal(), "not json").expect("should write malformed journal");
    fixture.assert_rotation_fails_unchanged(&bin);
}

#[test]
fn retry_after_interrupted_removal_confirms_absence() {
    let fixture = Fixture::new();
    fixture.record();
    let bin = fixture
        .fake_tool("test \"$1\" = '-L' || exit 99; echo 'Certificate Nickname Trust Attributes'");
    assert!(
        fixture
            .command("uninstall")
            .env("XDG_DATA_HOME", "relative")
            .env("PATH", &bin)
            .output()
            .expect("should retry removal")
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(fixture.journal()).expect("should read empty journal"),
        "[]"
    );
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_repeat_install_path_change_and_uninstall() {
    let fixture = Fixture::new();
    for _ in 0..2 {
        let out = fixture.run("install");
        assert!(out.status.success(), "{out:?}");
    }
    let original = fs::read(fixture.journal()).expect("should read first journal");
    let entries: serde_json::Value =
        serde_json::from_slice(&original).expect("should parse journal");
    assert_eq!(entries.as_array().expect("should be entries").len(), 1);
    // Creating the legacy path changes browser selection, but retains the modern import.
    fs::create_dir_all(fixture.root.path().join(".pki/nssdb"))
        .expect("should create legacy destination");
    assert!(fixture.run("install").status.success());
    let entries: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).expect("should read journal"))
            .expect("should parse journal");
    assert_eq!(entries.as_array().expect("should be entries").len(), 2);
    // Preserve an unrelated CA in the same DB.
    let tool = which::which("certutil").expect("should have NSS tools for this explicit test");
    let unrelated_path = fixture.root.path().join("unrelated.pem");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())
        .expect("should create unrelated CA params");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Unrelated example.com CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let key = rcgen::KeyPair::generate().expect("should create unrelated CA key");
    let certificate = params.self_signed(&key).expect("should sign unrelated CA");
    fs::write(&unrelated_path, certificate.pem()).expect("should write unrelated CA");
    let db = format!("sql:{}", fixture.db().display());
    assert!(
        Command::new(&tool)
            .args(["-A", "-n", "unrelated", "-t", "C,,", "-d", &db, "-i"])
            .arg(&unrelated_path)
            .status()
            .expect("should import unrelated CA")
            .success()
    );
    // Simulate deletion completed before the journal checkpoint.
    let nickname = entries[1]["nickname"]
        .as_str()
        .expect("should have nickname");
    let legacy = format!("sql:{}", fixture.root.path().join(".pki/nssdb").display());
    assert!(
        Command::new(&tool)
            .args(["-D", "-n", nickname, "-d", &legacy])
            .status()
            .expect("should simulate interrupted removal")
            .success()
    );
    for _ in 0..2 {
        let out = fixture.run("uninstall");
        assert!(out.status.success(), "{out:?}");
    }
    assert!(
        Command::new(&tool)
            .args(["-L", "-n", "unrelated", "-d", &db])
            .output()
            .expect("should query unrelated CA")
            .status
            .success()
    );
    // Simulate interruption after journal persistence but before import.
    fs::write(fixture.journal(), original).expect("should restore pending journal");
    assert!(fixture.run("install").status.success());
    let out = fixture.run("uninstall");
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn failed_import_keeps_the_destination_recorded_for_retry() {
    let fixture = Fixture::new();
    let bin = fixture.fake_tool("case \"$1:$2\" in\n-N:*|-L:-d) exit 0;;\n-L:-n) /bin/cat \"$HOME/ca/ca-cert.pem\";;\n-A:*) test -s \"$HOME/ca/managed-nss-trust.json\" || exit 99; echo 'recorded before import' >&2; exit 1;;\n*) exit 99;;\nesac");
    let out = fixture
        .command("install")
        .env("PATH", &bin)
        .output()
        .expect("should run install");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("recorded before import"));
    let entries: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).expect("should retain pending import"))
            .expect("should parse journal");
    assert_eq!(
        entries
            .as_array()
            .expect("should contain destinations")
            .len(),
        1
    );
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_corrupt_database_is_not_reset() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.db()).expect("should create database directory");
    let database = fixture.db().join("cert9.db");
    fs::write(&database, "not a database").expect("should write corrupt database");
    assert!(!fixture.run("install").status.success());
    assert_eq!(
        fs::read_to_string(database).expect("should preserve database"),
        "not a database"
    );
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_identity_conflict_preserves_key_and_record() {
    let fixture = Fixture::new();
    assert!(fixture.run("install").status.success());
    let entries: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).expect("should read journal"))
            .expect("should parse journal");
    let nickname = entries[0]["nickname"]
        .as_str()
        .expect("should have nickname");
    let db = format!("sql:{}", fixture.db().display());
    let tool = which::which("certutil").expect("should have NSS tools");
    assert!(
        Command::new(&tool)
            .args(["-D", "-n", nickname, "-d", &db])
            .status()
            .expect("should remove managed certificate")
            .success()
    );
    let other = Fixture::new();
    assert!(
        Command::new(&tool)
            .args(["-A", "-n", nickname, "-t", "C,,", "-d", &db, "-i"])
            .arg(other.ca().join("ca-cert.pem"))
            .status()
            .expect("should simulate identity conflict")
            .success()
    );
    fixture.assert_rotation_fails_unchanged(tool.parent().expect("should have tool directory"));
    // The mismatching certificate must still exist after the rejected rotation.
    assert!(
        Command::new(&tool)
            .args(["-L", "-n", nickname, "-d", &db])
            .output()
            .expect("should query preserved certificate")
            .status
            .success()
    );
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_same_subject_install_conflict_is_rejected_before_mutation() {
    for manual_nickname in [
        None,
        Some("Manually imported dev CA"),
        Some("Manually imported dev CA "),
        Some("Manually imported\ndev CA"),
    ] {
        let managed = manual_nickname.is_none();
        let first = Fixture::new();
        let second = Fixture::new();
        let tool = which::which("certutil").expect("should have NSS tools");
        let db = format!("sql:{}", first.db().display());
        let nickname = if managed {
            assert!(first.run("install").status.success());
            let record: serde_json::Value = serde_json::from_slice(
                &fs::read(first.journal()).expect("should read first journal"),
            )
            .expect("should parse journal");
            record[0]["nickname"]
                .as_str()
                .expect("should have nickname")
                .to_owned()
        } else {
            fs::create_dir_all(first.db()).expect("should create isolated NSS directory");
            assert!(
                Command::new(&tool)
                    .args(["-N", "--empty-password", "-d", &db])
                    .status()
                    .expect("should initialize NSS")
                    .success()
            );
            let nickname = manual_nickname.expect("should have manual nickname");
            assert!(
                Command::new(&tool)
                    .args(["-A", "-n", nickname, "-t", "C,,", "-d", &db, "-i"])
                    .arg(first.ca().join("ca-cert.pem"))
                    .status()
                    .expect("should import first CA manually")
                    .success()
            );
            nickname.to_owned()
        };
        let export = || {
            Command::new(&tool)
                .args(["-L", "-n", &nickname, "-r", "-d", &db])
                .output()
                .expect("should export first CA")
        };
        let before = export();
        assert!(before.status.success());
        let database_before = fs::read(first.db().join("cert9.db")).expect("should snapshot DB");
        let result = second
            .command("install")
            .env("XDG_DATA_HOME", first.root.path().join("data"))
            .output()
            .expect("should attempt second CA install");
        let after = export();
        assert!(after.status.success());
        assert!(
            before.stdout == after.stdout,
            "same-subject install must preserve original nickname identity; managed={managed}; install={result:?}"
        );
        assert!(
            !result.status.success(),
            "different-key same-subject CA must be rejected before import"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("managed certificate identity conflict"),
            "must identify the subject conflict regardless of nickname: {result:?}"
        );
        assert_eq!(
            database_before,
            fs::read(first.db().join("cert9.db")).expect("should read unchanged DB")
        );
        assert!(
            !second.journal().exists(),
            "rejected import must not be recorded as managed trust"
        );
        if managed {
            assert!(
                first.run("uninstall").status.success(),
                "first CA must remain removable through its original journal"
            );
        } else {
            assert!(
                Command::new(&tool)
                    .args(["-D", "-n", &nickname, "-d", &db])
                    .status()
                    .expect("should remove untouched manual import")
                    .success()
            );
        }
    }
}

#[test]
#[ignore = "requires certutil and openssl; all stores and HOME are disposable"]
fn real_nss_preflight_preserves_leaf_intermediate_and_multicert_exports() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.db()).expect("should create isolated DB");
    let tool = which::which("certutil").expect("should have NSS tools");
    let db = format!("sql:{}", fixture.db().display());
    assert!(
        Command::new(&tool)
            .args(["-N", "--empty-password", "-d", &db])
            .status()
            .expect("should initialize NSS")
            .success()
    );
    // Two unrelated leaves share a multi-valued RDN. A named PEM export contains both.
    for index in 0..2 {
        let cert = fixture.root.path().join(format!("leaf-{index}.pem"));
        let result = Command::new("openssl")
            .env("HOME", fixture.root.path())
            .args([
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/O=example.com/CN=leaf.example.com+OU=Test",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
                "-addext",
                "subjectAltName=DNS:leaf.example.com,URI:spiffe://example.com/service",
                "-keyout",
            ])
            .arg(fixture.root.path().join(format!("leaf-{index}-key.pem")))
            .arg("-out")
            .arg(&cert)
            .output()
            .expect("should generate multi-valued subject fixture");
        assert!(result.status.success(), "{result:?}");
        assert!(
            Command::new(&tool)
                .args([
                    "-A",
                    "-n",
                    &format!("Unrelated leaf {index}"),
                    "-t",
                    ",,",
                    "-d",
                    &db,
                    "-i"
                ])
                .arg(cert)
                .status()
                .expect("should import unrelated leaf")
                .success()
        );
    }
    let key_pem = fs::read_to_string(fixture.ca().join("ca-key.pem"))
        .expect("should read fixture issuer key");
    let cert_pem = fs::read_to_string(fixture.ca().join("ca-cert.pem"))
        .expect("should read fixture issuer cert");
    let issuer_key = rcgen::KeyPair::from_pem(&key_pem).expect("should parse issuer key");
    let issuer = rcgen::CertificateParams::from_ca_cert_pem(&cert_pem)
        .expect("should parse issuer")
        .self_signed(&issuer_key)
        .expect("should reconstruct issuer");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())
        .expect("should create intermediate params");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Intermediate example.com CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    let key = rcgen::KeyPair::generate().expect("should generate intermediate key");
    let cert = params
        .signed_by(&key, &issuer, &issuer_key)
        .expect("should issue intermediate");
    let path = fixture.root.path().join("intermediate.pem");
    fs::write(&path, cert.pem()).expect("should write intermediate");
    assert!(
        Command::new(&tool)
            .args([
                "-A",
                "-n",
                "Unrelated intermediate",
                "-t",
                ",,",
                "-d",
                &db,
                "-i"
            ])
            .arg(path)
            .status()
            .expect("should import intermediate")
            .success()
    );
    let export = || {
        Command::new(&tool)
            .args(["-L", "-n", "Unrelated leaf 0", "-a", "-d", &db])
            .output()
            .expect("should export leaf subject set")
    };
    let before = export();
    assert_eq!(
        rustls_pemfile::certs(&mut before.stdout.as_slice()).count(),
        2,
        "NSS named export must cover both same-subject leaves"
    );
    for _ in 0..2 {
        let out = fixture.run("install");
        assert!(
            out.status.success(),
            "ordinary existing certificates must not prevent install: {out:?}"
        );
    }
    assert!(fixture.run("uninstall").status.success());
    assert_eq!(
        before.stdout,
        export().stdout,
        "unrelated multi-certificate export must be preserved"
    );
    assert!(
        Command::new(&tool)
            .args(["-L", "-n", "Unrelated intermediate", "-d", &db])
            .output()
            .expect("should query preserved intermediate")
            .status
            .success()
    );
}

fn assert_foreign_nickname_does_not_block_lifecycle(fixture: &Fixture, nickname: &str) {
    fixture.import_certificate(nickname, &fixture.unrelated_certificate());
    let unrelated = fixture.inspect_certificate(nickname);
    for action in ["install", "install", "uninstall", "uninstall", "install"] {
        let out = fixture.run(action);
        assert!(
            out.status.success(),
            "nickname={nickname:?}; {action}: {out:?}"
        );
        assert_eq!(fixture.inspect_certificate(nickname), unrelated);
    }
    let key = fs::read(fixture.ca().join("ca-key.pem")).expect("should read original key");
    let out = fixture.run("regenerate");
    assert!(
        out.status.success(),
        "nickname={nickname:?}; regenerate: {out:?}"
    );
    assert_ne!(
        fs::read(fixture.ca().join("ca-key.pem")).expect("should read rotated key"),
        key
    );
    assert_eq!(fixture.inspect_certificate(nickname), unrelated);
    assert_eq!(
        fs::read_to_string(fixture.journal()).expect("should read cleared journal"),
        "[]"
    );
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_trailing_space_nickname_does_not_block_lifecycle() {
    let fixture = Fixture::new();
    fixture.initialize_nss();
    assert_foreign_nickname_does_not_block_lifecycle(&fixture, "Unrelated example.com CA ");
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_newline_nickname_does_not_block_lifecycle() {
    let fixture = Fixture::new();
    fixture.initialize_nss();
    assert_foreign_nickname_does_not_block_lifecycle(&fixture, "Line One\nLine Two");
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_managed_prefix_in_foreign_nickname_does_not_block_lifecycle() {
    let fixture = Fixture::new();
    assert!(fixture.run("install").status.success());
    let entries: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).expect("should read journal"))
            .expect("should parse journal");
    let nickname = entries[0]["nickname"]
        .as_str()
        .expect("should have nickname");
    assert!(fixture.run("uninstall").status.success());
    assert_foreign_nickname_does_not_block_lifecycle(
        &fixture,
        &format!("{nickname} foreign-alias"),
    );
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_trailing_space_alias_cannot_hide_same_subject_conflict() {
    let first = Fixture::new();
    let second = Fixture::new();
    first.initialize_nss();
    // The human-readable table renders these two distinct nicknames identically.
    let nickname = "Foreign example.com CA";
    let conflicting_nickname = "Foreign example.com CA ";
    first.import_certificate(nickname, &first.unrelated_certificate());
    first.import_certificate(conflicting_nickname, &first.ca().join("ca-cert.pem"));
    let unrelated = first.inspect_certificate(nickname);
    let conflicting = first.inspect_certificate(conflicting_nickname);
    let before = fs::read(first.db().join("cert9.db")).expect("should snapshot NSS database");
    let out = second
        .command("install")
        .env("XDG_DATA_HOME", first.root.path().join("data"))
        .output()
        .expect("should attempt conflicting install");
    assert!(
        fs::read(first.db().join("cert9.db")).expect("should read unchanged NSS database")
            == before,
        "conflicting install must not mutate NSS: {out:?}"
    );
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("managed certificate identity conflict"),
        "{out:?}"
    );
    assert!(!second.journal().exists());
    assert_eq!(first.inspect_certificate(nickname), unrelated);
    assert_eq!(first.inspect_certificate(conflicting_nickname), conflicting);
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_ambiguous_managed_nickname_preserves_material_and_trust() {
    let nickname = "ts-dev-proxy-0123456789abcdef";
    for foreign in [
        format!("{nickname} "),
        format!("Foreign\n{nickname} C,,\nAnother"),
    ] {
        let fixture = Fixture::new();
        fixture.record();
        fixture.initialize_nss();
        fixture.import_certificate(&foreign, &fixture.unrelated_certificate());
        let foreign_before = fixture.inspect_certificate(&foreign);
        let database_before =
            fs::read(fixture.db().join("cert9.db")).expect("should snapshot NSS database");
        let tool = which::which("certutil").expect("should have NSS tools");
        // Display padding or embedded newlines can imitate an actual managed row.
        // A failed exact lookup must not authorize deletion or rotation.
        let out = fixture
            .assert_rotation_fails_unchanged(tool.parent().expect("should have tool directory"));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("PR_FILE_NOT_FOUND_ERROR"),
            "{foreign:?}; regenerate: {out:?}"
        );
        for action in ["install", "uninstall"] {
            let out = fixture.run(action);
            assert!(!out.status.success(), "{foreign:?}; {action}: {out:?}");
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("PR_FILE_NOT_FOUND_ERROR"),
                "{foreign:?}; {action}: {out:?}"
            );
        }
        assert_eq!(fixture.inspect_certificate(&foreign), foreign_before);
        assert_eq!(
            fs::read(fixture.db().join("cert9.db")).expect("should read unchanged NSS database"),
            database_before
        );
    }
}

#[test]
#[ignore = "requires certutil; all stores and HOME are disposable"]
fn real_nss_filename_nickname_collision_fails_closed() {
    let fixture = Fixture::new();
    fixture.initialize_nss();
    let nickname = fixture.ca().join("ca-cert.pem");
    fixture.import_certificate(
        &nickname.to_string_lossy(),
        &fixture.unrelated_certificate(),
    );
    let before = fs::read(fixture.db().join("cert9.db")).expect("should snapshot NSS database");
    let out = fixture.run("install");
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("NSS subject query returned an unrelated certificate"),
        "{out:?}"
    );
    assert!(!fixture.journal().exists());
    assert!(
        fs::read(fixture.db().join("cert9.db")).expect("should read unchanged NSS database")
            == before
    );
}

#[test]
fn failed_or_incomplete_subject_queries_stop_before_import() {
    for query in [
        "echo 'subject query failed' >&2; exit 99",
        "exit 0",
        "/bin/cat \"$HOME/ca/ca-cert.pem\"; echo '-----BEGIN CERTIFICATE-----'; echo 'AQID'",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.fake_tool(&format!(
            "case \"$1:$2\" in\n-N:*|-L:-d) exit 0;;\n-L:-n) {query};;\n-A:*) : > \"$HOME/import-attempted\"; exit 99;;\n*) exit 99;;\nesac"
        ));
        let out = fixture
            .command("install")
            .env("PATH", bin)
            .output()
            .expect("should attempt subject query");
        assert!(!out.status.success(), "{query}: {out:?}");
        assert!(!fixture.journal().exists());
        assert!(!fixture.root.path().join("import-attempted").exists());
    }
}

#[test]
fn shared_nss_lock_blocks_commands_from_distinct_ca_directories() {
    let first = Fixture::new();
    let second = Fixture::new();
    first.record();
    let lock = fs::File::create(first.db().join("ts-dev-proxy-trust.lock"))
        .expect("should create shared lock");
    lock.try_lock().expect("should hold shared NSS lock");
    let bin = first.fake_tool("echo 'certutil must not run while locked' >&2; exit 99");
    let out = second
        .command("install")
        .env("XDG_DATA_HOME", first.root.path().join("data"))
        .env("PATH", &bin)
        .output()
        .expect("should attempt install from another CA directory");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("another CA or NSS trust operation"),
        "{stderr}"
    );
    assert!(!stderr.contains("certutil must not run"));
    first.assert_rotation_fails_unchanged(&bin);
    drop(lock);
    // After lock release, confirmed absence can clear the pending record.
    let bin = first
        .fake_tool("test \"$1\" = '-L' || exit 99; echo 'Certificate Nickname Trust Attributes'");
    assert!(
        first
            .command("uninstall")
            .env("PATH", bin)
            .output()
            .expect("should retry after lock release")
            .status
            .success()
    );
}

#[test]
fn malformed_nss_export_stops_install_before_import() {
    let fixture = Fixture::new();
    let bin = fixture.fake_tool("case \"$1:$2\" in\n-N:*) exit 0;;\n-L:-d) echo 'Existing nickname with spaces C,,';;\n-L:-n) echo '-----BEGIN CERTIFICATE-----'; echo 'AQID'; echo '-----END CERTIFICATE-----';;\n-A:*) : > \"$HOME/import-attempted\"; exit 99;;\n*) exit 99;;\nesac");
    let out = fixture
        .command("install")
        .env("PATH", bin)
        .output()
        .expect("should run preflight");
    assert!(!out.status.success());
    assert!(!fixture.journal().exists());
    assert!(!fixture.root.path().join("import-attempted").exists());
}
