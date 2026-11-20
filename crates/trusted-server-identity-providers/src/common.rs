//! Helpers shared by the vendor provider modules.

use std::borrow::Cow;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroUsize;
use std::time::Duration;

use error_stack::Report;
use serde::Deserialize;
use serde_json::value::RawValue;
use trusted_server_core::ec::eid_types::{SourceDomain, UnixSeconds};
use trusted_server_core::ec::provider::{
    EidLevelFields, EidShape, ProviderCapabilities, ProviderCommon, ProviderConfigError,
    ProviderEid, ProviderExpiry, ProviderKind, ProviderUid, RequestCookies, SourceCapability,
    SourceInput,
};

/// Returns whether `ip` is a public address worth sending to a vendor.
///
/// Drops every special-purpose range that identifies nothing outside the
/// operator's network or is never a real client: for IPv4 "this network"
/// `0.0.0.0/8`, private, carrier-grade NAT `100.64.0.0/10`, loopback,
/// link-local, IETF protocol assignments `192.0.0.0/24`, the TEST-NETs,
/// the 6to4 relay anycast `192.88.99.0/24`, benchmarking `198.18.0.0/15`,
/// multicast and the reserved `240.0.0.0/4` (including broadcast). For IPv6
/// it drops unspecified, loopback, IPv4-compatible, discard-only
/// `100::/64`, IETF protocol assignments `2001::/23`, documentation
/// `2001:db8::/32`, unique-local `fc00::/7`, link-local `fe80::/10`,
/// site-local `fec0::/10` and multicast. IPv4-mapped and NAT64
/// `64:ff9b::/96` addresses are judged by their embedded IPv4 address.
#[must_use]
pub(crate) fn is_routable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_routable_ipv4(v4),
        IpAddr::V6(v6) => is_routable_ipv6(v6),
    }
}

fn is_routable_ipv4(v4: Ipv4Addr) -> bool {
    let [first, second, third, _] = v4.octets();
    let this_network = first == 0;
    let carrier_grade_nat = first == 100 && (second & 0xc0) == 64;
    let protocol_assignments = first == 192 && second == 0 && third == 0;
    let test_net = matches!(
        (first, second, third),
        (192, 0, 2) | (198, 51, 100) | (203, 0, 113)
    );
    let relay_anycast = first == 192 && second == 88 && third == 99;
    let benchmarking = first == 198 && (second & 0xfe) == 18;
    let multicast = (first & 0xf0) == 224;
    let reserved = (first & 0xf0) == 240;
    !(this_network
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || carrier_grade_nat
        || protocol_assignments
        || test_net
        || relay_anycast
        || benchmarking
        || multicast
        || reserved)
}

fn is_routable_ipv6(v6: Ipv6Addr) -> bool {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_routable_ipv4(v4);
    }
    let segments = v6.segments();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., a, b, c, d] = v6.octets();
        return is_routable_ipv4(Ipv4Addr::new(a, b, c, d));
    }
    let first = segments[0];
    let ipv4_compatible = segments[..6] == [0; 6];
    let discard_only = segments[..4] == [0x100, 0, 0, 0];
    let protocol_assignments = first == 0x2001 && segments[1] < 0x200;
    let documentation = first == 0x2001 && segments[1] == 0x0db8;
    let unique_local = (first & 0xfe00) == 0xfc00;
    let link_local = (first & 0xffc0) == 0xfe80;
    let site_local = (first & 0xffc0) == 0xfec0;
    let multicast = (first & 0xff00) == 0xff00;
    !(ipv4_compatible
        || discard_only
        || protocol_assignments
        || documentation
        || unique_local
        || link_local
        || site_local
        || multicast)
}

/// TTL given to a vendor UID whose response states no expiry.
pub(crate) const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Shortest TTL a vendor UID is kept for.
pub(crate) const MIN_TTL: Duration = Duration::from_secs(60 * 60);

/// Longest TTL a vendor UID is kept for.
pub(crate) const MAX_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Turns a vendor-stated TTL into a bounded relative expiry.
///
/// A missing TTL becomes [`DEFAULT_TTL`] so a UID never lives forever. A
/// stated TTL is clamped to [`MIN_TTL`]..=[`MAX_TTL`]: a zero, past or very
/// short TTL keeps the UID for [`MIN_TTL`] instead of discarding a valid
/// ID, and the minimum still forces a refresh soon after.
#[must_use]
pub(crate) fn bounded_expiry(ttl: Option<Duration>) -> ProviderExpiry {
    ProviderExpiry::After(ttl.map_or(DEFAULT_TTL, |ttl| ttl.clamp(MIN_TTL, MAX_TTL)))
}

/// Joins GPP section IDs with commas, as vendors expect them.
#[must_use]
pub(crate) fn join_section_ids(ids: &[u16]) -> String {
    ids.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
}

/// Agent type of the person-based IDs read from vendor cookies.
pub(crate) const COOKIE_ATYPE: i32 = 3;

