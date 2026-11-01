//! Helpers shared by the vendor provider modules.

use std::borrow::Cow;
use std::net::IpAddr;

use serde::Deserialize;
use serde_json::value::RawValue;
use trusted_server_core::ec::provider::{ProviderEid, ProviderExpiry, ProviderUid};

/// Returns whether `ip` is a public address worth sending to a vendor.
///
/// Loopback, private, link-local, carrier-grade NAT and unique-local
/// addresses identify nothing outside the operator's network.
#[must_use]
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

/// Joins GPP section IDs with commas, as vendors expect them.
#[must_use]
pub(crate) fn join_section_ids(ids: &[u16]) -> String {
    ids.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
}

/// A complete `OpenRTB` EID as a vendor returns it, borrowed from the body.
///
/// Unknown fields are ignored so one new vendor field does not fail the
/// whole response.
#[derive(Debug, Deserialize)]
pub(crate) struct VendorEid<'body> {
    #[serde(borrow)]
    pub source: Cow<'body, str>,
    #[serde(default, borrow)]
    pub uids: Vec<VendorUid<'body>>,
    #[serde(default, borrow)]
    pub inserter: Option<Cow<'body, str>>,
    #[serde(default, borrow)]
    pub matcher: Option<Cow<'body, str>>,
    #[serde(default)]
    pub mm: Option<i32>,
    #[serde(default, borrow)]
    pub ext: Option<&'body RawValue>,
}

/// One UID of a [`VendorEid`].
#[derive(Debug, Deserialize)]
pub(crate) struct VendorUid<'body> {
    #[serde(borrow)]
    pub id: Cow<'body, str>,
    #[serde(default)]
    pub atype: Option<i32>,
    #[serde(default, borrow)]
    pub ext: Option<&'body RawValue>,
}

impl<'body> VendorEid<'body> {
    /// Converts to a [`ProviderEid`], giving every UID `expiry`.
    pub(crate) fn into_provider_eid(self, expiry: ProviderExpiry) -> ProviderEid<'body> {
        ProviderEid {
            source: self.source,
            inserter: self.inserter,
            matcher: self.matcher,
            mm: self.mm,
            ext: self.ext,
            uids: self
                .uids
                .into_iter()
                .map(|uid| ProviderUid {
                    id: uid.id,
                    atype: uid.atype,
                    ext: uid.ext,
                    expiry,
                })
                .collect(),
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
            "10.1.2.3",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "::1",
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
