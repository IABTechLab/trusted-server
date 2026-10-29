//! Secret redaction for diagnostics.

/// Replacement text for secret values in diagnostics.
pub(super) const REDACTED: &[u8] = b"[REDACTED]";

/// Replaces the union of every secret occurrence with one marker per span.
///
/// Matches are found on the original bytes and merged before replacement, so
/// overlapping or adjacent secrets cannot leave fragments behind.
pub(super) fn redact(bytes: &[u8], secrets: &[Vec<u8>]) -> Vec<u8> {
    let mut ranges: Vec<(usize, usize)> = secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .flat_map(|secret| {
            bytes
                .windows(secret.len())
                .enumerate()
                .filter(move |(_, window)| window == secret)
                .map(move |(start, _)| (start, start + secret.len()))
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    for (start, end) in merged {
        output.extend_from_slice(bytes.get(cursor..start).unwrap_or_default());
        output.extend_from_slice(REDACTED);
        cursor = end;
    }
    output.extend_from_slice(bytes.get(cursor..).unwrap_or_default());
    output
}

/// Replaces a trailing proper prefix of any secret, as left by a byte cap
/// that cut the stream inside the secret.
pub(super) fn redact_cut_tail(mut bytes: Vec<u8>, secrets: &[Vec<u8>]) -> Vec<u8> {
    let cut = secrets
        .iter()
        .flat_map(|secret| {
            (1..secret.len()).filter(|&length| {
                secret
                    .get(..length)
                    .is_some_and(|prefix| bytes.ends_with(prefix))
            })
        })
        .max();
    if let Some(length) = cut {
        bytes.truncate(bytes.len() - length);
        bytes.extend_from_slice(REDACTED);
    }
    bytes
}

pub(super) fn redacted_text(bytes: &[u8], secrets: &[Vec<u8>]) -> String {
    String::from_utf8_lossy(&redact(bytes, secrets)).into_owned()
}
