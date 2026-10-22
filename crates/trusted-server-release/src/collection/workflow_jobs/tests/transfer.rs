//! Adapter output fed through the existing exact-ID transfer validation.

use super::*;
use crate::collection::records::RoleProfile;
use crate::collection::{
    ArtifactSource, Download, Invocation, TrustedDependencies, digest, validate_transfers,
};

struct RecordingSource {
    archive: Vec<u8>,
    requested: Vec<u64>,
}

impl ArtifactSource for RecordingSource {
    fn metadata(&mut self, _repository: &str, id: u64) -> Result<Vec<u8>> {
        self.requested.push(id);
        Ok(serde_json::to_vec(&json!({
            "id": id,
            "name": format!("upload-{id}"),
            "expired": false,
            "digest": format!("sha256:{}", digest(&self.archive)),
            "workflow_run": {
                "id": RUN_ID,
                "head_sha": head_sha(),
                "repository_id": 17,
                "head_repository_id": 17,
            },
        }))
        .expect("should serialize fictional metadata"))
    }

    fn download(&mut self, _repository: &str, id: u64) -> Result<Download> {
        self.requested.push(id);
        Ok(Download {
            archive: self.archive.clone(),
            transfer_warning: false,
        })
    }
}

#[test]
fn adapted_jobs_feed_exact_uploads_and_absent_reproductions_into_transfer_validation() {
    let (never_started, mandatory_uploads) = with_native_reproductions("queued", &Value::Null, &[]);
    let (failed, _) = with_native_reproductions("completed", &json!("failure"), &executed_steps());
    // The failed native reproductions also report upload outputs, which the
    // adapter must drop rather than fetch.
    for (label, jobs, uploads, elapsed) in [
        (
            "never started",
            never_started,
            mandatory_uploads.clone(),
            DEADLINE,
        ),
        ("failed with upload output", failed, all_uploads(), 600),
    ] {
        let adapted = adapt_jobs(&topology(), &response(&jobs), &uploads, elapsed)
            .unwrap_or_else(|error| panic!("should adapt native reproductions {label}: {error:?}"));
        let dependencies = TrustedDependencies {
            invocation: Invocation {
                repository: "example/project".to_owned(),
                repository_id: 17,
                run_id: RUN_ID,
                source_commit: head_sha(),
                inventory_sha256: "b".repeat(64),
                profiles: RoleProfile::fixtures(),
            },
            required: adapted.required,
            optional: adapted.optional,
        };
        let mut source = RecordingSource {
            archive: archive(),
            requested: Vec::new(),
        };

        let validated = validate_transfers(&dependencies, &mut source)
            .expect("should validate adapted exact-ID transfers");

        assert_eq!(
            validated.transfers.len(),
            5,
            "should fetch every mandatory upload when native reproductions {label}"
        );
        assert_eq!(
            validated.absent.len(),
            3,
            "should keep absent native reproductions when {label}"
        );
        let mut fetched = source.requested.clone();
        fetched.dedup();
        let mandatory = mandatory_uploads
            .iter()
            .map(|upload| upload.output.artifact_id);
        assert_eq!(
            fetched,
            mandatory.collect::<Vec<_>>(),
            "should fetch only mandatory upload identities when native reproductions {label}"
        );
    }
}
