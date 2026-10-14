//! The TCF v2 consent string decoder, core segment only.
//!
//! Decodes a TC String from the `euconsent-v2` cookie, or the EU section of a
//! GPP string. Only the core segment (segment type 0) is decoded, so publisher
//! restrictions, disclosed vendors and allowed vendors are not read.
//!
//! # Binary format
//!
//! The TC String is a web-safe base64-encoded bitfield. The core segment
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

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use error_stack::Report;

/// Longest raw TC String decoded. Real strings are under 1 KB, and the limit
/// keeps a hostile cookie from forcing a large allocation.
const MAX_TC_STRING_LEN: usize = 4096;

/// Highest vendor identifier read. The vendor list has about 1200 entries,
/// and the cap keeps a hostile range entry from expanding into an enormous
/// list. Vendors beyond it are silently dropped.
const MAX_VENDOR_ID: u16 = 10_000;

/// The decoded core segment of a TC String.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcfConsent {
    /// TCF version (2).
    pub version: u8,
    /// CMP ID that collected this consent.
    pub cmp_id: u16,
    /// CMP version.
    pub cmp_version: u16,
    /// Consent screen number.
    pub consent_screen: u8,
    /// CMP language (ISO 639-1, two uppercase letters).
    pub consent_language: String,
    /// Vendor list version used.
    pub vendor_list_version: u16,
    /// TCF policy version.
    pub tcf_policy_version: u8,
    /// When consent was created, in deciseconds since the epoch.
    pub created_ds: u64,
    /// When consent was last updated, in deciseconds since the epoch.
    pub last_updated_ds: u64,

    /// Purpose consents (24 bits, 1-indexed).
    ///
    /// `true` at index 0 means purpose 1 is consented, and so on.
    pub purpose_consents: Vec<bool>,
    /// Purpose legitimate interests (24 bits, 1-indexed).
    pub purpose_legitimate_interests: Vec<bool>,

    /// Vendor IDs with consent granted.
    pub vendor_consents: Vec<u16>,
    /// Vendor IDs with legitimate interest established.
    pub vendor_legitimate_interests: Vec<u16>,

    /// Special feature opt-ins (12 bits).
    pub special_feature_opt_ins: Vec<bool>,
}

impl TcfConsent {
    /// Looks up a 1-indexed purpose in a bitfield, `false` for purpose 0
    /// and for an index past the end.
    fn purpose_bit(bits: &[bool], purpose: usize) -> bool {
        purpose
            .checked_sub(1)
            .and_then(|idx| bits.get(idx).copied())
            .unwrap_or(false)
    }

    /// Whether consent was given for a purpose, 1-indexed as the
    /// specification numbers them, `false` out of range.
    #[must_use]
    pub fn has_purpose_consent(&self, purpose: usize) -> bool {
        Self::purpose_bit(&self.purpose_consents, purpose)
    }

    /// Whether legitimate interest was established for a purpose, 1-indexed,
    /// `false` out of range.
    #[must_use]
    pub fn has_purpose_li(&self, purpose: usize) -> bool {
        Self::purpose_bit(&self.purpose_legitimate_interests, purpose)
    }

    /// Whether a vendor has consent.
    #[must_use]
    pub fn has_vendor_consent(&self, vendor_id: u16) -> bool {
        self.vendor_consents.contains(&vendor_id)
    }

    /// Whether a vendor has established legitimate interest.
    #[must_use]
    pub fn has_vendor_li(&self, vendor_id: u16) -> bool {
        self.vendor_legitimate_interests.contains(&vendor_id)
    }

    /// Whether Purpose 1 (store and/or access information on a device) is
    /// consented.
    #[must_use]
    pub fn has_storage_consent(&self) -> bool {
        self.has_purpose_consent(1)
    }

    /// Whether Purpose 2 (use limited data to select advertising) is
    /// consented.
    #[must_use]
    pub fn has_basic_ads_consent(&self) -> bool {
        self.has_purpose_consent(2)
    }

    /// Whether Purpose 4 (use profiles to select personalised advertising) is
    /// consented.
    #[must_use]
    pub fn has_personalized_ads_consent(&self) -> bool {
        self.has_purpose_consent(4)
    }
}

/// Why a TC String could not be decoded.
#[derive(Debug, derive_more::Display)]
pub enum TcfDecodeError {
    /// The string is too long, is not base64, is not version 2, or is too
    /// short to hold the core segment.
    #[display("invalid TC String: {reason}")]
    InvalidTcString {
        /// What was wrong with it.
        reason: String,
    },
}

impl core::error::Error for TcfDecodeError {}

