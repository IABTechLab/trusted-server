//! Pure adapter backend naming and auction-target capability policies.
//!
//! These policies contain no platform SDK calls. Startup validation, CLI
//! validation, and runtime adapters can therefore predict the same backend
//! names before any backend registration occurs.

use core::fmt::Write as _;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::PlatformBackendSpec;
use crate::host_header::validate_host_header_override_value;

const MAX_FASTLY_BACKEND_NAME_LEN: usize = 255;
const MAX_FASTLY_READABLE_PREFIX_LEN: usize = 200;
const FASTLY_SPEC_DIGEST_HEX_LEN: usize = 32;
const FASTLY_TRANSPORT_TIMEOUT_QUANTUM_MS: u32 = 250;
const FASTLY_TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS: u32 = 2000;
const FASTLY_SUB_QUANTUM_LADDER_MS: [u32; 4] = [200, 150, 100, 50];
const FASTLY_TRANSPORT_TIMEOUT_COARSE_LADDER_MS: [u32; 8] =
    [2000, 3000, 5000, 10000, 20000, 30000, 45000, 60000];
const FASTLY_DYNAMIC_BACKEND_LIMIT: usize = 200;
const FASTLY_NON_AUCTION_BACKEND_RESERVE: usize = 40;

/// A pure backend-name and transport-timeout policy for one adapter.
///
/// Cloudflare and Spin intentionally remain separate variants even though
/// their current no-registration name formats are identical. Keeping distinct
/// policies prevents a future adapter-specific change from silently affecting
/// the other target.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BackendNamingPolicy {
    /// Fastly dynamic backend naming and bounded timeout buckets.
    Fastly,
    /// Axum's environment-segment-compatible correlation name.
    Axum,
    /// Cloudflare's deterministic no-registration correlation name.
    Cloudflare,
    /// Spin's deterministic no-registration correlation name.
    Spin,
}

/// Pure backend prediction shared by validation and runtime registration.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PredictedBackend {
    /// Deterministic platform backend or correlation name.
    pub name: String,
    /// Explicit or scheme-derived target port.
    pub port: u16,
}

