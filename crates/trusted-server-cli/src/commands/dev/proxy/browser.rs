//! Browser launch/config and PAC generation (spec §9).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::process::Command;

use super::ProxyError;
use super::config::{Browser, ResolvedConfig};
use super::rewrite::RuleTable;
use crate::output;

/// Name of the file persisted under `ca_dir` that records the Safari proxy
/// state that was active before this tool set its own PAC URL.
///
/// Format: three lines, `<service>\n<prior_pac_url>\n<prior_enabled>`, where
/// `<prior_pac_url>` is empty when auto-proxy had no URL and `<prior_enabled>`
/// is `on` or `off`. A missing third line is tolerated when reading (treated as
/// `on` if a URL is present, else `off`) for forward-compatibility with the
/// earlier two-line format.
const SAFARI_RESTORE_FILE: &str = "safari-proxy-restore";

/// Generates a PAC script that proxies only `https://` requests for matched FROM hosts.
///
/// All other requests fall through to `DIRECT`.
#[must_use]
pub fn generate_pac(rules: &RuleTable, listen: SocketAddr) -> String {
    let proxy = proxy_connect_addr(listen);
    let mut checks = String::new();
    for rule in &rules.0 {
        checks.push_str(&format!(
            "  if (url.substring(0,6) == \"https:\" && host == \"{}\") return \"PROXY {proxy}\";\n",
            rule.from
        ));
    }
    format!("function FindProxyForURL(url, host) {{\n{checks}  return \"DIRECT\";\n}}\n")
}

/// The `host:port` a browser on this machine should connect to for the proxy.
///
/// A wildcard bind (`0.0.0.0` / `::`) is normalized to loopback — the proxy is
/// always local — while any explicit bind IP is used verbatim. `SocketAddr`'s
/// `Display` brackets IPv6 (e.g. `[::1]:18080`). Use this everywhere a browser is
/// pointed at the proxy so a non-default `--listen` is honored, not hard-coded.
fn proxy_connect_addr(listen: SocketAddr) -> String {
    SocketAddr::new(proxy_connect_ip(listen), listen.port()).to_string()
}

/// The bare host (no port) a browser should connect to — for configs like
/// Firefox prefs that take host and port separately.
fn proxy_connect_host(listen: SocketAddr) -> String {
    proxy_connect_ip(listen).to_string()
}

/// Normalizes a wildcard bind to loopback; passes any explicit IP through.
fn proxy_connect_ip(listen: SocketAddr) -> IpAddr {
    match listen.ip() {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    }
}

/// Launches and configures each requested browser against the proxy (spec §9).
///
/// A browser that cannot be launched or configured logs manual steps and is
/// skipped — `launch` returns `Ok(())` unless something truly unrecoverable
/// happens.
///
/// # Errors
///
/// Returns [`ProxyError::Browser`] only on an unrecoverable setup failure.
pub fn launch(
    browsers: &[Browser],
    cfg: &ResolvedConfig,
) -> core::result::Result<(), error_stack::Report<ProxyError>> {
    for browser in browsers {
        match browser {
            Browser::Chrome => launch_chrome(cfg),
            Browser::Firefox => launch_firefox(cfg),
            Browser::Safari => launch_safari(cfg),
        }
    }
    Ok(())
}

