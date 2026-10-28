//! A document too long for one Fastly config store entry, read back whole.
//!
//! A Fastly config store entry holds at most 8,000 characters. A longer
//! document is written as chunks under keys of their own, with a pointer at
//! the document's key naming each chunk's key, length and hash and the whole
//! document's length and hash. [`whole_entry`] follows such a pointer and
//! checks every length and hash, so the rest of the service only ever sees
//! whole documents.

use core::fmt::Write as _;

use error_stack::Report;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use trusted_server_core::platform::PlatformError;

/// The `edgezero_kind` a chunk pointer carries.
const CHUNK_POINTER_KIND: &str = "fastly_config_chunks";

/// The most characters one Fastly config store entry holds.
const ENTRY_LIMIT: usize = 8_000;

#[derive(Debug, Deserialize)]
struct ChunkPointer {
    chunks: Vec<ChunkRef>,
    edgezero_kind: String,
    envelope_len: usize,
    envelope_sha256: String,
    version: u8,
}

#[derive(Debug, Deserialize)]
struct ChunkRef {
    key: String,
    len: usize,
    sha256: String,
}

/// `value`, as read at a key, or the document its chunks hold when it is a
/// chunk pointer, each chunk read through `read`.
///
/// # Errors
///
/// When the pointer names a version this does not read, when it or a chunk
/// is longer than an entry can be, when a chunk cannot be read, or when any
/// length or hash disagrees with the pointer.
pub(crate) fn whole_entry(
    value: String,
    read: impl Fn(&str) -> Result<String, Report<PlatformError>>,
) -> Result<String, Report<PlatformError>> {
    let Ok(pointer) = serde_json::from_str::<ChunkPointer>(&value) else {
        return Ok(value);
    };
    if pointer.edgezero_kind != CHUNK_POINTER_KIND {
        return Ok(value);
    }
    if pointer.version != 1 {
        return refused(format!(
            "unsupported Fastly config chunk pointer version {}; expected 1",
            pointer.version
        ));
    }
    if value.len() > ENTRY_LIMIT {
        return refused(format!(
            "Fastly config chunk pointer is {} bytes, exceeding the {ENTRY_LIMIT} byte entry \
             limit",
            value.len()
        ));
    }

    let mut declared_len = 0usize;
    for chunk in &pointer.chunks {
        if chunk.len > ENTRY_LIMIT {
            return refused(format!(
                "Fastly config chunk `{}` declares {} bytes, exceeding the {ENTRY_LIMIT} byte \
                 entry limit",
                chunk.key, chunk.len
            ));
        }
        declared_len = match declared_len.checked_add(chunk.len) {
            Some(total) => total,
            None => return refused("Fastly config chunk lengths overflowed usize".to_string()),
        };
    }
    if declared_len != pointer.envelope_len {
        return refused(format!(
            "Fastly config chunk lengths total mismatch: expected envelope length {}, got \
             {declared_len}",
            pointer.envelope_len
        ));
    }

    let mut envelope = String::with_capacity(pointer.envelope_len);
    for chunk in pointer.chunks {
        let chunk_value = read(&chunk.key)?;
        if chunk_value.len() != chunk.len {
            return refused(format!(
                "Fastly config chunk `{}` length mismatch: expected {}, got {}",
                chunk.key,
                chunk.len,
                chunk_value.len()
            ));
        }
        if envelope.len().saturating_add(chunk_value.len()) > pointer.envelope_len {
            return refused(format!(
                "Fastly config envelope exceeded declared length {} while reading chunk `{}`",
                pointer.envelope_len, chunk.key
            ));
        }
        let chunk_sha = sha256_hex(chunk_value.as_bytes());
        if chunk_sha != chunk.sha256 {
            return refused(format!(
                "Fastly config chunk `{}` sha mismatch: expected {}, got {chunk_sha}",
                chunk.key, chunk.sha256
            ));
        }
        envelope.push_str(&chunk_value);
    }

    if envelope.len() != pointer.envelope_len {
        return refused(format!(
            "Fastly config envelope length mismatch: expected {}, got {}",
            pointer.envelope_len,
            envelope.len()
        ));
    }
    let envelope_sha = sha256_hex(envelope.as_bytes());
    if envelope_sha != pointer.envelope_sha256 {
        return refused(format!(
            "Fastly config envelope sha mismatch: expected {}, got {envelope_sha}",
            pointer.envelope_sha256
        ));
    }
    Ok(envelope)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("should write hex digit to string");
    }
    hex
}