/// Detailed pure backend prediction error.
#[derive(Debug, Clone, Eq, PartialEq, derive_more::Display)]
pub enum BackendNamingError {
    /// The backend host is empty.
    #[display("missing host")]
    MissingHost,
    /// A field contains a control character.
    #[display("{field} contains control characters")]
    ControlCharacters { field: &'static str },
    /// An outbound Host override is invalid.
    #[display("host header override {reason}")]
    InvalidHostHeaderOverride { reason: &'static str },
    /// A generated Fastly backend name exceeded its documented limit.
    #[display("backend name exceeds {limit}-char limit ({actual} chars)")]
    NameTooLong { limit: usize, actual: usize },
}

impl core::error::Error for BackendNamingError {}

impl BackendNamingPolicy {
    /// Predict a backend name and resolved port without platform I/O.
    ///
    /// # Errors
    ///
    /// Returns a naming error when a Fastly backend specification contains an
    /// invalid host, scheme, Host override, or cannot fit the platform limit.
    pub fn predict(
        self,
        spec: &PlatformBackendSpec,
    ) -> Result<PredictedBackend, Report<BackendNamingError>> {
        match self {
            Self::Fastly => predict_fastly(spec),
            Self::Axum => Ok(predict_axum(spec)),
            Self::Cloudflare => Ok(predict_cloudflare(spec)),
            Self::Spin => Ok(predict_spin(spec)),
        }
    }

    /// Canonicalize a transport timer without changing the logical budget.
    ///
    /// The result configures adapter transport timers and backend-name
    /// stability only. It is not an auction-wide deadline. Fastly bounds the
    /// cardinality of budget-derived timers because timers are encoded in
    /// dynamic backend names; other adapters preserve the exact bounded value.
    #[must_use]
    pub fn canonicalize_transport_timeout_ms(self, remaining_ms: u32, configured_ms: u32) -> u32 {
        match self {
            Self::Fastly => canonicalize_fastly_transport_timeout_ms(remaining_ms, configured_ms),
            Self::Axum | Self::Cloudflare | Self::Spin => remaining_ms.min(configured_ms),
        }
    }

    /// Return the target-specific backend-name budget available to auction providers.
    #[must_use]
    pub(crate) fn auction_dynamic_backend_budget(self) -> Option<usize> {
        matches!(self, Self::Fastly).then_some(
            FASTLY_DYNAMIC_BACKEND_LIMIT.saturating_sub(FASTLY_NON_AUCTION_BACKEND_RESERVE),
        )
    }

    /// Count every transport-timeout bucket one provider can reach.
    #[must_use]
    pub(crate) fn transport_timeout_bucket_count(self, configured_ms: u32) -> usize {
        if !matches!(self, Self::Fastly) || configured_ms == 0 {
            return 1;
        }
        let mut buckets = std::collections::BTreeSet::new();
        buckets.insert(configured_ms);
        for remaining_ms in FASTLY_SUB_QUANTUM_LADDER_MS {
            let timeout = self.canonicalize_transport_timeout_ms(remaining_ms, configured_ms);
            if timeout > 0 {
                buckets.insert(timeout);
            }
        }
        for remaining_ms in (FASTLY_TRANSPORT_TIMEOUT_QUANTUM_MS
            ..FASTLY_TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS)
            .step_by(FASTLY_TRANSPORT_TIMEOUT_QUANTUM_MS as usize)
        {
            let timeout = self.canonicalize_transport_timeout_ms(remaining_ms, configured_ms);
            if timeout > 0 {
                buckets.insert(timeout);
            }
        }
        for remaining_ms in FASTLY_TRANSPORT_TIMEOUT_COARSE_LADDER_MS {
            let timeout = self.canonicalize_transport_timeout_ms(remaining_ms, configured_ms);
            if timeout > 0 {
                buckets.insert(timeout);
            }
        }
        buckets.len()
    }
}

/// Canonical adapter target identifier accepted by Trusted Server tooling.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuctionTargetId {
    /// Fastly Compute.
    Fastly,
    /// Native Axum development server.
    Axum,
    /// Cloudflare Workers.
    Cloudflare,
    /// Fermyon Spin.
    Spin,
}

impl AuctionTargetId {
    /// Map a canonical `EdgeZero` adapter registry ID to an auction target.
    #[must_use]
    pub fn from_adapter_id(adapter_id: &str) -> Option<Self> {
        match adapter_id {
            "fastly" => Some(Self::Fastly),
            "axum" => Some(Self::Axum),
            "cloudflare" => Some(Self::Cloudflare),
            "spin" => Some(Self::Spin),
            _ => None,
        }
    }

    /// Return the canonical `EdgeZero` adapter registry ID.
    #[must_use]
    pub const fn adapter_id(self) -> &'static str {
        match self {
            Self::Fastly => "fastly",
            Self::Axum => "axum",
            Self::Cloudflare => "cloudflare",
            Self::Spin => "spin",
        }
    }

    /// Return the shared naming and capability descriptor for this target.
    #[must_use]
    pub const fn descriptor(self) -> AuctionTargetDescriptor {
        match self {
            Self::Fastly => AuctionTargetDescriptor {
                id: self,
                naming_policy: BackendNamingPolicy::Fastly,
                capabilities: AuctionTargetCapabilities {
                    concurrent_provider_fanout: true,
                    enforceable_total_request_deadline: false,
                },
            },
            Self::Axum => AuctionTargetDescriptor {
                id: self,
                naming_policy: BackendNamingPolicy::Axum,
                capabilities: AuctionTargetCapabilities {
                    concurrent_provider_fanout: true,
                    enforceable_total_request_deadline: false,
                },
            },
            Self::Cloudflare => AuctionTargetDescriptor {
                id: self,
                naming_policy: BackendNamingPolicy::Cloudflare,
                capabilities: AuctionTargetCapabilities {
                    concurrent_provider_fanout: false,
                    enforceable_total_request_deadline: false,
                },
            },
            Self::Spin => AuctionTargetDescriptor {
                id: self,
                naming_policy: BackendNamingPolicy::Spin,
                capabilities: AuctionTargetCapabilities {
                    concurrent_provider_fanout: false,
                    enforceable_total_request_deadline: false,
                },
            },
        }
    }
}

/// Adapter capabilities relevant to auction dispatch validation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct AuctionTargetCapabilities {
    concurrent_provider_fanout: bool,
    enforceable_total_request_deadline: bool,
}

