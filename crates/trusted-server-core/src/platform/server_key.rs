//! The start of every key a publisher's entries take in a shared store.

use core::fmt;

use crate::settings::Settings;

/// The start of every key a publisher's entries take in a shared store, so
/// two publishers sharing a store never share a key.
///
/// The publisher's domain from the loaded settings, never from the request,
/// because a `Host` header can say anything and a prefix built from one would
/// let any request start a key space of its own. A single leading `www.` is
/// folded, so `www.example.com` and `example.com` share keys, and every other
/// label is kept, so `clients.example.com` and `public.example.com` do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerKeyPrefix(String);

impl ServerKeyPrefix {
    /// The prefix for the publisher the settings describe.
    #[must_use]
    pub fn for_settings(settings: &Settings) -> Self {
        Self::for_domain(&settings.publisher.domain)
    }

    /// The prefix for a publisher's domain. Every shared store key's prefix is
    /// made here.
    #[must_use]
    pub fn for_domain(domain: &str) -> Self {
        let domain = domain.trim().to_ascii_lowercase();
        let folded = domain.strip_prefix("www.").unwrap_or(&domain);
        Self(format!("ts.{folded}"))
    }

    /// The prefix as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServerKeyPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_www_is_folded_and_every_other_label_kept() {
        for (domain, prefix) in [
            ("example.com", "ts.example.com"),
            ("www.example.com", "ts.example.com"),
            ("WWW.Example.COM", "ts.example.com"),
            ("clients.example.com", "ts.clients.example.com"),
            ("public.example.com", "ts.public.example.com"),
            ("www.www.example.com", "ts.www.example.com"),
        ] {
            assert_eq!(
                ServerKeyPrefix::for_domain(domain).as_str(),
                prefix,
                "{domain}"
            );
        }
    }

    #[test]
    fn the_prefix_comes_from_the_loaded_settings() {
        let mut settings = Settings::default();
        settings.publisher.domain = "www.example.com".to_owned();

        assert_eq!(
            ServerKeyPrefix::for_settings(&settings).as_str(),
            "ts.example.com"
        );
    }
}
