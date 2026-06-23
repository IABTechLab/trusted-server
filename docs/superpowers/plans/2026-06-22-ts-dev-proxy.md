# `ts dev proxy` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a local TLS-terminating (MITM) dev proxy, shipped as `ts dev proxy`, that serves a production publisher hostname from a dev/staging upstream by swapping the TLS SNI, using a per-machine local CA so Chrome/Firefox/Safari all trust it.

**Architecture:** A native host binary (`crates/trusted-server-cli`, **excluded** from the wasm workspace) built on tokio + hyper + rustls + rcgen. The accept loop handles `CONNECT`: it matches the authority against a rule table _before_ replying, blind-tunnels unmatched hosts, and MITM-terminates matched hosts with a leaf minted from a local CA, rewriting SNI→`TO` while preserving `Host: FROM`. Pure logic (rule matching, header outcomes, PAC generation, config resolution) is isolated from I/O so it is unit-testable without sockets.

**Tech Stack:** Rust 2024 edition; `tokio` (`net`, `rt-multi-thread`, `macros`, `io-util`), `hyper` 1 + `hyper-util`, `rustls` 0.23 + `tokio-rustls` 0.26, `rcgen` 0.13 (`Issuer`/leaf minting), `rustls-pemfile` 2, `clap` 4 (derive), `error-stack` 0.6, `derive_more` 2, `log`, `base64` 0.22, `directories` (platform data dir). The spec is the source of truth: [docs/superpowers/specs/2026-06-22-ts-dev-proxy-design.md](../specs/2026-06-22-ts-dev-proxy-design.md).

## Global Constraints

- Rust **2024 edition**; the crate is **excluded** from `[workspace]` (the repo pins `build.target = "wasm32-wasip1"` in `.cargo/config.toml`; this binary is native). Build/run with an explicit native target: `cargo … --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')"`.
- Excluded crates inherit no `[workspace.dependencies]`/`[workspace.lints]`: pin deps directly and declare an own `[lints.clippy]` mirroring the workspace (deny `unwrap_used`, deny `panic`).
- No `unwrap()`/`panic!`/`println!`/`eprintln!` in non-test code: use `expect("should …")` only where truly infallible, `error-stack` `Report<E>` for fallible paths, `log::*` for instrumentation, and a single binary-scoped output helper (`#![allow(clippy::print_stdout)]` only in that helper module) for user-facing stdout.
- Errors: concrete enums with `derive_more::Display` + `impl core::error::Error`; `ensure!`/`bail!`; `change_context`/`attach`. Import `Error` from `core::error`.
- Example/fictional data only in tests/docs (e.g. `www.example-publisher.com`, `*.edgecompute.app`, `example.com`). No real domains/credentials.
- Default `Host` upstream is **`FROM`** (preserve production host); `--rewrite-host` sends `Host = TO`. SNI is always `TO` **host only** (port stripped).
- Proxy binds **loopback only** unless `--allow-non-loopback`; off loopback, unmatched `CONNECT` is refused `403` (never blind-tunneled).
- CA: CN `Trusted Server DEV-ONLY Proxy CA — DO NOT TRUST IN PRODUCTION`; key file `0600`, dir `0700`; never committed; leaf SAN = host, validity ≤ 90 days; ALPN `http/1.1`.
- Commit after every green step. Commit subjects: sentence case, imperative, no semantic prefixes, no AI bylines.

---

## File Structure

```
crates/trusted-server-cli/
  Cargo.toml          # [[bin]] name = "ts"; native deps; own [lints.clippy]
  src/
    lib.rs            # library root (Cli, run); tests import this, not the bin
    main.rs           # thin bin: parse Cli, call run(), exit
    output.rs         # user-facing stdout/stderr helper (#![allow(clippy::print_stdout)])
    commands/
      mod.rs
      dev/
        mod.rs        # Dev subcommand group
        proxy/
          mod.rs      # ProxyArgs (clap), CaArgs; orchestration entrypoint
          rewrite.rs  # Rule, RuleTable, Match, RewriteOutcome — pure logic
          config.rs   # ResolvedConfig: arg resolution into a rule table
          ca.rs       # CertAuthority: load-or-generate, mint+cache leaves
          server.rs   # accept loop, CONNECT dispatch, blind tunnel, MITM, local routes
          browser.rs  # PAC generation; Chrome/Firefox/Safari launch+configure; ca install/uninstall
Cargo.toml (workspace root)  # add crates/trusted-server-cli to [workspace].exclude
```

One responsibility per file. `rewrite.rs`, `config.rs`, `browser.rs` (PAC gen) are pure and fully unit-tested; `ca.rs` is testable against a temp dir; `server.rs` is covered by the native integration test (Task 4).

---

## Task 1: Crate skeleton + workspace wiring + CLI surface

**Files:**

- Modify: `Cargo.toml` (workspace root) — add to `[workspace].exclude`
- Create: `crates/trusted-server-cli/Cargo.toml`
- Create: `crates/trusted-server-cli/src/lib.rs`
- Create: `crates/trusted-server-cli/src/main.rs`
- Create: `crates/trusted-server-cli/src/output.rs`
- Create: `crates/trusted-server-cli/src/commands/mod.rs`
- Create: `crates/trusted-server-cli/src/commands/dev/mod.rs`
- Create: `crates/trusted-server-cli/src/commands/dev/proxy/mod.rs`

**Interfaces:**

- Produces: `ProxyArgs` (clap-derived struct, fields below), `CaCommand` enum (`Path`/`Install`/`Uninstall`/`Regenerate`), `run(args: ProxyArgs) -> error_stack::Result<(), ProxyError>` (stub), `output::info(&str)` / `output::warn(&str)`.

- [ ] **Step 1: Add the crate to the workspace exclude list**

In root `Cargo.toml`, add the new crate beside `integration-tests`:

```toml
exclude = [
    "crates/integration-tests",
    "crates/openrtb-codegen",
    "crates/trusted-server-cli",
]
```

- [ ] **Step 2: Write `crates/trusted-server-cli/Cargo.toml`**

```toml
[package]
name = "trusted-server-cli"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
name = "trusted_server_cli"
path = "src/lib.rs"

[[bin]]
name = "ts"
path = "src/main.rs"

[dependencies]
tokio = { version = "1", features = ["net", "rt-multi-thread", "macros", "io-util", "signal"] }
hyper = { version = "1", features = ["http1", "server", "client"] }
hyper-util = { version = "0.1", features = ["tokio"] }
rustls = "0.23"
tokio-rustls = "0.26"
rcgen = "0.13"
time = "0.3"
rustls-pemfile = "2"
clap = { version = "4", features = ["derive"] }
error-stack = "0.6"
derive_more = { version = "2.0", features = ["display", "error"] }
log = "0.4"
env_logger = "0.11"
base64 = "0.22"
directories = "5"

[dev-dependencies]
tempfile = "3"
reqwest = { version = "0.12", features = ["blocking"] }

[lints.clippy]
unwrap_used = "deny"
panic = "deny"
print_stdout = "warn"
print_stderr = "warn"
```

- [ ] **Step 3: Write the output helper `src/output.rs`**

```rust
//! User-facing console output for the `ts` binary.
//!
//! This is the only module permitted to write to stdout/stderr directly;
//! everything else uses `log`.
#![allow(clippy::print_stdout, clippy::print_stderr)]

/// Prints an informational line to stdout.
pub fn info(message: &str) {
    println!("{message}");
}

/// Prints a warning line to stderr.
pub fn warn(message: &str) {
    eprintln!("warning: {message}");
}
```

- [ ] **Step 4: Write the library root `src/lib.rs`**

The crate is a **library + thin bin** so that integration tests (Task 5) can import `config`/`ca`/`server` — Rust integration tests can only reach a library target, not a binary's private modules.

```rust
//! Trusted Server developer CLI library. The `ts` binary is a thin wrapper;
//! all logic lives here so integration tests can exercise it.
pub mod commands;
pub mod output;

use clap::Parser;
use commands::dev::DevCommand;

/// The `ts` command-line interface.
#[derive(Debug, Parser)]
#[command(name = "ts", version, about = "Trusted Server developer CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Debug, clap::Subcommand)]
enum TopCommand {
    /// Local development tools.
    #[command(subcommand)]
    Dev(DevCommand),
}

impl Cli {
    /// Runs the parsed CLI, returning a process exit code.
    #[must_use]
    pub fn run(self) -> i32 {
        let result = match self.command {
            TopCommand::Dev(dev) => commands::dev::run(dev),
        };
        if let Err(report) = result {
            output::warn(&format!("{report:?}"));
            return 1;
        }
        0
    }
}
```

- [ ] **Step 5: Write the thin binary `src/main.rs`**

```rust
use clap::Parser as _;
use trusted_server_cli::Cli;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    std::process::exit(Cli::parse().run());
}
```

- [ ] **Step 5b: Write `src/commands/mod.rs` and `src/commands/dev/mod.rs`**

`src/commands/mod.rs`:

```rust
pub mod dev;
```

`src/commands/dev/mod.rs`:

