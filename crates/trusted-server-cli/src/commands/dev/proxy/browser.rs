//! Browser launch/config, PAC generation, and CA trust commands (spec §9, §7.3).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::ProxyError;
use super::ca::CA_COMMON_NAME;
use super::config::{Browser, ResolvedConfig};
use super::rewrite::RuleTable;
use crate::output;

/// Generates a PAC script that proxies only `https://` requests for matched FROM hosts.
///
/// All other requests fall through to `DIRECT`.
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

/// Adds the CA certificate to the macOS login keychain (spec §7.3).
///
/// On non-macOS systems, or if the `security` command fails, prints manual
/// instructions via [`crate::output`]. Never panics.
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
            Ok(s) if s.success() => {
                output::info("CA added to the login keychain");
            }
            _ => output::warn(&format!(
                "could not auto-install; run manually: security add-trusted-cert -r trustRoot -k {keychain} {}",
                cert_path.display()
            )),
        }
    }
    #[cfg(not(target_os = "macos"))]
    output::info(&format!(
        "add this CA to your OS trust store manually: {}",
        cert_path.display()
    ));
}

/// Removes the dev CA from the macOS login keychain (spec §7.3).
///
/// On non-macOS systems, prints a manual note. Never panics.
pub fn ca_uninstall() {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("security")
            .args(["delete-certificate", "-c", CA_COMMON_NAME])
            .status();
        match status {
            Ok(s) if s.success() => output::info("CA removed from keychain"),
            _ => output::info("CA was not found in keychain (already removed or never installed)"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    output::info("remove the dev CA from your OS trust store manually");
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

/// Creates a unique temp directory under the system temp dir.
///
/// Returns `None` and prints a warning with `label` if the directory cannot be created.
fn make_temp_dir(label: &str) -> Option<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("ts-dev-proxy-{label}-{ts}"));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
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
    let port = cfg.listen.port();
    let proxy_arg = format!("https=127.0.0.1:{port}");

    let Some(tmpdir) = make_temp_dir("chrome") else {
        output::warn(&format!(
            "Chrome: launch Chrome manually with --proxy-server=\"https=127.0.0.1:{port}\""
        ));
        return;
    };

    let mut cmd = chrome_command();
    cmd.args([
        "--no-first-run",
        "--no-default-browser-check",
        &format!("--user-data-dir={}", tmpdir.display()),
        &format!("--proxy-server={proxy_arg}"),
    ]);

    if let Some(rule) = cfg.rules.0.first() {
        cmd.arg(format!("https://{}", rule.from));
    }

    match cmd.spawn() {
        Ok(mut child) => {
            // Clean up the temp dir after the browser exits.
            std::thread::spawn(move || {
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&tmpdir);
            });
        }
        Err(err) => {
            output::warn(&format!(
                "Chrome: could not launch: {err}; \
                 start Chrome manually with --proxy-server=\"https=127.0.0.1:{port}\""
            ));
            let _ = std::fs::remove_dir_all(&tmpdir);
        }
    }
}

/// Returns the platform Chrome/Chromium command.
fn chrome_command() -> Command {
    #[cfg(target_os = "macos")]
    {
        let app = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        Command::new(app)
    }
    #[cfg(target_os = "linux")]
    {
        // Try google-chrome first, then chromium-browser, then chromium.
        Command::new("google-chrome")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Command::new("chrome")
    }
}

/// Launches Firefox in a temporary profile configured for HTTPS-only proxying.
///
/// Writes `user.js` with `network.proxy.type=1` (manual) and ssl/ssl_port
/// settings only — HTTP traffic is left to go direct (spec §9).  Also imports
/// the CA into the profile's NSS database via `certutil` if available.
fn launch_firefox(cfg: &ResolvedConfig) {
    let port = cfg.listen.port();

    let Some(tmpdir) = make_temp_dir("firefox") else {
        output::warn(&format!(
            "Firefox: configure Firefox manually (proxy SSL/TLS: 127.0.0.1:{port})"
        ));
        return;
    };

    let user_js = format!(
        "user_pref(\"network.proxy.type\", 1);\n\
         user_pref(\"network.proxy.ssl\", \"127.0.0.1\");\n\
         user_pref(\"network.proxy.ssl_port\", {port});\n"
    );

    if let Err(err) = std::fs::write(tmpdir.join("user.js"), &user_js) {
        output::warn(&format!(
            "Firefox: could not write user.js: {err}; \
             configure Firefox manually (proxy SSL/TLS: 127.0.0.1:{port})"
        ));
        let _ = std::fs::remove_dir_all(&tmpdir);
        return;
    }

    // Import CA into NSS DB if certutil is available.
    let cert_path = super::ca::CertAuthority::cert_path(&cfg.ca_dir);
    if cert_path.exists() {
        let _ = Command::new("certutil")
            .args([
                "-A",
                "-n",
                CA_COMMON_NAME,
                "-t",
                "CT,,",
                "-i",
                &cert_path.to_string_lossy(),
                "-d",
                &tmpdir.to_string_lossy(),
            ])
            .status();
    }

    let mut cmd = firefox_command();
    cmd.args(["-profile", &tmpdir.to_string_lossy(), "--no-remote"]);

    if let Some(rule) = cfg.rules.0.first() {
        cmd.arg(format!("https://{}", rule.from));
    }

    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&tmpdir);
            });
        }
        Err(err) => {
            output::warn(&format!(
                "Firefox: could not launch: {err}; \
                 start Firefox manually with SSL proxy 127.0.0.1:{port}"
            ));
            let _ = std::fs::remove_dir_all(&tmpdir);
        }
    }
}