/// Builds the capabilities of a cookie-only provider, which claims exactly
/// one source and stores one UID for it.
///
/// # Errors
///
/// Returns [`ProviderConfigError::Invalid`] unless exactly one source is
/// configured.
pub(crate) fn cookie_capabilities(
    common: &ProviderCommon<'_>,
    kind: &str,
) -> Result<(ProviderCapabilities, SourceDomain), Report<ProviderConfigError>> {
    let [source] = common.sources else {
        return Err(Report::new(ProviderConfigError::Invalid)
            .attach(format!("a {kind} provider needs exactly one source")));
    };
    let one = NonZeroUsize::MIN;
    let capabilities = ProviderCapabilities {
        sources: Box::new([SourceCapability {
            source: source.clone(),
            input: SourceInput::Anonymous,
        }]),
        shape: EidShape {
            max_eids: one,
            max_uids_per_eid: one,
            eid_level_fields: EidLevelFields::Ignored,
        },
    };
    Ok((capabilities, source.clone()))
}

/// Whether a cookie-carried ID is worth storing: not blank and not a
/// placeholder such as `0` or an all-zero UUID.
///
/// A placeholder reads as "no ID", not as an opt-out: no vendor read with
/// this check documents it as its opt-out signal. Providers whose vendor
/// does (ID5's `"0"`) check for it separately and report an opt-out.
#[must_use]
pub(crate) fn is_usable_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && !id.chars().all(|c| c == '0' || c == '-')
}

/// One EID for `source` holding the single UID `id` with agent type 3.
#[must_use]
pub(crate) fn cookie_eid<'cookie>(
    source: &SourceDomain,
    id: Cow<'cookie, str>,
    expiry: ProviderExpiry,
) -> ProviderEid<'cookie> {
    ProviderEid {
        source: Cow::Owned(source.as_str().to_owned()),
        inserter: None,
        matcher: None,
        mm: None,
        ext: None,
        uids: vec![ProviderUid {
            id,
            atype: Some(COOKIE_ATYPE),
            ext: None,
            expiry,
            refresh: None,
        }],
    }
}

/// Agent type of the device-based IDs read from browser-ID cookies.
pub(crate) const DEVICE_ATYPE: i32 = 1;

/// One EID for `source` holding the single device-based UID `id` with
/// agent type 1.
#[must_use]
pub(crate) fn device_eid<'cookie>(
    source: &SourceDomain,
    id: Cow<'cookie, str>,
    expiry: ProviderExpiry,
) -> ProviderEid<'cookie> {
    let mut eid = cookie_eid(source, id, expiry);
    for uid in &mut eid.uids {
        uid.atype = Some(DEVICE_ATYPE);
    }
    eid
}

/// Percent-decodes a cookie value, keeping it unchanged when it is not
/// valid percent-encoded UTF-8.
#[must_use]
pub(crate) fn percent_decoded(value: &str) -> Cow<'_, str> {
    urlencoding::decode(value).unwrap_or(Cow::Borrowed(value))
}

/// Settings for the Lockr capability, under a provider's
/// `[ec.identity_providers.settings.lockr]`.
///
/// Lockr AIM writes other vendors' IDs into first-party cookies on the
/// publisher domain, under key names configured per publisher in Lockr.
/// Only kinds implementing [`ReadsLockr`] accept this table; the settings
/// of every other kind reject it as an unknown field.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockrSettings {
    /// Whether the Lockr cookies are read. Off by default.
    #[serde(default)]
    pub enabled: bool,
    /// Lockr cookies read in order, after the vendor's own cookies.
    /// Defaults to the vendor's known Lockr key.
    #[serde(default)]
    pub cookie_names: Option<Vec<String>>,
}

/// The Lockr capability: declared by provider kinds whose IDs Lockr AIM
/// delivers.
pub(crate) trait ReadsLockr: ProviderKind {
    /// The key names Lockr uses for this vendor by default.
    const LOCKR_COOKIE_NAMES: &'static [&'static str];
}

/// Reads a provider's cookie-carried ID: the vendor's own cookies in order,
/// then, when the Lockr capability is enabled, the Lockr cookies in order.
///
/// Each value goes to the provider's own parser, so Lockr reuses it.
#[derive(Debug)]
pub(crate) struct CookieReader {
    provider_id: String,
    own: Box<[String]>,
    lockr: Box<[String]>,
}

impl CookieReader {
    /// A reader for a kind without the Lockr capability.
    pub(crate) fn own(common: &ProviderCommon<'_>, own: Vec<String>) -> Self {
        Self {
            provider_id: common.id.to_owned(),
            own: own.into_boxed_slice(),
            lockr: Box::default(),
        }
    }