```rust
pub mod proxy;

use proxy::{ProxyArgs, ProxyError};

/// The `ts dev …` command group.
#[derive(Debug, clap::Subcommand)]
pub enum DevCommand {
    /// Run the local production-hostname dev proxy.
    Proxy(ProxyArgs),
}

/// Dispatches a `dev` subcommand.
///
/// # Errors
/// Propagates failures from the chosen subcommand.
pub fn run(command: DevCommand) -> error_stack::Result<(), ProxyError> {
    match command {
        DevCommand::Proxy(args) => proxy::run(args),
    }
}
```

- [ ] **Step 6: Write `src/commands/dev/proxy/mod.rs` with the full arg surface and a stub `run`**

```rust
pub mod ca;
pub mod config;
pub mod rewrite;

use crate::output;

/// Errors surfaced by `ts dev proxy`.
#[derive(Debug, derive_more::Display)]
pub enum ProxyError {
    /// A rewrite rule could not be parsed or resolved.
    #[display("invalid rule configuration")]
    Config,
    /// The local certificate authority could not be loaded or generated.
    #[display("certificate authority error")]
    CertAuthority,
    /// The proxy server failed to start or run.
    #[display("proxy server error")]
    Server,
    /// A browser could not be launched or configured.
    #[display("browser orchestration error")]
    Browser,
}

impl core::error::Error for ProxyError {}

/// `ts dev proxy [OPTIONS]` — see the design spec §4.
#[derive(Debug, clap::Args)]
pub struct ProxyArgs {
    /// Rewrite rule `FROM=TO` (repeatable).
    #[arg(long = "map", value_name = "FROM=TO")]
    pub map: Vec<String>,

    /// Shorthand single-rule FROM (pairs with --to).
    #[arg(short = 'f', long = "from", value_name = "HOST")]
    pub from: Option<String>,

    /// Shorthand single-rule TO (`HOST[:PORT]`).
    #[arg(short = 't', long = "to", value_name = "HOST[:PORT]")]
    pub to: Option<String>,

    /// Proxy listen address. Non-loopback requires `--allow-non-loopback`.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8080")]
    pub listen: String,

    /// Permit binding a non-loopback `--listen` (disables blind tunnel/forward).
    #[arg(long)]
    pub allow_non_loopback: bool,

    /// Browsers to launch + configure (comma list or `all`).
    #[arg(long, value_name = "LIST")]
    pub launch: Option<String>,

    /// Send `Host: <TO>` upstream instead of the default `<FROM>`.
    #[arg(long)]
    pub rewrite_host: bool,

    /// Inject `Authorization: Basic …` (convenience only — visible in `ps`).
    #[arg(long, value_name = "USER:PASS")]
    pub basic_auth: Option<String>,

    /// Read `USER:PASS` from a file (preferred over `--basic-auth`).
    #[arg(long, value_name = "PATH")]
    pub basic_auth_file: Option<String>,

    /// Skip upstream certificate verification.
    #[arg(long)]
    pub insecure: bool,

    /// Connect to upstream over plaintext HTTP.
    #[arg(long)]
    pub upstream_plaintext: bool,

    /// Directory holding the per-machine CA cert/key.
    #[arg(long, value_name = "PATH")]
    pub ca_dir: Option<String>,

    /// Optional nested subcommand (`ts dev proxy ca …`). When absent, the proxy
    /// runs with the options above.
    #[command(subcommand)]
    pub command: Option<ProxySub>,
}

/// Nested `ts dev proxy <sub>` commands. A single `ca` wrapper gives the
/// **two-level** path `ts dev proxy ca <action>` required by spec §4.2 — a bare
/// `#[command(subcommand)] CaCommand` would have produced `ts dev proxy install`.
#[derive(Debug, clap::Subcommand)]
pub enum ProxySub {
    /// Manage the per-machine dev CA.
    Ca {
        #[command(subcommand)]
        action: CaCommand,
    },
}

/// `ts dev proxy ca …` companion actions (spec §4.2).
#[derive(Debug, clap::Subcommand)]
pub enum CaCommand {
    /// Print the per-machine CA certificate path.
    Path,
    /// Add the CA to the OS trust store (macOS login keychain).
    Install,
    /// Remove the CA from the OS trust store.
    Uninstall,
    /// Regenerate the per-machine CA (invalidates prior trust).
    Regenerate,
}

/// Runs `ts dev proxy`.
///
/// # Errors
/// Returns [`ProxyError`] if configuration, the CA, the server, or browser
/// orchestration fails.
pub fn run(args: ProxyArgs) -> error_stack::Result<(), ProxyError> {
    output::info(&format!("ts dev proxy: listen={}", args.listen));
    Ok(())
}
```

Add empty `ca.rs`, `config.rs`, `rewrite.rs` with a `//!` doc line so the modules compile (later tasks fill them).

- [ ] **Step 7: Verify it builds and runs on the native target**

Run:

```bash
cargo run --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/host: //p')" -- dev proxy --help
```

Expected: clap prints the `ts dev proxy` help including `--map`, `--rewrite-host`, `--allow-non-loopback`, and the `ca` subcommand. No build errors.

- [ ] **Step 8: Verify the workspace gates still pass (crate stays out of wasm build)**

Run: `cargo check --workspace`
Expected: PASS — the excluded crate is not compiled for `wasm32-wasip1`.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/trusted-server-cli
git commit -m "Add trusted-server-cli crate skeleton with ts dev proxy CLI surface"
```

---

## Task 2: Rewrite core (rule table, matching, header outcomes)

Pure logic, no I/O. Implements spec §8.1–§8.4. This is the most heavily unit-tested module.

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/rewrite.rs`
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**

