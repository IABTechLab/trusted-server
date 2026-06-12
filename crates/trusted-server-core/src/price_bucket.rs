use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PriceGranularity {
    Low,
    Medium,
    #[default]
    Dense,
    High,
    Auto,
}

#[must_use]
pub fn price_bucket(cpm: f64, granularity: PriceGranularity) -> String {
    // Reject NaN / Inf early so the `(x * 100.0).floor() as u64` cast below
    // can never see a non-finite value (the cast's behaviour for NaN/Inf is
    // implementation-defined in Rust and "saturate to 0" only by convention).
    if !cpm.is_finite() || cpm <= 0.0 {
        return "0.00".to_string();
    }
    match granularity {
        PriceGranularity::Low => {
            let capped = cpm.min(5.0);
            let cents = (capped * 100.0).floor() as u64;
            let bucketed_cents = (cents / 50) * 50;
            format!("{:.2}", bucketed_cents as f64 / 100.0)
        }
        PriceGranularity::Medium => {
            let capped = cpm.min(20.0);
            let cents = (capped * 100.0).floor() as u64;
            let bucketed_cents = (cents / 10) * 10;
            format!("{:.2}", bucketed_cents as f64 / 100.0)
        }
        PriceGranularity::High => {
            let capped = cpm.min(20.0);
            let cents = (capped * 100.0).floor() as u64;
            format!("{:.2}", cents as f64 / 100.0)
        }
        PriceGranularity::Dense | PriceGranularity::Auto => dense_bucket(cpm),
    }
}

fn dense_bucket(cpm: f64) -> String {
    if cpm >= 20.0 {
        return "20.00".to_string();
    }
    if cpm >= 8.0 {
        let cents = (cpm * 100.0).floor() as u64;
        let bucketed_cents = (cents / 50) * 50;
        return format!("{:.2}", bucketed_cents as f64 / 100.0);
    }
    if cpm >= 3.0 {
        let cents = (cpm * 100.0).floor() as u64;
        let bucketed_cents = (cents / 5) * 5;
        return format!("{:.2}", bucketed_cents as f64 / 100.0);
    }
    let cents = (cpm * 100.0).floor() as u64;
    format!("{:.2}", cents as f64 / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_below_3_increments_by_0_01() {
        assert_eq!(price_bucket(0.0, PriceGranularity::Dense), "0.00");
        assert_eq!(price_bucket(0.01, PriceGranularity::Dense), "0.01");
        assert_eq!(price_bucket(0.015, PriceGranularity::Dense), "0.01");
        assert_eq!(price_bucket(1.23, PriceGranularity::Dense), "1.23");
        assert_eq!(price_bucket(2.99, PriceGranularity::Dense), "2.99");
    }

    #[test]
    fn dense_3_to_8_increments_by_0_05() {
        assert_eq!(price_bucket(3.00, PriceGranularity::Dense), "3.00");
        assert_eq!(price_bucket(3.03, PriceGranularity::Dense), "3.00");
        assert_eq!(price_bucket(3.05, PriceGranularity::Dense), "3.05");
        assert_eq!(price_bucket(7.99, PriceGranularity::Dense), "7.95");
    }

    #[test]
    fn dense_8_to_20_increments_by_0_50() {
        assert_eq!(price_bucket(8.00, PriceGranularity::Dense), "8.00");
        assert_eq!(price_bucket(8.49, PriceGranularity::Dense), "8.00");
        assert_eq!(price_bucket(8.50, PriceGranularity::Dense), "8.50");
        assert_eq!(price_bucket(19.99, PriceGranularity::Dense), "19.50");
    }

    #[test]
    fn dense_above_20_caps_at_20() {
        assert_eq!(price_bucket(20.00, PriceGranularity::Dense), "20.00");
        assert_eq!(price_bucket(50.00, PriceGranularity::Dense), "20.00");
    }

    #[test]
    fn low_increments_by_0_50_capped_at_5() {
        assert_eq!(price_bucket(0.49, PriceGranularity::Low), "0.00");
        assert_eq!(price_bucket(0.50, PriceGranularity::Low), "0.50");
        assert_eq!(price_bucket(5.01, PriceGranularity::Low), "5.00");
    }

    #[test]
    fn medium_increments_by_0_10_capped_at_20() {
        assert_eq!(price_bucket(1.05, PriceGranularity::Medium), "1.00");
        assert_eq!(price_bucket(1.10, PriceGranularity::Medium), "1.10");
        assert_eq!(price_bucket(20.5, PriceGranularity::Medium), "20.00");
    }

    #[test]
    fn high_increments_by_0_01_capped_at_20() {
        assert_eq!(price_bucket(1.234, PriceGranularity::High), "1.23");
        assert_eq!(price_bucket(20.5, PriceGranularity::High), "20.00");
    }

    #[test]
    fn auto_routes_through_dense() {
        assert_eq!(
            price_bucket(2.53, PriceGranularity::Auto),
            price_bucket(2.53, PriceGranularity::Dense)
        );
    }

    #[test]
    fn non_finite_cpm_returns_zero_bucket() {
        for granularity in [
            PriceGranularity::Dense,
            PriceGranularity::Low,
            PriceGranularity::Medium,
            PriceGranularity::High,
            PriceGranularity::Auto,
        ] {
            assert_eq!(
                price_bucket(f64::NAN, granularity),
                "0.00",
                "NaN cpm should bucket to 0.00 for granularity {granularity:?}"
            );
            assert_eq!(
                price_bucket(f64::INFINITY, granularity),
                "0.00",
                "+Inf cpm should bucket to 0.00 for granularity {granularity:?}"
            );
            assert_eq!(
                price_bucket(f64::NEG_INFINITY, granularity),
                "0.00",
                "-Inf cpm should bucket to 0.00 for granularity {granularity:?}"
            );
        }
    }
}
