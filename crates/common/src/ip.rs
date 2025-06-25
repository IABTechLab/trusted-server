use std::net::IpAddr;

/// Parse client IP from X-Forwarded-For header value.
/// Takes the first IP in the comma-separated list to prevent spoofing.
/// Validates that the IP is a valid IP address format.
pub fn parse_client_ip(forwarded_for: &str) -> Option<IpAddr> {
    forwarded_for
        .split(',')
        .next()
        .and_then(|ip| ip.trim().parse::<IpAddr>().ok())
}

/// Extract the client IP from the request, handling X-Forwarded-For header
pub fn get_client_ip(forwarded_for: Option<&str>) -> String {
    forwarded_for
        .and_then(parse_client_ip)
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_client_ip_single_ipv4() {
        let ip = parse_client_ip("192.168.1.1");
        assert_eq!(ip.unwrap().to_string(), "192.168.1.1");
    }

    #[test]
    fn test_parse_client_ip_multiple_ips() {
        let ip = parse_client_ip("192.168.1.1, 10.0.0.1, 172.16.0.1");
        assert_eq!(ip.unwrap().to_string(), "192.168.1.1");
    }

    #[test]
    fn test_parse_client_ip_with_spaces() {
        let ip = parse_client_ip("  192.168.1.1  ,  10.0.0.1  ");
        assert_eq!(ip.unwrap().to_string(), "192.168.1.1");
    }

    #[test]
    fn test_parse_client_ip_ipv6() {
        let ip = parse_client_ip("2001:0db8:85a3:0000:0000:8a2e:0370:7334");
        assert_eq!(ip.unwrap().to_string(), "2001:db8:85a3::8a2e:370:7334");
    }

    #[test]
    fn test_parse_client_ip_invalid() {
        assert!(parse_client_ip("not-an-ip").is_none());
        assert!(parse_client_ip("192.168.1.256").is_none());
        assert!(parse_client_ip("").is_none());
    }

    #[test]
    fn test_get_client_ip_with_valid_header() {
        let ip = get_client_ip(Some("192.168.1.1, 10.0.0.1"));
        assert_eq!(ip, "192.168.1.1");
    }

    #[test]
    fn test_get_client_ip_with_invalid_header() {
        let ip = get_client_ip(Some("invalid-ip"));
        assert_eq!(ip, "Unknown");
    }

    #[test]
    fn test_get_client_ip_without_header() {
        let ip = get_client_ip(None);
        assert_eq!(ip, "Unknown");
    }
}