impl AuctionTargetCapabilities {
    /// Return whether multiple provider requests can be in flight concurrently.
    #[must_use]
    pub const fn supports_concurrent_provider_fanout(self) -> bool {
        self.concurrent_provider_fanout
    }

    /// Return whether the adapter enforces a total request deadline per provider.
    ///
    /// Transport first-byte or between-byte timers do not satisfy this
    /// capability because they do not cap the complete request lifetime.
    #[must_use]
    pub const fn has_enforceable_total_request_deadline(self) -> bool {
        self.enforceable_total_request_deadline
    }
}

/// Shared target descriptor used by plan validation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct AuctionTargetDescriptor {
    id: AuctionTargetId,
    naming_policy: BackendNamingPolicy,
    capabilities: AuctionTargetCapabilities,
}

impl AuctionTargetDescriptor {
    /// Return the canonical target identity.
    #[must_use]
    pub const fn id(self) -> AuctionTargetId {
        self.id
    }

    /// Return the pure backend naming and transport timer policy.
    #[must_use]
    pub const fn naming_policy(self) -> BackendNamingPolicy {
        self.naming_policy
    }

    /// Return this target's auction dispatch capabilities.
    #[must_use]
    pub const fn capabilities(self) -> AuctionTargetCapabilities {
        self.capabilities
    }
}

fn default_port(scheme: &str, https_case_insensitive: bool) -> u16 {
    let https = if https_case_insensitive {
        scheme.eq_ignore_ascii_case("https")
    } else {
        scheme == "https"
    };
    if https { 443 } else { 80 }
}

fn sanitize_fastly_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn canonical_fastly_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .filter(|value| value.parse::<std::net::Ipv6Addr>().is_ok())
        .unwrap_or(host)
}

fn fastly_canonical_spec(spec: &PlatformBackendSpec, target_port: u16) -> String {
    fn push_field(buffer: &mut String, field: &str) {
        buffer.push_str(&field.len().to_string());
        buffer.push(':');
        buffer.push_str(field);
    }

    let mut buffer = String::new();
    push_field(&mut buffer, &spec.scheme);
    push_field(&mut buffer, canonical_fastly_host(&spec.host));
    push_field(&mut buffer, &target_port.to_string());
    push_field(&mut buffer, if spec.certificate_check { "1" } else { "0" });
    match spec.host_header_override.as_deref() {
        Some(value) => {
            buffer.push('s');
            push_field(&mut buffer, value);
        }
        None => buffer.push('n'),
    }
    match spec.discriminator.as_deref() {
        Some(value) => {
            buffer.push('s');
            push_field(&mut buffer, value);
        }
        None => buffer.push('n'),
    }
    push_field(
        &mut buffer,
        &spec.first_byte_timeout.as_millis().to_string(),
    );
    push_field(
        &mut buffer,
        &spec.between_bytes_timeout.as_millis().to_string(),
    );
    buffer
}

fn fastly_spec_digest(canonical: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(FASTLY_SPEC_DIGEST_HEX_LEN);
    for byte in digest.iter().take(FASTLY_SPEC_DIGEST_HEX_LEN / 2) {
        write!(hex, "{byte:02x}").expect("should write hex digit to string");
    }
    hex
}

fn predict_fastly(
    spec: &PlatformBackendSpec,
) -> Result<PredictedBackend, Report<BackendNamingError>> {
    if spec.host.is_empty() {
        return Err(Report::new(BackendNamingError::MissingHost));
    }
    if spec.host.chars().any(char::is_control) {
        return Err(Report::new(BackendNamingError::ControlCharacters {
            field: "host",
        }));
    }
    if spec.scheme.chars().any(char::is_control) {
        return Err(Report::new(BackendNamingError::ControlCharacters {
            field: "scheme",
        }));
    }
    if let Some(host_header_override) = spec.host_header_override.as_deref() {
        validate_host_header_override_value(host_header_override).map_err(|reason| {
            Report::new(BackendNamingError::InvalidHostHeaderOverride { reason })
        })?;
    }

    let port = spec
        .port
        .unwrap_or_else(|| default_port(&spec.scheme, true));
    let name_base = format!(
        "{}_{}_{}",
        spec.scheme,
        canonical_fastly_host(&spec.host),
        port
    );
    let host_override_suffix = spec
        .host_header_override
        .as_deref()
        .map(|host| format!("_oh_{}", sanitize_fastly_component(host)))
        .unwrap_or_default();
    let cert_suffix = if spec.certificate_check {
        ""
    } else {
        "_nocert"
    };
    let discriminator_suffix = spec
        .discriminator
        .as_deref()
        .map(|value| format!("_p_{}", sanitize_fastly_component(value)))
        .unwrap_or_default();
    let readable_full = format!(
        "{}{}{}{}_fb{}_bb{}",
        sanitize_fastly_component(&name_base),
        host_override_suffix,
        cert_suffix,
        discriminator_suffix,
        spec.first_byte_timeout.as_millis(),
        spec.between_bytes_timeout.as_millis()
    );
    let readable = readable_full
        .chars()
        .take(MAX_FASTLY_READABLE_PREFIX_LEN)
        .collect::<String>();
    let digest = fastly_spec_digest(&fastly_canonical_spec(spec, port));
    let name = format!("backend_{readable}_{digest}");
    if name.len() > MAX_FASTLY_BACKEND_NAME_LEN {
        return Err(Report::new(BackendNamingError::NameTooLong {
            limit: MAX_FASTLY_BACKEND_NAME_LEN,
            actual: name.len(),
        }));
    }

    Ok(PredictedBackend { name, port })
}

