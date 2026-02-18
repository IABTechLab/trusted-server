//! TCF v2 consent string decoder (core segment only).
//!
//! Decodes the IAB Transparency & Consent Framework v2 consent string from the
//! `euconsent-v2` cookie. Only the core segment (segment type 0) is decoded;
//! publisher restrictions, disclosed vendors, and allowed vendors segments are
//! not yet supported.
//!
//! # Binary format
//!
//! The TC String is a web-safe base64-encoded binary bitfield. The core segment
//! layout (after base64 decoding) is:
//!
//! | Field | Bits | Offset |
//! |-------|------|--------|
//! | Version | 6 | 0 |
//! | Created | 36 | 6 |
//! | `LastUpdated` | 36 | 42 |
//! | `CmpId` | 12 | 78 |
//! | `CmpVersion` | 12 | 90 |
//! | `ConsentScreen` | 6 | 102 |
//! | `ConsentLanguage` | 12 | 108 |
//! | `VendorListVersion` | 12 | 120 |
//! | `TcfPolicyVersion` | 6 | 132 |
//! | `IsServiceSpecific` | 1 | 138 |
//! | `UseNonStandardTexts` | 1 | 139 |
//! | `SpecialFeatureOptIns` | 12 | 140 |
//! | `PurposesConsent` | 24 | 152 |
//! | `PurposesLITransparency` | 24 | 176 |
//! | `PurposeOneTreatment` | 1 | 200 |
//! | `PublisherCC` | 12 | 201 |
//! | `MaxVendorConsentId` | 16 | 213 |
//! | `IsRangeEncoding` | 1 | 229 |
//! | ...vendor consents... | variable | 230 |
//!
//! Segments in a TC String are separated by `.` characters. The first segment
//! is always the core segment; additional segments carry supplementary data.
//!
//! # References
//!
//! - [IAB TCF v2.0 specification](https://github.com/InteractiveAdvertisingBureau/GDPR-Transparency-and-Consent-Framework/blob/master/TCFv2/IAB%20Tech%20Lab%20-%20Consent%20string%20and%20vendor%20list%20formats%20v2.md)

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use error_stack::Report;

use super::types::{ConsentDecodeError, TcfConsent};

