//! Resolves `ProxyArgs` (+ defaults) into a concrete [`ResolvedConfig`].

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use base64::Engine as _;
use error_stack::{Report, ResultExt as _};
use hyper::header::HeaderValue;

use super::ProxyArgs;
use super::rewrite::{Authority, Rule, RuleTable};
use super::upstream::key::AddressPolicy;

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
    /// A `--resolve` value was not `HOST:IP` with a valid hostname and IP.
    #[display("invalid --resolve `{value}` (expected HOST:IP, e.g. ts.example.com:192.0.2.10)")]
    Resolve { value: String },
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
    /// `--basic-auth`/`--basic-auth-file` was combined with a non-loopback listen.
    #[display(
        "refusing to inject --basic-auth on non-loopback --listen {value}: a matched CONNECT is \
         MITM'd even off loopback, so the upstream credentials would be exposed to any client that \
         can reach the proxy. Bind a loopback address, or drop --basic-auth."
    )]
    BasicAuthNonLoopback { value: String },
    /// An unknown browser name was passed to `--launch`.
    #[display("unknown browser `{value}` (expected chrome|firefox|safari|all)")]
    Browser { value: String },
}

impl core::error::Error for ConfigError {}

/// Basic-auth credentials to inject upstream.
#[derive(Clone)]
pub struct BasicAuth {
    header: HeaderValue,
}

impl BasicAuth {
    /// Precomputes a validated `Authorization` header.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::BasicAuth`] if the generated value is not a valid
    /// HTTP header value.
    pub fn new(user: &str, pass: &str) -> Result<Self, ConfigError> {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        let header =
            HeaderValue::from_str(&format!("Basic {token}")).map_err(|_| ConfigError::BasicAuth)?;
        Ok(Self { header })
    }

    /// The prevalidated `Authorization` header value.
    #[must_use]
    pub fn header_value(&self) -> &HeaderValue {
        &self.header
    }

    fn parse(raw: &str) -> Result<Self, ConfigError> {
        let (user, pass) = raw.split_once(':').ok_or(ConfigError::BasicAuth)?;
        Self::new(user, pass)
    }
}

impl core::fmt::Debug for BasicAuth {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("BasicAuth([REDACTED])")
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
    /// DNS pins from `--resolve`: lowercase hostname → connection address. When
    /// an upstream host is present here, the proxy dials this IP instead of
    /// resolving the name, leaving the SNI/`Host` untouched.
    pub resolve: HashMap<String, IpAddr>,
    /// Upstream connect timeout (`--connect-timeout`), bounding each dial so a
    /// black-holed upstream fails fast into a `502`.
    pub connect_timeout: std::time::Duration,
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
    // `--rewrite-host` only chooses the `Host` header; the SNI always follows TO.
    for entry in &args.map {
        let (from, to) = entry.split_once('=').ok_or(ConfigError::Rule)?;
        rules.push(make_rule(
            from,
            to,
            args.rewrite_host,
            args.upstream_plaintext,
            args.insecure,
        )?);
    }
    if let (Some(from), Some(to)) = (&args.from, &args.to) {
        rules.push(make_rule(
            from,
            to,
            args.rewrite_host,
            args.upstream_plaintext,
            args.insecure,
        )?);
    }
    Ok(RuleTable(rules))
}

/// Parses `--resolve HOST:IP` entries into a lowercase-host → address map.
///
/// Splits on the first `:` so the IP (including IPv6, which contains `:`) is the
/// remainder. The host is validated as a hostname and the address as an
/// [`IpAddr`].
fn build_resolve(args: &ProxyArgs) -> Result<HashMap<String, IpAddr>, ConfigError> {
    let mut map = HashMap::new();
    for entry in &args.resolve {
        let (host, ip) = entry.split_once(':').ok_or_else(|| ConfigError::Resolve {
            value: entry.clone(),
        })?;
        let host = host.to_ascii_lowercase();
        let ip: IpAddr = ip.parse().map_err(|_| ConfigError::Resolve {
            value: entry.clone(),
        })?;
        if !is_valid_host(&host) {
            return Err(ConfigError::Resolve {
                value: entry.clone(),
            });
        }
        map.insert(host, ip);
    }
    Ok(map)
}

