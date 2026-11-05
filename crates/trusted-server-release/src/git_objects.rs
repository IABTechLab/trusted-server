//! Collision-checked Git objects under fixed source and Cargo material budgets.

use std::cell::Cell;
use std::io::{self, BufRead, Cursor, Read as _};

use error_stack::Report;

use crate::error::ReleaseError;

fn object_error(detail: impl std::fmt::Display) -> Report<ReleaseError> {
    Report::new(ReleaseError::GitObject).attach(detail.to_string())
}

/// A shared byte budget across every request in one object collection.
///
/// These checks authenticate bytes, not a Git executable or installed checkout.
pub(crate) struct ObjectReader {
    object_limit: u64,
    remaining: Cell<u64>,
}

impl ObjectReader {
    /// Creates the source-checkout policy: 64 MiB per object, 2 GiB total.
    pub(crate) fn source() -> Self {
        Self {
            object_limit: 64 * 1024 * 1024,
            remaining: Cell::new(2 * 1024 * 1024 * 1024),
        }
    }

    /// Creates one Cargo collection policy: 32 MiB per object, 512 MiB total.
    pub(crate) fn cargo_materials() -> Self {
        Self {
            object_limit: 32 * 1024 * 1024,
            remaining: Cell::new(512 * 1024 * 1024),
        }
    }

    #[cfg(test)]
    /// Lowers a test's budget without allocating production-sized bodies.
    pub(crate) fn set_budget_for_tests(&self, bytes: u64) {
        self.remaining.set(bytes);
    }

    /// Preflights sizes, reads bounded batches and delivers only rehashed bytes.
    ///
    /// The caller's executor must enforce each supplied output bound and its
    /// shared deadline/cancellation policy. This reader does not authenticate
    /// the executable or supervise processes itself.
    ///
    /// # Errors
    ///
    /// Returns an error for failed commands, malformed or inconsistent replies,
    /// exceeded byte budgets, identity mismatches or detected SHA-1 collisions.
    pub(crate) fn read_objects(
        &self,
        kind: &'static str,
        oids: &[&str],
        execute: &mut impl FnMut(&[&str], Vec<u8>, u64) -> Result<Vec<u8>, Report<ReleaseError>>,
        mut visit: impl FnMut(usize, &[u8]) -> Result<(), Report<ReleaseError>>,
    ) -> Result<(), Report<ReleaseError>> {
        let sizes = self.object_sizes(kind, oids, execute)?;
        let mut start = 0;
        while start < oids.len() {
            let mut end = start;
            let mut limit = 0;
            while end < oids.len() && limit + sizes[end] + 128 <= self.object_limit + 128 {
                limit += sizes[end] + 128;
                end += 1;
            }
            let output = execute(&["cat-file", "--batch"], request(&oids[start..end]), limit)?;
            let mut reader = Cursor::new(output.as_slice());
            read_batch(
                &mut reader,
                &Batch {
                    kind,
                    object_limit: self.object_limit,
                    oids: &oids[start..end],
                    sizes: &sizes[start..end],
                    budget: &self.remaining,
                },
                &mut |index, bytes| visit(start + index, bytes),
            )?;
            if reader.position() != output.len() as u64 {
                return Err(object_error("unexpected trailing Git batch output"));
            }
            start = end;
        }
        Ok(())
    }

    fn object_sizes(
        &self,
        kind: &str,
        oids: &[&str],
        execute: &mut impl FnMut(&[&str], Vec<u8>, u64) -> Result<Vec<u8>, Report<ReleaseError>>,
    ) -> Result<Vec<u64>, Report<ReleaseError>> {
        let limit = (oids.len() as u64)
            .checked_mul(128)
            .ok_or_else(|| object_error("object header budget overflow"))?;
        let output = execute(&["cat-file", "--batch-check"], request(oids), limit)?;
        let mut reader = Cursor::new(output.as_slice());
        let mut total = 0_u64;
        let mut sizes = Vec::with_capacity(oids.len());
        for oid in oids {
            let size = read_header(&mut reader, kind, oid)?;
            total = total
                .checked_add(size)
                .ok_or_else(|| object_error("object byte budget overflow"))?;
            if size > self.object_limit || total > self.remaining.get() {
                return Err(object_error(
                    "object exceeds the per-object or aggregate object byte budget",
                ));
            }
            sizes.push(size);
        }
        if reader.position() != output.len() as u64 {
            return Err(object_error("unexpected trailing Git object metadata"));
        }
        Ok(sizes)
    }
}

