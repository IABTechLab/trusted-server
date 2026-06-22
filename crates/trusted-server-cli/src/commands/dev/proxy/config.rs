//! Resolves `ProxyArgs` (+ defaults) into a concrete [`ResolvedConfig`].

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use base64::Engine as _;
use error_stack::{Report, ResultExt as _};

use super::ProxyArgs;
use super::rewrite::{Authority, Rule, RuleTable};

/// Errors from configuration resolution.
#[derive(Debug, derive_more::Display)]
pub enum ConfigError {
    /// No usable rule could be formed and none was inferable.
    #[display("no rewrite rule: pass --map FROM=TO (or --to with an inferable FROM)")]
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
    // NOTE: lone --to / lone --from + project-config inference is added in Task 7.
    Ok(RuleTable(rules))
}

fn make_rule(
    from: &str,
    to: &str,
    preserve_host: bool,
    plaintext: bool,
) -> Result<Rule, ConfigError> {
    let to = Authority::parse(to, plaintext).map_err(|_| ConfigError::Rule)?;
    Ok(Rule {
        from: from.to_ascii_lowercase(),
        to,
        preserve_host,
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
        assert!(rule.preserve_host, "default preserves FROM host");
        assert_eq!(rule.to.host(), "to.edgecompute.app");
    }

    #[test]
    fn rewrite_host_flag_clears_preserve_host() {
        let mut args = base_args();
        args.map = vec!["www.example-publisher.com=to.edgecompute.app".into()];
        args.rewrite_host = true;
        let cfg = resolve(&args).expect("should resolve");
        assert!(
            !cfg.rules
                .first_match("www.example-publisher.com")
                .expect("rule")
                .preserve_host
        );
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
}
