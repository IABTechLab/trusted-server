use std::net::{Ipv4Addr, Ipv6Addr};

pub(crate) fn validate_host_header_override_value(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }
    if value.trim() != value {
        return Err("must not include leading or trailing whitespace");
    }
    if value.contains("://") {
        return Err("must not include a scheme");
    }
    if value.contains(['/', '\\', '?', '#', '@']) {
        return Err("must not include userinfo, path, query, or fragment");
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err("must not contain whitespace or control characters");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '[' | ']'))
    {
        return Err("contains invalid characters");
    }

    if value.starts_with('[') {
        validate_ipv6_literal_host(value)
    } else {
        validate_dns_or_ipv4_host_with_optional_port(value)
    }
}

fn validate_ipv6_literal_host(value: &str) -> Result<(), &'static str> {
    let closing_bracket = value
        .find(']')
        .ok_or("IPv6 literals must be enclosed in brackets")?;
    let host = &value[1..closing_bracket];
    if host.is_empty() {
        return Err("must include a host");
    }
    if host.parse::<Ipv6Addr>().is_err() {
        return Err("invalid IPv6 address");
    }

    validate_port_suffix(&value[closing_bracket + 1..])
}

fn validate_port_suffix(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Ok(());
    }

    let port = value
        .strip_prefix(':')
        .ok_or("only an optional :port may follow the host")?;
    validate_port(port)
}

fn validate_dns_or_ipv4_host_with_optional_port(value: &str) -> Result<(), &'static str> {
    if value.contains(['[', ']']) {
        return Err("IPv6 literals must be enclosed in brackets");
    }

    let (host, port) = split_host_and_port(value)?;
    validate_dns_or_ipv4_host(host)?;
    if let Some(port) = port {
        validate_port(port)?;
    }

    Ok(())
}

fn split_host_and_port(value: &str) -> Result<(&str, Option<&str>), &'static str> {
    if let Some((host, port)) = value.rsplit_once(':') {
        if host.contains(':') {
            return Err("IPv6 literals must be enclosed in brackets");
        }
        if host.is_empty() {
            return Err("must include a host");
        }
        return Ok((host, Some(port)));
    }

    if value.contains(':') {
        return Err("IPv6 literals must be enclosed in brackets");
    }

    Ok((value, None))
}

fn validate_dns_or_ipv4_host(host: &str) -> Result<(), &'static str> {
    if host.is_empty() {
        return Err("must include a host");
    }
    if host.len() > 253 {
        return Err("host is too long");
    }

    if host.contains('.') && host.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return host
            .parse::<Ipv4Addr>()
            .map(|_| ())
            .map_err(|_| "invalid IPv4 address");
    }

    for label in host.split('.') {
        if label.is_empty() {
            return Err("host labels must not be empty");
        }
        if label.len() > 63 {
            return Err("host labels must not exceed 63 characters");
        }
        let bytes = label.as_bytes();
        if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
            return Err("host labels must not start or end with '-'");
        }
        if !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return Err("host labels may only contain ASCII letters, digits, or '-'");
        }
    }

    Ok(())
}

fn validate_port(port: &str) -> Result<(), &'static str> {
    if port.is_empty() {
        return Err("port must not be empty");
    }
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("port must contain only digits");
    }

    match port.parse::<u16>() {
        Ok(0) => Err("port must be between 1 and 65535"),
        Ok(_) => Ok(()),
        Err(_) => Err("port must be between 1 and 65535"),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_host_header_override_value;

    #[test]
    fn accepts_valid_host_header_override_values() {
        for value in [
            "www.example.com",
            "www.example.com:8443",
            "localhost",
            "localhost:9090",
            "192.168.1.1",
            "192.168.1.1:8080",
            "[::1]",
            "[::1]:8443",
            "[2001:db8::1]",
        ] {
            assert!(
                validate_host_header_override_value(value).is_ok(),
                "{value:?} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_invalid_host_header_override_values() {
        for value in [
            "",
            " www.example.com",
            "www.example.com ",
            "https://www.example.com",
            "www.example.com/path",
            "www.example.com?query=1",
            "www.example.com#fragment",
            "user@www.example.com",
            "www.example.com\n",
            "www.example.com:",
            "www.example.com:0",
            "www.example.com:99999",
            "example..com",
            ".example.com",
            "example.com.",
            "-",
            "-example.com",
            "example-.com",
            "999.999.999.999",
            "::1",
            "[::1",
            "[::1]suffix",
            "[not-ipv6]",
            "www_example.com",
        ] {
            assert!(
                validate_host_header_override_value(value).is_err(),
                "{value:?} should be rejected"
            );
        }
    }
}
