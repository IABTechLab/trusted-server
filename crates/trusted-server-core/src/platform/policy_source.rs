//! Where a domain's permission policy file is read from.
//!
//! On most adapters the push writes the file into the domain's blob, so the
//! runtime loader takes it from there. An adapter that keeps policy files
//! somewhere of its own reads them through a [`PlatformPolicySource`].
//!
//! A blob for one host carries no file. Each file is stored once, in a shared
//! entry under [`policy_key`], however many hosts' blobs pin it, as an
//! envelope whose data is a [`PolicyEntry`].

use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use serde::{Deserialize, Serialize};

use super::PlatformError;
use crate::permissions::PolicyReference;

/// What the key of a shared policy entry starts with, before the file's
/// SHA-256.
///
/// No host name holds an underscore, so no `Host` header names a policy entry
/// under a selector that is a host alone.
pub const POLICY_KEY_PREFIX: &str = "permissions_";

/// Reads the permission policy file a domain's settings name.
pub trait PlatformPolicySource: Send + Sync {
    /// The text of the file `reference` names.
    ///
    /// The runtime loader checks the text against the digest the reference
    /// pins, so a source returns the file as it finds it.
    ///
    /// # Errors
    ///
    /// [`PlatformError::PolicySource`] when the file cannot be read.
    fn policy_text(&self, reference: &PolicyReference) -> Result<String, Report<PlatformError>>;
}

/// Where the runtime loader takes a domain's policy file from.
#[derive(Clone, Copy)]
pub enum PolicyOrigin<'a> {
    /// The domain's blob, as `[permissions] text`.
    Blob,
    /// A source of the adapter's own. Any text the blob carries is not used.
    Source(&'a dyn PlatformPolicySource),
}

/// The config store key of the shared entry holding the policy file whose
/// SHA-256 is `sha256`.
#[must_use]
pub fn policy_key(sha256: &str) -> String {
    format!("{POLICY_KEY_PREFIX}{sha256}")
}

/// The data of a shared policy entry's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyEntry {
    /// The file's stem.
    pub name: String,
    /// The file itself.
    pub text: String,
}

/// The policy file a shared entry holds, from the entry's value as the store
/// returns it once any chunks are put back together.
///
/// The runtime loader checks the file against the digest the host's blob
/// pins.
///
/// # Errors
///
/// [`PlatformError::PolicySource`] when the value is not an envelope, fails
/// the envelope's check of its own data, or does not hold a policy entry.
pub fn policy_entry_text(value: &str) -> Result<String, Report<PlatformError>> {
    let refuse = |reason: String| Report::new(PlatformError::PolicySource).attach(reason);
    let envelope: BlobEnvelope = serde_json::from_str(value)
        .map_err(|error| refuse(format!("the policy entry is not an envelope: {error}")))?;
    envelope.verify().map_err(|error| {
        refuse(format!(
            "the policy entry fails its envelope's check: {error}"
        ))
    })?;
    let entry: PolicyEntry = serde_json::from_value(envelope.into_data())
        .map_err(|error| refuse(format!("the envelope holds no policy entry: {error}")))?;
    Ok(entry.text)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::permissions::{SAMPLE_POLICY_NAME, SAMPLE_POLICY_TEXT, policy_sha256};

    fn envelope_of(data: serde_json::Value) -> String {
        serde_json::to_string(&BlobEnvelope::new(data, "2026-01-01T00:00:00Z".to_owned()))
            .expect("should serialize the envelope")
    }

    fn sample_entry() -> String {
        envelope_of(
            serde_json::to_value(PolicyEntry {
                name: SAMPLE_POLICY_NAME.to_owned(),
                text: SAMPLE_POLICY_TEXT.to_owned(),
            })
            .expect("should serialize the entry"),
        )
    }

    #[test]
    fn a_policy_key_is_the_prefix_then_the_file_s_digest() {
        let sha256 = policy_sha256(SAMPLE_POLICY_TEXT);

        assert_eq!(policy_key(&sha256), format!("permissions_{sha256}"));
    }

    #[test]
    fn an_entry_gives_back_the_file_it_holds() {
        let text = policy_entry_text(&sample_entry()).expect("should read the entry");

        assert_eq!(text, SAMPLE_POLICY_TEXT);
    }

    #[test]
    fn a_value_that_is_not_a_checked_policy_entry_is_refused() {
        let mut changed: serde_json::Value =
            serde_json::from_str(&sample_entry()).expect("should parse the entry");
        changed["data"]["text"] = json!("groups: {}\n");

        for (value, reason) in [
            ("not an envelope".to_owned(), "is not an envelope"),
            (changed.to_string(), "fails its envelope's check"),
            (
                envelope_of(json!({ "publisher": { "domain": "example.com" } })),
                "holds no policy entry",
            ),
        ] {
            let error = policy_entry_text(&value).expect_err("the value should be refused");

            assert!(
                matches!(error.current_context(), PlatformError::PolicySource),
                "{error:?}"
            );
            assert!(format!("{error:?}").contains(reason), "{error:?}");
        }
    }
}
