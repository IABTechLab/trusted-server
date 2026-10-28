//! The start of every key a publisher's entries take in a shared store.

use core::fmt;

/// The start of every key a publisher's entries take in a shared store, so
/// two publishers sharing a store never share a key.
///
/// Written `<head>.<domain>`, where the head names who keeps the store, such
/// as `ts`, and the domain is the publisher's. The selected prefix module
/// gives both, see [`crate::prefix`], taking the domain from the publisher the
/// request resolved to and never from the raw request, because a `Host`
/// header can say anything and a prefix built from one would let any request
/// start a key space of its own.
///
/// A single leading `www.` is folded, so `www.example.com` and `example.com`
/// share keys, and every other label is kept, so `clients.example.com` and
/// `public.example.com` do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerKeyPrefix {
    text: String,
    domain_start: usize,
}

impl ServerKeyPrefix {
    /// The prefix `head` and the publisher's `domain` make. Every shared store
    /// key's prefix is made here.
    #[must_use]
    pub fn new(head: &str, domain: &str) -> Self {
        let head = head.trim();
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let folded = domain.strip_prefix("www.").unwrap_or(&domain);
        Self {
            text: format!("{head}.{folded}"),
            domain_start: head.len() + 1,
        }
    }

    /// The prefix as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The publisher's domain the prefix carries, lowercased and with a
    /// leading `www.` folded.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.text[self.domain_start..]
    }
}

impl fmt::Display for ServerKeyPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
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
            ("example.com.", "ts.example.com"),
            ("clients.example.com", "ts.clients.example.com"),
            ("public.example.com", "ts.public.example.com"),
            ("www.www.example.com", "ts.www.example.com"),
        ] {
            assert_eq!(
                ServerKeyPrefix::new("ts", domain).as_str(),
                prefix,
                "{domain}"
            );
        }
    }

    #[test]
    fn the_domain_is_the_folded_one() {
        let prefix = ServerKeyPrefix::new("ts", "www.example.com");

        assert_eq!(prefix.domain(), "example.com");
        assert_eq!(prefix.to_string(), "ts.example.com");
    }
}
