use std::net::IpAddr;
use std::sync::Arc;

/// Upstream transport protocol.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum Transport {
    Plaintext,
    Tls,
}

/// Certificate-verification policy for a TLS connection.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum VerifyMode {
    Secure,
    Insecure,
}

/// HTTP application protocols a connection may negotiate.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum ApplicationMode {
    Http1Required,
    Http2Eligible,
}

/// How the logical origin selects its connection address.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum AddressPolicy {
    Dns,
    Resolve(IpAddr),
}

/// Exact DNS or IP identity authenticated by upstream TLS.
#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub enum ReferenceIdentity {
    Dns(Arc<str>),
    Ip(IpAddr),
}

impl ReferenceIdentity {
    #[must_use]
    pub fn dns(host: &str) -> Self {
        Self::Dns(Arc::from(host.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn ip(address: IpAddr) -> Self {
        Self::Ip(address)
    }
}

/// Complete identity for selecting a reusable upstream connection.
#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct OriginKey {
    transport: Transport,
    reference: ReferenceIdentity,
    port: u16,
    verify: VerifyMode,
    application: ApplicationMode,
    address: AddressPolicy,
}

impl OriginKey {
    #[must_use]
    pub fn new(
        transport: Transport,
        reference: ReferenceIdentity,
        port: u16,
        verify: VerifyMode,
        application: ApplicationMode,
        address: AddressPolicy,
    ) -> Self {
        Self {
            transport,
            reference,
            port,
            verify,
            application,
            address,
        }
    }

    #[must_use]
    pub fn transport(&self) -> Transport {
        self.transport
    }

    #[must_use]
    pub fn reference(&self) -> &ReferenceIdentity {
        &self.reference
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn verify_mode(&self) -> VerifyMode {
        self.verify
    }

    #[must_use]
    pub fn application_mode(&self) -> ApplicationMode {
        self.application
    }

    #[must_use]
    pub fn address_policy(&self) -> AddressPolicy {
        self.address
    }

    pub(crate) fn set_address_policy(&mut self, address: AddressPolicy) {
        self.address = address;
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn base_key() -> OriginKey {
        OriginKey::new(
            Transport::Tls,
            ReferenceIdentity::dns("to.example.com"),
            443,
            VerifyMode::Secure,
            ApplicationMode::Http1Required,
            AddressPolicy::Dns,
        )
    }

    #[test]
    fn origin_key_separates_every_transport_and_security_field() {
        let base = base_key();
        let pinned = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let variants = [
            OriginKey::new(
                Transport::Plaintext,
                base.reference().clone(),
                base.port(),
                base.verify_mode(),
                base.application_mode(),
                base.address_policy(),
            ),
            OriginKey::new(
                base.transport(),
                ReferenceIdentity::dns("other.example.com"),
                base.port(),
                base.verify_mode(),
                base.application_mode(),
                base.address_policy(),
            ),
            OriginKey::new(
                base.transport(),
                base.reference().clone(),
                8443,
                base.verify_mode(),
                base.application_mode(),
                base.address_policy(),
            ),
            OriginKey::new(
                base.transport(),
                base.reference().clone(),
                base.port(),
                VerifyMode::Insecure,
                base.application_mode(),
                base.address_policy(),
            ),
            OriginKey::new(
                base.transport(),
                base.reference().clone(),
                base.port(),
                base.verify_mode(),
                ApplicationMode::Http2Eligible,
                base.address_policy(),
            ),
            OriginKey::new(
                base.transport(),
                base.reference().clone(),
                base.port(),
                base.verify_mode(),
                base.application_mode(),
                AddressPolicy::Resolve(pinned),
            ),
        ];

        for variant in variants {
            assert_ne!(base, variant, "should separate every key field");
        }
    }

    #[test]
    fn dns_peer_selection_does_not_fragment_logical_origin() {
        let first_peer = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let second_peer = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));

        let first = base_key();
        let second = base_key();

        assert_ne!(first_peer, second_peer, "test peers should differ");
        assert_eq!(
            first, second,
            "selected peer address should not be part of a DNS origin key"
        );
    }

    #[test]
    fn shared_resolve_address_does_not_coalesce_reference_identities() {
        let pin = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let first = OriginKey::new(
            Transport::Tls,
            ReferenceIdentity::dns("one.example.com"),
            443,
            VerifyMode::Secure,
            ApplicationMode::Http1Required,
            AddressPolicy::Resolve(pin),
        );
        let second = OriginKey::new(
            Transport::Tls,
            ReferenceIdentity::dns("two.example.com"),
            443,
            VerifyMode::Secure,
            ApplicationMode::Http1Required,
            AddressPolicy::Resolve(pin),
        );

        assert_ne!(first, second, "different TLS identities must not coalesce");
    }
}
