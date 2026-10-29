//! Parsing and validation of browser-supplied EIDs.
//!
//! The `/auction` request body and the `ts-eids` cookie carry the same
//! OpenRTB-style EID array, which TSJS copies from the EIDs Prebid placed in
//! the `trustedServer` bidder request. Both go through [`parse_client_eids`],
//! so a malformed entry or field costs only itself, whichever transport or
//! client sent it.
//!
//! Rules, per entry:
//!
//! - The entry parses on its own and is a JSON object, or it is dropped.
//! - `source` is a string, not blank, at most [`MAX_CLIENT_EID_LABEL_BYTES`]
//!   UTF-8 bytes, or the entry is dropped.
//! - `uids` holds at least one valid UID, or the entry is dropped. The first
//!   [`MAX_CLIENT_UIDS_PER_ENTRY`] valid UIDs are kept.
//! - `uids[].id` is a string, not blank, at most `MAX_UID_LENGTH` bytes, or
//!   the UID is dropped. `uids[].atype` is an integer from 0 to `i32::MAX`;
//!   `uids[].ext` is an object of at most [`MAX_CLIENT_EID_EXT_BYTES`] as
//!   compact JSON. An invalid `atype` or `ext` drops only that field.
//! - `inserter` and `matcher` follow the `source` rule, `mm` the `atype` rule
//!   and `ext` the UID `ext` rule. An invalid value drops only that field.
//! - Unknown keys are ignored. Order is preserved and a source may repeat.
//!
//! An entry equal to an earlier kept entry is dropped, and then the first
//! [`MAX_CLIENT_EID_ENTRIES`] entries are kept. "Blank" means empty or only
//! whitespace by `str::trim`. "Integer" means a JSON number with no fractional
//! part, so `4`, `4.0` and `4e0` are the same value.

use serde_json::Value as JsonValue;
use serde_json::value::RawValue;

use crate::openrtb::{Eid, Uid};

use super::kv_types::MAX_UID_LENGTH;

/// Maximum number of entries kept from one browser EID array.
pub(crate) const MAX_CLIENT_EID_ENTRIES: usize = 64;

/// Maximum number of valid UIDs kept per entry.
pub(crate) const MAX_CLIENT_UIDS_PER_ENTRY: usize = 32;

/// Maximum UTF-8 length of `source`, `inserter` and `matcher`.
pub(crate) const MAX_CLIENT_EID_LABEL_BYTES: usize = 255;

/// Maximum compact-JSON length of an EID-level or UID-level `ext` object.
pub(crate) const MAX_CLIENT_EID_EXT_BYTES: usize = 2048;

/// Parses and validates browser-supplied EID entries from an `/auction` body
/// or a `ts-eids` cookie.
///
/// See the module documentation for the rules. Anything other than a JSON
/// array yields no entries.
///
/// # Performance
///
/// Parses each element into a [`JsonValue`] once. Duplicate detection is
/// quadratic in the kept entries, which are capped at
/// [`MAX_CLIENT_EID_ENTRIES`].
#[must_use]
pub(crate) fn parse_client_eids(raw: &RawValue) -> Vec<Eid> {
    // Capturing elements as raw JSON does not validate string escapes, so an
    // element holding a lone surrogate fails only its own parse below.
    let Ok(elements) = serde_json::from_str::<Vec<&RawValue>>(raw.get()) else {
        log::debug!("Client EIDs: payload is not a JSON array; ignoring it");
        return Vec::new();
    };

    let mut eids: Vec<Eid> = Vec::new();
    for element in &elements {
        if eids.len() >= MAX_CLIENT_EID_ENTRIES {
            log::debug!(
                "Client EIDs: keeping the first {MAX_CLIENT_EID_ENTRIES} entries; dropping the rest"
            );
            break;
        }
        let Some(eid) = parse_client_eid(element) else {
            continue;
        };
        if eids.contains(&eid) {
            log::debug!(
                "Client EIDs: dropping a duplicate entry for source '{}'",
                eid.source
            );
            continue;
        }
        eids.push(eid);
    }
    eids
}

