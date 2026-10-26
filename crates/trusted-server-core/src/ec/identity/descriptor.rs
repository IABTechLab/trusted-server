//! What a provider is, may write, needs and must respect.

/// An input a provider can ask for.
///
/// Consent strings, the provider's stored IDs, the publisher domain, the
/// time and the declared endpoint are always available. Private state is
/// available to providers declared `with_private_state()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display)]
#[non_exhaustive]
pub enum Input {
    /// The platform's client address.
    #[display("client_ip")]
    ClientIp,
    /// The request's `User-Agent`.
    #[display("user_agent")]
    UserAgent,
    /// The top-level page URL without query string or fragment.
    #[display("page_url")]
    PageUrl,
    /// Country and subdivision only.
    #[display("region")]
    Region,
    /// A SHA-256 of a normalized email address. Never available in v1.
    #[display("hashed_email")]
    HashedEmail,
}

/// Something a provider read without declaring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display)]
#[non_exhaustive]
pub enum Access {
    /// An undeclared [`Input`].
    #[display("{_0}")]
    Input(Input),
    /// The endpoint, when the descriptor declares no `resolves`.
    #[display("endpoint")]
    Endpoint,
    /// Private state, without `with_private_state()`.
    #[display("private_state")]
    PrivateState,
}