- Produces:
  - `struct Authority { host: String, port: u16, default_port: u16 }` with `fn host(&self) -> &str`, `fn is_default_port(&self) -> bool` (port equals the scheme default it was parsed with), `fn host_with_port(&self) -> String` (host, plus `:port` only when non-default), `fn parse(raw: &str, plaintext: bool) -> Result<Authority, RuleError>`.
  - `struct Rule { from: String, to: Authority, preserve_host: bool, plaintext: bool }`.
  - `struct RuleTable(Vec<Rule>)` with `fn first_match(&self, host: &str) -> Option<&Rule>`.
  - `struct RewriteOutcome { sni: String, host_header: String, orig_host: String, scheme_is_tls: bool }`.
  - `fn rewrite_for(rule: &Rule) -> RewriteOutcome`.
  - `enum RuleError` (`derive_more::Display` + `Error`).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rule(from: &str, to: &str, preserve_host: bool, plaintext: bool) -> Rule {
        Rule {
            from: from.to_string(),
            to: Authority::parse(to, plaintext).expect("should parse authority"),
            preserve_host,
            plaintext,
        }
    }

    #[test]
    fn authority_defaults_port_443_for_tls() {
        let a = Authority::parse("staging.example.net", false).expect("should parse");
        assert_eq!(a.host(), "staging.example.net", "should keep host");
        assert_eq!(a.port, 443, "should default to 443 for TLS");
        assert!(a.is_default_port(), "443 is default for TLS");
        assert_eq!(a.host_with_port(), "staging.example.net", "default port omitted");
    }

    #[test]
    fn authority_defaults_port_80_for_plaintext() {
        let a = Authority::parse("localhost", true).expect("should parse");
        assert_eq!(a.port, 80, "should default to 80 for plaintext");
        assert_eq!(a.host_with_port(), "localhost", "default port omitted");
    }

    #[test]
    fn authority_keeps_non_default_port_in_host_header_only() {
        let a = Authority::parse("localhost:3000", true).expect("should parse");
        assert_eq!(a.port, 3000, "should parse explicit port");
        assert!(!a.is_default_port(), "3000 is not default");
        assert_eq!(a.host(), "localhost", "SNI host must exclude port");
        assert_eq!(a.host_with_port(), "localhost:3000", "Host header includes non-default port");
    }

    #[test]
    fn is_default_port_is_scheme_relative() {
        // TLS authority on :80 is NOT default — :80 must appear in Host.
        let tls_80 = Authority::parse("host.example.com:80", false).expect("parse");
        assert!(!tls_80.is_default_port(), "80 is not the TLS default");
        assert_eq!(tls_80.host_with_port(), "host.example.com:80", "Host keeps :80 for TLS");
        // Plaintext authority on :443 is NOT default — :443 must appear in Host.
        let plain_443 = Authority::parse("host.example.com:443", true).expect("parse");
        assert!(!plain_443.is_default_port(), "443 is not the plaintext default");
        assert_eq!(plain_443.host_with_port(), "host.example.com:443", "Host keeps :443 for plaintext");
    }

    #[test]
    fn matching_is_case_insensitive_and_port_stripped() {
        let table = RuleTable(vec![rule("www.example-publisher.com", "to.edgecompute.app", true, false)]);
        let m = table.first_match("WWW.Example-Publisher.COM:443").expect("should match");
        assert_eq!(m.from, "www.example-publisher.com", "match ignores case and port");
        assert!(table.first_match("other.example.com").is_none(), "unmatched host returns None");
    }

    #[test]
    fn first_match_wins() {
        let table = RuleTable(vec![
            rule("a.example.com", "first.edgecompute.app", true, false),
            rule("a.example.com", "second.edgecompute.app", true, false),
        ]);
        assert_eq!(table.first_match("a.example.com").expect("should match").to.host(), "first.edgecompute.app");
    }

    #[test]
    fn rewrite_default_preserves_from_host_and_sets_sni_to_to() {
        let r = rule("www.example-publisher.com", "to.edgecompute.app:8443", true, false);
        let out = rewrite_for(&r);
        assert_eq!(out.sni, "to.edgecompute.app", "SNI is TO host only, no port");
        assert_eq!(out.host_header, "www.example-publisher.com", "default Host is FROM");
        assert_eq!(out.orig_host, "www.example-publisher.com", "X-Orig-Host is FROM");
    }

    #[test]
    fn rewrite_host_uses_to_authority_with_port() {
        let r = rule("www.example-publisher.com", "localhost:3000", false, true);
        let out = rewrite_for(&r);
        assert_eq!(out.sni, "localhost", "SNI never carries a port");
        assert_eq!(out.host_header, "localhost:3000", "rewrite-host sends TO host:port");
        assert_eq!(out.orig_host, "www.example-publisher.com", "X-Orig-Host stays FROM");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" rewrite::`
Expected: FAIL to compile (`Authority`, `RuleTable`, `rewrite_for` undefined).

- [ ] **Step 3: Implement `rewrite.rs`**

```rust
//! Pure request-rewriting logic: rule matching and header outcomes (spec §8).

/// A rewrite-target authority: host plus a resolved port and its scheme default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    /// Hostname only — never used with a port for SNI.
    host: String,
    /// Resolved port (explicit, or the scheme default).
    pub port: u16,
    /// Scheme default for this authority (443 for TLS, 80 for plaintext).
    default_port: u16,
}

/// Errors from parsing/validating rules.
#[derive(Debug, derive_more::Display)]
pub enum RuleError {
    /// The `--map FROM=TO` value was not `FROM=TO`.
    #[display("expected FROM=TO, got `{value}`")]
    Map { value: String },
    /// The authority port was not a valid `u16`.
    #[display("invalid port in `{value}`")]
    Port { value: String },
    /// The authority host was empty.
    #[display("empty host in `{value}`")]
    EmptyHost { value: String },
}

impl core::error::Error for RuleError {}

impl Authority {
    /// Parses `HOST[:PORT]`, defaulting the port from `plaintext` (80) or TLS (443).
    ///
    /// # Errors
    /// Returns [`RuleError`] on an empty host or an unparseable port.
    pub fn parse(raw: &str, plaintext: bool) -> Result<Self, RuleError> {
        let default_port = if plaintext { 80 } else { 443 };
        let (host, port) = match raw.rsplit_once(':') {
            Some((h, p)) => {
                let port = p
                    .parse::<u16>()
                    .map_err(|_| RuleError::Port { value: raw.to_string() })?;
                (h, port)
            }
            None => (raw, default_port),
        };
        if host.is_empty() {
            return Err(RuleError::EmptyHost { value: raw.to_string() });
        }
        Ok(Self { host: host.to_ascii_lowercase(), port, default_port })
    }

    /// The bare hostname (for SNI and connection target).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether the port equals this authority's scheme default (443 TLS / 80
    /// plaintext) — so `:port` is omitted from the `Host` header.
    #[must_use]
    pub fn is_default_port(&self) -> bool {
        self.port == self.default_port
    }

    /// `host`, plus `:port` only when the port is non-default — for the `Host` header.
    #[must_use]
    pub fn host_with_port(&self) -> String {
        if self.is_default_port() {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// A single rewrite rule.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Production hostname to match (stored lowercase, port-stripped).
    pub from: String,
    /// Upstream target.
    pub to: Authority,
    /// When true (default), send `Host: FROM`; when false, send `Host: TO`.
    pub preserve_host: bool,
    /// Connect to the upstream over plaintext HTTP.
    pub plaintext: bool,
}

/// An ordered set of rules; first match wins.
#[derive(Debug, Clone, Default)]
pub struct RuleTable(pub Vec<Rule>);

impl RuleTable {
    /// Returns the first rule matching `host`, comparing case-insensitively and
    /// ignoring any `:port`.
    #[must_use]
    pub fn first_match(&self, host: &str) -> Option<&Rule> {
        let needle = host
            .rsplit_once(':')
            .map_or(host, |(h, _)| h)
            .to_ascii_lowercase();
        self.0.iter().find(|r| r.from == needle)
    }
}

/// The header/SNI decisions for a matched rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// SNI to present upstream (TO host only, no port).
    pub sni: String,
    /// Value for the upstream `Host` header.
    pub host_header: String,
    /// Value for the `X-Orig-Host` header (always FROM).
    pub orig_host: String,
    /// Whether the upstream leg is TLS (`!plaintext`).
    pub scheme_is_tls: bool,
}

/// Computes the rewrite outcome for a matched rule (spec §8.3).
#[must_use]
pub fn rewrite_for(rule: &Rule) -> RewriteOutcome {
    let host_header = if rule.preserve_host {
        rule.from.clone()
    } else {
        rule.to.host_with_port()
    };
    RewriteOutcome {
        sni: rule.to.host().to_string(),
        host_header,
        orig_host: rule.from.clone(),
        scheme_is_tls: !rule.plaintext,
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" rewrite::`
Expected: PASS (8 tests).

- [ ] **Step 5: Lint the crate**

Run: `cargo clippy --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/commands/dev/proxy/rewrite.rs
git commit -m "Add rewrite core with rule matching and header outcomes"
```

---

## Task 3: Config resolution (args + rule construction)

Turns `ProxyArgs` into a `ResolvedConfig` holding a `RuleTable` and effective settings. Pure logic. Rules are passed explicitly (`--map`/`-f`/`-t`); a missing rule produces a clear `NoRule` error. The tool is **flags-only** — no `TS_DEV_PROXY_*` env overrides and no `trusted-server.toml` inference.

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/config.rs`
- Test: same file

**Interfaces:**

- Consumes: `ProxyArgs` (Task 1), `Rule`/`RuleTable`/`Authority`/`RuleError` (Task 2).
- Produces:
  - `struct ResolvedConfig { rules: RuleTable, listen: SocketAddr, allow_non_loopback: bool, launch: Vec<Browser>, insecure: bool, basic_auth: Option<BasicAuth>, ca_dir: PathBuf }`.
  - `struct BasicAuth { user: String, pass: String }` with `fn header_value(&self) -> String` (returns `Basic base64(user:pass)`).
  - `enum Browser { Chrome, Firefox, Safari }` with `fn parse_list(raw: &str) -> Result<Vec<Browser>, ConfigError>`.
  - `fn resolve(args: &ProxyArgs) -> error_stack::Result<ResolvedConfig, ConfigError>`.
  - `fn ca_dir(args: &ProxyArgs) -> PathBuf` — CA-dir resolution **independent of rules**, so `ca` subcommands run without a rewrite rule.
  - `enum ConfigError` (`Display` + `Error`).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> crate::commands::dev::proxy::ProxyArgs {
        // Construct via clap so defaults match the real surface.
        use clap::Parser;
        #[derive(clap::Parser)]
        struct W { #[command(flatten)] a: crate::commands::dev::proxy::ProxyArgs }
        W::parse_from(["ts"]).a
    }

    #[test]
    fn single_rule_from_to_defaults_to_preserve_host() {
        let mut args = base_args();
        args.from = Some("www.example-publisher.com".into());
        args.to = Some("to.edgecompute.app".into());
        let cfg = resolve(&args).expect("should resolve");
        let rule = cfg.rules.first_match("www.example-publisher.com").expect("rule present");
        assert!(rule.preserve_host, "default preserves FROM host");
        assert_eq!(rule.to.host(), "to.edgecompute.app");
    }

    #[test]
    fn rewrite_host_flag_clears_preserve_host() {
        let mut args = base_args();
        args.map = vec!["www.example-publisher.com=to.edgecompute.app".into()];
        args.rewrite_host = true;
        let cfg = resolve(&args).expect("should resolve");
        assert!(!cfg.rules.first_match("www.example-publisher.com").expect("rule").preserve_host);
    }

    #[test]
    fn map_value_must_be_from_equals_to() {
        let mut args = base_args();
        args.map = vec!["not-a-map".into()];
        assert!(resolve(&args).is_err(), "malformed --map errors");
    }

    #[test]
    fn non_loopback_listen_requires_flag() {
        let mut args = base_args();
        args.map = vec!["a.example.com=b.edgecompute.app".into()];
        args.listen = "0.0.0.0:8080".into();
        assert!(resolve(&args).is_err(), "non-loopback without flag is rejected");
        args.allow_non_loopback = true;
        assert!(resolve(&args).is_ok(), "non-loopback allowed with flag");
    }

    #[test]
    fn basic_auth_header_is_base64() {
        let auth = BasicAuth { user: "dev".into(), pass: "secret".into() };
        assert_eq!(auth.header_value(), "Basic ZGV2OnNlY3JldA==", "Basic base64(user:pass)");
    }

    #[test]
    fn browser_list_parses_all() {
        assert_eq!(Browser::parse_list("all").expect("parses"), vec![Browser::Chrome, Browser::Firefox, Browser::Safari]);
        assert_eq!(Browser::parse_list("firefox,chrome").expect("parses"), vec![Browser::Firefox, Browser::Chrome]);
        assert!(Browser::parse_list("netscape").is_err(), "unknown browser errors");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" config::`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `config.rs`**

```rust
//! Resolves `ProxyArgs` (+ env, defaults) into a concrete [`ResolvedConfig`].

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use base64::Engine as _;
use error_stack::{Report, ResultExt as _};

use super::ProxyArgs;
use super::rewrite::{Authority, Rule, RuleTable};

/// Errors from configuration resolution.
#[derive(Debug, derive_more::Display)]
pub enum ConfigError {
    /// No usable rule was passed.
    #[display("no rewrite rule: pass --map FROM=TO (or -f/--from with -t/--to)")]
    NoRule,
    /// A `--map`/authority value was malformed.
    #[display("invalid rule value")]
    Rule,
    /// `--listen` was not a valid socket address.
    #[display("invalid --listen address `{value}`")]
    Listen { value: String },
    /// A non-loopback listen address was given without `--allow-non-loopback`.
    #[display("--listen {value} is non-loopback; pass --allow-non-loopback to allow it")]
    NonLoopback { value: String },
    /// `--basic-auth`/file value was not `USER:PASS`.
    #[display("--basic-auth must be USER:PASS")]
    BasicAuth,
    /// An unknown browser name was passed to `--launch`.
    #[display("unknown browser `{value}` (expected chrome|firefox|safari|all)")]
    Browser { value: String },
}

impl core::error::Error for ConfigError {}

/// Basic-auth credentials to inject upstream.
#[derive(Debug, Clone)]
pub struct BasicAuth {
    pub user: String,
    pub pass: String,
}

impl BasicAuth {
    /// The `Authorization` header value (`Basic base64(user:pass)`).
    #[must_use]
    pub fn header_value(&self) -> String {
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.user, self.pass));
        format!("Basic {token}")
    }

    fn parse(raw: &str) -> Result<Self, ConfigError> {
        let (user, pass) = raw.split_once(':').ok_or(ConfigError::BasicAuth)?;
        Ok(Self { user: user.to_string(), pass: pass.to_string() })
    }
}

/// A browser the proxy can launch and configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Firefox,
    Safari,
}

impl Browser {
    /// Parses a comma list (or `all`) of browser names.
    ///
    /// # Errors
    /// Returns [`ConfigError::Browser`] on an unknown name.
    pub fn parse_list(raw: &str) -> Result<Vec<Self>, ConfigError> {
        if raw.trim() == "all" {
            return Ok(vec![Self::Chrome, Self::Firefox, Self::Safari]);
        }
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|name| match name {
                "chrome" => Ok(Self::Chrome),
                "firefox" => Ok(Self::Firefox),
                "safari" => Ok(Self::Safari),
                other => Err(ConfigError::Browser { value: other.to_string() }),
            })
            .collect()
    }
}

/// Fully-resolved proxy configuration.
#[derive(Debug)]
pub struct ResolvedConfig {
    pub rules: RuleTable,
    pub listen: SocketAddr,
    pub allow_non_loopback: bool,
    pub launch: Vec<Browser>,
    pub insecure: bool,
    pub basic_auth: Option<BasicAuth>,
    pub ca_dir: PathBuf,
}

/// Default CA directory (spec §7.1/§12): `$XDG_DATA_HOME/trusted-server/dev-proxy`,
/// or the platform data dir on macOS (`~/Library/Application Support/...`).
///
/// `ProjectDirs::from(...)` is **not** used — it yields a reverse-DNS leaf
/// (`com.trusted-server.dev-proxy`), not the spec's `trusted-server/dev-proxy`.
fn default_ca_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| directories::BaseDirs::new().map(|d| d.data_dir().to_path_buf()));
    match base {
        Some(dir) => dir.join("trusted-server").join("dev-proxy"),
        None => PathBuf::from(".trusted-server-dev-proxy"),
    }
}

/// Resolves the CA directory **independently of rule resolution**, so the `ca`
/// subcommands work without a `--map`/`--to` (spec §4.2).
#[must_use]
pub fn ca_dir(args: &ProxyArgs) -> PathBuf {
    args.ca_dir.as_ref().map_or_else(default_ca_dir, PathBuf::from)
}

fn build_rules(args: &ProxyArgs) -> Result<RuleTable, ConfigError> {
    let mut rules = Vec::new();
    let preserve_host = !args.rewrite_host;
    for entry in &args.map {
        let (from, to) = entry.split_once('=').ok_or(ConfigError::Rule)?;
        rules.push(make_rule(from, to, preserve_host, args.upstream_plaintext)?);
    }
    if let (Some(from), Some(to)) = (&args.from, &args.to) {
        rules.push(make_rule(from, to, preserve_host, args.upstream_plaintext)?);
    }
    Ok(RuleTable(rules))
}

fn make_rule(from: &str, to: &str, preserve_host: bool, plaintext: bool) -> Result<Rule, ConfigError> {
    let to = Authority::parse(to, plaintext).map_err(|_| ConfigError::Rule)?;
    Ok(Rule { from: from.to_ascii_lowercase(), to, preserve_host, plaintext })
}

/// Resolves arguments into a [`ResolvedConfig`].
///
/// # Errors
/// Returns [`ConfigError`] on malformed rules, an invalid/forbidden listen
/// address, malformed credentials, or an unknown browser.
pub fn resolve(args: &ProxyArgs) -> error_stack::Result<ResolvedConfig, ConfigError> {
    let rules = build_rules(args).map_err(Report::from)?;
    if rules.0.is_empty() {
        return Err(Report::new(ConfigError::NoRule));
    }

    let listen: SocketAddr = args
        .listen
        .parse()
        .change_context_lazy(|| ConfigError::Listen { value: args.listen.clone() })?;
    let is_loopback = match listen.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    };
    if !is_loopback && !args.allow_non_loopback {
        return Err(Report::new(ConfigError::NonLoopback { value: args.listen.clone() }));
    }

    let launch = match &args.launch {
        Some(raw) => Browser::parse_list(raw).map_err(Report::from)?,
        None => Vec::new(),
    };

    let basic_auth = resolve_basic_auth(args).map_err(Report::from)?;
    let ca_dir = ca_dir(args);

    Ok(ResolvedConfig {
        rules,
        listen,
        allow_non_loopback: args.allow_non_loopback,
        launch,
        insecure: args.insecure,
        basic_auth,
        ca_dir,
    })
}

/// Credential precedence: `--basic-auth-file` > `--basic-auth`.
fn resolve_basic_auth(args: &ProxyArgs) -> Result<Option<BasicAuth>, ConfigError> {
    if let Some(path) = &args.basic_auth_file {
        let raw = std::fs::read_to_string(path).map_err(|_| ConfigError::BasicAuth)?;
        return Ok(Some(BasicAuth::parse(raw.trim())?));
    }
    match &args.basic_auth {
        Some(raw) => Ok(Some(BasicAuth::parse(raw)?)),
        None => Ok(None),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" config::`
Expected: PASS (6 tests).

- [ ] **Step 5: Wire `resolve` into `run` and lint**

In `proxy/mod.rs::run`, replace the stub body with config resolution and a log line:

```rust
pub fn run(args: ProxyArgs) -> error_stack::Result<(), ProxyError> {
    let cfg = config::resolve(&args).change_context(ProxyError::Config)?;
    output::info(&format!(
        "ts dev proxy: listen={} rules={} launch={:?}",
        cfg.listen,
        cfg.rules.0.len(),
        cfg.launch,
    ));
    Ok(())
}
```

Add `use error_stack::ResultExt as _;` at the top of `proxy/mod.rs`. Then run:
`cargo clippy --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/commands/dev/proxy
git commit -m "Resolve proxy args and env into a concrete rule table and settings"
```

---

## Task 4: Local Certificate Authority (load-or-generate, mint+cache leaves)

Implements spec §7. Testable against a temp `--ca-dir`.

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/ca.rs`
- Test: same file

**Interfaces:**

- Produces:
  - `struct CertAuthority` with:
    - `fn load_or_generate(ca_dir: &Path) -> error_stack::Result<CertAuthority, CaError>` — reads `ca-cert.pem`/`ca-key.pem` or generates and persists them (dir `0700`, key `0600`); logs a one-time trust hint on generation.
    - `fn server_config(&self, host: &str) -> error_stack::Result<Arc<rustls::ServerConfig>, CaError>` — returns a cached-or-minted leaf `ServerConfig` (ALPN `http/1.1`), keyed by host.
    - `fn cert_path(ca_dir: &Path) -> PathBuf`.
  - `const CA_COMMON_NAME: &str = "Trusted Server DEV-ONLY Proxy CA — DO NOT TRUST IN PRODUCTION";`
  - `enum CaError` (`Display` + `Error`).

> **Crate-API note:** rcgen 0.13 exposes `KeyPair`, `CertificateParams`, `Certificate`, and `Issuer`. Method names (`self_signed`, `signed_by`, `serialize_pem`) have shifted across 0.13.x — verify against `cargo doc -p rcgen` for the pinned version and adjust the calls below if needed; the _shape_ (generate CA → persist PEM → load issuer → mint leaf with SAN) is stable.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn generates_then_reloads_with_0600_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ca1 = CertAuthority::load_or_generate(dir.path()).expect("should generate");
        let key_path = dir.path().join("ca-key.pem");
        assert!(key_path.exists(), "key persisted");
        let mode = std::fs::metadata(&key_path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file is 0600");

        // Second run reloads the same CA cert bytes (no regeneration).
        let cert_before = std::fs::read(dir.path().join("ca-cert.pem")).expect("read");
        let _ca2 = CertAuthority::load_or_generate(dir.path()).expect("should reload");
        let cert_after = std::fs::read(dir.path().join("ca-cert.pem")).expect("read");
        assert_eq!(cert_before, cert_after, "reload does not rewrite the CA");
        drop(ca1);
    }

    #[test]
    fn leaf_cache_returns_same_arc_for_same_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ca = CertAuthority::load_or_generate(dir.path()).expect("generate");
        let a = ca.server_config("www.example-publisher.com").expect("mint");
        let b = ca.server_config("www.example-publisher.com").expect("cached");
        assert!(Arc::ptr_eq(&a, &b), "same host returns the cached Arc");
        let c = ca.server_config("other.example.com").expect("mint other");
        assert!(!Arc::ptr_eq(&a, &c), "different host mints a new config");
    }

    #[test]
    fn mints_leaf_for_ip_literal_host() {
        // An IP-literal host must mint successfully (IP-type SAN, not DNS) — spec §8.3.
        let dir = tempfile::tempdir().expect("tempdir");
        let ca = CertAuthority::load_or_generate(dir.path()).expect("generate");
        assert!(ca.server_config("127.0.0.1").is_ok(), "IP-literal host mints a leaf");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" ca::`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `ca.rs`**

```rust
//! Per-machine local CA: load-or-generate, mint and cache per-host leaves (spec §7).

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use error_stack::{Report, ResultExt as _};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Distinguished CA common name (spec §12).
pub const CA_COMMON_NAME: &str = "Trusted Server DEV-ONLY Proxy CA — DO NOT TRUST IN PRODUCTION";

const CA_CERT_FILE: &str = "ca-cert.pem";
const CA_KEY_FILE: &str = "ca-key.pem";
const LEAF_VALIDITY_DAYS: i64 = 90;

/// Errors from the certificate authority.
#[derive(Debug, derive_more::Display)]
pub enum CaError {
    /// The CA directory could not be created or secured.
    #[display("cannot prepare CA directory")]
    Dir,
    /// Reading/writing a CA PEM file failed.
    #[display("CA file I/O failed")]
    Io,
    /// Certificate generation/signing failed.
    #[display("certificate generation failed")]
    Generate,
    /// Building the rustls server config failed.
    #[display("rustls server config failed")]
    Rustls,
}

impl core::error::Error for CaError {}

/// Loaded CA material plus a per-host leaf cache.
pub struct CertAuthority {
    issuer: Issuer<'static, KeyPair>,
    ca_cert_der: CertificateDer<'static>,
    leaves: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CertAuthority {
    /// Path to the CA certificate under `ca_dir`.
    #[must_use]
    pub fn cert_path(ca_dir: &Path) -> PathBuf {
        ca_dir.join(CA_CERT_FILE)
    }

    /// Loads the CA from `ca_dir`, generating and persisting it on first run.
    ///
    /// # Errors
    /// Returns [`CaError`] on directory, I/O, or generation failures.
    pub fn load_or_generate(ca_dir: &Path) -> error_stack::Result<Self, CaError> {
        let cert_path = ca_dir.join(CA_CERT_FILE);
        let key_path = ca_dir.join(CA_KEY_FILE);

        let (cert_pem, key_pem) = if cert_path.exists() && key_path.exists() {
            (
                fs::read_to_string(&cert_path).change_context(CaError::Io)?,
                fs::read_to_string(&key_path).change_context(CaError::Io)?,
            )
        } else {
            let (cert_pem, key_pem) = Self::generate_pems()?;
            Self::persist(ca_dir, &cert_path, &key_path, &cert_pem, &key_pem)?;
            log::info!(
                "generated dev CA at {} — run `ts dev proxy ca install` to trust it",
                cert_path.display()
            );
            (cert_pem, key_pem)
        };

        let key = KeyPair::from_pem(&key_pem).change_context(CaError::Generate)?;
        let params =
            CertificateParams::from_ca_cert_pem(&cert_pem).change_context(CaError::Generate)?;
        let ca_cert_der = pem_to_cert_der(&cert_pem)?;
        let issuer = Issuer::new(params, key);

        Ok(Self { issuer, ca_cert_der, leaves: Mutex::new(HashMap::new()) })
    }

    /// Returns a cached or freshly minted leaf [`ServerConfig`] for `host`.
    ///
    /// # Errors
    /// Returns [`CaError`] if minting or rustls config construction fails.
    pub fn server_config(&self, host: &str) -> error_stack::Result<Arc<ServerConfig>, CaError> {
        // Fast path: return a cached config without holding the lock during minting.
        {
            let cache = self.leaves.lock().expect("leaf cache lock should not be poisoned");
            if let Some(existing) = cache.get(host) {
                return Ok(Arc::clone(existing));
            }
        }
        let config = Arc::new(self.mint(host)?);
        let mut cache = self.leaves.lock().expect("leaf cache lock should not be poisoned");
        // Double-check: another task may have minted concurrently.
        let entry = cache.entry(host.to_string()).or_insert(config);
        Ok(Arc::clone(entry))
    }

    fn mint(&self, host: &str) -> error_stack::Result<ServerConfig, CaError> {
        let leaf_key = KeyPair::generate().change_context(CaError::Generate)?;
        // Build the SAN explicitly so an IP-literal host gets an IP-type SAN,
        // not a DNS SAN (spec §8.3). DNS names use an Ia5String.
        let san = match host.parse::<std::net::IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(host.try_into().change_context(CaError::Generate)?),
        };
        let mut params =
            CertificateParams::new(Vec::<String>::new()).change_context(CaError::Generate)?;
        params.subject_alt_names = vec![san];
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(LEAF_VALIDITY_DAYS);
        let leaf = params.signed_by(&leaf_key, &self.issuer).change_context(CaError::Generate)?;

        let chain = vec![leaf.der().clone(), self.ca_cert_der.clone()];
        let key_der = PrivateKeyDer::try_from(leaf_key.serialize_der())
            .map_err(|_| Report::new(CaError::Rustls))?;

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key_der)
            .change_context(CaError::Rustls)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }

    fn generate_pems() -> error_stack::Result<(String, String), CaError> {
        let key = KeyPair::generate().change_context(CaError::Generate)?;
        let mut params = CertificateParams::new(Vec::new()).change_context(CaError::Generate)?;
        params.distinguished_name.push(DnType::CommonName, CA_COMMON_NAME);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        // ~10 years from generation (spec §7.1); rotate via `ca regenerate`.
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(3650);
        let cert = params.self_signed(&key).change_context(CaError::Generate)?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    fn persist(
        ca_dir: &Path,
        cert_path: &Path,
        key_path: &Path,
        cert_pem: &str,
        key_pem: &str,
    ) -> error_stack::Result<(), CaError> {
        fs::create_dir_all(ca_dir).change_context(CaError::Dir)?;
        fs::set_permissions(ca_dir, fs::Permissions::from_mode(0o700)).change_context(CaError::Dir)?;
        fs::write(cert_path, cert_pem).change_context(CaError::Io)?;
        fs::write(key_path, key_pem).change_context(CaError::Io)?;
        fs::set_permissions(key_path, fs::Permissions::from_mode(0o600)).change_context(CaError::Io)?;
        Ok(())
    }
}

fn pem_to_cert_der(cert_pem: &str) -> error_stack::Result<CertificateDer<'static>, CaError> {
    let mut reader = std::io::BufReader::new(cert_pem.as_bytes());
    let der = rustls_pemfile::certs(&mut reader)
        .next()
        .ok_or_else(|| Report::new(CaError::Io))?
        .change_context(CaError::Io)?;
    Ok(der)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" ca::`
Expected: PASS (3 tests). If rcgen method names differ for the pinned 0.13.x, adjust per the crate-API note, then re-run.

- [ ] **Step 5: Lint and commit**

Run clippy (as in Task 3 step 5), then:

```bash
git add crates/trusted-server-cli/src/commands/dev/proxy/ca.rs
git commit -m "Add per-machine local CA with leaf minting and caching"
```

---

## Task 5: CONNECT/MITM proxy server (with blind tunnel + local routes)

Implements spec §5. This is the I/O core; it is exercised by a native integration test rather than pure unit tests.

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/server.rs`
- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/mod.rs` (call `server::bind`/`serve_on`)
- Test: `crates/trusted-server-cli/tests/proxy_e2e.rs` (+ `tests/support/mod.rs`)

**Interfaces:**

- Consumes: `ResolvedConfig` (Task 3), `CertAuthority` (Task 4), `RuleTable`/`rewrite_for` (Task 2).
- Produces (both `pub`, reachable from integration tests via the `trusted_server_cli` lib target):
  - `async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener>` — binds the listen socket so the port is open (connections queue) **before** browsers are launched (Task 6).
  - `async fn serve_on(listener: TcpListener, cfg: Arc<ResolvedConfig>, ca: Arc<CertAuthority>, pac: Arc<str>) -> error_stack::Result<(), ProxyError>` — accept loop; serves until the task is dropped. Splitting bind from serve is what makes the launch ordering in Task 6 safe.

**Behavior contract (from spec §5, §8, §11):**

1. Read the first request line. If it is `CONNECT host:port`: match `host` (authority) against `cfg.rules`.
   - **Match:** reply `200`, select `ca.server_config(host)`, TLS-accept, then loop reading HTTP/1.1 requests; for each apply `rewrite_for`, open the upstream (TLS unless `plaintext`; skip cert verify if `insecure`), forward, stream the response back. Close on `Upgrade:`.
   - **No match, loopback bind:** connect upstream **first**, reply `200` only on success (else `502`), then copy bytes both directions.
   - **No match, non-loopback bind:** reply `403`, close.
2. If it is an origin-form `GET /proxy.pac` (a local route): serve `pac` with `Content-Type: application/x-ns-proxy-autoconfig`.
3. Any other absolute-form plain-HTTP request: blind-forward (loopback) or `403` (non-loopback).

- [ ] **Step 1: Write the failing integration test**

Create `crates/trusted-server-cli/tests/proxy_e2e.rs`. It starts a local TLS "upstream" with a self-signed cert, runs the proxy with `--insecure` in a background task, and drives it with a proxy-aware client.

```rust
//! End-to-end proxy test: a matched host is MITM'd, rewritten, and forwarded.
//! Run with: cargo test --manifest-path crates/trusted-server-cli/Cargo.toml \
//!   --target "$(rustc -vV | sed -n 's/host: //p')" --test proxy_e2e

use std::sync::Arc;

// Helper: spin a local HTTPS server that echoes the Host and X-Orig-Host it saw.
// (Implementation uses tokio + tokio-rustls + a rcgen self-signed cert for
// "upstream.localhost"; see fixtures below.)

#[tokio::test]
async fn matched_host_is_rewritten_and_forwarded() {
    // Arrange: start echo upstream; capture its addr.
    let upstream = start_echo_upstream().await;

    // Build a ResolvedConfig mapping FROM=www.example-publisher.com to the
    // upstream addr, preserve_host = true (default), insecure = true.
    let cfg = test_config(&upstream.addr);
    // CA + helpers come from the lib target (Task 1 added `src/lib.rs`):
    // use trusted_server_cli::commands::dev::proxy::{ca, config, server};
    let ca = Arc::new(support::dev_ca());
    // Act: serve in the background, then issue a request through the proxy.
    // The client CONNECTs to www.example-publisher.com:443 via the proxy and
    // trusts the dev CA; SNI/Host are set by the proxy.
    let response = drive_request_through_proxy(cfg, ca).await;

    // Assert: upstream saw Host = FROM and X-Orig-Host = FROM.
    assert_eq!(response.seen_host, "www.example-publisher.com", "Host preserved as FROM");
    assert_eq!(response.seen_orig_host, "www.example-publisher.com", "X-Orig-Host is FROM");
    assert_eq!(response.status, 200, "response streamed back");
}

#[tokio::test]
async fn unmatched_host_is_blind_tunneled_on_loopback() {
    // Arrange: upstream with self-signed CN "upstream.localhost"; NO rule for it; loopback.
    let upstream = start_echo_upstream().await;
    let cfg = test_config_without_rules();
    let ca = Arc::new(support::dev_ca());
    // Act: CONNECT to upstream.localhost through the proxy and capture the leaf
    // certificate the client received during the TLS handshake.
    let observed = support::connect_through_proxy_capturing_cert(
        cfg, ca, &upstream.addr, "upstream.localhost",
    ).await;
    // Assert: the handshake terminated at the UPSTREAM cert, not the dev CA —
    // i.e. the proxy blind-tunneled and did not MITM.
    assert_eq!(observed.issuer_common_name, "upstream.localhost", "blind tunnel presents the upstream cert");
    assert_ne!(observed.issuer_common_name, ca::CA_COMMON_NAME, "proxy did not MITM an unmatched host");
}

#[tokio::test]
async fn basic_auth_injected_when_absent_clears_401() {
    // Arrange: upstream returns 401 unless Authorization is present; cfg injects basic auth.
    let upstream = start_gated_upstream().await;
    let mut cfg = test_config(&upstream.addr);
    cfg.basic_auth = Some(config::BasicAuth { user: "dev".into(), pass: "secret".into() });
    let ca = Arc::new(support::dev_ca());
    // Act + Assert: the injected Authorization clears the gate.
    let response = drive_request_through_proxy(cfg, ca).await;
    assert_eq!(response.status, 200, "injected Basic auth clears the 401");
}

#[tokio::test]
async fn keep_alive_serves_multiple_sequential_requests() {
    // Spec §5/§14: one tunnel carries many sequential keep-alive requests.
    let upstream = start_echo_upstream().await;
    let cfg = test_config(&upstream.addr);
    let ca = Arc::new(support::dev_ca());
    // Act: two requests over ONE MITM tunnel (Connection: keep-alive).
    let responses = support::drive_sequential_requests(cfg, ca, &["/one", "/two"]).await;
    // Assert: both answered in order on the same tunnel.
    assert_eq!(responses.len(), 2, "both requests answered");
    assert!(responses.iter().all(|r| r.status == 200), "each request gets 200");
    assert_eq!(responses[0].path, "/one", "first request");
    assert_eq!(responses[1].path, "/two", "second request reused the tunnel");
}
```

> The test-support module `tests/support/mod.rs` provides: `dev_ca()` (a `CertAuthority` in a `tempfile::tempdir()`); `start_echo_upstream()` (HTTPS server, self-signed CN `upstream.localhost`, echoes the `Host`/`X-Orig-Host`/path it saw); `start_gated_upstream()` (returns `401` unless `Authorization` present); `test_config(addr)` / `test_config_without_rules()`; `drive_request_through_proxy(cfg, ca)`; `connect_through_proxy_capturing_cert(cfg, ca, addr, sni)` (returns the leaf the client saw); and `drive_sequential_requests(cfg, ca, paths)` (multiple requests over one keep-alive tunnel). Build them on `tokio` + `tokio-rustls` + rcgen + a `hyper` client that CONNECTs through `cfg.listen`. They import the crate under test as `use trusted_server_cli::commands::dev::proxy::{ca, config, server};` — possible only because Task 1 made the crate a lib + bin.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" --test proxy_e2e`
Expected: FAIL to compile (`bind`/`serve_on` and helpers undefined).

- [ ] **Step 3: Implement `server.rs`**

Implement `bind` + `serve_on` per the behavior contract. Core skeleton (fill in the forwarding bodies):

```rust
//! Accept loop, CONNECT dispatch, blind tunnel, MITM, and local routes (spec §5).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use error_stack::{Report, ResultExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::ca::CertAuthority;
use super::config::ResolvedConfig;
use super::rewrite::rewrite_for;
use super::ProxyError;

/// Binds the listen socket. Separate from [`serve_on`] so the caller can open
/// the port (queueing connections) before launching browsers (spec §9, Task 6).
///
/// # Errors
/// Returns the bind I/O error if the address is unavailable.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

/// Accepts and serves connections on `listener` until the task is dropped.
///
/// # Errors
/// Returns [`ProxyError::Server`] only on an unrecoverable accept-loop failure.
pub async fn serve_on(
    listener: TcpListener,
    cfg: Arc<ResolvedConfig>,
    ca: Arc<CertAuthority>,
    pac: Arc<str>,
) -> error_stack::Result<(), ProxyError> {
    let is_loopback = matches!(cfg.listen.ip(), IpAddr::V4(v) if v.is_loopback())
        || matches!(cfg.listen.ip(), IpAddr::V6(v) if v.is_loopback());
    log::info!("listening on {}", cfg.listen);
    loop {
        let (client, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                log::warn!("accept failed: {err}");
                continue;
            }
        };
        let cfg = Arc::clone(&cfg);
        let ca = Arc::clone(&ca);
        let pac = Arc::clone(&pac);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(client, is_loopback, &cfg, &ca, &pac).await {
                log::debug!("connection from {peer} ended: {err:?}");
            }
        });
    }
}

async fn handle_connection(
    mut client: TcpStream,
    is_loopback: bool,
    cfg: &ResolvedConfig,
    ca: &CertAuthority,
    pac: &str,
) -> error_stack::Result<(), ProxyError> {
    let head = read_request_head(&mut client).await?;
    if let Some(authority) = head.connect_authority() {
        return handle_connect(client, authority, is_loopback, cfg, ca).await;
    }
    if head.is_local_pac_route() {
        return serve_pac(&mut client, pac).await;
    }
    // Stray absolute-form plain HTTP.
    if is_loopback {
        blind_forward_http(client, &head).await
    } else {
        respond_status(&mut client, 403, "Forbidden").await
    }
}

// read_request_head: parse method/target/Host without consuming the body.
// connect_authority(): Some(host) if method == CONNECT.
// handle_connect(): match rules; on match reply 200 + MITM via ca.server_config;
//   on no-match loopback connect-first-then-200 blind tunnel; non-loopback -> 403.
// For each MITM request: let out = rewrite_for(rule); set Host/X-Orig-Host/SNI;
//   inject cfg.basic_auth if Authorization absent; open upstream (TLS unless
//   plaintext; skip verify if cfg.insecure); stream response; close on Upgrade.
// Redact Authorization/Cookie in any logging.
```

Implement the helper bodies (`read_request_head`, `handle_connect`, `mitm_loop`, `blind_tunnel`, `serve_pac`, `respond_status`, upstream connect with optional `insecure` `ClientConfig`). Use `tokio::io::copy_bidirectional` for the blind tunnel. For the non-loopback path, `handle_connect` must return `403` for unmatched authorities _before_ any upstream connect.

- [ ] **Step 4: Wire `bind` + `serve_on` into `run` (interim — no browsers yet)**

Replace `run` in `proxy/mod.rs` with a running proxy. Task 6 finalizes `run` (adds `ca` subcommand dispatch and browser launch); this interim version just binds and serves:

```rust
pub fn run(args: ProxyArgs) -> error_stack::Result<(), ProxyError> {
    let cfg = Arc::new(config::resolve(&args).change_context(ProxyError::Config)?);
    let ca = Arc::new(ca::CertAuthority::load_or_generate(&cfg.ca_dir).change_context(ProxyError::CertAuthority)?);
    // PAC generator arrives in Task 6; stub it locally for now.
    let pac: Arc<str> = Arc::from("function FindProxyForURL(u,h){return \"DIRECT\";}");
    let runtime = tokio::runtime::Runtime::new().change_context(ProxyError::Server)?;
    runtime.block_on(async move {
        let listener = server::bind(cfg.listen).await.change_context(ProxyError::Server)?;
        output::info(&format!("ts dev proxy listening on {}", cfg.listen));
        server::serve_on(listener, cfg, ca, pac).await
    })
}
```

(`use std::sync::Arc;` at the top of `proxy/mod.rs`.)

- [ ] **Step 5: Run the integration test to verify it passes**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" --test proxy_e2e`
Expected: PASS (4 tests) — matched host rewritten with `Host=FROM` + `X-Orig-Host`; unmatched host blind-tunneled (upstream cert, not dev CA); basic-auth clears `401`; keep-alive serves two sequential requests over one tunnel.

- [ ] **Step 6: Lint and commit**

```bash
git add crates/trusted-server-cli/src/commands/dev/proxy/server.rs crates/trusted-server-cli/src/commands/dev/proxy/mod.rs crates/trusted-server-cli/tests
git commit -m "Add CONNECT MITM proxy server with blind tunnel and local PAC route"
```

---

## Task 6: Browser orchestration, PAC generation, and `ca` subcommands

Implements spec §9 and §4.2/§7.3.

**Files:**

- Create: `crates/trusted-server-cli/src/commands/dev/proxy/browser.rs`
- Modify: `crates/trusted-server-cli/src/commands/dev/proxy/mod.rs` (finalize `run`: `ca` dispatch before resolve + browser launch after bind)
- Test: `browser.rs` (`#[cfg(test)]`) for PAC generation (pure)

**Interfaces:**

- Consumes: `RuleTable` (Task 2), `Browser` (Task 3), `CertAuthority::cert_path` (Task 4).
- Produces:
  - `fn generate_pac(rules: &RuleTable, listen: SocketAddr) -> String`.
  - `fn launch(browsers: &[Browser], cfg: &ResolvedConfig) -> error_stack::Result<(), ProxyError>`.
  - `fn ca_install(cert_path: &Path)`, `fn ca_uninstall()`, `fn ca_path(cert_path: &Path)`, `fn ca_regenerate(ca_dir: &Path)` — invoked from `run` for the `ca` subcommand.

- [ ] **Step 1: Write the failing PAC test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dev::proxy::rewrite::{Authority, Rule, RuleTable};

    #[test]
    fn pac_proxies_only_https_for_from_hosts() {
        let rules = RuleTable(vec![Rule {
            from: "www.example-publisher.com".into(),
            to: Authority::parse("to.edgecompute.app", false).expect("auth"),
            preserve_host: true,
            plaintext: false,
        }]);
        let pac = generate_pac(&rules, "127.0.0.1:8080".parse().expect("addr"));
        assert!(pac.contains("https:"), "PAC guards on https scheme");
        assert!(pac.contains("www.example-publisher.com"), "PAC lists the FROM host");
        assert!(pac.contains("PROXY 127.0.0.1:8080"), "PAC points at the listen addr");
        assert!(pac.contains("return \"DIRECT\""), "everything else is direct");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" browser::`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `browser.rs`**

```rust
//! Browser launch/config, PAC generation, and CA trust commands (spec §9, §7.3).

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;

use error_stack::ResultExt as _;

use super::config::{Browser, ResolvedConfig};
use super::rewrite::RuleTable;
use super::ProxyError;
use crate::output;

/// Generates a PAC that proxies only `https://` requests for matched FROM hosts.
#[must_use]
pub fn generate_pac(rules: &RuleTable, listen: SocketAddr) -> String {
    let mut checks = String::new();
    for rule in &rules.0 {
        checks.push_str(&format!(
            "  if (url.substring(0,6) == \"https:\" && host == \"{}\") return \"PROXY {}\";\n",
            rule.from, listen
        ));
    }
    format!("function FindProxyForURL(url, host) {{\n{checks}  return \"DIRECT\";\n}}\n")
}

/// Adds the CA to the macOS login keychain (spec §7.3).
pub fn ca_install(cert_path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let keychain = format!("{home}/Library/Keychains/login.keychain-db");
        let status = Command::new("security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-k", &keychain])
            .arg(cert_path)
            .status();
        match status {
            Ok(s) if s.success() => output::info("CA added to login keychain"),
            _ => output::warn(&format!(
                "could not auto-install; run: security add-trusted-cert -r trustRoot -k {keychain} {}",
                cert_path.display()
            )),
        }
    }
    #[cfg(not(target_os = "macos"))]
    output::info(&format!("trust this CA manually: {}", cert_path.display()));
}