fn parse_client_eid(element: &RawValue) -> Option<Eid> {
    let entry = match serde_json::from_str::<JsonValue>(element.get()) {
        Ok(JsonValue::Object(entry)) => entry,
        Ok(_) => {
            log::debug!("Client EIDs: dropping an entry that is not a JSON object");
            return None;
        }
        Err(_) => {
            log::debug!("Client EIDs: dropping an entry that does not parse");
            return None;
        }
    };

    let Some(source) = entry.get("source").and_then(parse_client_eid_label) else {
        log::debug!("Client EIDs: dropping an entry without a valid `source`");
        return None;
    };

    let uids: Vec<Uid> = match entry.get("uids") {
        Some(JsonValue::Array(raw_uids)) => raw_uids
            .iter()
            .filter_map(|raw_uid| parse_client_uid(raw_uid, &source))
            .take(MAX_CLIENT_UIDS_PER_ENTRY)
            .collect(),
        _ => Vec::new(),
    };
    if uids.is_empty() {
        log::debug!("Client EIDs: dropping source '{source}' without a valid UID");
        return None;
    }

    let inserter = parse_optional_field(&entry, "EID", "inserter", &source, parse_client_eid_label);
    let matcher = parse_optional_field(&entry, "EID", "matcher", &source, parse_client_eid_label);
    let mm = parse_optional_field(&entry, "EID", "mm", &source, parse_client_eid_integer);
    let ext = parse_optional_field(&entry, "EID", "ext", &source, parse_client_eid_ext);

    Some(Eid {
        source,
        uids,
        inserter,
        matcher,
        mm,
        ext,
    })
}

fn parse_client_uid(raw: &JsonValue, source: &str) -> Option<Uid> {
    let JsonValue::Object(uid) = raw else {
        log::debug!("Client EIDs: dropping a UID that is not an object for source '{source}'");
        return None;
    };

    let Some(id) = uid
        .get("id")
        .and_then(JsonValue::as_str)
        .filter(|id| !id.trim().is_empty())
        .filter(|id| id.len() <= MAX_UID_LENGTH)
    else {
        log::debug!("Client EIDs: dropping a UID without a valid `id` for source '{source}'");
        return None;
    };

    Some(Uid {
        id: id.to_owned(),
        atype: parse_optional_field(uid, "UID", "atype", source, parse_client_eid_integer),
        ext: parse_optional_field(uid, "UID", "ext", source, parse_client_eid_ext),
    })
}

/// Applies `parse` to `key` when it is present, logging the field name and
/// source, never the value, when the value is invalid.
fn parse_optional_field<T>(
    object: &serde_json::Map<String, JsonValue>,
    scope: &str,
    key: &str,
    source: &str,
    parse: impl Fn(&JsonValue) -> Option<T>,
) -> Option<T> {
    let value = object.get(key)?;
    let parsed = parse(value);
    if parsed.is_none() {
        log::debug!("Client EIDs: dropping invalid {scope} `{key}` for source '{source}'");
    }
    parsed
}

/// Reads `source`, `inserter` or `matcher`: a string that is not blank and at
/// most [`MAX_CLIENT_EID_LABEL_BYTES`] long, kept as supplied.
fn parse_client_eid_label(value: &JsonValue) -> Option<String> {
    value
        .as_str()
        .filter(|label| !label.trim().is_empty())
        .filter(|label| label.len() <= MAX_CLIENT_EID_LABEL_BYTES)
        .map(str::to_owned)
}

/// Reads `atype` or `mm`: a JSON number with no fractional part, from 0 to
/// `i32::MAX`.
fn parse_client_eid_integer(value: &JsonValue) -> Option<i32> {
    let number = value.as_number()?;
    if let Some(integer) = number.as_i64() {
        return i32::try_from(integer).ok().filter(|integer| *integer >= 0);
    }
    if number.is_u64() {
        // Above `i64::MAX`, so above the accepted range.
        return None;
    }
    // serde_json reads `4.0`, `4e0` and `-0` as floats.
    let float = number.as_f64()?;
    if float.fract() != 0.0 || !(0.0..=f64::from(i32::MAX)).contains(&float) {
        return None;
    }
    // Integral and within `0..=i32::MAX`, so the conversion is exact.
    Some(float as i32)
}

