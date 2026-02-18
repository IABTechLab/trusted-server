//! Benchmarks for the consent decoding pipeline.
//!
//! Measures the computational cost of decoding consent signals (TCF v2, GPP,
//! US Privacy) to determine whether wiring decoding into the auction hot path
//! introduces unacceptable latency.
//!
//! Run with: `cargo bench -p trusted-server-common`

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use trusted_server_common::consent::tcf::decode_tc_string;
use trusted_server_common::consent::types::RawConsentSignals;
use trusted_server_common::consent::us_privacy::decode_us_privacy;
use trusted_server_common::consent::{build_context_from_signals, gpp};

// ---------------------------------------------------------------------------
// Test data
// ---------------------------------------------------------------------------

/// Known-good GPP string with US Privacy section only (section ID 6).
const GPP_USP_ONLY: &str = "DBABTA~1YNN";

/// GPP string with both TCF EU v2 and US Privacy sections.
const GPP_TCF_AND_USP: &str = "DBACNY~CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA~1YNN";

/// Builds a minimal TC String v2 byte buffer for benchmarking.
///
/// This duplicates the test helper from `tcf.rs` since `#[cfg(test)]` helpers
/// are not available in bench targets.
fn build_tc_bytes(vendor_count: u16, use_range_encoding: bool) -> Vec<u8> {
    let total_bits = if use_range_encoding {
        // Core fields (213) + maxVendorId (16) + isRange (1) + numEntries (12)
        // + one range entry per vendor group: isRange(1) + start(16) + end(16)
        // We'll encode as one big range: vendors 1..=vendor_count
        213 + 17 + 12 + 1 + 32
    } else {
        // Bitfield: core fields + maxVendorId + isRange + one bit per vendor
        213 + 17 + usize::from(vendor_count)
    };
    let total_bytes = total_bits.div_ceil(8);
    let mut buf = vec![0u8; total_bytes];

    // Version (6 bits) = 2
    write_bits(&mut buf, 0, 6, 2);
    // Created (36 bits) = 100000 (arbitrary)
    write_bits(&mut buf, 6, 36, 100_000);
    // LastUpdated (36 bits) = 200000
    write_bits(&mut buf, 42, 36, 200_000);
    // CmpId (12 bits) = 7
    write_bits(&mut buf, 78, 12, 7);
    // CmpVersion (12 bits) = 1
    write_bits(&mut buf, 90, 12, 1);
    // ConsentScreen (6 bits) = 1
    write_bits(&mut buf, 102, 6, 1);
    // ConsentLanguage (12 bits) = EN
    write_bits(&mut buf, 108, 6, u64::from(b'E' - b'A'));
    write_bits(&mut buf, 114, 6, u64::from(b'N' - b'A'));
    // VendorListVersion (12 bits) = 42
    write_bits(&mut buf, 120, 12, 42);
    // TcfPolicyVersion (6 bits) = 2
    write_bits(&mut buf, 132, 6, 2);
    // IsServiceSpecific (1) = 0, UseNonStandardTexts (1) = 0
    // SpecialFeatureOptIns (12) = 0b000000000011 (features 11, 12)
    write_bits(&mut buf, 140, 12, 0b0000_0000_0011);
    // PurposesConsent (24) = purposes 1-4 consented
    write_bits(&mut buf, 152, 24, 0b1111_0000_0000_0000_0000_0000);
    // PurposesLITransparency (24) = purposes 1-2
    write_bits(&mut buf, 176, 24, 0b1100_0000_0000_0000_0000_0000);
    // PurposeOneTreatment (1) = 0
    // PublisherCC (12) = EN
    write_bits(&mut buf, 201, 6, u64::from(b'E' - b'A'));
    write_bits(&mut buf, 207, 6, u64::from(b'N' - b'A'));

    // MaxVendorConsentId (16)
    write_bits(&mut buf, 213, 16, u64::from(vendor_count));

    if use_range_encoding {
        // IsRangeEncoding (1) = 1
        write_bit(&mut buf, 229, true);
        // NumEntries (12) = 1 (one range covering all vendors)
        write_bits(&mut buf, 230, 12, 1);
        // Entry: IsRangeEntry (1) = 1
        write_bit(&mut buf, 242, true);
        // StartVendorId (16) = 1
        write_bits(&mut buf, 243, 16, 1);
        // EndVendorId (16) = vendor_count
        write_bits(&mut buf, 259, 16, u64::from(vendor_count));
    } else {
        // IsRangeEncoding (1) = 0 (bitfield)
        write_bit(&mut buf, 229, false);
        // Set every other vendor as consented (realistic pattern)
        for i in 0..usize::from(vendor_count) {
            if i % 2 == 0 {
                write_bit(&mut buf, 230 + i, true);
            }
        }
    }

    buf
}