/// Decodes the core segment of a TC String v2. Additional segments, after a
/// `.`, are ignored.
///
/// # Errors
///
/// [`TcfDecodeError::InvalidTcString`] when the string exceeds
/// `MAX_TC_STRING_LEN`, base64 decoding fails, the version is not 2, or the
/// bitfield is too short.
pub fn decode_tc_string(tc_string: &str) -> Result<TcfConsent, Report<TcfDecodeError>> {
    if tc_string.len() > MAX_TC_STRING_LEN {
        return Err(Report::new(TcfDecodeError::InvalidTcString {
            reason: format!(
                "TC string too long: {} bytes, max {MAX_TC_STRING_LEN}",
                tc_string.len()
            ),
        }));
    }

    // The first segment is always the core segment.
    let core_segment = tc_string.split('.').next().unwrap_or(tc_string);

    let bytes = URL_SAFE_NO_PAD
        .decode(core_segment)
        .or_else(|_| {
            // Some CMPs use standard base64 with padding.
            use base64::engine::general_purpose::STANDARD;
            STANDARD.decode(core_segment)
        })
        .map_err(|e| {
            Report::new(TcfDecodeError::InvalidTcString {
                reason: format!("base64 decode failed: {e}"),
            })
        })?;

    let reader = BitReader::new(&bytes);

    // The core fields up to IsRangeEncoding take 230 bits.
    if reader.bit_len() < 230 {
        return Err(Report::new(TcfDecodeError::InvalidTcString {
            reason: format!(
                "bitfield too short: {} bits, need at least 230",
                reader.bit_len()
            ),
        }));
    }

    let version = reader.read_u8(0, 6);
    if version != 2 {
        return Err(Report::new(TcfDecodeError::InvalidTcString {
            reason: format!("unsupported version {version}, expected 2"),
        }));
    }

    let created_ds = reader.read_u64(6, 36);
    let last_updated_ds = reader.read_u64(42, 36);
    let cmp_id = reader.read_u16(78, 12);
    let cmp_version = reader.read_u16(90, 12);
    let consent_screen = reader.read_u8(102, 6);

    // Consent language: two 6-bit values, each offset from 'A', so 0 to 25.
    let lang_a = reader.read_u8(108, 6);
    let lang_b = reader.read_u8(114, 6);
    if lang_a > 25 || lang_b > 25 {
        return Err(Report::new(TcfDecodeError::InvalidTcString {
            reason: format!("invalid consent language values: ({lang_a}, {lang_b}), expected 0-25"),
        }));
    }
    let consent_language = format!("{}{}", char::from(b'A' + lang_a), char::from(b'A' + lang_b));

    let vendor_list_version = reader.read_u16(120, 12);
    let tcf_policy_version = reader.read_u8(132, 6);
    // Skipped: IsServiceSpecific (138, 1), UseNonStandardTexts (139, 1).

    let special_feature_opt_ins = reader.read_bool_vec(140, 12);
    let purpose_consents = reader.read_bool_vec(152, 24);
    let purpose_legitimate_interests = reader.read_bool_vec(176, 24);
    // Skipped: PurposeOneTreatment (200, 1), PublisherCC (201, 12).

    let vendor_consents = decode_vendor_section(&reader, 213);

    // Vendor legitimate interests follow the vendor consents.
    let vendor_li_offset = vendor_section_end_offset(&reader, 213);
    let vendor_legitimate_interests = if vendor_li_offset + 17 <= reader.bit_len() {
        decode_vendor_section(&reader, vendor_li_offset)
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

/// Decodes a vendor section, consents or legitimate interests, which starts
/// with `MaxVendorId` (16 bits) and `IsRangeEncoding` (1 bit) and is then a
/// bitfield of one bit per vendor, or `NumEntries` (12 bits) of entries.
fn decode_vendor_section(reader: &BitReader<'_>, offset: usize) -> Vec<u16> {
    if offset + 17 > reader.bit_len() {
        return Vec::new();
    }

    let raw_max_vendor_id = reader.read_u16(offset, 16);
    let max_vendor_id = raw_max_vendor_id.min(MAX_VENDOR_ID);
    let is_range = reader.read_bool(offset + 16);

    if is_range {
        let num_entries_offset = offset + 17;
        if num_entries_offset + 12 > reader.bit_len() {
            return Vec::new();
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
                // StartVendorId (16) and EndVendorId (16).
                if pos + 32 > reader.bit_len() {
                    break;
                }
                let start = reader.read_u16(pos, 16);
                let end = reader.read_u16(pos + 16, 16);
                pos += 32;

                // Clamped to MAX_VENDOR_ID so a hostile range cannot amplify.
                if start == 0 || start > end {
                    continue;
                }
                for id in start..=end.min(MAX_VENDOR_ID) {
                    vendors.push(id);
                }
            } else {
                // A single VendorId (16).
                if pos + 16 > reader.bit_len() {
                    break;
                }
                let id = reader.read_u16(pos, 16);
                pos += 16;
                if id > 0 && id <= MAX_VENDOR_ID {
                    vendors.push(id);
                }
            }
        }
        vendors
    } else {
        // One bit per vendor, 1..=max_vendor_id.
        let mut vendors = Vec::new();
        let bitfield_start = offset + 17;
        for i in 0..usize::from(max_vendor_id) {
            let bit_pos = bitfield_start + i;
            if bit_pos >= reader.bit_len() {
                break;
            }
            if reader.read_bool(bit_pos) {
                vendors.push((i + 1) as u16);
            }
        }
        vendors
    }
}

/// The bit offset after a vendor section ends.
///
/// Reads the raw `MaxVendorId` rather than the capped one, because the
/// section's true end in the bitstream depends on it.
fn vendor_section_end_offset(reader: &BitReader<'_>, offset: usize) -> usize {
    if offset + 17 > reader.bit_len() {
        return offset;
    }

    let max_vendor_id = reader.read_u16(offset, 16);
    let is_range = reader.read_bool(offset + 16);

    if is_range {
        let num_entries_offset = offset + 17;
        if num_entries_offset + 12 > reader.bit_len() {
            return num_entries_offset;
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
                pos += 32;
            } else {
                pos += 16;
            }
        }
        pos
    } else {
        offset + 17 + usize::from(max_vendor_id)
    }
}

