use serde_json::{Value, json};

use super::aws::{Aws, verify_identity};
use super::config::{Deployment, invalid};
use super::{Output, PbsError, Result};

/// Read only the explicitly declared EC2 instances. Never infer PBS health from EC2 health.
///
/// # Errors
/// Rejects missing instance targets, wrong accounts, and failed identity verification.
/// Identity failures abort the entire report. Resource-query failures are included as unknown in a
/// partial report with a failing exit status.
pub(super) fn status(deployment: &Deployment, aws: &dyn Aws) -> Result<Output> {
    if deployment
        .regions
        .values()
        .any(|region| region.instance_ids.is_empty())
    {
        return Err(invalid(
            "status requires explicit instance_ids in every selected descriptor region",
        ));
    }
    let mut regions = Vec::new();
    let mut details = Vec::new();
    let mut failed = false;
    for (region, target) in &deployment.regions {
        verify_identity(deployment, region, aws)?;
        let response = aws.call(
            &deployment.aws,
            region,
            "ec2",
            "describe-instance-status",
            &json!({"InstanceIds": target.instance_ids, "IncludeAllInstances": true}),
        );
        let statuses = response
            .as_ref()
            .ok()
            .and_then(|value| value.get("InstanceStatuses"))
            .and_then(Value::as_array);
        let mut instances = Vec::new();
        for id in &target.instance_ids {
            let matching: Vec<_> = statuses
                .into_iter()
                .flatten()
                .filter(|value| {
                    value.get("InstanceId").and_then(Value::as_str) == Some(id.as_str())
                })
                .collect();
            let state = if matching.len() == 1 {
                Some(matching[0])
            } else {
                None
            };
            let instance_state = allowed(
                state,
                "/InstanceState/Name",
                &[
                    "pending",
                    "running",
                    "shutting-down",
                    "terminated",
                    "stopping",
                    "stopped",
                ],
            );
            let instance_health = allowed(
                state,
                "/InstanceStatus/Status",
                &[
                    "ok",
                    "impaired",
                    "initializing",
                    "insufficient-data",
                    "not-applicable",
                ],
            );
            let system_health = allowed(
                state,
                "/SystemStatus/Status",
                &[
                    "ok",
                    "impaired",
                    "initializing",
                    "insufficient-data",
                    "not-applicable",
                ],
            );
            failed |= state.is_none()
                || instance_state == "unknown"
                || instance_health == "unknown"
                || system_health == "unknown";
            details.push(format!("{region} {id}: state={instance_state}, EC2 instance={instance_health}, system={system_health}; PBS health/release unknown"));
            instances.push(json!({"instance_id": id, "state": instance_state, "instance_health": instance_health,
                "system_health": system_health, "pbs_health": "unknown", "release_id": null, "secret_versions": null}));
        }
        regions.push(json!({"region": region, "query": if statuses.is_some() { "completed" } else { "failed_or_invalid" }, "instances": instances}));
    }
    Ok(Output {
        summary: format!(
            "EC2 infrastructure status for {}; PBS runtime status is not implemented",
            deployment.environment
        ),
        details,
        data: json!({"environment": deployment.environment, "account_id": deployment.aws.account_id,
            "complete": !failed, "regions": regions}),
        failure: failed.then_some(PbsError::Aws("status report is incomplete")),
    })
}

fn allowed(record: Option<&Value>, pointer: &str, choices: &[&'static str]) -> &'static str {
    let value = record
        .and_then(|record| record.pointer(pointer))
        .and_then(Value::as_str);
    choices
        .iter()
        .copied()
        .find(|choice| Some(*choice) == value)
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pbs::aws::tests::FakeAws;
    use crate::commands::pbs::config::tests::fixture;

    #[test]
    fn queries_only_declared_instances_and_does_not_claim_pbs_health() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        let aws = FakeAws::new(vec![
            json!({"Account": "123456789012"}),
            json!({"InstanceStatuses": [{
                "InstanceId": "i-0123456789abcdef0", "InstanceState": {"Name": "running"},
                "InstanceStatus": {"Status": "ok"}, "SystemStatus": {"Status": "ok"}
            }]}),
        ]);
        let output = status(&deployment, &aws).expect("should read status");
        assert!(output.failure.is_none());
        assert_eq!(
            output.data["regions"][0]["instances"][0]["pbs_health"],
            "unknown"
        );
        assert!(output.data["regions"][0]["instances"][0]["release_id"].is_null());
        assert_eq!(
            aws.calls.borrow()[1].2["InstanceIds"],
            json!(["i-0123456789abcdef0"])
        );
    }

    #[test]
    fn absent_and_failed_queries_produce_incomplete_unknown_reports() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        for replies in [
            vec![json!({"Account": "123456789012"})],
            vec![
                json!({"Account": "123456789012"}),
                json!({"InstanceStatuses": []}),
            ],
        ] {
            let aws = FakeAws::new(replies);
            let output = status(&deployment, &aws).expect("should preserve partial report");
            assert!(output.failure.is_some());
            assert_eq!(output.data["complete"], false);
            assert_eq!(
                output.data["regions"][0]["instances"][0]["state"],
                "unknown"
            );
        }
    }

    #[test]
    fn wrong_account_prevents_resource_queries() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        let aws = FakeAws::new(vec![json!({"Account": "999999999999"})]);
        let error = status(&deployment, &aws)
            .err()
            .expect("should reject account mismatch");
        assert!(matches!(error.current_context(), PbsError::AccountMismatch));
        assert_eq!(aws.calls.borrow().len(), 1);
    }
}
