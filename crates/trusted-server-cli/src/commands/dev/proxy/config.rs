//! Resolves `ProxyArgs` (+ defaults) into a concrete [`ResolvedConfig`].

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use base64::Engine as _;
use error_stack::{Report, ResultExt as _};

use super::ProxyArgs;
use super::rewrite::{Authority, HostMode, Rule, RuleTable};

/// Errors from configuration resolution.
#[derive(Debug, derive_more::Display)]
pub enum ConfigError {
    /// No usable rule was passed.
    #[display("no rewrite rule: pass --map FROM=TO (or -f/--from with -t/--to)")]
    NoRule,
    /// A `--map`/authority value was malformed.
    #[display("invalid rule value")]
    Rule,
    /// The FROM host contained characters not valid in a hostname.
    #[display("invalid FROM host `{value}` (expected a hostname: letters, digits, '-', '.')")]
    InvalidFrom { value: String },
    /// The `--rewrite-host <HOST>` value was not a valid hostname.
    #[display("invalid --rewrite-host `{value}` (expected a hostname: letters, digits, '-', '.')")]
    InvalidRewriteHost { value: String },
    /// `--listen` was not a valid socket address.
    #[display("invalid --listen address `{value}`")]
    Listen { value: String },
    /// A non-loopback listen address was given without `--allow-non-loopback`.
    #[display("--listen {value} is non-loopback; pass --allow-non-loopback to allow it")]
    NonLoopback { value: String },
    /// `--basic-auth`/file value was not `USER:PASS`.
    #[display("--basic-auth must be USER:PASS")]
    BasicAuth,
    /// `--basic-auth-file` could not be read.
    #[display("cannot read --basic-auth-file `{path}`")]
    BasicAuthFile { path: String },
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
        Ok(Self {
            user: user.to_string(),
            pass: pass.to_string(),
        })
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
    ///
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
                other => Err(ConfigError::Browser {
                    value: other.to_string(),
                }),
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
    args.ca_dir
        .as_ref()
        .map_or_else(default_ca_dir, PathBuf::from)
}

/// Resolves the `--rewrite-host` flag into a [`HostMode`] applied to every rule:
/// absent → preserve `FROM`; bare → use `TO`; with a value → that explicit host
/// (validated, lowercased) for both the `Host` header and the TLS SNI.
fn host_mode(args: &ProxyArgs) -> Result<HostMode, ConfigError> {
    match &args.rewrite_host {
        None => Ok(HostMode::PreserveFrom),
        Some(None) => Ok(HostMode::UseTo),
        Some(Some(host)) => {
            let host = host.to_ascii_lowercase();
            if !is_valid_host(&host) {
                return Err(ConfigError::InvalidRewriteHost { value: host });
            }
            Ok(HostMode::Explicit(host))
        }
    }
}

fn build_rules(args: &ProxyArgs) -> Result<RuleTable, ConfigError> {
    let mut rules = Vec::new();
    let mode = host_mode(args)?;
    for entry in &args.map {
        let (from, to) = entry.split_once('=').ok_or(ConfigError::Rule)?;
        rules.push(make_rule(from, to, mode.clone(), args.upstream_plaintext)?);
    }
    if let (Some(from), Some(to)) = (&args.from, &args.to) {
        rules.push(make_rule(from, to, mode.clone(), args.upstream_plaintext)?);
    }
    Ok(RuleTable(rules))
}

/// Whether `host` is a syntactically valid hostname — ASCII letters, digits,
/// `-`, and `.` only — so it is safe to embed verbatim in the generated PAC
/// JavaScript, the browser URL, and the upstream `Host` header.
fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

fn make_rule(
    from: &str,
    to: &str,
    host_mode: HostMode,
    plaintext: bool,
) -> Result<Rule, ConfigError> {
    let from = from.to_ascii_lowercase();
    if !is_valid_host(&from) {
        return Err(ConfigError::InvalidFrom { value: from });
    }
    let to = Authority::parse(to, plaintext).map_err(|_| ConfigError::Rule)?;
    Ok(Rule {
        from,
        to,
        host_mode,
        plaintext,
    })
}