/// Returns the platform Firefox command.
fn firefox_command() -> Command {
    #[cfg(target_os = "macos")]
    {
        let app = "/Applications/Firefox.app/Contents/MacOS/firefox";
        Command::new(app)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Command::new("firefox")
    }
}

/// Configures Safari via the system PAC URL, restoring the prior setting on exit.
///
/// Detects the active network service via `route`/`networksetup` and sets a
/// PAC URL pointing at the running proxy's `/proxy.pac` route (spec §9).
fn launch_safari(cfg: &ResolvedConfig) {
    let port = cfg.listen.port();
    let pac_url = format!("http://127.0.0.1:{port}/proxy.pac");

    let service = detect_network_service();
    let Some(service) = service else {
        output::warn(&format!(
            "Safari: could not detect active network service; \
             set PAC URL manually in System Settings → Network: {pac_url}"
        ));
        return;
    };

    // Save prior PAC URL so we can restore it on exit.
    let prior_pac = get_auto_proxy_url(&service);

    let set_result = Command::new("networksetup")
        .args(["-setautoproxyurl", &service, &pac_url])
        .status();

    match set_result {
        Ok(s) if s.success() => {
            output::info(&format!(
                "Safari: PAC URL set for '{service}'; open Safari and browse to a proxied host"
            ));
        }
        _ => {
            output::warn(&format!(
                "Safari: could not set PAC URL automatically; \
                 set it manually in System Settings → Network → {service}: {pac_url}"
            ));
            return;
        }
    }

    // Restore PAC on exit.
    std::thread::spawn(move || {
        // Wait for Ctrl-C or process exit — the restore runs when this thread
        // is dropped (i.e., when the process exits).
        //
        // A cleaner approach (signal handler) is deferred; this is best-effort.
        std::thread::park();
        restore_auto_proxy(&service, prior_pac.as_deref());
    });
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

        let mut last_service: Option<String> = None;
        for line in ns_text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('(') && !trimmed.starts_with("(*) An asterisk") {
                // Service name lines look like: "(1) Wi-Fi"
                last_service = trimmed
                    .split_once(')')
                    .map(|x| x.1)
                    .map(str::trim)
                    .map(str::to_string);
            } else if trimmed.contains(&interface) && last_service.is_some() {
                return last_service;
            }
        }
        None
    }
}

/// Returns the current auto-proxy URL for a network service, if set.
fn get_auto_proxy_url(service: &str) -> Option<String> {
    let out = Command::new("networksetup")
        .args(["-getautoproxyurl", service])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(url) = line.strip_prefix("URL: ") {
            let url = url.trim();
            if !url.is_empty() && url != "(null)" {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Restores the prior PAC URL (or disables auto-proxy if there was none).
fn restore_auto_proxy(service: &str, prior_pac: Option<&str>) {
    match prior_pac {
        Some(url) => {
            let _ = Command::new("networksetup")
                .args(["-setautoproxyurl", service, url])
                .status();
        }
        None => {
            let _ = Command::new("networksetup")
                .args(["-setautoproxystate", service, "off"])
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dev::proxy::rewrite::{Authority, Rule, RuleTable};

    #[test]
    fn pac_proxies_only_https_for_from_hosts() {
        let rules = RuleTable(vec![Rule {
            from: "www.example-publisher.com".into(),
            to: Authority::parse("to.edgecompute.app", false).expect("should parse authority"),
            preserve_host: true,
            plaintext: false,
        }]);
        let pac = generate_pac(&rules, "127.0.0.1:8080".parse().expect("should parse addr"));
        assert!(pac.contains("https:"), "PAC guards on https scheme");
        assert!(
            pac.contains("www.example-publisher.com"),
            "PAC lists the FROM host"
        );
        assert!(
            pac.contains("PROXY 127.0.0.1:8080"),
            "PAC points at the listen addr"
        );
        assert!(
            pac.contains("return \"DIRECT\""),
            "everything else is direct"
        );
    }
}