/// Whether `host` is a syntactically valid hostname — ASCII letters, digits,
/// `-`, and `.` only — so it is safe to embed verbatim in the generated PAC
/// JavaScript, the browser URL, and the upstream `Host` header.
///
/// Underscores are intentionally rejected: they are not valid in DNS hostnames
/// and excluding them keeps the allowed set strictly safe for the contexts
/// above. A publisher host that needs `_` is out of scope for this dev tool.
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
    rewrite_host: bool,
    plaintext: bool,
    insecure: bool,
) -> Result<Rule, ConfigError> {
    let from = from.to_ascii_lowercase();
    if !is_valid_host(&from) {
        return Err(ConfigError::InvalidFrom { value: from });
    }
    let to = Authority::parse(to, plaintext).map_err(|_| ConfigError::Rule)?;
    Rule::new(
        from,
        to,
        rewrite_host,
        plaintext,
        insecure,
        AddressPolicy::Dns,
    )
    .map_err(|_| ConfigError::Rule)
}

/// Resolves arguments into a [`ResolvedConfig`].
///
/// # Errors
///
/// Returns [`ConfigError`] on malformed rules, a malformed `--resolve` entry, an
/// invalid/forbidden listen address, malformed credentials, or an unknown
/// browser.
pub fn resolve(args: &ProxyArgs) -> Result<ResolvedConfig, Report<ConfigError>> {
    let mut rules = build_rules(args).map_err(Report::from)?;
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
    // Injected Basic auth on a non-loopback bind would hand the developer's
    // upstream credentials to any client that can reach the proxy (a matched
    // CONNECT is MITM'd even off loopback). Refuse the combination.
    if !is_loopback && basic_auth.is_some() {
        return Err(Report::new(ConfigError::BasicAuthNonLoopback {
            value: args.listen.clone(),
        }));
    }
    let ca_dir = ca_dir(args);
    let resolve = build_resolve(args).map_err(Report::from)?;

    for rule in &mut rules.0 {
        if let Some(address) = resolve.get(rule.to.host()) {
            rule.set_address_policy(AddressPolicy::Resolve(*address));
        }
    }

    // A `--resolve HOST:IP` whose HOST matches no rule's TO host is almost
    // certainly a typo: the pin would silently never apply. Warn rather than
    // error, so a deliberate pin for a host reached indirectly still works.
    for host in resolve.keys() {
        if !rules.0.iter().any(|rule| rule.to.host() == host) {
            log::warn!(
                "--resolve {host}:… does not match any rule's TO host; the pin will not be used"
            );
        }
    }

    Ok(ResolvedConfig {
        rules,
        listen,
        allow_non_loopback: args.allow_non_loopback,
        launch,
        insecure: args.insecure,
        basic_auth,
        ca_dir,
        resolve,
        connect_timeout: std::time::Duration::from_secs(args.connect_timeout),
    })
}