/// Removes the CA from the macOS keychain (spec §7.3).
pub fn ca_uninstall() {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("security")
            .args(["delete-certificate", "-c", super::ca::CA_COMMON_NAME])
            .status();
        output::info("CA removed from keychain (if present)");
    }
    #[cfg(not(target_os = "macos"))]
    output::info("remove the CA from your OS trust store manually");
}

/// Launches and configures each requested browser against the proxy (spec §9).
///
/// # Errors
/// Returns [`ProxyError::Browser`] only on unrecoverable setup failures; a
/// single browser that cannot be configured logs manual steps and is skipped.
pub fn launch(browsers: &[Browser], cfg: &ResolvedConfig) -> error_stack::Result<(), ProxyError> {
    for browser in browsers {
        match browser {
            Browser::Chrome => launch_chrome(cfg),
            Browser::Firefox => launch_firefox(cfg),
            Browser::Safari => launch_safari(cfg),
        }
    }
    Ok(())
}
```

Implement `launch_chrome` (temp `--user-data-dir`, `--proxy-server="https=127.0.0.1:<port>"`, `--no-first-run`, open the first rule's `FROM` URL), `launch_firefox` (temp profile, write `user.js` with `network.proxy.type=1` + `network.proxy.ssl`/`network.proxy.ssl_port` only, `certutil -A` into the profile NSS DB), and `launch_safari` (serve PAC via the server's local route; detect the active service via `route -n get default` → device → `networksetup -listnetworkserviceorder` mapping → `networksetup -setautoproxyurl <service> http://127.0.0.1:<port>/proxy.pac`; persist prior state to a file and restore on exit + on next run). Each helper logs manual steps on failure and continues.