/// Decodes a TC String v2 into a [`TcfConsent`] struct.
///
/// Only the core segment is decoded. Additional segments (separated by `.`)
/// are ignored.
///
/// # Errors
///
/// - [`ConsentDecodeError::InvalidTcString`] if base64 decoding fails, the
///   version is not 2, or the bitfield is too short.
pub fn decode_tc_string(tc_string: &str) -> Result<TcfConsent, Report<ConsentDecodeError>> {
    // TC String may have multiple segments separated by '.'
    // The first segment is always the core segment.
    let core_segment = tc_string.split('.').next().unwrap_or(tc_string);

    let bytes = URL_SAFE_NO_PAD
        .decode(core_segment)
        .or_else(|_| {
            // Some CMPs use standard base64 with padding
            use base64::engine::general_purpose::STANDARD;
            STANDARD.decode(core_segment)
        })
        .map_err(|e| {
            Report::new(ConsentDecodeError::InvalidTcString {
                reason: format!("base64 decode failed: {e}"),
            })
        })?;

    let reader = BitReader::new(&bytes);

    // Minimum size: 230 bits for core fields up to IsRangeEncoding
    if reader.bit_len() < 230 {
        return Err(Report::new(ConsentDecodeError::InvalidTcString {
            reason: format!(
                "bitfield too short: {} bits, need at least 230",
                reader.bit_len()
            ),
        }));
    }

    let version = reader.read_u8(0, 6);
    if version != 2 {
        return Err(Report::new(ConsentDecodeError::InvalidTcString {
            reason: format!("unsupported version {version}, expected 2"),
        }));
    }

    let created_ds = reader.read_u64(6, 36);
    let last_updated_ds = reader.read_u64(42, 36);
    let cmp_id = reader.read_u16(78, 12);
    let cmp_version = reader.read_u16(90, 12);
    let consent_screen = reader.read_u8(102, 6);

    // Consent language: two 6-bit values, each offset by 'A' (65)
    let lang_a = reader.read_u8(108, 6);
    let lang_b = reader.read_u8(114, 6);
    let consent_language = format!("{}{}", char::from(b'A' + lang_a), char::from(b'A' + lang_b),);

    let vendor_list_version = reader.read_u16(120, 12);
    let tcf_policy_version = reader.read_u8(132, 6);
    // Skip: IsServiceSpecific (138, 1), UseNonStandardTexts (139, 1)

    let special_feature_opt_ins = reader.read_bool_vec(140, 12);
    let purpose_consents = reader.read_bool_vec(152, 24);
    let purpose_legitimate_interests = reader.read_bool_vec(176, 24);
    // Skip: PurposeOneTreatment (200, 1), PublisherCC (201, 12)

    // Vendor consents
    let vendor_consents = decode_vendor_section(&reader, 213)?;

    // Vendor legitimate interests follow after vendor consents
    let vendor_li_offset = vendor_section_end_offset(&reader, 213)?;
    let vendor_legitimate_interests = if vendor_li_offset + 17 <= reader.bit_len() {
        decode_vendor_section(&reader, vendor_li_offset).unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(TcfConsent {
        version,
        cmp_id,
        cmp_version,
        consent_screen,
        consent_language,
        vendor_list_version,
        tcf_policy_version,
        created_ds,
        last_updated_ds,
        purpose_consents,
        purpose_legitimate_interests,
        vendor_consents,
        vendor_legitimate_interests,
        special_feature_opt_ins,
    })
}

/// Decodes a vendor section (consents or legitimate interests).
///
/// The section starts with:
/// - `MaxVendorId` (16 bits)
/// - `IsRangeEncoding` (1 bit)
///
/// If bitfield encoding: one bit per vendor up to `MaxVendorId`.
/// If range encoding: `NumEntries` (12 bits), then entries.
fn decode_vendor_section(
    reader: &BitReader<'_>,
    offset: usize,
) -> Result<Vec<u16>, Report<ConsentDecodeError>> {
    if offset + 17 > reader.bit_len() {
        return Ok(Vec::new());
    }

    let max_vendor_id = reader.read_u16(offset, 16);
    let is_range = reader.read_bool(offset + 16);

    if !is_range {
        // Bitfield: one bit per vendor, 1..=max_vendor_id
        let mut vendors = Vec::new();
        let bitfield_start = offset + 17;
        for i in 0..usize::from(max_vendor_id) {
            let bit_pos = bitfield_start + i;
            if bit_pos >= reader.bit_len() {
                break;
            }
            if reader.read_bool(bit_pos) {
                // Vendor IDs are 1-indexed
                vendors.push((i + 1) as u16);
            }
        }
        Ok(vendors)
    } else {
        // Range encoding
        let num_entries_offset = offset + 17;
        if num_entries_offset + 12 > reader.bit_len() {
            return Ok(Vec::new());
        }
        let num_entries = reader.read_u16(num_entries_offset, 12);
        let mut vendors = Vec::new();
        let mut pos = num_entries_offset + 12;

        for _ in 0..num_entries {
            if pos >= reader.bit_len() {
                break;
            }
            let is_range_entry = reader.read_bool(pos);
            pos += 1;

            if is_range_entry {
                // Range: StartVendorId (16) + EndVendorId (16)
                if pos + 32 > reader.bit_len() {
                    break;
                }
                let start = reader.read_u16(pos, 16);
                let end = reader.read_u16(pos + 16, 16);
                pos += 32;
                for id in start..=end {
                    vendors.push(id);
                }
            } else {
                // Single vendor: VendorId (16)
                if pos + 16 > reader.bit_len() {
                    break;
                }
                let id = reader.read_u16(pos, 16);
                pos += 16;
                vendors.push(id);
            }
        }
        Ok(vendors)
    }
}

/// Calculates the bit offset after a vendor section ends.
fn vendor_section_end_offset(
    reader: &BitReader<'_>,
    offset: usize,
) -> Result<usize, Report<ConsentDecodeError>> {
    if offset + 17 > reader.bit_len() {
        return Ok(offset);
    }

    let max_vendor_id = reader.read_u16(offset, 16);
    let is_range = reader.read_bool(offset + 16);

    if !is_range {
        Ok(offset + 17 + usize::from(max_vendor_id))
    } else {
        let num_entries_offset = offset + 17;
        if num_entries_offset + 12 > reader.bit_len() {
            return Ok(num_entries_offset);
        }
        let num_entries = reader.read_u16(num_entries_offset, 12);
        let mut pos = num_entries_offset + 12;

        for _ in 0..num_entries {
            if pos >= reader.bit_len() {
                break;
            }
            let is_range_entry = reader.read_bool(pos);
            pos += 1;

            if is_range_entry {
                pos += 32; // StartVendorId (16) + EndVendorId (16)
            } else {
                pos += 16; // Single VendorId
            }
        }
        Ok(pos)
    }
}

// ---------------------------------------------------------------------------
// Bit reader utility
// ---------------------------------------------------------------------------

/// A simple bit-level reader over a byte slice.
///
/// All reads are specified as (`bit_offset`, `num_bits`) from the start of the
/// buffer. No internal cursor is maintained — callers manage offsets explicitly.
struct BitReader<'a> {
    bytes: &'a [u8],
}