/// Restores the macOS Safari system auto-proxy to its state before the last
/// `launch_safari` call, if a pending restore file exists under `ca_dir`.
///
/// Called both at startup (to recover from a previously hard-killed run, with
/// `interactive = false` so it never blocks an unrelated launch on a password
/// prompt) and on clean Ctrl-C exit (with `interactive = true` so the restore
/// can prompt for the password the cached sudo credential may have outlived).
///
/// On non-macOS systems or when no restore file is present, this is a no-op.
/// The restore file is **kept** when the restore commands fail so a later run
/// (or the manual command printed via [`crate::output::warn`]) can still fix the
/// system proxy; it is deleted only after a successful restore or when the file
/// is malformed (so a bad file cannot loop forever). Never panics.
pub fn restore_system_proxy_if_pending(ca_dir: &Path, interactive: bool) {
    #[cfg(target_os = "macos")]
    {
        let restore_path = ca_dir.join(SAFARI_RESTORE_FILE);
        if !restore_path.exists() {
            return;
        }

        let contents = match std::fs::read_to_string(&restore_path) {
            Ok(s) => s,
            Err(err) => {
                output::warn(&format!(
                    "Safari: could not read proxy restore file: {err}; \
                     restore the auto-proxy URL in System Settings → Network manually"
                ));
                // Unreadable file: remove it so we don't retry forever.
                let _ = std::fs::remove_file(&restore_path);
                return;
            }
        };

        let mut lines = contents.lines();
        let service = lines.next().unwrap_or("").trim().to_string();
        let prior_url = lines.next().unwrap_or("").trim().to_string();
        // Tolerate a missing third line (older two-line format): a saved URL
        // implies it was enabled; no URL implies auto-proxy was off.
        let prior_enabled = match lines.next() {
            Some(line) => line.trim().eq_ignore_ascii_case("on"),
            None => !prior_url.is_empty(),
        };

        if service.is_empty() {
            output::warn(
                "Safari: proxy restore file has no service name; \
                 restore the auto-proxy URL in System Settings → Network manually",
            );
            // Malformed file: remove it so a bad file cannot loop forever.
            let _ = std::fs::remove_file(&restore_path);
            return;
        }

        let prior_url = (!prior_url.is_empty()).then_some(prior_url.as_str());
        if restore_auto_proxy(&service, prior_url, prior_enabled, interactive) {
            // Only drop the file once the system proxy is actually restored.
            let _ = std::fs::remove_file(&restore_path);
        } else {
            // Keep the file; print the exact manual recovery steps.
            let manual = manual_restore_command(&service, prior_url, prior_enabled);
            output::warn(&format!(
                "Safari: could not auto-restore the system proxy for '{service}' (needs admin). \
                 Run: {manual} \
                 (or in System Settings → Network → {service} → Details → Proxies → \
                 Automatic Proxy Configuration)"
            ));
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS: nothing to restore (Safari/networksetup don't exist).
        let _ = (ca_dir, interactive);
    }
}

/// Creates a secure, unique temp directory for a throwaway browser profile.
///
/// Uses `tempfile` (random name, `0700`, created with `O_EXCL`) rather than a
/// predictable, timestamp-named path with `create_dir_all` — which succeeds if
/// the directory already exists and lets a local racer supply an
/// attacker-controlled profile. Returns `None` and prints a warning with `label`
/// on failure. The caller keeps the returned [`tempfile::TempDir`] alive until
/// the browser exits; dropping it removes the profile.
fn make_temp_dir(label: &str) -> Option<tempfile::TempDir> {
    match tempfile::Builder::new()
        .prefix(&format!("ts-dev-proxy-{label}-"))
        .tempdir()
    {
        Ok(dir) => Some(dir),
        Err(err) => {
            output::warn(&format!("{label}: could not create temp dir: {err}"));
            None
        }
    }
}

/// Launches Chrome in a temporary profile configured for HTTPS-only proxying.
///
/// Uses `--proxy-server="https=<addr>"` so only HTTPS traffic goes through the
/// proxy; HTTP and other schemes bypass it (spec §9).
fn launch_chrome(cfg: &ResolvedConfig) {
    let addr = proxy_connect_addr(cfg.listen);
    let proxy_arg = format!("https={addr}");

    let Some(tmpdir) = make_temp_dir("chrome") else {
        output::warn(&format!(
            "Chrome: launch Chrome manually with --proxy-server=\"https={addr}\""
        ));
        return;
    };

    let Some(mut cmd) = chrome_command() else {
        output::warn(
            "Chrome: no native launcher found; tried google-chrome, google-chrome-stable, chromium, chromium-browser. Snap/Flatpak automation is unsupported.",
        );
        return;
    };
    configure_chrome(
        &mut cmd,
        tmpdir.path(),
        &proxy_arg,
        cfg.rules.0.first().map(|rule| rule.from.as_str()),
    );

    match cmd.spawn() {
        Ok(mut child) => {
            // Keep the profile dir alive until the browser exits; dropping the
            // TempDir then removes it.
            std::thread::spawn(move || {
                let _ = child.wait();
                drop(tmpdir);
            });
        }
        Err(err) => {
            output::warn(&format!(
                "Chrome: could not launch: {err}; \
                 start Chrome manually with --proxy-server=\"https={addr}\""
            ));
            // `tmpdir` drops here, removing the profile.
        }
    }
}

fn configure_chrome(cmd: &mut Command, profile: &Path, proxy: &str, first_host: Option<&str>) {
    cmd.args([
        "--no-first-run",
        "--no-default-browser-check",
        &format!("--user-data-dir={}", profile.display()),
        &format!("--proxy-server={proxy}"),
    ]);
    if let Some(host) = first_host {
        cmd.arg(format!("https://{host}"));
    }
}

#[cfg(target_os = "linux")]
const CHROME_LAUNCHERS: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];

/// Discovers launchers in stable order, skipping known Snap/Flatpak paths.
/// Shell wrappers are not classified; callers must use native installations.
#[cfg(target_os = "linux")]
fn find_native_browser(
    candidates: &[&str],
    lookup: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> Option<Command> {
    candidates
        .iter()
        .filter_map(|name| lookup(name))
        .find(|path| {
            !path
                .components()
                .any(|part| matches!(part.as_os_str().to_str(), Some("snap" | "flatpak")))
        })
        .map(Command::new)
}

fn chrome_command() -> Option<Command> {
    #[cfg(target_os = "macos")]
    {
        Some(Command::new(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        find_native_browser(CHROME_LAUNCHERS, |name| {
            which::which(name)
                .ok()
                .and_then(|path| path.canonicalize().ok())
        })
    }
}

/// Launches Firefox in a temporary profile configured for HTTPS-only proxying.
///
/// Writes `user.js` with `network.proxy.type=1` (manual) and `ssl`/`ssl_port`
/// settings only — HTTP traffic is left to go direct (spec §9).  Also imports
/// the CA into the profile's NSS database via `certutil` if available.
fn launch_firefox(cfg: &ResolvedConfig) {
    let port = cfg.listen.port();
    let host = proxy_connect_host(cfg.listen);

    let Some(tmpdir) = make_temp_dir("firefox") else {
        output::warn(&format!(
            "Firefox: configure Firefox manually (proxy SSL/TLS host={host} port={port})"
        ));
        return;
    };

    let user_js = firefox_preferences(cfg.listen);

    if let Err(err) = std::fs::write(tmpdir.path().join("user.js"), &user_js) {
        output::warn(&format!(
            "Firefox: could not write user.js: {err}; \
             configure Firefox manually (proxy SSL/TLS host={host} port={port})"
        ));
        // `tmpdir` drops here, removing the profile.
        return;
    }

    let cert_path = super::ca::CertAuthority::cert_path(&cfg.ca_dir);
    if let Err(err) = super::trust::import_firefox(tmpdir.path(), &cert_path) {
        output::warn(&format!(
            "Firefox: could not configure CA trust: {err:?}; skipping launch"
        ));
        return;
    }

    let Some(mut cmd) = firefox_command() else {
        output::warn(
            "Firefox: native firefox not found on PATH. Snap/Flatpak automation is unsupported.",
        );
        return;
    };
    configure_firefox(
        &mut cmd,
        tmpdir.path(),
        cfg.rules.0.first().map(|rule| rule.from.as_str()),
    );

    match cmd.spawn() {
        Ok(mut child) => {
            // Keep the profile dir alive until the browser exits; dropping the
            // TempDir then removes it.
            std::thread::spawn(move || {
                let _ = child.wait();
                drop(tmpdir);
            });
        }
        Err(err) => {
            output::warn(&format!(
                "Firefox: could not launch: {err}; \
                 start Firefox manually with SSL proxy host={host} port={port}"
            ));
            // `tmpdir` drops here, removing the profile.
        }
    }
}

fn firefox_preferences(listen: SocketAddr) -> String {
    let host = proxy_connect_host(listen);
    let port = listen.port();
    format!(
        "user_pref(\"network.proxy.type\", 1);\n\
         user_pref(\"network.proxy.ssl\", \"{host}\");\n\
         user_pref(\"network.proxy.ssl_port\", {port});\n"
    )
}

fn configure_firefox(cmd: &mut Command, profile: &Path, first_host: Option<&str>) {
    cmd.arg("-profile").arg(profile).arg("--no-remote");
    if let Some(host) = first_host {
        cmd.arg(format!("https://{host}"));
    }
}

/// Returns the platform Firefox command.
fn firefox_command() -> Option<Command> {
    #[cfg(target_os = "macos")]
    {
        Some(Command::new(
            "/Applications/Firefox.app/Contents/MacOS/firefox",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        find_native_browser(&["firefox"], |name| {
            which::which(name)
                .ok()
                .and_then(|path| path.canonicalize().ok())
        })
    }
}

/// Configures Safari via the system PAC URL (spec §9).
///
/// Detects the active network service via `route`/`networksetup`, persists the
/// prior auto-proxy state to `<ca_dir>/safari-proxy-restore`, then sets the
/// PAC URL pointing at the running proxy's `/proxy.pac` route.
///
/// The restore file is consumed by [`restore_system_proxy_if_pending`] — either
/// at the next startup (crash recovery) or on clean Ctrl-C exit.  If the
/// process is SIGKILL'd the file remains and is recovered on the next run.
fn launch_safari(cfg: &ResolvedConfig) {
    let pac_url = format!("http://{}/proxy.pac", proxy_connect_addr(cfg.listen));

    // A restore file left by a previous (hard-killed) run records the user's real
    // original proxy state. The startup recovery in `run()` is non-interactive, so
    // it may have failed to restore. If we captured state now we would record the
    // dead dev-proxy PAC as the "original" and lose the user's setting forever.
    // So first try an interactive restore; only proceed once the file is gone
    // (meaning the current system state really is the user's original).
    let restore_path = cfg.ca_dir.join(SAFARI_RESTORE_FILE);
    if restore_path.exists() {
        restore_system_proxy_if_pending(&cfg.ca_dir, true);
        if restore_path.exists() {
            output::warn(
                "Safari: a previous proxy setting is still pending restore; skipping Safari \
                 auto-configuration to avoid losing it. Restore it first (see the printed \
                 networksetup command).",
            );
            return;
        }
    }

    let service = detect_network_service();
    let Some(service) = service else {
        output::warn(&format!(
            "Safari: could not detect active network service; \
             set PAC URL manually in System Settings → Network: {pac_url}"
        ));
        return;
    };

    // Read prior state (URL + enabled flag) before changing anything. The
    // restore file (if any) was cleared above, so this captures the user's real
    // original setting, not a stale dev-proxy PAC.
    let (prior_url, prior_enabled) = get_auto_proxy_state(&service);

    // Persist the prior state so it can be recovered even after a hard kill.
    let restore_contents = format!(
        "{service}\n{url}\n{enabled}\n",
        url = prior_url.as_deref().unwrap_or(""),
        enabled = if prior_enabled { "on" } else { "off" },
    );
    if let Err(err) = std::fs::write(&restore_path, &restore_contents) {
        output::warn(&format!(
            "Safari: could not write the proxy restore file ({err}); skipping Safari \
             auto-configuration so the system proxy is not changed without a way to restore it"
        ));
        return;
    }

    // Changing the system network proxy requires admin, so the `networksetup`
    // call is elevated with `sudo` (only this command — the proxy itself keeps
    // running as the current user). sudo prompts once in this terminal; if the
    // cached credential outlives a long run, the Ctrl-C restore may prompt again
    // (and otherwise prints the manual command).
    output::info(
        "Safari: setting the system auto-proxy needs admin — sudo will prompt for your password \
         (only `networksetup` is elevated; the proxy keeps running as you).",
    );
    let set_result = Command::new("sudo")
        .args(["networksetup", "-setautoproxyurl", &service, &pac_url])
        .status();

    match set_result {
        Ok(s) if s.success() => {
            // Open Safari at the first rule's FROM URL, like Chrome and Firefox do.
            if let Some(rule) = cfg.rules.0.first() {
                let url = format!("https://{}", rule.from);
                let _ = Command::new("open").args(["-a", "Safari", &url]).status();
                output::info(&format!("Safari: PAC set for '{service}'; opened {url}"));
            } else {
                output::info(&format!(
                    "Safari: PAC URL set for '{service}'; open Safari and browse to a proxied host"
                ));
            }
        }
        _ => {
            // The service name is a local config value that can contain shell
            // metacharacters; shell-quote both the service and URL so the
            // copy-pasteable command cannot execute unintended shell code.
            let quoted_service = shell_quote(&service);
            let quoted_url = shell_quote(&pac_url);
            output::warn(&format!(
                "Safari: could not set the system PAC (sudo declined or no terminal). Set it \
                 manually in System Settings → Network → {service} → Details → Proxies → \
                 Automatic Proxy Configuration: {pac_url} \
                 (or run: sudo networksetup -setautoproxyurl {quoted_service} {quoted_url})"
            ));
            // Remove the restore file — nothing was applied, nothing to restore.
            let _ = std::fs::remove_file(&restore_path);
        }
    }
}

/// Returns the active Wi-Fi/Ethernet network service name, or `None`.
fn detect_network_service() -> Option<String> {
    #[cfg(not(target_os = "macos"))]
    return None;

    #[cfg(target_os = "macos")]
    {
        // Find the default-route interface name.
        let route_out = Command::new("route")
            .args(["-n", "get", "default"])
            .output()
            .ok()?;
        let route_text = String::from_utf8_lossy(&route_out.stdout);
        let interface = route_text
            .lines()
            .find(|l| l.trim_start().starts_with("interface:"))?
            .split(':')
            .nth(1)?
            .trim()
            .to_string();

        // Map interface → service name via networksetup -listnetworkserviceorder.
        let ns_out = Command::new("networksetup")
            .arg("-listnetworkserviceorder")
            .output()
            .ok()?;
        let ns_text = String::from_utf8_lossy(&ns_out.stdout);
        service_for_interface(&ns_text, &interface)
    }
}

/// Maps a default-route interface (e.g. `en0`) to its macOS network-service name
/// (e.g. `Wi-Fi`) given `networksetup -listnetworkserviceorder` output, whose
/// entries look like:
///
/// ```text
/// (7) Wi-Fi
/// (Hardware Port: Wi-Fi, Device: en0)
/// ```
///
/// Both lines start with `(`, so the service line (`(N) Name`) is distinguished
/// from the hardware-port line by the digit after `(`; the device is matched on
/// the exact `Device:` field value (so `en1` does not match `Device: en11`).
#[cfg(target_os = "macos")]
fn service_for_interface(ns_output: &str, interface: &str) -> Option<String> {
    let mut last_service: Option<String> = None;
    for line in ns_output.lines() {
        let trimmed = line.trim();
        // Match the `Device:` field exactly: take the value after `Device: `,
        // strip the trailing `)`, and compare — `en1` must not match `en11`.
        if let Some(after) = trimmed.split_once("Device: ")
            && after.1.trim_end_matches(')').trim() == interface
        {
            return last_service;
        }
        // Service-name line "(N) Name": a '(' immediately followed by a digit
        // (the "(Hardware Port: …)" line starts with '(' + 'H', so it is skipped).
        if let Some(rest) = trimmed.strip_prefix('(')
            && rest.starts_with(|c: char| c.is_ascii_digit())
        {
            last_service = trimmed
                .split_once(')')
                .map(|(_, name)| name.trim().to_string())
                .filter(|s| !s.is_empty());
        }
    }
    None
}

/// Parses `networksetup -getautoproxyurl` output into `(url, enabled)`.
///
/// The command prints two lines, e.g.:
///
/// ```text
/// URL: http://127.0.0.1:18080/proxy.pac
/// Enabled: Yes
/// ```
///
/// A `URL:` of `(null)` (or empty) yields `None`; `Enabled:` is `true` only for
/// a `Yes` (case-insensitive). Pure and unit-testable.
fn parse_auto_proxy_state(text: &str) -> (Option<String>, bool) {
    let mut url = None;
    let mut enabled = false;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("URL:") {
            let value = value.trim();
            if !value.is_empty() && value != "(null)" {
                url = Some(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("Enabled:") {
            enabled = value.trim().eq_ignore_ascii_case("Yes");
        }
    }
    (url, enabled)
}

/// Returns the current auto-proxy `(url, enabled)` state for a network service.
///
/// A failure to run `networksetup` is reported as `(None, false)`.
fn get_auto_proxy_state(service: &str) -> (Option<String>, bool) {
    let Ok(out) = Command::new("networksetup")
        .args(["-getautoproxyurl", service])
        .output()
    else {
        return (None, false);
    };
    parse_auto_proxy_state(&String::from_utf8_lossy(&out.stdout))
}

/// Single-quotes a value for safe inclusion in a printed POSIX shell command
/// (handles spaces, `&`, and other metacharacters; embedded `'` are escaped).
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Builds the manual `networksetup` command line that recovers `service`'s prior
/// auto-proxy state, for printing when the automatic restore fails.
#[cfg(target_os = "macos")]
fn manual_restore_command(service: &str, prior_url: Option<&str>, prior_enabled: bool) -> String {
    let svc = shell_quote(service);
    match prior_url {
        Some(url) if prior_enabled => {
            format!(
                "sudo networksetup -setautoproxyurl {svc} {}",
                shell_quote(url)
            )
        }
        Some(url) => format!(
            "sudo networksetup -setautoproxyurl {svc} {url} && \
             sudo networksetup -setautoproxystate {svc} off",
            url = shell_quote(url)
        ),
        None => format!("sudo networksetup -setautoproxystate {svc} off"),
    }
}

/// Restores `service`'s prior auto-proxy state, preserving the enabled/disabled
/// flag, and returns whether every invoked command succeeded.
///
/// `-setautoproxyurl` re-enables auto-proxy, so when the prior state had a URL
/// that was **disabled** a follow-up `-setautoproxystate off` is issued; when
/// there was no prior URL the state is simply turned off. When `interactive` is
/// true the `networksetup` calls run under `sudo` (which may prompt for a
/// password — used on clean Ctrl-C exit, where the cached credential may have
/// expired); when false they run under `sudo -n` (never prompts — used during
/// an unrelated startup recovery so it cannot stall on a password prompt).
#[cfg(target_os = "macos")]
fn restore_auto_proxy(
    service: &str,
    prior_url: Option<&str>,
    prior_enabled: bool,
    interactive: bool,
) -> bool {
    // Run `networksetup <args>` under sudo, honoring the interactive flag.
    let run = |args: &[&str]| -> bool {
        let mut cmd = Command::new("sudo");
        if !interactive {
            cmd.arg("-n");
        }
        cmd.arg("networksetup");
        cmd.args(args);
        matches!(cmd.status(), Ok(s) if s.success())
    };

    let ok = match prior_url {
        Some(url) => {
            // Re-apply the URL (this re-enables auto-proxy)...
            let set = run(&["-setautoproxyurl", service, url]);
            // ...then disable it again if it was previously off.
            if prior_enabled {
                set
            } else {
                let off = run(&["-setautoproxystate", service, "off"]);
                set && off
            }
        }
        None => run(&["-setautoproxystate", service, "off"]),
    };

    if ok {
        output::info(&format!(
            "Safari: restored prior auto-proxy setting for '{service}'"
        ));
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dev::proxy::rewrite::{Authority, Rule, RuleTable};
    use crate::commands::dev::proxy::upstream::key::AddressPolicy;

    fn rule(from: &str, to: &str) -> Rule {
        Rule::new(
            from.to_string(),
            Authority::parse(to, false).expect("should parse authority"),
            false,
            false,
            false,
            AddressPolicy::Dns,
        )
        .expect("should build rule")
    }

    #[test]
    fn chrome_arguments_preserve_https_only_proxy_and_first_navigation() {
        let mut command = Command::new("test-browser");
        configure_chrome(
            &mut command,
            Path::new("/tmp/profile"),
            "https=[::1]:19000",
            Some("www.example.com"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--no-first-run",
                "--no-default-browser-check",
                "--user-data-dir=/tmp/profile",
                "--proxy-server=https=[::1]:19000",
                "https://www.example.com"
            ]
        );
    }

    #[test]
    fn firefox_arguments_and_preferences_keep_http_direct() {
        let prefs = firefox_preferences("127.0.0.2:19000".parse().expect("should parse address"));
        assert_eq!(
            prefs,
            "user_pref(\"network.proxy.type\", 1);\nuser_pref(\"network.proxy.ssl\", \"127.0.0.2\");\nuser_pref(\"network.proxy.ssl_port\", 19000);\n"
        );
        let mut command = Command::new("test-firefox");
        configure_firefox(
            &mut command,
            Path::new("/tmp/profile"),
            Some("www.example.com"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-profile",
                "/tmp/profile",
                "--no-remote",
                "https://www.example.com"
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_discovery_order_variants_missing_and_packaged_paths() {
        for candidate in CHROME_LAUNCHERS {
            let found = find_native_browser(CHROME_LAUNCHERS, |name| {
                (name == *candidate).then(|| std::path::PathBuf::from(format!("/usr/bin/{name}")))
            });
            assert!(found.is_some(), "should discover {candidate}");
        }
        let found = find_native_browser(CHROME_LAUNCHERS, |name| {
            Some(std::path::PathBuf::from(format!("/usr/bin/{name}")))
        })
        .expect("should discover first launcher");
        assert_eq!(found.get_program(), "/usr/bin/google-chrome");
        assert!(find_native_browser(CHROME_LAUNCHERS, |_| None).is_none());
        assert!(
            find_native_browser(CHROME_LAUNCHERS, |_| Some("/snap/bin/chromium".into())).is_none()
        );
    }

    #[test]
    fn shell_quote_wraps_and_escapes() {
        // Metacharacters (`&`, space, `?`) are neutralized by single-quoting.
        assert_eq!(
            shell_quote("http://h/proxy.pac?a=1&b=2"),
            "'http://h/proxy.pac?a=1&b=2'",
            "ampersand/query must be quoted, not left bare"
        );
        assert_eq!(shell_quote("a b"), "'a b'", "spaces are quoted");
        // An embedded single quote is closed, escaped, and reopened.
        assert_eq!(
            shell_quote("it's"),
            r"'it'\''s'",
            "embedded quote is escaped"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn service_for_interface_maps_device_to_service() {
        // Real shape of `networksetup -listnetworkserviceorder` output. `en1` and
        // `en11` both appear so the test proves the `Device:` match is exact (a
        // substring match would let `en1` match `Device: en11`).
        let ns = "An asterisk (*) denotes that a network service is disabled.\n\
                  (1) Display Ethernet\n\
                  (Hardware Port: Display Ethernet, Device: en11)\n\
                  \n\
                  (4) Thunderbolt Bridge\n\
                  (Hardware Port: Thunderbolt Bridge, Device: en1)\n\
                  \n\
                  (7) Wi-Fi\n\
                  (Hardware Port: Wi-Fi, Device: en0)\n";
        assert_eq!(
            service_for_interface(ns, "en0").as_deref(),
            Some("Wi-Fi"),
            "en0 should map to its preceding service name, not the hardware-port line"
        );
        assert_eq!(
            service_for_interface(ns, "en11").as_deref(),
            Some("Display Ethernet"),
            "en11 should map to Display Ethernet"
        );
        assert_eq!(
            service_for_interface(ns, "en1").as_deref(),
            Some("Thunderbolt Bridge"),
            "en1 should map to Thunderbolt Bridge, not cross-match Device: en11"
        );
        assert_eq!(
            service_for_interface(ns, "en99"),
            None,
            "an unknown interface yields no service"
        );
    }

    #[test]
    fn proxy_connect_addr_normalizes_wildcard_and_passes_through_explicit() {
        // Wildcard binds normalize to loopback; explicit IPs pass through; IPv6
        // is bracketed.
        assert_eq!(
            proxy_connect_addr("0.0.0.0:18080".parse().expect("addr")),
            "127.0.0.1:18080",
            "IPv4 wildcard maps to loopback"
        );
        assert_eq!(
            proxy_connect_addr("[::]:18080".parse().expect("addr")),
            "[::1]:18080",
            "IPv6 wildcard maps to loopback, bracketed"
        );
        assert_eq!(
            proxy_connect_addr("127.0.0.2:9000".parse().expect("addr")),
            "127.0.0.2:9000",
            "explicit loopback IP passes through"
        );
        assert_eq!(
            proxy_connect_addr("192.0.2.10:443".parse().expect("addr")),
            "192.0.2.10:443",
            "explicit non-loopback IP passes through verbatim"
        );
        assert_eq!(
            proxy_connect_host("0.0.0.0:18080".parse().expect("addr")),
            "127.0.0.1",
            "host-only form drops the port and normalizes the wildcard"
        );
    }

    #[test]
    fn pac_uses_normalized_connect_address_for_wildcard_bind() {
        let rules = RuleTable(vec![rule(
            "www.example-publisher.com",
            "to.edgecompute.app",
        )]);
        let pac = generate_pac(&rules, "0.0.0.0:18080".parse().expect("addr"));
        assert!(
            pac.contains("PROXY 127.0.0.1:18080"),
            "PAC points the browser at loopback, not the 0.0.0.0 wildcard. Got: {pac}"
        );
        assert!(
            !pac.contains("0.0.0.0"),
            "PAC must not hand the browser an unconnectable wildcard address"
        );
    }

    #[test]
    fn pac_proxies_only_https_for_from_hosts() {
        let rules = RuleTable(vec![rule(
            "www.example-publisher.com",
            "to.edgecompute.app",
        )]);
        let pac = generate_pac(
            &rules,
            "127.0.0.1:18080".parse().expect("should parse addr"),
        );
        assert!(pac.contains("https:"), "PAC guards on https scheme");
        assert!(
            pac.contains("www.example-publisher.com"),
            "PAC lists the FROM host"
        );
        assert!(
            pac.contains("PROXY 127.0.0.1:18080"),
            "PAC points at the listen addr"
        );
        assert!(
            pac.contains("return \"DIRECT\""),
            "everything else is direct"
        );
    }

    #[test]
    fn restore_system_proxy_if_pending_is_noop_when_no_file() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        // No file present — should not panic or error.
        restore_system_proxy_if_pending(dir.path(), false);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn restore_system_proxy_if_pending_removes_file_with_empty_service() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let restore_path = dir.path().join(SAFARI_RESTORE_FILE);
        // Write a malformed restore file (no service name).
        std::fs::write(&restore_path, "\nhttp://127.0.0.1:18080/proxy.pac\noff\n")
            .expect("should write restore file");
        restore_system_proxy_if_pending(dir.path(), false);
        // File should be removed even when service name is missing.
        assert!(
            !restore_path.exists(),
            "restore file should be removed after failed parse"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_restore_leaves_files_untouched() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let path = dir.path().join(SAFARI_RESTORE_FILE);
        std::fs::write(&path, "unchanged").expect("should write state");
        restore_system_proxy_if_pending(dir.path(), false);
        assert_eq!(
            std::fs::read_to_string(path).expect("should read state"),
            "unchanged"
        );
    }

    #[test]
    fn parse_auto_proxy_state_reads_url_and_enabled() {
        let (url, enabled) =
            parse_auto_proxy_state("URL: http://127.0.0.1:18080/proxy.pac\nEnabled: Yes\n");
        assert_eq!(
            url.as_deref(),
            Some("http://127.0.0.1:18080/proxy.pac"),
            "should read the URL line"
        );
        assert!(enabled, "Enabled: Yes parses as enabled");
    }

    #[test]
    fn parse_auto_proxy_state_handles_disabled_with_url() {
        // A saved-but-disabled PAC URL: URL present, Enabled No.
        let (url, enabled) =
            parse_auto_proxy_state("URL: http://example.com/old.pac\nEnabled: No\n");
        assert_eq!(
            url.as_deref(),
            Some("http://example.com/old.pac"),
            "URL is read even when disabled"
        );
        assert!(!enabled, "Enabled: No parses as disabled");
    }

    #[test]
    fn parse_auto_proxy_state_treats_null_url_as_none() {
        let (url, enabled) = parse_auto_proxy_state("URL: (null)\nEnabled: No\n");
        assert_eq!(url, None, "(null) URL parses as no URL");
        assert!(!enabled, "disabled with no URL");
    }
}