- [ ] **Step 4: Run the PAC test to verify it passes**

Run: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" browser::`
Expected: PASS.

- [ ] **Step 5: Finalize `run` — `ca` dispatch before resolution, then bind → spawn → launch → await**

Replace the interim `run` (Task 5 step 4) with the final version. Two ordering fixes are essential:

- **`ca` subcommands must be handled _before_ `config::resolve`** — `resolve` errors when no rewrite rule exists, but `ca path/install/uninstall/regenerate` are standalone (spec §4.2). Use `config::ca_dir(&args)`, which needs no rule.
- **Bind the listener _before_ launching browsers**, and keep the runtime alive by awaiting the server. Launching before the socket is bound would point browsers at a dead port; blocking on `serve_on` before launching would never reach the launch.

```rust
pub fn run(args: ProxyArgs) -> error_stack::Result<(), ProxyError> {
    // CA subcommands need only the CA directory — handle them before rule resolution.
    if let Some(ProxySub::Ca { action }) = &args.command {
        let ca_dir = config::ca_dir(&args);
        let cert_path = ca::CertAuthority::cert_path(&ca_dir);
        match action {
            CaCommand::Path => {
                // Ensure the CA exists so the printed path points at a real file.
                ca::CertAuthority::load_or_generate(&ca_dir).change_context(ProxyError::CertAuthority)?;
                output::info(&cert_path.display().to_string());
            }
            CaCommand::Install => {
                // A fresh machine has no CA yet — generate before trusting it.
                ca::CertAuthority::load_or_generate(&ca_dir).change_context(ProxyError::CertAuthority)?;
                browser::ca_install(&cert_path);
            }
            CaCommand::Uninstall => browser::ca_uninstall(),
            CaCommand::Regenerate => {
                std::fs::remove_file(&cert_path).ok();
                std::fs::remove_file(ca_dir.join("ca-key.pem")).ok();
                ca::CertAuthority::load_or_generate(&ca_dir).change_context(ProxyError::CertAuthority)?;
                output::info("regenerated CA — re-run `ca install` to trust it");
            }
        }
        return Ok(());
    }

    let cfg = Arc::new(config::resolve(&args).change_context(ProxyError::Config)?);
    let ca = Arc::new(ca::CertAuthority::load_or_generate(&cfg.ca_dir).change_context(ProxyError::CertAuthority)?);
    let pac: Arc<str> = Arc::from(browser::generate_pac(&cfg.rules, cfg.listen).as_str());

    let runtime = tokio::runtime::Runtime::new().change_context(ProxyError::Server)?;
    runtime.block_on(async move {
        // Bind first: the port is open and connections queue before we launch browsers.
        let listener = server::bind(cfg.listen).await.change_context(ProxyError::Server)?;
        output::info(&format!("ts dev proxy listening on {}", cfg.listen));
        let server = tokio::spawn(server::serve_on(listener, Arc::clone(&cfg), Arc::clone(&ca), Arc::clone(&pac)));

        if !cfg.launch.is_empty() {
            // Browser launch spawns processes (blocking) — keep it off the reactor thread.
            let launch_cfg = Arc::clone(&cfg);
            tokio::task::spawn_blocking(move || browser::launch(&launch_cfg.launch, &launch_cfg))
                .await
                .change_context(ProxyError::Browser)??;
        }
        // Keep the runtime alive: serve until the accept loop ends (Ctrl-C / drop).
        server.await.change_context(ProxyError::Server)?
    })
}
```