impl<'a> BitReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    const fn bit_len(&self) -> usize {
        self.bytes.len() * 8
    }

    /// Reads a single bit as a boolean.
    fn read_bool(&self, bit_offset: usize) -> bool {
        let byte_idx = bit_offset / 8;
        let bit_idx = 7 - (bit_offset % 8);
        if byte_idx >= self.bytes.len() {
            return false;
        }
        (self.bytes[byte_idx] >> bit_idx) & 1 == 1
    }

    /// Reads up to 8 bits as a [`u8`].
    fn read_u8(&self, bit_offset: usize, num_bits: usize) -> u8 {
        debug_assert!(num_bits <= 8);
        self.read_u64(bit_offset, num_bits) as u8
    }

    /// Reads up to 16 bits as a [`u16`].
    fn read_u16(&self, bit_offset: usize, num_bits: usize) -> u16 {
        debug_assert!(num_bits <= 16);
        self.read_u64(bit_offset, num_bits) as u16
    }

    /// Reads up to 64 bits as a [`u64`].
    fn read_u64(&self, bit_offset: usize, num_bits: usize) -> u64 {
        debug_assert!(num_bits <= 64);
        let mut value: u64 = 0;
        for i in 0..num_bits {
            if self.read_bool(bit_offset + i) {
                value |= 1 << (num_bits - 1 - i);
            }
        }
        value
    }

    /// Reads a sequence of bits as a [`Vec<bool>`].
    fn read_bool_vec(&self, bit_offset: usize, num_bits: usize) -> Vec<bool> {
        (0..num_bits)
            .map(|i| self.read_bool(bit_offset + i))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A known-good TC String v2 generated by the IAB reference implementation.
    // This encodes: version=2, cmpId=7, cmpVersion=1, consent for purposes 1-4,
    // vendor consents for vendors 1-10 (bitfield encoding).
    //
    // To generate test strings: https://iabeurope.github.io/TCF-v2-consent-string-editor/
    // Or use the IAB reference implementation in JavaScript.

    #[test]
    fn decodes_minimal_tc_string() {
        // This is a minimal valid TC String v2 core segment.
        // Generated with: version=2, created=1970-01-01, lastUpdated=1970-01-01,
        // cmpId=1, cmpVersion=1, consentScreen=0, language=EN,
        // vendorListVersion=1, policyVersion=2, purposes=none, vendors=none (max=0)
        //
        // Bitfield construction:
        // version(6)=2, created(36)=0, lastUpdated(36)=0, cmpId(12)=1,
        // cmpVersion(12)=1, consentScreen(6)=0, language(12)=EN,
        // vendorListVersion(12)=1, policyVersion(6)=2,
        // isServiceSpecific(1)=0, useNonStandard(1)=0,
        // specialFeatures(12)=0, purposeConsents(24)=0,
        // purposeLI(24)=0, purposeOneTreatment(1)=0, publisherCC(12)=EN,
        // maxVendorId(16)=0, isRange(1)=0
        //
        // We'll build this manually.
        let bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &[], &[]);
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);

        let result = decode_tc_string(&encoded).expect("should decode minimal TC string");
        assert_eq!(result.version, 2);
        assert_eq!(result.cmp_id, 1);
        assert_eq!(result.cmp_version, 1);
        assert_eq!(result.consent_language, "EN");
        assert_eq!(result.vendor_list_version, 1);
        assert!(result.vendor_consents.is_empty(), "should have no vendors");
    }

    #[test]
    fn decodes_purpose_consents() {
        let purposes = vec![true, true, false, true]; // purposes 1,2,4
        let bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &purposes, &[]);
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);

        let result = decode_tc_string(&encoded).expect("should decode purposes");
        assert!(result.purpose_consents[0], "purpose 1 should be consented");
        assert!(result.purpose_consents[1], "purpose 2 should be consented");
        assert!(
            !result.purpose_consents[2],
            "purpose 3 should not be consented"
        );
        assert!(result.purpose_consents[3], "purpose 4 should be consented");
    }

    #[test]
    fn decodes_vendor_consents_bitfield() {
        // Vendors 1, 3, 5 consented (bitfield encoding, max=5)
        let vendor_bits = vec![true, false, true, false, true];
        let bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &[], &vendor_bits);
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);

        let result = decode_tc_string(&encoded).expect("should decode vendor bitfield");
        assert_eq!(
            result.vendor_consents,
            vec![1, 3, 5],
            "should have vendors 1, 3, 5"
        );
    }

    #[test]
    fn rejects_version_1() {
        // Build bytes with version=1
        let mut bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &[], &[]);
        // Clear version bits (first 6 bits) and set to 1
        bytes[0] = (bytes[0] & 0x03) | (1 << 2); // version=1 in first 6 bits
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);

        let result = decode_tc_string(&encoded);
        assert!(result.is_err(), "should reject version 1");
    }

    #[test]
    fn rejects_too_short() {
        let encoded = URL_SAFE_NO_PAD.encode([0u8; 10]); // only 80 bits
        let result = decode_tc_string(&encoded);
        assert!(result.is_err(), "should reject short bitfield");
    }

    #[test]
    fn rejects_invalid_base64() {
        let result = decode_tc_string("!!!invalid!!!");
        assert!(result.is_err(), "should reject invalid base64");
    }

    #[test]
    fn handles_segmented_tc_string() {
        // TC Strings can have multiple segments separated by '.'
        let bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &[], &[]);
        let encoded = format!("{}.extra-segment", URL_SAFE_NO_PAD.encode(&bytes));

        let result = decode_tc_string(&encoded).expect("should decode first segment");
        assert_eq!(result.version, 2);
    }

    #[test]
    fn decodes_consent_language() {
        let bytes = build_minimal_tc_bytes(1, 1, b"FR", 1, &[], &[]);
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);

        let result = decode_tc_string(&encoded).expect("should decode language");
        assert_eq!(result.consent_language, "FR");
    }

    // -----------------------------------------------------------------------
    // Test helper: builds a minimal TC String v2 byte buffer
    // -----------------------------------------------------------------------

    fn build_minimal_tc_bytes(
        cmp_id: u16,
        cmp_version: u16,
        language: &[u8; 2],
        vendor_list_version: u16,
        purpose_consents: &[bool],
        vendor_consent_bits: &[bool],
    ) -> Vec<u8> {
        let max_vendor_id = vendor_consent_bits.len() as u16;
        // Calculate total bits needed
        // Core fields: 213 bits + 16 (maxVendorId) + 1 (isRange) + max_vendor_id (bitfield)
        let total_bits = 213 + 17 + usize::from(max_vendor_id);
        let total_bytes = total_bits.div_ceil(8);
        let mut buf = vec![0u8; total_bytes];

        let mut writer = BitWriter::new(&mut buf);

        // Version (6 bits) = 2
        writer.write(0, 6, 2);
        // Created (36 bits) = 0
        writer.write(6, 36, 0);
        // LastUpdated (36 bits) = 0
        writer.write(42, 36, 0);
        // CmpId (12 bits)
        writer.write(78, 12, u64::from(cmp_id));
        // CmpVersion (12 bits)
        writer.write(90, 12, u64::from(cmp_version));
        // ConsentScreen (6 bits) = 0
        writer.write(102, 6, 0);
        // ConsentLanguage (12 bits) - two 6-bit chars offset by 'A'
        writer.write(108, 6, u64::from(language[0] - b'A'));
        writer.write(114, 6, u64::from(language[1] - b'A'));
        // VendorListVersion (12 bits)
        writer.write(120, 12, u64::from(vendor_list_version));
        // TcfPolicyVersion (6 bits) = 2
        writer.write(132, 6, 2);
        // IsServiceSpecific (1 bit) = 0
        // UseNonStandardTexts (1 bit) = 0
        // SpecialFeatureOptIns (12 bits) = 0
        // PurposesConsent (24 bits)
        for (i, &consented) in purpose_consents.iter().enumerate() {
            if consented && i < 24 {
                writer.write_bool(152 + i, true);
            }
        }
        // PurposesLITransparency (24 bits) = 0
        // PurposeOneTreatment (1 bit) = 0
        // PublisherCC (12 bits) - same as language
        writer.write(201, 6, u64::from(language[0] - b'A'));
        writer.write(207, 6, u64::from(language[1] - b'A'));
        // MaxVendorConsentId (16 bits)
        writer.write(213, 16, u64::from(max_vendor_id));
        // IsRangeEncoding (1 bit) = 0 (bitfield)
        writer.write_bool(229, false);
        // Vendor consent bits
        for (i, &consented) in vendor_consent_bits.iter().enumerate() {
            if consented {
                writer.write_bool(230 + i, true);
            }
        }

        buf
    }

    /// Simple bit writer for test data construction.
    struct BitWriter<'a> {
        bytes: &'a mut [u8],
    }

    impl<'a> BitWriter<'a> {
        fn new(bytes: &'a mut [u8]) -> Self {
            Self { bytes }
        }

        fn write_bool(&mut self, bit_offset: usize, value: bool) {
            if value {
                let byte_idx = bit_offset / 8;
                let bit_idx = 7 - (bit_offset % 8);
                if byte_idx < self.bytes.len() {
                    self.bytes[byte_idx] |= 1 << bit_idx;
                }
            }
        }

        fn write(&mut self, bit_offset: usize, num_bits: usize, value: u64) {
            for i in 0..num_bits {
                let bit = (value >> (num_bits - 1 - i)) & 1 == 1;
                self.write_bool(bit_offset + i, bit);
            }
        }
    }
}
