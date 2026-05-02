//! Prebid price granularity bucketing.

use serde::{Deserialize, Serialize};

/// Prebid price granularity setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PriceGranularity {
    /// Low granularity.
    Low,
    /// Medium granularity.
    Medium,
    /// Dense granularity.
    #[default]
    Dense,
    /// High granularity.
    High,
    /// Auto granularity, treated as dense for Phase 1.
    Auto,
}

impl PriceGranularity {
    /// Returns [`PriceGranularity::Dense`]; used as a serde default function pointer.
    #[must_use]
    pub const fn dense() -> Self {
        Self::Dense
    }
}

/// Convert raw CPM to the `hb_pb` price bucket string.
#[must_use]
pub fn price_bucket(cpm: f64, granularity: PriceGranularity) -> String {
    if cpm <= 0.0 {
        return "0.00".to_string();
    }

    match granularity {
        PriceGranularity::Low => bucket(cpm, 5.0, 0.50),
        PriceGranularity::Medium => bucket(cpm, 20.0, 0.10),
        PriceGranularity::High => bucket(cpm, 20.0, 0.01),
        PriceGranularity::Dense | PriceGranularity::Auto => dense_bucket(cpm),
    }
}

fn dense_bucket(cpm: f64) -> String {
    if cpm >= 20.0 {
        return "20.00".to_string();
    }
    if cpm >= 8.0 {
        return bucket(cpm, 20.0, 0.50);
    }
    if cpm >= 3.0 {
        return bucket(cpm, 8.0, 0.05);
    }
    bucket(cpm, 3.0, 0.01)
}

fn bucket(cpm: f64, cap: f64, increment: f64) -> String {
    let capped = cpm.min(cap);
    format!("{:.2}", ((capped / increment) + 1e-9).floor() * increment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_below_3_increments_by_0_01() {
        assert_eq!(price_bucket(0.0, PriceGranularity::Dense), "0.00");
        assert_eq!(price_bucket(0.015, PriceGranularity::Dense), "0.01");
        assert_eq!(price_bucket(2.99, PriceGranularity::Dense), "2.99");
    }

    #[test]
    fn dense_3_to_8_increments_by_0_05() {
        assert_eq!(price_bucket(3.03, PriceGranularity::Dense), "3.00");
        assert_eq!(price_bucket(3.05, PriceGranularity::Dense), "3.05");
        assert_eq!(price_bucket(7.99, PriceGranularity::Dense), "7.95");
    }

    #[test]
    fn dense_8_to_20_increments_by_0_50() {
        assert_eq!(price_bucket(8.49, PriceGranularity::Dense), "8.00");
        assert_eq!(price_bucket(8.50, PriceGranularity::Dense), "8.50");
        assert_eq!(price_bucket(19.99, PriceGranularity::Dense), "19.50");
    }

    #[test]
    fn built_in_granularities_cap_correctly() {
        assert_eq!(price_bucket(5.01, PriceGranularity::Low), "5.00");
        assert_eq!(price_bucket(20.5, PriceGranularity::Medium), "20.00");
        assert_eq!(price_bucket(20.5, PriceGranularity::High), "20.00");
        assert_eq!(price_bucket(50.0, PriceGranularity::Dense), "20.00");
    }

    #[test]
    fn auto_routes_to_dense() {
        assert_eq!(
            price_bucket(2.53, PriceGranularity::Auto),
            price_bucket(2.53, PriceGranularity::Dense)
        );
    }
}
