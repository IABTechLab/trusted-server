//! The hashing both of the module's keys use.

use sha2::Digest as _;

/// Writes `part` into `hasher` with its length before it, so no two lists of
/// parts run together into the same digest.
pub(crate) fn hash_length_prefixed(hasher: &mut sha2::Sha256, part: &[u8]) {
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}