fn normalize_axum_segment(value: &str) -> String {
    value.to_uppercase().replace(['-', '.', ' '], "_")
}

fn predict_axum(spec: &PlatformBackendSpec) -> PredictedBackend {
    let port = spec
        .port
        .unwrap_or_else(|| default_port(&spec.scheme, false));
    let discriminator = spec
        .discriminator
        .as_deref()
        .map(|value| format!("_p_{}", normalize_axum_segment(value)))
        .unwrap_or_default();
    PredictedBackend {
        name: format!(
            "{}_{}_{}{}",
            normalize_axum_segment(&spec.scheme),
            normalize_axum_segment(&spec.host),
            port,
            discriminator
        ),
        port,
    }
}

fn predict_no_registration(spec: &PlatformBackendSpec) -> PredictedBackend {
    let port = spec
        .port
        .unwrap_or_else(|| default_port(&spec.scheme, false));
    let cert_suffix = if spec.certificate_check {
        ""
    } else {
        "_nocert"
    };
    let discriminator = spec
        .discriminator
        .as_deref()
        .map(|value| format!("_p_{value}"))
        .unwrap_or_default();
    PredictedBackend {
        name: format!(
            "{}_{}_{}_{}ms{cert_suffix}{discriminator}",
            spec.scheme,
            spec.host,
            port,
            spec.first_byte_timeout.as_millis()
        ),
        port,
    }
}

fn predict_cloudflare(spec: &PlatformBackendSpec) -> PredictedBackend {
    predict_no_registration(spec)
}

fn predict_spin(spec: &PlatformBackendSpec) -> PredictedBackend {
    predict_no_registration(spec)
}

