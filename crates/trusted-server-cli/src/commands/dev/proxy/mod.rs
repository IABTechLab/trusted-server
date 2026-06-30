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

    /// Shorthand single-rule FROM (pairs with `--to`).
    #[arg(short = 'f', long = "from", value_name = "HOST")]
    pub from: Option<String>,

    /// Shorthand single-rule TO (`HOST[:PORT]`; pairs with `--from`). Keep this a
    /// hostname so the TLS SNI and certificate stay valid; to reach a specific
    /// server by address, pin it with `--resolve` instead of using a bare IP.
    #[arg(short = 't', long = "to", value_name = "HOST[:PORT]")]
    pub to: Option<String>,

    /// Proxy listen address. Non-loopback requires `--allow-non-loopback`.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:18080")]
    pub listen: String,

    /// Permit binding a non-loopback `--listen` (disables blind tunnel/forward).
    #[arg(long)]
    pub allow_non_loopback: bool,

    /// Browsers to launch + configure (comma list or `all`).
    #[arg(long, value_name = "LIST")]
    pub launch: Option<String>,

    /// Send `Host: <TO>` upstream instead of the default `<FROM>`. The TLS SNI is
    /// always the `--to` host; to reach a specific server by address, pin it with
    /// `--resolve` rather than changing the host here.
    #[arg(long)]
    pub rewrite_host: bool,

    /// Pin a host's upstream connection to an address instead of using DNS
    /// (repeatable; like curl's `--resolve`). Keeps `--to` a hostname — so SNI
    /// and the certificate stay valid — while the socket dials the given IP.
    /// Format: `HOST:IP` (e.g. `ts.example.com:192.0.2.10`).
    #[arg(long = "resolve", value_name = "HOST:IP")]
    pub resolve: Vec<String>,

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
            CaCommand::Uninstall => {
                browser::ca_uninstall();
            }
            CaCommand::Regenerate => {
                // Revoke OS trust for the OLD CA first. The old and new CA share
                // CA_COMMON_NAME, so `ca_uninstall` (delete-by-CN, a no-op when
                // absent) removes the soon-to-be-stale cert from the keychain
                // before we replace the files on disk. If revocation cannot be
                // confirmed, ABORT — rotating the local key while the old CA
                // stays trusted would contradict the "invalidates prior trust"
                // promise and leave an exfiltrated old key usable.
                if !browser::ca_uninstall() {
                    return Err(error_stack::Report::new(ProxyError::CertAuthority).attach(
                        "could not revoke the previously-installed CA from the keychain; \
                         aborting regenerate so on-disk key material still matches OS trust. \
                         Remove the old CA manually (Keychain Access), then retry.",
                    ));
                }
                // Delete the old cert/key BEFORE regenerating. `load_or_generate`
                // reloads any existing pair, so a silently-ignored delete failure
                // would leave the old key in use while we print "regenerated" —
                // breaking the invalidates-prior-trust promise. Treat already-absent
                // as success; abort on any other removal error.
                for path in [&cert_path, &ca_dir.join("ca-key.pem")] {
                    match std::fs::remove_file(path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                        Err(err) => {
                            return Err(error_stack::Report::new(ProxyError::CertAuthority)
                                .attach(format!(
                                    "could not remove old CA file {} during regenerate ({err}); \
                                     aborting so the stale key is not silently reused",
                                    path.display()
                                )));
                        }
                    }
                }
                ca::CertAuthority::load_or_generate(&ca_dir)
                    .change_context(ProxyError::CertAuthority)?;
                output::info("regenerated CA — re-run `ca install` to trust it");
            }
        }
        return Ok(());
    }

    // Recover a leftover Safari proxy state from a previously hard-killed run
    // BEFORE resolving rules: a missing/bad rule must not strand the system
    // proxy. `ca_dir` needs no rule. Non-interactive so an unrelated startup
    // never blocks on a sudo password prompt.
    browser::restore_system_proxy_if_pending(&config::ca_dir(&args), false);

    let cfg = Arc::new(config::resolve(&args).change_context(ProxyError::Config)?);

    let ca = Arc::new(
        ca::CertAuthority::load_or_generate(&cfg.ca_dir)
            .change_context(ProxyError::CertAuthority)?,
    );
    let pac: Arc<str> = Arc::from(browser::generate_pac(&cfg.rules, cfg.listen).as_str());

    // `--insecure` disables all upstream TLS verification — make it loud.
    if cfg.insecure {
        output::warn("--insecure: upstream TLS verification is DISABLED for all upstreams");
    }

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
                // Interactive: the cached sudo credential may have expired during
                // a long run, so allow `sudo networksetup` to prompt for it.
                browser::restore_system_proxy_if_pending(&cfg.ca_dir, true);
                Ok(())
            }
        }
    })
}
