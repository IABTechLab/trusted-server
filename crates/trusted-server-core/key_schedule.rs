//! The attestation key schedule a build is given, read into the constant
//! the build script writes.
//!
//! A module of the build script and of the tests in `src/attestation.rs`,
//! which check what a schedule may hold. Both also hold `signing_key.rs` as
//! `signing_key`, which reads each key the way the signer does.

/// The source of the `ATTESTATION_KEYS` constant for the text of a key
/// schedule, or of an empty schedule for `None`.
///
/// A schedule holds one key on each line, being the key id, the Unix second
/// the key comes into force and the private key as base64 DER, separated by
/// tabs. Blank lines are passed over.
///
/// A key id and a key may hold only letters, digits and `-_.:+/=`. Each is
/// written into Rust source as a string, so nothing a schedule holds can end
/// the string it is written in. No two lines share a key id or a start, so
/// one key is in force at any second and a key id names one key. Each key
/// must be one the signer can read, however late it starts.
///
/// # Errors
///
/// Returns the number of the first line of any other shape, of the first
/// that repeats an earlier line's key id or start, or of the first whose key
/// cannot sign, and what is wrong with it. The key itself is never repeated.
pub(crate) fn constant_source(schedule: Option<&str>) -> Result<String, String> {
    let mut source = String::from("const ATTESTATION_KEYS: &[(&str, i64, &str)] = &[");
    let mut seen: Vec<(usize, &str, i64)> = Vec::new();
    for (index, line) in schedule.unwrap_or_default().lines().enumerate() {
        let line = line.trim_matches([' ', '\r']);
        if line.is_empty() {
            continue;
        }
        let number = index + 1;
        let fields: Vec<&str> = line.split('\t').collect();
        let [key_id, starts_at, private_key] = fields.as_slice() else {
            return Err(format!(
                "line {number} should hold a key id, a start and a key, separated by tabs"
            ));
        };
        let starts_at: i64 = starts_at
            .parse()
            .map_err(|_| format!("line {number} should give the key's start as Unix seconds"))?;
        if !is_plain(key_id) || !is_plain(private_key) {
            return Err(format!(
                "line {number} should hold a key id and a key of letters, digits and -_.:+/= only"
            ));
        }
        if let Some((earlier, _, _)) = seen.iter().find(|(_, id, _)| id == key_id) {
            return Err(format!(
                "line {number} should not repeat the key id of line {earlier}"
            ));
        }
        if let Some((earlier, _, _)) = seen.iter().find(|(_, _, start)| *start == starts_at) {
            return Err(format!(
                "line {number} should not start at the same second as line {earlier}"
            ));
        }
        if let Err(reason) = super::signing_key::read(private_key) {
            return Err(format!(
                "line {number} holds key `{key_id}`, which cannot sign, as {reason}"
            ));
        }
        seen.push((number, *key_id, starts_at));
        source.push_str(&format!("({key_id:?}, {starts_at}, {private_key:?}),"));
    }
    source.push_str("];");
    Ok(source)
}

/// Whether a schedule field holds only characters a key id or base64 uses.
fn is_plain(field: &str) -> bool {
    !field.is_empty()
        && field
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:+/=".contains(&b))
}