/// Reads an `ext`: a JSON object of at most [`MAX_CLIENT_EID_EXT_BYTES`] as
/// compact JSON.
fn parse_client_eid_ext(value: &JsonValue) -> Option<JsonValue> {
    if !value.is_object() {
        return None;
    }
    let compact_len = serde_json::to_vec(value).ok()?.len();
    (compact_len <= MAX_CLIENT_EID_EXT_BYTES).then(|| value.clone())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, Once};

    use serde_json::json;

    use super::*;

    fn parse(text: &str) -> Vec<Eid> {
        let raw = RawValue::from_string(text.to_owned()).expect("should capture raw JSON");
        parse_client_eids(&raw)
    }

    fn parsed_json(text: &str) -> JsonValue {
        serde_json::to_value(parse(text)).expect("should serialize parsed EIDs")
    }

    fn good(source: &str) -> String {
        format!(r#"{{"source":"{source}","uids":[{{"id":"uid-{source}"}}]}}"#)
    }

    fn good_json(source: &str) -> JsonValue {
        json!({ "source": source, "uids": [{ "id": format!("uid-{source}") }] })
    }

    /// An `ext` object whose compact JSON is exactly `bytes` long.
    fn ext_of_len(bytes: usize) -> String {
        // `{"k":""}` is 8 bytes.
        format!(r#"{{"k":"{}"}}"#, "x".repeat(bytes - 8))
    }

    static CAPTURED_LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static INSTALL_LOGGER: Once = Once::new();

    struct CapturingLogger;

    impl log::Log for CapturingLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Debug
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                CAPTURED_LOGS
                    .lock()
                    .expect("should lock captured logs")
                    .push(record.args().to_string());
            }
        }

        fn flush(&self) {}
    }

    fn install_capturing_logger() {
        INSTALL_LOGGER.call_once(|| {
            log::set_logger(&CapturingLogger).expect("should install the capturing test logger");
            log::set_max_level(log::LevelFilter::Debug);
        });
    }

    fn captured_logs() -> Vec<String> {
        CAPTURED_LOGS
            .lock()
            .expect("should lock captured logs")
            .clone()
    }

    #[test]
    fn keeps_a_valid_entry_with_every_field() {
        let parsed = parsed_json(
            r#"[{"source":"ids.example.com","inserter":"inserter.example.com","matcher":"matcher.example.com","mm":4,"ext":{"segments":["s1"]},"uids":[{"id":"uid-1","atype":1,"ext":{"rtiPartner":"TDID"}}]}]"#,
        );

        assert_eq!(
            parsed,
            json!([{
                "source": "ids.example.com",
                "inserter": "inserter.example.com",
                "matcher": "matcher.example.com",
                "mm": 4,
                "ext": { "segments": ["s1"] },
                "uids": [{ "id": "uid-1", "atype": 1, "ext": { "rtiPartner": "TDID" } }]
            }]),
            "should keep every valid field"
        );
    }

    #[test]
    fn yields_nothing_for_a_payload_that_is_not_an_array() {
        for text in [
            r#"{"source":"ids.example.com"}"#,
            r#""text""#,
            "null",
            "1",
            "true",
        ] {
            assert_eq!(
                parsed_json(text),
                json!([]),
                "should ignore non-array payload {text}"
            );
        }
    }

    #[test]
    fn drops_only_the_entry_that_does_not_parse() {
        // `\ud800` is a lone surrogate escape: serde_json rejects it when the
        // element is parsed on its own.
        let bad_entries = [
            r#"{"source":"bad-\ud800.example.com","uids":[{"id":"u"}]}"#,
            r#"{"source":"bad.example.com","uids":[{"id":"u-\ud800"}]}"#,
            r#"{"source":"bad.example.com","inserter":"i-\ud800","uids":[{"id":"u"}]}"#,
            r#"{"source":"bad.example.com","matcher":"m-\ud800","uids":[{"id":"u"}]}"#,
            r#"{"source":"bad.example.com","ext":{"k-\ud800":1},"uids":[{"id":"u"}]}"#,
            r#"{"source":"bad.example.com","ext":{"k":"v-\ud800"},"uids":[{"id":"u"}]}"#,
            r#"{"source":"bad.example.com","uids":[{"id":"u","ext":{"k-\ud800":1}}]}"#,
            r#"{"source":"bad.example.com","uids":[{"id":"u","ext":{"k":"v-\ud800"}}]}"#,
        ];
        for bad in bad_entries {
            let text = format!(
                "[{},{bad},{}]",
                good("a.example.com"),
                good("b.example.com")
            );

            assert_eq!(
                parsed_json(&text),
                json!([good_json("a.example.com"), good_json("b.example.com")]),
                "should drop only the unparsable entry {bad}"
            );
        }
    }

    #[test]
    fn drops_entries_that_are_not_objects() {
        let text = format!(r#"[1,"text",null,[],true,{}]"#, good("a.example.com"));

        assert_eq!(
            parsed_json(&text),
            json!([good_json("a.example.com")]),
            "should keep only the object entry"
        );
    }

    #[test]
    fn validates_source() {
        let source_255 = format!("a{}", "é".repeat(127));
        let source_256 = "é".repeat(128);
        let text = format!(
            r#"[{{"uids":[{{"id":"u"}}]}},{{"source":7,"uids":[{{"id":"u"}}]}},{{"source":"   ","uids":[{{"id":"u"}}]}},{{"source":"{source_256}","uids":[{{"id":"u"}}]}},{{"source":"{source_255}","uids":[{{"id":"u"}}]}}]"#
        );

        assert_eq!(
            parsed_json(&text),
            json!([{ "source": source_255, "uids": [{ "id": "u" }] }]),
            "should keep only a non-blank string source of at most 255 UTF-8 bytes"
        );
    }

    #[test]
    fn drops_an_entry_without_a_valid_uid() {
        let text = format!(
            r#"[{{"source":"no-uids.example.com"}},{{"source":"string-uids.example.com","uids":"u"}},{{"source":"empty-uids.example.com","uids":[]}},{{"source":"bad-uids.example.com","uids":[{{"id":""}},7]}},{}]"#,
            good("a.example.com")
        );

        assert_eq!(
            parsed_json(&text),
            json!([good_json("a.example.com")]),
            "should drop entries whose uids hold no valid UID"
        );
    }

    #[test]
    fn validates_uid_ids() {
        let id_512 = "x".repeat(512);
        let id_513 = "x".repeat(513);
        let text = format!(
            r#"[{{"source":"ids.example.com","uids":[{{"id":7}},{{"id":"  "}},{{"id":"{id_513}"}},{{"atype":1}},{{"id":"{id_512}"}}]}}]"#
        );

        assert_eq!(
            parsed_json(&text),
            json!([{ "source": "ids.example.com", "uids": [{ "id": id_512 }] }]),
            "should keep only a non-blank string id of at most 512 bytes"
        );
    }

    #[test]
    fn keeps_the_first_32_valid_uids() {
        let uids: Vec<String> = [r#"{"id":""}"#.to_owned(), r#"{"id":7}"#.to_owned()]
            .into_iter()
            .chain((0..33).map(|index| format!(r#"{{"id":"u{index}"}}"#)))
            .collect();
        let text = format!(
            r#"[{{"source":"ids.example.com","uids":[{}]}}]"#,
            uids.join(",")
        );

        let expected_uids: Vec<JsonValue> = (0..32)
            .map(|index| json!({ "id": format!("u{index}") }))
            .collect();
        assert_eq!(
            parsed_json(&text),
            json!([{ "source": "ids.example.com", "uids": expected_uids }]),
            "should keep the first 32 valid UIDs, not counting invalid ones"
        );
    }

    #[test]
    fn reads_atype_and_mm_as_integers_in_range() {
        let accepted = [
            ("4", 4),
            ("4.0", 4),
            ("4e0", 4),
            ("-0", 0),
            ("0", 0),
            ("2147483647", 2_147_483_647),
            ("2147483647.0", 2_147_483_647),
            ("571187", 571_187),
        ];
        for (spelling, value) in accepted {
            let text = format!(
                r#"[{{"source":"ids.example.com","mm":{spelling},"uids":[{{"id":"u","atype":{spelling}}}]}}]"#
            );

            assert_eq!(
                parsed_json(&text),
                json!([{ "source": "ids.example.com", "mm": value, "uids": [{ "id": "u", "atype": value }] }]),
                "should accept {spelling} as {value}"
            );
        }

        let rejected = [
            "2147483648",
            "9007199254740993",
            "18446744073709551616",
            "1.5",
            "-1",
            "1e300",
            r#""1""#,
            "null",
            "true",
        ];
        for spelling in rejected {
            let text = format!(
                r#"[{{"source":"ids.example.com","mm":{spelling},"uids":[{{"id":"u","atype":{spelling}}}]}}]"#
            );

            assert_eq!(
                parsed_json(&text),
                json!([{ "source": "ids.example.com", "uids": [{ "id": "u" }] }]),
                "should drop only the fields for {spelling}"
            );
        }
    }

    #[test]
    fn validates_ext_objects_and_their_size() {
        let ext_2048 = ext_of_len(2048);
        let ext_2049 = ext_of_len(2049);
        let text = format!(
            r#"[{{"source":"keep.example.com","ext":{ext_2048},"uids":[{{"id":"u","ext":{ext_2048}}}]}},{{"source":"big.example.com","ext":{ext_2049},"uids":[{{"id":"u","ext":{ext_2049}}}]}},{{"source":"shape.example.com","ext":[1],"uids":[{{"id":"u","ext":"text"}}]}}]"#
        );
        let kept_ext: JsonValue = serde_json::from_str(&ext_2048).expect("should parse ext");

        assert_eq!(
            parsed_json(&text),
            json!([
                { "source": "keep.example.com", "ext": kept_ext, "uids": [{ "id": "u", "ext": kept_ext }] },
                { "source": "big.example.com", "uids": [{ "id": "u" }] },
                { "source": "shape.example.com", "uids": [{ "id": "u" }] }
            ]),
            "should keep objects of at most 2048 compact bytes and drop only the bad fields"
        );
    }

    #[test]
    fn validates_inserter_and_matcher() {
        let label_255 = format!("a{}", "é".repeat(127));
        let label_256 = "é".repeat(128);
        let text = format!(
            r#"[{{"source":"keep.example.com","inserter":"{label_255}","matcher":"{label_255}","uids":[{{"id":"u"}}]}},{{"source":"drop.example.com","inserter":"{label_256}","matcher":"  ","uids":[{{"id":"u"}}]}},{{"source":"type.example.com","inserter":7,"matcher":null,"uids":[{{"id":"u"}}]}}]"#
        );

        assert_eq!(
            parsed_json(&text),
            json!([
                { "source": "keep.example.com", "inserter": label_255, "matcher": label_255, "uids": [{ "id": "u" }] },
                { "source": "drop.example.com", "uids": [{ "id": "u" }] },
                { "source": "type.example.com", "uids": [{ "id": "u" }] }
            ]),
            "should keep non-blank strings of at most 255 bytes and drop only the bad fields"
        );
    }

    #[test]
    fn drops_duplicates_before_the_entry_cap() {
        let mut entries: Vec<String> = (0..64)
            .map(|index| good(&format!("s{index}.example.com")))
            .collect();
        // Equal to entry 0 once validated: unknown keys are ignored.
        entries.insert(
            1,
            r#"{"source":"s0.example.com","uids":[{"id":"uid-s0.example.com"}],"unknown":true}"#
                .to_owned(),
        );
        let text = format!("[{}]", entries.join(","));

        let expected: Vec<JsonValue> = (0..64)
            .map(|index| good_json(&format!("s{index}.example.com")))
            .collect();
        assert_eq!(
            parsed_json(&text),
            JsonValue::Array(expected),
            "a duplicate should not take one of the 64 places"
        );
    }

    #[test]
    fn keeps_the_first_64_entries() {
        let entries: Vec<String> = (0..70)
            .map(|index| good(&format!("s{index}.example.com")))
            .collect();
        let text = format!("[{}]", entries.join(","));

        let expected: Vec<JsonValue> = (0..64)
            .map(|index| good_json(&format!("s{index}.example.com")))
            .collect();
        assert_eq!(
            parsed_json(&text),
            JsonValue::Array(expected),
            "should keep the first 64 entries"
        );
    }

    #[test]
    fn keeps_order_and_repeated_sources_and_ignores_unknown_keys() {
        let parsed = parsed_json(
            r#"[{"source":"b.example.com","uids":[{"id":"u","unknown":1}],"unknown":2},{"source":"a.example.com","mm":1,"uids":[{"id":"u"}]},{"source":"a.example.com","mm":2,"uids":[{"id":"u"}]}]"#,
        );

        assert_eq!(
            parsed,
            json!([
                { "source": "b.example.com", "uids": [{ "id": "u" }] },
                { "source": "a.example.com", "mm": 1, "uids": [{ "id": "u" }] },
                { "source": "a.example.com", "mm": 2, "uids": [{ "id": "u" }] }
            ]),
            "should keep wire order and both same-source entries"
        );
    }

    #[test]
    fn logs_dropped_entries_and_fields_by_name_and_source_without_values() {
        install_capturing_logger();

        let parsed = parsed_json(
            r#"[{"source":"log-a.example.com","mm":-1,"uids":[{"id":"secret-uid-a","atype":"1"}]},{"source":"log-b.example.com","uids":[{"id":""}]}]"#,
        );

        assert_eq!(
            parsed,
            json!([{ "source": "log-a.example.com", "uids": [{ "id": "secret-uid-a" }] }]),
            "should keep the entry and drop only its invalid fields"
        );
        let logs = captured_logs();
        for (field, source) in [
            ("`mm`", "log-a.example.com"),
            ("`atype`", "log-a.example.com"),
            ("without a valid UID", "log-b.example.com"),
        ] {
            assert!(
                logs.iter()
                    .any(|line| line.contains(field) && line.contains(source)),
                "should log {field} for {source}; captured: {logs:?}"
            );
        }
        assert!(
            !logs.iter().any(|line| line.contains("secret-uid-a")),
            "should never log a UID value"
        );
    }
}