/// A bit-level reader over a byte slice, addressed by bit offset.
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

    fn read_bool(&self, bit_offset: usize) -> bool {
        let byte_idx = bit_offset / 8;
        let bit_idx = 7 - (bit_offset % 8);
        if byte_idx >= self.bytes.len() {
            return false;
        }
        (self.bytes[byte_idx] >> bit_idx) & 1 == 1
    }

    fn read_u8(&self, bit_offset: usize, num_bits: usize) -> u8 {
        debug_assert!(num_bits <= 8);
        self.read_u64(bit_offset, num_bits) as u8
    }

    fn read_u16(&self, bit_offset: usize, num_bits: usize) -> u16 {
        debug_assert!(num_bits <= 16);
        self.read_u64(bit_offset, num_bits) as u16
    }

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

    fn read_bool_vec(&self, bit_offset: usize, num_bits: usize) -> Vec<bool> {
        (0..num_bits)
            .map(|i| self.read_bool(bit_offset + i))
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn decodes_minimal_tc_string() {
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
        let purposes = vec![true, true, false, true];
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
        assert!(result.has_storage_consent() && result.has_basic_ads_consent());
        assert!(result.has_personalized_ads_consent());
        assert!(!result.has_purpose_consent(0) && !result.has_purpose_consent(99));
        assert!(!result.has_purpose_li(1));
    }

    #[test]
    fn decodes_vendor_consents_bitfield() {
        let vendor_bits = vec![true, false, true, false, true];
        let bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &[], &vendor_bits);
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);

        let result = decode_tc_string(&encoded).expect("should decode vendor bitfield");
        assert_eq!(
            result.vendor_consents,
            vec![1, 3, 5],
            "should have vendors 1, 3, 5"
        );
        assert!(result.has_vendor_consent(3) && !result.has_vendor_consent(2));
        assert!(!result.has_vendor_li(3));
    }

    #[test]
    fn rejects_version_1() {
        let mut bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &[], &[]);
        bytes[0] = (bytes[0] & 0x03) | (1 << 2);
        let encoded = URL_SAFE_NO_PAD.encode(&bytes);

        let result = decode_tc_string(&encoded);
        assert!(result.is_err(), "should reject version 1");
    }

    #[test]
    fn rejects_too_short() {
        let encoded = URL_SAFE_NO_PAD.encode([0_u8; 10]);
        let result = decode_tc_string(&encoded);
        assert!(result.is_err(), "should reject short bitfield");
    }

    #[test]
    fn rejects_invalid_base64() {
        let result = decode_tc_string("!!!invalid!!!");
        assert!(result.is_err(), "should reject invalid base64");
    }

    #[test]
    fn rejects_an_oversized_string() {
        let oversized = "A".repeat(MAX_TC_STRING_LEN + 1);
        assert!(decode_tc_string(&oversized).is_err());
    }

    #[test]
    fn handles_segmented_tc_string() {
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

    #[test]
    fn clamps_range_encoded_vendors_to_max() {
        let core_bits: usize = 213;
        let total_bits = core_bits + 16 + 1 + 12 + 1 + 16 + 16;
        let total_bytes = total_bits.div_ceil(8);
        let mut buf = vec![0_u8; total_bytes];
        let mut writer = BitWriter::new(&mut buf);

        writer.write(0, 6, 2);
        writer.write(6, 36, 0);
        writer.write(42, 36, 0);
        writer.write(78, 12, 1);
        writer.write(90, 12, 1);
        writer.write(102, 6, 0);
        writer.write(108, 6, 4);
        writer.write(114, 6, 13);
        writer.write(120, 12, 1);
        writer.write(132, 6, 2);
        writer.write(201, 6, 4);
        writer.write(207, 6, 13);

        writer.write(213, 16, 65535);
        writer.write_bool(229, true);
        writer.write(230, 12, 1);
        writer.write_bool(242, true);
        writer.write(243, 16, 9990);
        writer.write(259, 16, 65535);

        let encoded = URL_SAFE_NO_PAD.encode(&buf);
        let result = decode_tc_string(&encoded).expect("should decode with clamped vendors");

        assert_eq!(result.vendor_consents.len(), 11);
        assert_eq!(
            *result.vendor_consents.first().expect("should have first"),
            9990
        );
        assert_eq!(
            *result.vendor_consents.last().expect("should have last"),
            10_000
        );
    }

    #[test]
    fn rejects_invalid_consent_language() {
        let mut bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, &[], &[]);
        let mut writer = BitWriter::new(&mut bytes);
        writer.write(108, 6, 30);

        let encoded = URL_SAFE_NO_PAD.encode(&bytes);
        let result = decode_tc_string(&encoded);
        assert!(result.is_err(), "should reject consent language value > 25");
    }

    #[test]
    fn skips_invalid_range_start_gt_end() {
        let core_bits: usize = 213;
        let total_bits = core_bits + 16 + 1 + 12 + 1 + 16 + 16;
        let total_bytes = total_bits.div_ceil(8);
        let mut buf = vec![0_u8; total_bytes];
        let mut writer = BitWriter::new(&mut buf);

        writer.write(0, 6, 2);
        writer.write(78, 12, 1);
        writer.write(90, 12, 1);
        writer.write(108, 6, 4);
        writer.write(114, 6, 13);
        writer.write(120, 12, 1);
        writer.write(132, 6, 2);
        writer.write(201, 6, 4);
        writer.write(207, 6, 13);

        writer.write(213, 16, 100);
        writer.write_bool(229, true);
        writer.write(230, 12, 1);
        writer.write_bool(242, true);
        writer.write(243, 16, 50);
        writer.write(259, 16, 10);

        let encoded = URL_SAFE_NO_PAD.encode(&buf);
        let result = decode_tc_string(&encoded).expect("should decode with skipped range");
        assert!(
            result.vendor_consents.is_empty(),
            "should skip range where start > end"
        );
    }

    /// A TC String consenting to `purpose_consents` (1-indexed from index 0),
    /// last updated at `last_updated_ds`, encoded as a cookie carries it.
    pub(crate) fn encode_tc_string(purpose_consents: &[bool], last_updated_ds: u64) -> String {
        let mut bytes = build_minimal_tc_bytes(1, 1, b"EN", 1, purpose_consents, &[]);
        let mut writer = BitWriter::new(&mut bytes);
        writer.write(42, 36, last_updated_ds);
        URL_SAFE_NO_PAD.encode(&bytes)
    }

    /// Builds a minimal TC String v2 byte buffer: version 2, the given CMP,
    /// language, vendor list version and purposes, and a vendor bitfield.
    fn build_minimal_tc_bytes(
        cmp_id: u16,
        cmp_version: u16,
        language: &[u8; 2],
        vendor_list_version: u16,
        purpose_consents: &[bool],
        vendor_consent_bits: &[bool],
    ) -> Vec<u8> {
        let max_vendor_id = vendor_consent_bits.len() as u16;
        let total_bits = 213 + 17 + usize::from(max_vendor_id);
        let total_bytes = total_bits.div_ceil(8);
        let mut buf = vec![0_u8; total_bytes];

        let mut writer = BitWriter::new(&mut buf);

        writer.write(0, 6, 2);
        writer.write(6, 36, 0);
        writer.write(42, 36, 0);
        writer.write(78, 12, u64::from(cmp_id));
        writer.write(90, 12, u64::from(cmp_version));
        writer.write(102, 6, 0);
        writer.write(108, 6, u64::from(language[0] - b'A'));
        writer.write(114, 6, u64::from(language[1] - b'A'));
        writer.write(120, 12, u64::from(vendor_list_version));
        writer.write(132, 6, 2);
        for (i, &consented) in purpose_consents.iter().enumerate() {
            if consented && i < 24 {
                writer.write_bool(152 + i, true);
            }
        }
        writer.write(201, 6, u64::from(language[0] - b'A'));
        writer.write(207, 6, u64::from(language[1] - b'A'));
        writer.write(213, 16, u64::from(max_vendor_id));
        writer.write_bool(229, false);
        for (i, &consented) in vendor_consent_bits.iter().enumerate() {
            if consented {
                writer.write_bool(230 + i, true);
            }
        }

        buf
    }

    /// A bit writer for building test strings.
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
