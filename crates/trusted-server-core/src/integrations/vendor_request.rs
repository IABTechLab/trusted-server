//! Helpers shared by the identity vendors' requests.

use std::net::IpAddr;

/// Whether `ip` can identify a visitor to a vendor: not loopback,
/// unspecified, private, link-local or carrier-grade NAT.
///
/// A vendor that matches on the client IP answers no match for such an
/// address, as it does for a request from a local dev server, so the IP is
/// left out instead.
pub(crate) fn is_routable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            let carrier_grade_nat = first == 100 && (second & 0xc0) == 64;
            !(v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || carrier_grade_nat)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_routable_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            let unique_local = (first & 0xfe00) == 0xfc00;
            let link_local = (first & 0xffc0) == 0xfe80;
            !(v6.is_loopback() || v6.is_unspecified() || unique_local || link_local)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_public_addresses_and_drops_local_ones() {
        for ip in ["192.0.2.10", "198.51.100.7", "2001:db8::1"] {
            assert!(
                is_routable_ip(ip.parse().expect("should parse")),
                "should keep {ip}"
            );
        }
        for ip in [
            "127.0.0.1",
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                !is_routable_ip(ip.parse().expect("should parse")),
                "should drop {ip}"
            );
        }
    }
}