fn canonicalize_fastly_transport_timeout_ms(remaining_ms: u32, configured_ms: u32) -> u32 {
    if remaining_ms >= configured_ms {
        return configured_ms;
    }
    if remaining_ms >= FASTLY_TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS {
        return FASTLY_TRANSPORT_TIMEOUT_COARSE_LADDER_MS
            .into_iter()
            .rev()
            .find(|&rung| rung <= remaining_ms)
            .unwrap_or(FASTLY_TRANSPORT_TIMEOUT_QUANTUM_CEILING_MS);
    }
    let floored =
        (remaining_ms / FASTLY_TRANSPORT_TIMEOUT_QUANTUM_MS) * FASTLY_TRANSPORT_TIMEOUT_QUANTUM_MS;
    if floored > 0 {
        return floored;
    }
    FASTLY_SUB_QUANTUM_LADDER_MS
        .into_iter()
        .find(|&rung| rung <= remaining_ms)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn spec() -> PlatformBackendSpec {
        PlatformBackendSpec {
            scheme: "https".to_owned(),
            host: "origin.example.com".to_owned(),
            port: None,
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_millis(1500),
            between_bytes_timeout: Duration::from_millis(1500),
            discriminator: Some("provider-one".to_owned()),
        }
    }

    #[test]
    fn adapter_prediction_outputs_are_pinned() {
        let spec = spec();
        assert_eq!(
            BackendNamingPolicy::Fastly
                .predict(&spec)
                .expect("should predict Fastly backend"),
            PredictedBackend {
                name: "backend_https_origin_example_com_443_p_provider-one_fb1500_bb1500_51fb8dba14db39e759e2c1c5d204a6eb".to_owned(),
                port: 443,
            }
        );
        assert_eq!(
            BackendNamingPolicy::Axum
                .predict(&spec)
                .expect("should predict Axum backend"),
            PredictedBackend {
                name: "HTTPS_ORIGIN_EXAMPLE_COM_443_p_PROVIDER_ONE".to_owned(),
                port: 443,
            }
        );
        let no_registration = PredictedBackend {
            name: "https_origin.example.com_443_1500ms_p_provider-one".to_owned(),
            port: 443,
        };
        assert_eq!(
            BackendNamingPolicy::Cloudflare
                .predict(&spec)
                .expect("should predict Cloudflare backend"),
            no_registration
        );
        assert_eq!(
            BackendNamingPolicy::Spin
                .predict(&spec)
                .expect("should predict Spin backend"),
            no_registration
        );
    }

    #[test]
    fn fastly_prediction_canonicalizes_only_bracketed_ipv6_hosts() {
        let mut bare = spec();
        bare.host = "2001:db8::1".to_string();
        bare.port = Some(8443);
        let mut bracketed = bare.clone();
        bracketed.host = "[2001:db8::1]".to_string();

        let bare_prediction = BackendNamingPolicy::Fastly
            .predict(&bare)
            .expect("should predict bare IPv6 backend");
        let bracketed_prediction = BackendNamingPolicy::Fastly
            .predict(&bracketed)
            .expect("should predict bracketed IPv6 backend");
        assert_eq!(bare_prediction, bracketed_prediction);
        assert_eq!(
            bare_prediction
                .name
                .rsplit_once('_')
                .map(|(_, digest)| digest),
            bracketed_prediction
                .name
                .rsplit_once('_')
                .map(|(_, digest)| digest),
            "canonical forms must hash the identical Fastly specification"
        );

        assert_eq!(
            canonical_fastly_host("origin.example.com"),
            "origin.example.com"
        );
        assert_eq!(canonical_fastly_host("192.0.2.1"), "192.0.2.1");
        assert_eq!(canonical_fastly_host("[not-ipv6]"), "[not-ipv6]");
    }

    #[test]
    fn target_descriptors_pin_all_capabilities_and_policies() {
        let cases = [
            (AuctionTargetId::Fastly, true, BackendNamingPolicy::Fastly),
            (AuctionTargetId::Axum, true, BackendNamingPolicy::Axum),
            (
                AuctionTargetId::Cloudflare,
                false,
                BackendNamingPolicy::Cloudflare,
            ),
            (AuctionTargetId::Spin, false, BackendNamingPolicy::Spin),
        ];
        for (id, fanout, naming_policy) in cases {
            let descriptor = id.descriptor();
            assert_eq!(descriptor.id(), id);
            assert_eq!(descriptor.naming_policy(), naming_policy);
            assert_eq!(
                descriptor
                    .capabilities()
                    .supports_concurrent_provider_fanout(),
                fanout,
                "should declare fanout accurately for {}",
                id.adapter_id()
            );
            assert!(
                !descriptor
                    .capabilities()
                    .has_enforceable_total_request_deadline(),
                "no current adapter should claim a total request deadline"
            );
            assert_eq!(AuctionTargetId::from_adapter_id(id.adapter_id()), Some(id));
        }
        assert_eq!(AuctionTargetId::from_adapter_id("FASTLY"), None);
        assert_eq!(AuctionTargetId::from_adapter_id("unknown"), None);
    }

    #[test]
    fn timeout_policies_preserve_logical_transport_distinction() {
        assert_eq!(
            BackendNamingPolicy::Fastly.canonicalize_transport_timeout_ms(999, 2000),
            750
        );
        assert_eq!(
            BackendNamingPolicy::Fastly.canonicalize_transport_timeout_ms(2000, 100),
            100
        );
        for policy in [
            BackendNamingPolicy::Axum,
            BackendNamingPolicy::Cloudflare,
            BackendNamingPolicy::Spin,
        ] {
            assert_eq!(policy.canonicalize_transport_timeout_ms(999, 2000), 999);
            assert_eq!(policy.canonicalize_transport_timeout_ms(2000, 100), 100);
        }
    }
}
