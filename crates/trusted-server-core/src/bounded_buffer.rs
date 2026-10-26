//! Shared admission and allocation policy for retained byte buffers.

/// Reserve room for an append within `limit`, using capped geometric growth.
///
/// Returns `None` without modifying the buffer when length arithmetic overflows
/// or the resulting length exceeds `limit`. The caller owns appending bytes and
/// handling rejection.
pub(crate) fn reserve_bounded(buffer: &mut Vec<u8>, additional: usize, limit: usize) -> Option<()> {
    let next = buffer
        .len()
        .checked_add(additional)
        .filter(|size| *size <= limit)?;
    if next > buffer.capacity() {
        let capacity = buffer.capacity().saturating_mul(2).max(next).min(limit);
        buffer.reserve_exact(capacity - buffer.len());
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_reservations_preserve_the_buffer() {
        let mut buffer = b"page".to_vec();
        let capacity = buffer.capacity();

        assert!(
            reserve_bounded(&mut buffer, 1, 4).is_none(),
            "should reject appends beyond the cap"
        );
        assert!(
            reserve_bounded(&mut buffer, usize::MAX, usize::MAX).is_none(),
            "should reject overflowing length arithmetic"
        );
        assert_eq!(buffer, b"page", "should preserve accepted bytes");
        assert_eq!(
            buffer.capacity(),
            capacity,
            "should reject before allocating"
        );
    }

    #[test]
    fn empty_reservation_with_zero_limit_does_not_allocate() {
        let mut buffer = Vec::new();

        reserve_bounded(&mut buffer, 0, 0).expect("should accept an empty reservation");

        assert_eq!(buffer.capacity(), 0, "should allocate lazily");
        assert!(
            reserve_bounded(&mut buffer, 1, 0).is_none(),
            "should reject bytes when the limit is zero"
        );
    }
}