fn refused<T>(message: String) -> Result<T, Report<PlatformError>> {
    Err(Report::new(PlatformError::ConfigStore).attach(message))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    const KEY: &str = "trusted_server_config";

    fn reader(
        entries: BTreeMap<String, String>,
    ) -> impl Fn(&str) -> Result<String, Report<PlatformError>> {
        move |key: &str| {
            entries.get(key).cloned().ok_or_else(|| {
                Report::new(PlatformError::ConfigStore).attach(format!("missing key `{key}`"))
            })
        }
    }

    fn pointer_for(document: &str, chunks: &[(&str, &str)]) -> String {
        json!({
            "edgezero_kind": CHUNK_POINTER_KIND,
            "version": 1,
            "envelope_sha256": sha256_hex(document.as_bytes()),
            "envelope_len": document.len(),
            "chunks": chunks.iter().map(|(key, piece)| json!({
                "key": key,
                "sha256": sha256_hex(piece.as_bytes()),
                "len": piece.len(),
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }

    #[test]
    fn a_document_that_fits_is_returned_as_it_is() {
        let document = r#"{"data":{},"sha256":"x"}"#.to_owned();
        let read = reader(BTreeMap::new());
        assert_eq!(
            whole_entry(document.clone(), read).expect("should pass it through"),
            document
        );
    }

    #[test]
    fn a_chunked_document_is_read_back_whole() {
        let document = r#"{"data":{"publisher":{"domain":"example.com"}},"sha256":"x"}"#;
        let (first, second) = document.split_at(document.len() / 2);
        let first_key = format!("{KEY}.__edgezero_chunks.test.0");
        let second_key = format!("{KEY}.__edgezero_chunks.test.1");
        let pointer = pointer_for(document, &[(&first_key, first), (&second_key, second)]);
        let read = reader(BTreeMap::from([
            (first_key, first.to_owned()),
            (second_key, second.to_owned()),
        ]));

        assert_eq!(
            whole_entry(pointer, read).expect("should reassemble the document"),
            document
        );
    }

    #[test]
    fn a_pointer_whose_lengths_disagree_with_the_document_is_refused() {
        let chunk_key = format!("{KEY}.__edgezero_chunks.test.0");
        let pointer = json!({
            "edgezero_kind": CHUNK_POINTER_KIND,
            "version": 1,
            "envelope_sha256": sha256_hex(b"ab"),
            "envelope_len": 1,
            "chunks": [{"key": chunk_key, "sha256": sha256_hex(b"ab"), "len": 2}],
        })
        .to_string();

        let error =
            whole_entry(pointer, reader(BTreeMap::new())).expect_err("should refuse the pointer");
        assert!(
            format!("{error:?}").contains("chunk lengths total mismatch"),
            "{error:?}"
        );
    }

    #[test]
    fn a_chunk_whose_hash_disagrees_is_refused() {
        let document = "abcd";
        let chunk_key = format!("{KEY}.__edgezero_chunks.test.0");
        let pointer = pointer_for(document, &[(&chunk_key, document)]);
        let read = reader(BTreeMap::from([(chunk_key, "abce".to_owned())]));

        let error = whole_entry(pointer, read).expect_err("should refuse the chunk");
        assert!(format!("{error:?}").contains("sha mismatch"), "{error:?}");
    }

    #[test]
    fn another_kind_of_json_at_the_key_is_returned_as_it_is() {
        let value = r#"{"edgezero_kind":"something_else","version":1,"chunks":[],"envelope_len":0,"envelope_sha256":""}"#.to_owned();
        assert_eq!(
            whole_entry(value.clone(), reader(BTreeMap::new())).expect("should pass it through"),
            value
        );
    }

    #[test]
    fn a_written_document_is_read_back_whole() {
        // What `ts config push --local --key a.publisher.example` wrote for a
        // document too long for one entry, in the order it wrote them.
        let written: Vec<(String, String)> =
            serde_json::from_str(include_str!("fixtures/config-push-chunks.json"))
                .expect("should parse the fixture");
        assert!(written.len() > 2, "the fixture should hold chunks");
        let (key, pointer) = written.last().cloned().expect("should hold the pointer");
        assert_eq!(
            key, "a.publisher.example",
            "the pointer should be written last"
        );

        let document = whole_entry(pointer, reader(written.into_iter().collect()))
            .expect("should read the written document back whole");

        let envelope: serde_json::Value =
            serde_json::from_str(&document).expect("should read back the envelope");
        assert_eq!(
            envelope["data"]["publisher"]["domain"], "a.publisher.example",
            "should reassemble the pushed app config"
        );
    }
}
