pub mod browser;
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
pub fn run(args: ProxyArgs) -> core::result::Result<(), error_stack::Report<ProxyError>> {
    // CA subcommands need only the CA directory — handle them before rule resolution.
    if let Some(ProxySub::Ca { action }) = &args.command {
        let ca_dir = config::ca_dir(&args);
        let cert_path = ca::CertAuthority::cert_path(&ca_dir);
        match action {
            CaCommand::Path => {
                // Ensure the CA exists so the printed path points at a real file.
                ca::CertAuthority::load_or_generate(&ca_dir)
                    .change_context(ProxyError::CertAuthority)?;
                output::info(&cert_path.display().to_string());
            }
            CaCommand::Install => {
                // A fresh machine has no CA yet — generate before trusting it.
                ca::CertAuthority::load_or_generate(&ca_dir)
                    .change_context(ProxyError::CertAuthority)?;
                browser::ca_install(&cert_path);
            }
            CaCommand::Uninstall => browser::ca_uninstall(),
            CaCommand::Regenerate => {
                std::fs::remove_file(&cert_path).ok();
                std::fs::remove_file(ca_dir.join("ca-key.pem")).ok();
                ca::CertAuthority::load_or_generate(&ca_dir)
                    .change_context(ProxyError::CertAuthority)?;
                output::info("regenerated CA — re-run `ca install` to trust it");
            }
        }
        return Ok(());
    }

    let cfg = Arc::new(config::resolve(&args).change_context(ProxyError::Config)?);

    // Recover a leftover Safari proxy state from a previously hard-killed run.
    browser::restore_system_proxy_if_pending(&cfg.ca_dir);

    let ca = Arc::new(
        ca::CertAuthority::load_or_generate(&cfg.ca_dir)
            .change_context(ProxyError::CertAuthority)?,
    );
    let pac: Arc<str> = Arc::from(browser::generate_pac(&cfg.rules, cfg.listen).as_str());

    let runtime = tokio::runtime::Runtime::new().change_context(ProxyError::Server)?;
    runtime.block_on(async move {
        // Bind first: the port is open and connections queue before we launch browsers.
        let listener = server::bind(cfg.listen)
            .await
            .change_context(ProxyError::Server)?;
        output::info(&format!("ts dev proxy listening on {}", cfg.listen));
        let server = tokio::spawn(server::serve_on(
            listener,
            Arc::clone(&cfg),
            Arc::clone(&ca),
            Arc::clone(&pac),
        ));

        if !cfg.launch.is_empty() {
            // Browser launch spawns processes (blocking) — keep it off the reactor thread.
            let launch_cfg = Arc::clone(&cfg);
            tokio::task::spawn_blocking(move || browser::launch(&launch_cfg.launch, &launch_cfg))
                .await
                .change_context(ProxyError::Browser)??;
        }

        // Race the server against Ctrl-C.  On clean interrupt, restore any
        // system proxy state that was changed for Safari before exiting.
        tokio::select! {
            result = server => result.change_context(ProxyError::Server)?,
            _ = tokio::signal::ctrl_c() => {
                browser::restore_system_proxy_if_pending(&cfg.ca_dir);
                Ok(())
            }
        }
    })
}