/// Credential precedence: `--basic-auth-file` > `--basic-auth`.
fn resolve_basic_auth(args: &ProxyArgs) -> Result<Option<BasicAuth>, ConfigError> {
    if let Some(path) = &args.basic_auth_file {
        let raw = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::BasicAuthFile { path: path.clone() })?;
        // Strip only a trailing newline (the file's line terminator), not all
        // whitespace — a password may legitimately begin or end with a space.
        return Ok(Some(BasicAuth::parse(raw.trim_end_matches(['\r', '\n']))?));
    }
    match &args.basic_auth {
        Some(raw) => Ok(Some(BasicAuth::parse(raw)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use hyper::header::HeaderValue;
    use rustls::pki_types::ServerName;

    use super::*;
    use crate::commands::dev::proxy::rewrite::rewrite_for;
    use crate::commands::dev::proxy::upstream::key::{
        AddressPolicy, ApplicationMode, OriginKey, ReferenceIdentity, Transport, VerifyMode,
    };

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
    fn clap_parses_rewrite_host_as_a_bool() {
        assert!(
            !parse_args(&["ts"]).rewrite_host,
            "absent --rewrite-host is false"
        );
        assert!(
            parse_args(&["ts", "--rewrite-host"]).rewrite_host,
            "present --rewrite-host is true"
        );
    }

    #[test]
    fn single_rule_from_to_keeps_from_host_by_default() {
        let mut args = base_args();
        args.from = Some("www.example-publisher.com".into());
        args.to = Some("to.edgecompute.app".into());
        let cfg = resolve(&args).expect("should resolve");
        let rule = cfg
            .rules
            .first_match("www.example-publisher.com")
            .expect("rule present");
        assert_eq!(
            rewrite_for(rule).host_header,
            HeaderValue::from_static("www.example-publisher.com"),
            "default preserves FROM host"
        );
        assert_eq!(rule.to.host(), "to.edgecompute.app");
    }

    #[test]
    fn rewrite_host_uses_to() {
        let mut args = base_args();
        args.map = vec!["www.example-publisher.com=to.edgecompute.app".into()];
        args.rewrite_host = true;
        let cfg = resolve(&args).expect("should resolve");
        assert_eq!(
            rewrite_for(
                cfg.rules
                    .first_match("www.example-publisher.com")
                    .expect("rule")
            )
            .host_header,
            HeaderValue::from_static("to.edgecompute.app"),
            "--rewrite-host sends Host: TO"
        );
    }

    #[test]
    fn resolve_pins_host_to_ip() {
        let mut args = base_args();
        args.map = vec!["www.example-publisher.com=ts.edgecompute.app".into()];
        // Mixed case to confirm the host key is lowercased.
        args.resolve = vec!["TS.EdgeCompute.app:192.0.2.10".into()];
        let cfg = resolve(&args).expect("should resolve");
        assert_eq!(
            cfg.resolve.get("ts.edgecompute.app"),
            Some(&"192.0.2.10".parse().expect("should parse ipv4")),
            "--resolve pins the lowercased host to the address"
        );
    }

    #[test]
    fn resolve_accepts_ipv6_target() {
        let mut args = base_args();
        args.map = vec!["a.example.com=b.edgecompute.app".into()];
        // Split-on-first-colon must keep the colon-bearing IPv6 address intact.
        args.resolve = vec!["b.edgecompute.app:::1".into()];
        let cfg = resolve(&args).expect("should resolve");
        assert_eq!(
            cfg.resolve.get("b.edgecompute.app"),
            Some(&"::1".parse().expect("should parse ipv6")),
            "IPv6 --resolve target is parsed whole"
        );
    }

    #[test]
    fn resolve_host_not_matching_any_rule_warns_but_succeeds() {
        let mut args = base_args();
        args.map = vec!["a.example.com=b.edgecompute.app".into()];
        // A pin for a host that is no rule's TO is a likely typo: it should warn
        // (not error) and still be recorded.
        args.resolve = vec!["typo.edgecompute.app:192.0.2.10".into()];
        let cfg = resolve(&args).expect("an unmatched --resolve host should warn, not error");
        assert!(
            cfg.resolve.contains_key("typo.edgecompute.app"),
            "the pin is recorded even when it matches no rule's TO host"
        );
    }

    #[test]
    fn resolve_rejects_malformed_value() {
        let mut args = base_args();
        args.map = vec!["a.example.com=b.edgecompute.app".into()];
        args.resolve = vec!["b.edgecompute.app:not-an-ip".into()];
        let err = resolve(&args).expect_err("a non-IP --resolve target should error");
        assert!(
            matches!(err.current_context(), ConfigError::Resolve { .. }),
            "should be a Resolve error for a non-IP target"
        );
    }

    #[test]
    fn map_value_must_be_from_equals_to() {
        let mut args = base_args();
        args.map = vec!["not-a-map".into()];
        assert!(resolve(&args).is_err(), "malformed --map errors");
    }

    #[test]
    fn basic_auth_on_non_loopback_listen_is_rejected() {
        // Injected Basic auth on a non-loopback bind would expose the upstream
        // credentials to any reachable network client.
        let mut args = base_args();
        args.map = vec!["a.example.com=b.edgecompute.app".into()];
        args.listen = "0.0.0.0:18080".into();
        args.allow_non_loopback = true;
        args.basic_auth = Some("dev:secret".into());
        let err =
            resolve(&args).expect_err("non-loopback listen with --basic-auth should be rejected");
        assert!(
            matches!(
                err.current_context(),
                ConfigError::BasicAuthNonLoopback { .. }
            ),
            "should be a BasicAuthNonLoopback error"
        );

        // The same non-loopback bind without credentials is allowed.
        args.basic_auth = None;
        assert!(
            resolve(&args).is_ok(),
            "non-loopback without --basic-auth is allowed"
        );
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
        let auth = BasicAuth::new("dev", "secret").expect("should build auth");
        assert_eq!(
            auth.header_value(),
            &HeaderValue::from_static("Basic ZGV2OnNlY3JldA=="),
            "Basic base64(user:pass)"
        );
        let debug = format!("{auth:?}");
        assert_eq!(debug, "BasicAuth([REDACTED])", "Debug should be redacted");
        assert!(!debug.contains("dev"), "Debug should not contain the user");
        assert!(
            !debug.contains("secret") && !debug.contains("ZGV2OnNlY3JldA=="),
            "Debug should not contain raw or encoded credentials"
        );
    }

    #[test]
    fn resolve_precomputes_typed_rule_identity_and_headers() {
        let mut args = base_args();
        args.map = vec!["www.example.com=TO.Example.com:8443".into()];
        args.rewrite_host = true;
        args.insecure = true;
        args.resolve = vec!["to.example.com:192.0.2.10".into()];

        let cfg = resolve(&args).expect("should resolve");
        let rule = cfg
            .rules
            .first_match("www.example.com")
            .expect("should find rule");
        let outcome = rewrite_for(rule);

        assert_eq!(
            outcome.host_header,
            HeaderValue::from_static("to.example.com:8443"),
            "should prevalidate upstream Host"
        );
        assert_eq!(
            outcome.orig_host,
            HeaderValue::from_static("www.example.com"),
            "should prevalidate forwarding host"
        );
        assert_eq!(
            outcome.sni,
            Some(ServerName::try_from("to.example.com").expect("should parse server name")),
            "should prevalidate normalized SNI"
        );
        assert_eq!(
            rule.origin_key(),
            &OriginKey::new(
                Transport::Tls,
                ReferenceIdentity::dns("to.example.com"),
                8443,
                VerifyMode::Insecure,
                ApplicationMode::Http2Eligible,
                AddressPolicy::Resolve("192.0.2.10".parse().expect("should parse pin")),
            ),
            "should precompute the complete transport identity"
        );
    }

    #[test]
    fn resolve_keeps_ip_reference_identities_http1_only() {
        let mut args = base_args();
        args.map = vec!["www.example.com=127.0.0.1".into()];
        args.rewrite_host = true;

        let cfg = resolve(&args).expect("should resolve");
        let rule = cfg
            .rules
            .first_match("www.example.com")
            .expect("should find rule");

        assert_eq!(
            rule.origin_key(),
            &OriginKey::new(
                Transport::Tls,
                ReferenceIdentity::ip("127.0.0.1".parse().expect("should parse IP")),
                443,
                VerifyMode::Secure,
                ApplicationMode::Http1Required,
                AddressPolicy::Dns,
            ),
            "IP identities should validate as IP and remain HTTP/1-only"
        );
        assert_eq!(
            rewrite_for(rule).sni,
            Some(ServerName::from(
                "127.0.0.1"
                    .parse::<IpAddr>()
                    .expect("should parse IP server name"),
            )),
            "TLS should use an IP server name rather than DNS SNI"
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