fn write_bit(buf: &mut [u8], bit_offset: usize, value: bool) {
    if value {
        let byte_idx = bit_offset / 8;
        let bit_idx = 7 - (bit_offset % 8);
        if byte_idx < buf.len() {
            buf[byte_idx] |= 1 << bit_idx;
        }
    }
}

fn write_bits(buf: &mut [u8], bit_offset: usize, num_bits: usize, value: u64) {
    for i in 0..num_bits {
        let bit = (value >> (num_bits - 1 - i)) & 1 == 1;
        write_bit(buf, bit_offset + i, bit);
    }
}

fn encode_tc_string(vendor_count: u16, use_range: bool) -> String {
    let bytes = build_tc_bytes(vendor_count, use_range);
    URL_SAFE_NO_PAD.encode(&bytes)
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_us_privacy(c: &mut Criterion) {
    c.bench_function("us_privacy_decode", |b| {
        b.iter(|| decode_us_privacy(black_box("1YNN")));
    });
}

fn bench_tcf_decode(c: &mut Criterion) {
    let small_tc = encode_tc_string(10, false);
    let medium_tc = encode_tc_string(100, false);
    let large_tc_bitfield = encode_tc_string(500, false);
    let large_tc_range = encode_tc_string(500, true);

    let mut group = c.benchmark_group("tcf_decode");

    group.bench_with_input(
        BenchmarkId::new("bitfield", "10_vendors"),
        &small_tc,
        |b, tc| {
            b.iter(|| decode_tc_string(black_box(tc)));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("bitfield", "100_vendors"),
        &medium_tc,
        |b, tc| {
            b.iter(|| decode_tc_string(black_box(tc)));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("bitfield", "500_vendors"),
        &large_tc_bitfield,
        |b, tc| {
            b.iter(|| decode_tc_string(black_box(tc)));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("range", "500_vendors"),
        &large_tc_range,
        |b, tc| {
            b.iter(|| decode_tc_string(black_box(tc)));
        },
    );

    group.finish();
}

fn bench_gpp_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("gpp_decode");

    group.bench_function("usp_only", |b| {
        b.iter(|| gpp::decode_gpp_string(black_box(GPP_USP_ONLY)));
    });

    group.bench_function("with_tcf", |b| {
        b.iter(|| gpp::decode_gpp_string(black_box(GPP_TCF_AND_USP)));
    });

    group.finish();
}

fn bench_full_pipeline(c: &mut Criterion) {
    // Build a realistic TC string (500 vendors, range encoding)
    let tc_string = encode_tc_string(500, true);

    let all_signals = RawConsentSignals {
        raw_tc_string: Some(tc_string),
        raw_gpp_string: Some(GPP_USP_ONLY.to_owned()),
        raw_gpp_sid: Some("6".to_owned()),
        raw_us_privacy: Some("1YNN".to_owned()),
        gpc: true,
    };

    let empty_signals = RawConsentSignals::default();

    let tc_only = RawConsentSignals {
        raw_tc_string: Some(encode_tc_string(500, true)),
        ..Default::default()
    };

    let mut group = c.benchmark_group("full_pipeline");

    group.bench_function("all_signals", |b| {
        b.iter(|| build_context_from_signals(black_box(&all_signals)));
    });

    group.bench_function("empty_signals", |b| {
        b.iter(|| build_context_from_signals(black_box(&empty_signals)));
    });

    group.bench_function("tcf_only", |b| {
        b.iter(|| build_context_from_signals(black_box(&tc_only)));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_us_privacy,
    bench_tcf_decode,
    bench_gpp_decode,
    bench_full_pipeline,
);
criterion_main!(benches);