- [ ] **Step 6: Lint and commit**

```bash
git add crates/trusted-server-cli/src/commands/dev/proxy
git commit -m "Add browser orchestration, PAC generation, and ca trust subcommands"
```

---

## Task 7: ~~Project-config inference~~ — DROPPED

**Status: removed (scope change 2026-06-22).** Rewrite rules must be passed
explicitly via `--map`/`-f`/`-t`; the proxy does **not** infer them from
`trusted-server.toml` (or any config file). There is no `infer_from_host` /
`infer_to_host`, no `[dev_proxy].upstream` field, and no `toml` dependency. With
no rule, `resolve` returns `ConfigError::NoRule` with a message naming
`--map`/`-f`/`-t` (see spec §10.2).

---

## Task 8: User-facing documentation

Implements spec §15 step 8.

**Files:**

- Create: `docs/guide/ts-dev-proxy.md`

**Interfaces:** none (docs only).

- [ ] **Step 1: Write the guide** — cover: what the proxy does and why (SNI swap, MITM); install/build (`cargo … --manifest-path … --target …`); first-run CA generation + `ca install` per browser (Chrome/Firefox NSS/Safari keychain); the default `Host = FROM` behavior and when to use `--rewrite-host`; `--allow-non-loopback` safety; the §13 troubleshooting table (unknown domain, `401`, `503`, untrusted CA, addr in use); and the per-machine CA security note + `ca uninstall`. Use only example domains.