/// Resolves arguments into a [`ResolvedConfig`].
///
/// # Errors
///
/// Returns [`ConfigError`] on malformed rules, an invalid/forbidden listen
/// address, malformed credentials, or an unknown browser.
pub fn resolve(args: &ProxyArgs) -> Result<ResolvedConfig, Report<ConfigError>> {
    let rules = build_rules(args).map_err(Report::from)?;
    if rules.0.is_empty() {
        return Err(Report::new(ConfigError::NoRule));
    }

    let listen: SocketAddr = args
        .listen
        .parse()
        .change_context_lazy(|| ConfigError::Listen {
            value: args.listen.clone(),
        })?;
    let is_loopback = match listen.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    };
    if !is_loopback && !args.allow_non_loopback {
        return Err(Report::new(ConfigError::NonLoopback {
            value: args.listen.clone(),
        }));
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
        let raw = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::BasicAuthFile { path: path.clone() })?;
        return Ok(Some(BasicAuth::parse(raw.trim())?));
    }
    match &args.basic_auth {
        Some(raw) => Ok(Some(BasicAuth::parse(raw)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> crate::commands::dev::proxy::ProxyArgs {
        // Construct via clap so defaults match the real surface.
        use clap::Parser;
        #[derive(clap::Parser)]
        struct W {
            #[command(flatten)]
            a: crate::commands::dev::proxy::ProxyArgs,
        }
        W::parse_from(["ts"]).a
    }

    fn parse_args(argv: &[&str]) -> crate::commands::dev::proxy::ProxyArgs {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct W {
            #[command(flatten)]
            a: crate::commands::dev::proxy::ProxyArgs,
        }
        W::parse_from(argv).a
    }

    #[test]
    fn clap_parses_the_three_rewrite_host_forms() {
        assert_eq!(
            parse_args(&["ts"]).rewrite_host,
            None,
            "absent --rewrite-host parses to None"
        );
        assert_eq!(
            parse_args(&["ts", "--rewrite-host"]).rewrite_host,
            Some(None),
            "bare --rewrite-host parses to Some(None)"
        );
        assert_eq!(
            parse_args(&["ts", "--rewrite-host", "app.example.com"]).rewrite_host,
            Some(Some("app.example.com".to_string())),
            "--rewrite-host <HOST> parses to Some(Some(host))"
        );
    }

    #[test]
    fn single_rule_from_to_defaults_to_preserve_host() {
        let mut args = base_args();
        args.from = Some("www.example-publisher.com".into());
        args.to = Some("to.edgecompute.app".into());
        let cfg = resolve(&args).expect("should resolve");
        let rule = cfg
            .rules
            .first_match("www.example-publisher.com")
            .expect("rule present");
        assert_eq!(
            rule.host_mode,
            HostMode::PreserveFrom,
            "default preserves FROM host"
        );
        assert_eq!(rule.to.host(), "to.edgecompute.app");
    }

    #[test]
    fn bare_rewrite_host_uses_to() {
        let mut args = base_args();
        args.map = vec!["www.example-publisher.com=to.edgecompute.app".into()];
        // Bare `--rewrite-host` (present, no value) parses to `Some(None)`.
        args.rewrite_host = Some(None);
        let cfg = resolve(&args).expect("should resolve");
        assert_eq!(
            cfg.rules
                .first_match("www.example-publisher.com")
                .expect("rule")
                .host_mode,
            HostMode::UseTo,
            "bare --rewrite-host sends Host: TO"
        );
    }

    #[test]
    fn rewrite_host_with_value_is_explicit_for_ip_to() {
        let mut args = base_args();
        // TO is a bare IP; the explicit value supplies the Host header and SNI.
        args.map = vec!["www.example-publisher.com=192.0.2.10".into()];
        args.rewrite_host = Some(Some("App.EdgeCompute.app".into()));
        let cfg = resolve(&args).expect("should resolve");
        assert_eq!(
            cfg.rules
                .first_match("www.example-publisher.com")
                .expect("rule")
                .host_mode,
            HostMode::Explicit("app.edgecompute.app".to_string()),
            "explicit --rewrite-host is lowercased and stored verbatim"
        );
    }

    #[test]
    fn rewrite_host_with_invalid_value_is_rejected() {
        let mut args = base_args();
        args.map = vec!["www.example-publisher.com=192.0.2.10".into()];
        args.rewrite_host = Some(Some("bad/host".into()));
        let err = resolve(&args).expect_err("an invalid --rewrite-host should error");
        assert!(
            matches!(
                err.current_context(),
                ConfigError::InvalidRewriteHost { .. }
            ),
            "should be InvalidRewriteHost for a non-hostname value"
        );
    }

    #[test]
    fn map_value_must_be_from_equals_to() {
        let mut args = base_args();
        args.map = vec!["not-a-map".into()];
        assert!(resolve(&args).is_err(), "malformed --map errors");
    }

    #[test]
    fn invalid_from_host_is_rejected() {
        // A FROM with characters that would break the PAC JS / Host header.
        let mut args = base_args();
        args.map = vec!["bad\"host=to.edgecompute.app".into()];
        let err = resolve(&args).expect_err("a malformed FROM host should error");
        assert!(
            matches!(err.current_context(), ConfigError::InvalidFrom { .. }),
            "should be InvalidFrom for a hostname with invalid characters"
        );
    }

    #[test]
    fn non_loopback_listen_requires_flag() {
        let mut args = base_args();
        args.map = vec!["a.example.com=b.edgecompute.app".into()];
        args.listen = "0.0.0.0:18080".into();
        assert!(
            resolve(&args).is_err(),
            "non-loopback without flag is rejected"
        );
        args.allow_non_loopback = true;
        assert!(resolve(&args).is_ok(), "non-loopback allowed with flag");
    }

    #[test]
    fn basic_auth_header_is_base64() {
        let auth = BasicAuth {
            user: "dev".into(),
            pass: "secret".into(),
        };
        assert_eq!(
            auth.header_value(),
            "Basic ZGV2OnNlY3JldA==",
            "Basic base64(user:pass)"
        );
    }

    #[test]
    fn browser_list_parses_all() {
        assert_eq!(
            Browser::parse_list("all").expect("parses"),
            vec![Browser::Chrome, Browser::Firefox, Browser::Safari]
        );
        assert_eq!(
            Browser::parse_list("firefox,chrome").expect("parses"),
            vec![Browser::Firefox, Browser::Chrome]
        );
        assert!(
            Browser::parse_list("netscape").is_err(),
            "unknown browser errors"
        );
    }

    #[test]
    fn basic_auth_file_missing_is_a_file_error() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let missing = dir.path().join("no-such-file.txt");

        let mut args = base_args();
        args.map = vec!["a.example.com=b.edgecompute.app".into()];
        args.basic_auth_file = Some(missing.to_string_lossy().into_owned());

        let err = resolve(&args).expect_err("should fail when file is missing");
        assert!(
            matches!(err.current_context(), ConfigError::BasicAuthFile { .. }),
            "should be a BasicAuthFile error, not BasicAuth"
        );
    }

    #[test]
    fn no_rule_passed_is_a_no_rule_error() {
        let args = base_args();
        let err = resolve(&args).expect_err("should error when no rule is passed");
        assert!(
            matches!(err.current_context(), ConfigError::NoRule),
            "should be a NoRule error when no --map/-f/-t is given"
        );
    }
}
