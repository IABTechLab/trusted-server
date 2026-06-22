pub mod ca;
pub mod config;
pub mod rewrite;
pub mod server;

use std::sync::Arc;

use error_stack::ResultExt as _;

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

    /// Shorthand single-rule FROM (optional when inferable from config).
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
///
/// Returns [`ProxyError`] if configuration, the CA, the server, or browser
/// orchestration fails.
pub fn run(args: ProxyArgs) -> Result<(), error_stack::Report<ProxyError>> {
    let cfg = Arc::new(config::resolve(&args).change_context(ProxyError::Config)?);
    let ca = Arc::new(
        ca::CertAuthority::load_or_generate(&cfg.ca_dir).change_context(ProxyError::CertAuthority)?,
    );
    // PAC generation arrives in Task 6; serve a DIRECT stub for now.
    let pac: Arc<str> = Arc::from("function FindProxyForURL(u, h) { return \"DIRECT\"; }");
    let runtime = tokio::runtime::Runtime::new().change_context(ProxyError::Server)?;
    runtime.block_on(async move {
        let listener = server::bind(cfg.listen)
            .await
            .change_context(ProxyError::Server)?;
        output::info(&format!("ts dev proxy listening on {}", cfg.listen));
        server::serve_on(listener, cfg, ca, pac).await
    })
}