- [ ] **Step 2: Format docs**

Run: `cd docs && npm run format`
Expected: PASS, no diff on re-run.

- [ ] **Step 3: Commit**

```bash
git add docs/guide/ts-dev-proxy.md
git commit -m "Document ts dev proxy setup, trust, and troubleshooting"
```

---

## Self-Review

**Spec coverage:**

- §3 decisions → Tasks 1–6 (crate exclude, default Host=FROM, blind tunnel, non-loopback). ✓
- §4 CLI surface + §4.2 ca subcommands → Task 1 (args), Task 6 (`ca`). ✓
- §5 architecture/flow (200-ordering, blind tunnel, keep-alive loop, Upgrade close, local routes) → Task 5. ✓
- §6 module structure / workspace exclude / native target → Task 1. ✓
- §7 CA (load-or-generate, 0600/0700, mint+cache, install/uninstall) → Tasks 4, 6. ✓
- §8 rewrite (Authority/RuleTable/matching/header outcomes/port-vs-SNI) → Task 2. ✓
- §9 browser orchestration (HTTPS-only Chrome/Firefox, Safari PAC + active-service) → Task 6. ✓
- §10 config (precedence; explicit rules only — no env vars, no config-file inference) → Task 3. ✓
- §11 security (non-loopback guard, redaction, credential input, blind-tunnel privacy) → Tasks 3, 5. ✓
- §12 constants → encoded in Tasks 2/4 (ports, ALPN, validity, CN). ✓
- §13 error handling → Task 5 status mapping + Task 8 troubleshooting table. ✓
- §14 testing (rewrite unit, ca unit, native integration incl. blind-tunnel, basic-auth, and keep-alive/sequential-request coverage) → Tasks 2, 4, 5. ✓
- §16 out-of-scope (HTTP/2, WebSocket, plain-HTTP rewriting) → respected (Upgrade closed; stray HTTP blind-forwarded only). ✓