fn request(oids: &[&str]) -> Vec<u8> {
    oids.iter()
        .flat_map(|oid| oid.bytes().chain(std::iter::once(b'\n')))
        .collect()
}

/// Collision-checked Git SHA-1 object id of `bytes` stored as an object of `kind`.
fn object_id(kind: &str, bytes: &[u8]) -> Result<String, Report<ReleaseError>> {
    let header = format!("{kind} {}\0", bytes.len());
    checked_sha1(&[header.as_bytes(), bytes])
}

/// One `cat-file --batch` request and the shared aggregate byte budget.
struct Batch<'a> {
    kind: &'a str,
    object_limit: u64,
    oids: &'a [&'a str],
    sizes: &'a [u64],
    budget: &'a Cell<u64>,
}

fn read_batch(
    reader: &mut impl BufRead,
    batch: &Batch<'_>,
    visit: &mut impl FnMut(usize, &[u8]) -> Result<(), Report<ReleaseError>>,
) -> Result<(), Report<ReleaseError>> {
    let io_error = |error: io::Error| object_error(format!("cannot read batch output: {error}"));
    let kind = batch.kind;
    if batch.sizes.len() != batch.oids.len() {
        return Err(object_error(
            "Git preflight size count differs from requested object count",
        ));
    }
    for (index, oid) in batch.oids.iter().enumerate() {
        let size = read_header(reader, kind, oid)?;
        if size != batch.sizes[index] {
            return Err(object_error(
                "Git object body size differs from preflight metadata",
            ));
        }
        let remaining = batch.budget.get();
        if size > batch.object_limit || size > remaining {
            return Err(object_error(format!(
                "object {oid} exceeds the per-object or aggregate object byte budget"
            )));
        }
        batch.budget.set(remaining - size);
        let length = usize::try_from(size).map_err(|_| object_error("object size overflows"))?;
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).map_err(io_error)?;
        let mut terminator = [0];
        reader.read_exact(&mut terminator).map_err(io_error)?;
        if terminator != *b"\n" {
            return Err(object_error(format!("batch output for {oid} is malformed")));
        }
        if object_id(kind, &bytes)? != *oid {
            return Err(object_error(format!(
                "object identity mismatch: {kind} {oid} content does not hash to its id"
            )));
        }
        visit(index, &bytes)?;
    }
    Ok(())
}

/// Reads one `<oid> <kind> <size>` header and returns the object size.
fn read_header(
    reader: &mut impl BufRead,
    kind: &str,
    oid: &str,
) -> Result<u64, Report<ReleaseError>> {
    let mut header = Vec::new();
    reader
        .take(128)
        .read_until(b'\n', &mut header)
        .map_err(|error| object_error(format!("cannot read batch header: {error}")))?;
    let header =
        std::str::from_utf8(&header).map_err(|_| object_error("batch header is not UTF-8"))?;
    let fields: Vec<&str> = header.trim_end_matches('\n').split(' ').collect();
    match fields.as_slice() {
        [returned, returned_kind, size] if *returned == oid && *returned_kind == kind => size
            .parse::<u64>()
            .map_err(|_| object_error("batch object size is malformed")),
        _ => Err(object_error(format!(
            "object {oid} is not a readable {kind}"
        ))),
    }
}

