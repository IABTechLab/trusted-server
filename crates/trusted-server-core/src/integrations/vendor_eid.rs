//! Permissive parsing of complete EIDs that identity vendors return.
//!
//! [`crate::openrtb::Eid`] denies unknown fields, so it suits Trusted
//! Server's own output, not a vendor's input: one new vendor field would fail
//! the whole response.

use serde::Deserialize;

use crate::ec::identity::{ResolvedEid, ResolvedUid, SourceDomain};

/// One EID as a vendor returns it.
#[derive(Debug, Deserialize)]
pub(crate) struct VendorEid {
    pub source: String,
    #[serde(default)]
    pub uids: Vec<VendorUid>,
    pub inserter: Option<String>,
    pub matcher: Option<String>,
    pub mm: Option<i32>,
    pub ext: Option<serde_json::Value>,
}

/// One UID of a [`VendorEid`].
#[derive(Debug, Deserialize)]
pub(crate) struct VendorUid {
    pub id: String,
    pub atype: Option<i32>,
    pub ext: Option<serde_json::Value>,
}

impl VendorEid {
    /// The EID under `source`, with every UID and EID-level field the vendor
    /// sent. `None` when it holds no UID with an ID.
    pub(crate) fn to_resolved(&self, source: &SourceDomain) -> Option<ResolvedEid> {
        let mut uids = self
            .uids
            .iter()
            .filter(|uid| !uid.id.is_empty())
            .map(VendorUid::to_resolved);
        let mut eid = ResolvedEid::new(source, uids.next()?);
        for uid in uids {
            eid = eid.with_uid(uid);
        }
        if let Some(inserter) = &self.inserter {
            eid = eid.with_inserter(inserter);
        }
        if let Some(matcher) = &self.matcher {
            eid = eid.with_matcher(matcher);
        }
        if let Some(mm) = self.mm {
            eid = eid.with_mm(mm);
        }
        if let Some(ext) = &self.ext {
            eid = eid.with_ext(ext.clone());
        }
        Some(eid)
    }
}

impl VendorUid {
    fn to_resolved(&self) -> ResolvedUid {
        let mut uid = ResolvedUid::new(self.id.clone());
        if let Some(atype) = self.atype {
            uid = uid.with_atype(atype);
        }
        if let Some(ext) = &self.ext {
            uid = uid.with_ext(ext.clone());
        }
        uid
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn keeps_every_uid_and_eid_field() {
        let eid: VendorEid = serde_json::from_value(json!({
            "source": "ids.example.com",
            "uids": [
                { "id": "first", "atype": 1, "ext": { "linkType": 2 } },
                { "id": "" },
                { "id": "second", "future": true }
            ],
            "inserter": "inserter.example.com",
            "matcher": "matcher.example.com",
            "mm": 2,
            "ext": { "segments": ["s1"] },
            "future": "ignored"
        }))
        .expect("should parse a vendor EID with unknown fields");
        let source: SourceDomain = "ids.example.com".parse().expect("should parse the source");

        let resolved = eid.to_resolved(&source).expect("should resolve");

        let ids: Vec<&str> = resolved.uids().iter().map(ResolvedUid::id).collect();
        assert_eq!(
            ids,
            ["first", "second"],
            "should keep non-empty UIDs in order"
        );
        assert_eq!(resolved.uids()[0].atype(), Some(1), "should keep the atype");
        assert_eq!(
            resolved.inserter(),
            Some("inserter.example.com"),
            "should keep inserter"
        );
        assert_eq!(
            resolved.matcher(),
            Some("matcher.example.com"),
            "should keep matcher"
        );
        assert_eq!(resolved.mm(), Some(2), "should keep mm");
        assert_eq!(
            resolved.ext(),
            Some(&json!({ "segments": ["s1"] })),
            "should keep the EID ext"
        );
    }

    #[test]
    fn an_eid_without_ids_resolves_to_none() {
        let eid: VendorEid =
            serde_json::from_value(json!({ "source": "ids.example.com", "uids": [{ "id": "" }] }))
                .expect("should parse");
        let source: SourceDomain = "ids.example.com".parse().expect("should parse the source");

        assert!(
            eid.to_resolved(&source).is_none(),
            "should skip an EID with no ID"
        );
    }
}