**Placeholder scan:** I/O-bound helper bodies in Tasks 5–6 (forwarding loops, browser launch) are described by an explicit behavior contract with signatures rather than full literal bodies, because their exact code depends on the pinned tokio/hyper/rcgen APIs; the pure-logic tasks (2, 3, 6-PAC) carry complete code and tests. Flagged the rcgen API drift explicitly in Task 4. No `TODO`/`TBD` left in committed code.

**Type consistency:** `Authority::{host,host_with_port,is_default_port}` (now scheme-relative via the stored `default_port`), `RuleTable::first_match`, `rewrite_for → RewriteOutcome{sni,host_header,orig_host,scheme_is_tls}`, `ResolvedConfig`, `config::ca_dir`, `CertAuthority::{load_or_generate,server_config,cert_path}`, `server::{bind,serve_on}`, `Browser::parse_list`, `generate_pac` are used consistently across tasks.

**Review-round fixes (2026-06-22):** (1) crate is a **lib + bin** so integration tests reach internal modules; (2) `ca` subcommands resolve via `config::ca_dir` _before_ rule resolution; (3) `run` binds the listener, spawns `serve_on`, launches browsers via `spawn_blocking`, then awaits the server — correct ordering and the runtime stays alive; (4) `Authority` stores its scheme `default_port` so `:80`/`:443` are kept/omitted per scheme.

**Second review round (2026-06-22):** (6) `ca` is a **nested** subcommand (`ProxySub::Ca { action }`) so the path is `ts dev proxy ca <action>`, not `ts dev proxy <action>`; (7) `ca path`/`ca install` call `load_or_generate` first so a fresh machine works before any proxy run; (8) `default_ca_dir` builds `…/trusted-server/dev-proxy` from `XDG_DATA_HOME`/`BaseDirs` (not `ProjectDirs`, which yields a reverse-DNS leaf); (9) CA validity is ~10 years and the leaf ≤ 90 days, both `now`-relative via `time`; (10) the blind-tunnel and basic-auth E2E tests have real assertions, plus a new keep-alive/sequential-request test.

**Third review round (2026-06-22):** (11) `mint` builds the SAN explicitly — `SanType::IpAddress` for an IP-literal host, `SanType::DnsName` otherwise (spec §8.3), with a `127.0.0.1` test.

**Scope change (2026-06-22):** environment-variable support (`TS_DEV_PROXY_*`, former spec §10.3) was **dropped** — the tool is flags-only. `ProxyArgs` no longer carries clap `env`, `build_rules` has no `TS_DEV_PROXY_MAP` path, and `warn_unknown_env` is gone (config tests: 6).

**Scope change (2026-06-22):** project-config inference (former Task 7 / spec §10.2) was **dropped** — rules must be passed explicitly via `--map`/`-f`/`-t`. Removed `infer_from_host`/`infer_to_host`, the `resolve_in` plumbing, the `[dev_proxy].upstream` idea, the `toml` dependency, and the inference tests; `resolve` returns `ConfigError::NoRule` when no rule is given.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-22-ts-dev-proxy.md`.