/// Computes a Git identity with SHA-1 collision detection.
///
/// # Errors
///
/// Returns an error when the collision detector rejects the supplied bytes.
pub(crate) fn checked_sha1(parts: &[&[u8]]) -> Result<String, Report<ReleaseError>> {
    let mut hasher = sha1_checked::Sha1::new();
    for part in parts {
        sha1_checked::Digest::update(&mut hasher, part);
    }
    let result = hasher.try_finalize();
    if result.has_collision() {
        return Err(object_error(
            "SHA-1 collision detection flags a Git object; its identity is not trusted",
        ));
    }
    Ok(hex::encode(result.hash()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn shared_reader_preserves_collision_checked_object_delivery() {
        let bytes = b"fictional object";
        let mut hash = sha1_checked::Sha1::new();
        sha1_checked::Digest::update(&mut hash, format!("blob {}\0", bytes.len()).as_bytes());
        sha1_checked::Digest::update(&mut hash, bytes);
        let oid = hex::encode(hash.try_finalize().hash());
        let mut delivered = false;
        ObjectReader::source()
            .read_objects(
                "blob",
                &[&oid],
                &mut |arguments, _, _| {
                    let mut output = format!("{oid} blob {}\n", bytes.len()).into_bytes();
                    if arguments.contains(&"--batch") {
                        output.extend_from_slice(bytes);
                        output.push(b'\n');
                    }
                    Ok(output)
                },
                |_, body| {
                    assert_eq!(body, bytes, "should deliver exact verified bytes");
                    delivered = true;
                    Ok(())
                },
            )
            .expect("should verify a complete object through shared reader");
        assert!(delivered, "should deliver verified object");
    }

    #[test]
    fn cargo_materials_reject_over_32_mib_before_requesting_body() {
        let oid = "a".repeat(40);
        for (reader, expects_body) in [
            (ObjectReader::cargo_materials(), false),
            (ObjectReader::source(), true),
        ] {
            let body_requested = Cell::new(false);
            let result = reader.read_objects(
                "blob",
                &[&oid],
                &mut |arguments, _, _| {
                    if arguments.contains(&"--batch-check") {
                        Ok(format!("{oid} blob {}\n", 32 * 1024 * 1024 + 1).into_bytes())
                    } else {
                        body_requested.set(true);
                        Err(Report::new(ReleaseError::Source)
                            .attach("fixture stops before allocating body"))
                    }
                },
                |_, _| panic!("should never deliver fixture object"),
            );
            assert!(
                result.is_err(),
                "should reject fixture without allocating large body"
            );
            assert_eq!(
                body_requested.get(),
                expects_body,
                "should preserve distinct source and Cargo per-object policies"
            );
        }
    }

    #[test]
    fn cargo_collection_rejects_over_512_mib_before_any_body_request() {
        let oid = "a".repeat(40);
        let oids = vec![oid.as_str(); 17];
        for (reader, expects_body) in [
            (ObjectReader::cargo_materials(), false),
            (ObjectReader::source(), true),
        ] {
            let mut body_requested = false;
            let result = reader.read_objects(
                "blob",
                &oids,
                &mut |arguments, _, _| {
                    if arguments.contains(&"--batch-check") {
                        Ok(format!("{oid} blob {}\n", 32 * 1024 * 1024)
                            .repeat(oids.len())
                            .into_bytes())
                    } else {
                        body_requested = true;
                        Err(object_error("fixture stops before body allocation"))
                    }
                },
                |_, _| panic!("should never deliver large fixture object"),
            );
            assert!(
                result.is_err(),
                "should reject fixture without materializing bodies"
            );
            assert_eq!(
                body_requested, expects_body,
                "should preserve distinct aggregate source and Cargo limits"
            );
        }
    }

    #[test]
    fn body_size_disagreement_rejects_before_budget_or_delivery() {
        let bytes = b"fictional object";
        let oid = object_id("blob", bytes).expect("should hash fixture blob");
        let mut raw = format!("{oid} blob {}\n", bytes.len()).into_bytes();
        raw.extend_from_slice(bytes);
        raw.push(b'\n');
        let budget = Cell::new(1024);
        let batch = Batch {
            kind: "blob",
            object_limit: 64 * 1024 * 1024,
            oids: &[&oid],
            sizes: &[1],
            budget: &budget,
        };
        let mut delivered = false;
        assert!(
            read_batch(&mut Cursor::new(raw), &batch, &mut |_, _| {
                delivered = true;
                Ok(())
            })
            .is_err(),
            "should reject body size inconsistent with preflight"
        );
        assert!(!delivered, "should reject before delivering object bytes");
        assert_eq!(
            budget.get(),
            1024,
            "should preserve budget before metadata joins"
        );
    }

    #[test]
    fn cargo_budget_accumulates_across_separate_object_requests() {
        let bytes = b"fictional object";
        let oid = object_id("blob", bytes).expect("should hash fixture object");
        let reader = ObjectReader::cargo_materials();
        reader.set_budget_for_tests(bytes.len() as u64);
        let mut execute = |arguments: &[&str], _: Vec<u8>, _: u64| {
            let mut output = format!("{oid} blob {}\n", bytes.len()).into_bytes();
            if arguments.contains(&"--batch") {
                output.extend_from_slice(bytes);
                output.push(b'\n');
            }
            Ok(output)
        };
        reader
            .read_objects("blob", &[&oid], &mut execute, |_, _| Ok(()))
            .expect("should consume entire shared budget");
        assert!(
            reader
                .read_objects("blob", &[&oid], &mut execute, |_, _| panic!(
                    "should not deliver after exhaustion"
                ))
                .is_err(),
            "should preserve aggregate budget across requests"
        );
    }
}