    /// A reader for a kind with the Lockr capability. Lockr names already
    /// in `own` are skipped, since they were read first.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderConfigError::Invalid`] when Lockr is enabled with
    /// an empty `cookie_names` list.
    pub(crate) fn with_lockr<K: ReadsLockr>(
        common: &ProviderCommon<'_>,
        own: Vec<String>,
        lockr: LockrSettings,
    ) -> Result<Self, Report<ProviderConfigError>> {
        let mut reader = Self::own(common, own);
        if !lockr.enabled {
            return Ok(reader);
        }
        let names = lockr.cookie_names.unwrap_or_else(|| {
            K::LOCKR_COOKIE_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        });
        if names.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
                "lockr for a {} provider needs at least one cookie name",
                K::KIND
            )));
        }
        reader.lockr = names
            .into_iter()
            .filter(|name| !reader.own.contains(name))
            .collect();
        Ok(reader)
    }

    /// The Lockr cookie names to read for this request: none where GDPR
    /// applies, because Lockr AIM does not run there, so a Lockr cookie on
    /// such a request is a leftover rather than a current delivery.
    fn lockr_names<'reader>(
        &'reader self,
        cookies: RequestCookies<'_>,
    ) -> impl Iterator<Item = &'reader String> + 'reader {
        let names: &[String] = if cookies.gdpr_applies() {
            &[]
        } else {
            &self.lockr
        };
        names.iter()
    }

    /// Whether any cookie name is configured.
    pub(crate) fn is_empty(&self) -> bool {
        self.own.is_empty() && self.lockr.is_empty()
    }

    /// Whether any configured cookie, own or Lockr, has a value matching
    /// `matches`.
    pub(crate) fn any(&self, cookies: RequestCookies<'_>, matches: impl Fn(&str) -> bool) -> bool {
        self.own
            .iter()
            .chain(self.lockr_names(cookies))
            .flat_map(|name| cookies.get_all(name))
            .any(matches)
    }

    /// Returns the first value `parse` accepts, trying every value of each
    /// own cookie before the Lockr cookies, so an unusable or expired value
    /// never hides a later usable one.
    pub(crate) fn read<'cookie, T>(
        &self,
        cookies: RequestCookies<'cookie>,
        mut parse: impl FnMut(&'cookie str) -> Option<T>,
    ) -> Option<T> {
        if let Some(found) = self
            .own
            .iter()
            .flat_map(|name| cookies.get_all(name))
            .find_map(&mut parse)
        {
            return Some(found);
        }
        let (name, found) = self.lockr_names(cookies).find_map(|name| {
            let found = cookies.get_all(name).find_map(&mut parse)?;
            Some((name, found))
        })?;
        log::debug!(
            "Identity provider '{}' reads its ID from cookie '{name}' (origin lockr)",
            self.provider_id
        );
        Some(found)
    }
}

/// Whether a cookie's stated expiry has passed at request time `now`.
///
/// Cookie readers skip such a value so it never hides a later live one.
pub(crate) fn is_expired(expiry: &ProviderExpiry, now: UnixSeconds) -> bool {
    matches!(expiry, ProviderExpiry::At(at) if *at <= now)
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
            ext: self.ext.map(Cow::Borrowed),
            uids: self
                .uids
                .into_iter()
                .map(|uid| ProviderUid {
                    id: uid.id,
                    atype: uid.atype,
                    ext: uid.ext.map(Cow::Borrowed),
                    expiry,
                    refresh: None,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_public_addresses() {
        for ip in [
            "11.22.33.44",
            "203.0.112.9",
            "2a00:1450::1",
            "::ffff:11.22.33.44",
        ] {
            assert!(
                is_routable_ip(ip.parse().expect("should parse")),
                "should keep {ip}"
            );
        }
    }

    #[test]
    fn drops_special_purpose_addresses() {
        for ip in [
            "0.1.2.3",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "192.0.0.8",
            "192.0.2.10",
            "198.51.100.7",
            "203.0.113.5",
            "192.88.99.1",
            "198.18.0.1",
            "198.19.255.1",
            "224.0.0.1",
            "239.1.1.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::10.1.2.3",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "fc00::1",
            "fd00::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:100.64.0.1",
            "64:ff9b::10.1.2.3",
        ] {
            assert!(
                !is_routable_ip(ip.parse().expect("should parse")),
                "should drop {ip}"
            );
        }
    }

    #[test]
    fn filters_blank_and_opt_out_ids() {
        for id in ["", "  ", "0", "00000000-0000-0000-0000-000000000000"] {
            assert!(!is_usable_id(id), "should drop {id:?}");
        }
        assert!(is_usable_id("id-example-1"), "should keep a real ID");
    }

    #[test]
    fn percent_decodes_leniently() {
        assert_eq!(percent_decoded("a%20b"), "a b", "should decode");
        assert_eq!(
            percent_decoded("a+b%zz"),
            "a+b%zz",
            "should keep invalid input"
        );
    }

    #[test]
    fn bounds_vendor_expiry() {
        for (ttl, expected) in [
            (None, DEFAULT_TTL),
            (Some(Duration::ZERO), MIN_TTL),
            (Some(Duration::from_secs(7200)), Duration::from_secs(7200)),
            (Some(Duration::from_secs(u64::MAX)), MAX_TTL),
        ] {
            assert!(
                matches!(bounded_expiry(ttl), ProviderExpiry::After(after) if after == expected),
                "should bound {ttl:?} to {expected:?}"
            );
        }
    }
}
