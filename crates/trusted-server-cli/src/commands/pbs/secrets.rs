use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use clap::Args;
use error_stack::Report;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use super::aws::{Aws, WRITE_NOT_CONFIRMED, verify_identity};
use super::config::{Binding, Deployment, invalid};
use super::{Interaction, Output, PbsError, Result, read_bounded, read_text};

const MAX_SECRET_BYTES: usize = 65_536;

/// No argument accepts a secret value. File/stdin and terminal input carry the payload.
#[derive(Debug, Args)]
pub(super) struct SetArgs {
    /// Bidder identifier declared in the binding file.
    bidder: String,
    /// Deployment descriptor; paths inside it are relative to this file.
    #[arg(long)]
    pub(super) deployment: PathBuf,
    /// Exactly one declared region; replicas cannot be written independently.
    #[arg(long)]
    region: String,
    /// Read a complete JSON string-valued object from this file; never changes the file.
    #[arg(long, conflicts_with = "stdin")]
    file: Option<PathBuf>,
    /// Read the JSON object from stdin; requires --yes and --request-token.
    #[arg(long)]
    stdin: bool,
    /// Approve the declared target noninteractively, for an externally approved automation job.
    #[arg(long, requires = "request_token")]
    yes: bool,
    /// Stable UUID for retries of this logical write. Reuse with identical values only.
    #[arg(long)]
    request_token: Option<Uuid>,
}

struct Payload(BTreeMap<String, String>);

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct UniqueKeys;
        impl<'de> Visitor<'de> for UniqueKeys {
            type Value = Payload;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object with unique keys and string values")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate credential key"));
                    }
                }
                Ok(Payload(values))
            }
        }
        deserializer.deserialize_map(UniqueKeys)
    }
}

/// Validate a complete payload without returning secret-bearing parser diagnostics.
///
/// # Errors
/// Rejects duplicate/unknown/missing keys, non-string values, empty required values and oversized JSON.
fn validate_payload(text: &str, binding: &Binding) -> Result<String> {
    if text.len() > MAX_SECRET_BYTES {
        return Err(invalid("secret JSON exceeds Secrets Manager size limit"));
    }
    let payload: Payload = serde_json::from_str(text)
        .map_err(|_| invalid("secret input must be a JSON object with unique keys and string values; contents withheld"))?;
    if payload.0.keys().any(|key| !binding.keys.contains_key(key)) {
        return Err(invalid("secret JSON includes undeclared keys"));
    }
    for (key, rule) in &binding.keys {
        if rule.required && payload.0.get(key).is_none_or(String::is_empty) {
            return Err(invalid("secret JSON is missing a required nonempty string"));
        }
    }
    if payload.0.is_empty() {
        return Err(invalid(
            "secret JSON must contain at least one declared key",
        ));
    }
    let encoded =
        serde_json::to_string(&payload.0).map_err(|_| invalid("cannot encode credential JSON"))?;
    if encoded.len() > MAX_SECRET_BYTES {
        return Err(invalid(
            "encoded credential JSON exceeds Secrets Manager size limit",
        ));
    }
    Ok(encoded)
}

/// Set a declared credential version after identity, metadata, input and approval checks.
///
/// # Errors
/// Rejects unapproved input, account/secret mismatches, replicas, deletion, malformed values and AWS errors.
pub(super) fn set(
    args: &SetArgs,
    deployment: &Deployment,
    aws: &dyn Aws,
    interaction: &mut dyn Interaction,
) -> Result<Output> {
    if (args.yes && args.request_token.is_none()) || (args.stdin && !args.yes) {
        return Err(invalid(
            "noninteractive writes require --yes and --request-token",
        ));
    }
    let binding = deployment
        .bindings
        .get(&args.bidder)
        .ok_or_else(|| invalid("bidder has no declared secret binding"))?;
    let arn = binding
        .secrets
        .get(&args.region)
        .ok_or_else(|| invalid("bidder has no secret in the selected region"))?;
    let target = format!(
        "Environment: {}; account: {}; profile: {}; region: {}; bidder: {}; secret: {arn}",
        deployment.environment,
        deployment.aws.account_id,
        deployment.aws.profile,
        args.region,
        args.bidder
    );
    interaction.notice(&target)?;
    verify_identity(deployment, &args.region, aws)?;
    let metadata = aws.call(
        &deployment.aws,
        &args.region,
        "secretsmanager",
        "describe-secret",
        &json!({"SecretId": arn}),
    )?;
    if metadata.get("ARN").and_then(Value::as_str) != Some(arn.as_str())
        || metadata
            .get("DeletedDate")
            .is_some_and(|value| !value.is_null())
    {
        return Err(invalid(
            "declared secret identity differs or is scheduled for deletion",
        ));
    }
    if metadata
        .get("PrimaryRegion")
        .and_then(Value::as_str)
        .is_some_and(|region| region != args.region)
    {
        return Err(invalid(
            "selected secret is a replica; write to its approved primary region instead",
        ));
    }
    let text = if let Some(path) = &args.file {
        read_text(path, MAX_SECRET_BYTES)?
    } else if args.stdin {
        read_bounded(io::stdin().lock(), MAX_SECRET_BYTES)?
    } else {
        interaction.secret()?
    };
    let payload = validate_payload(&text, binding)?;
    let token = args.request_token.unwrap_or_else(Uuid::new_v4).to_string();
    interaction.notice(&format!("Request token for identical retries: {token}"))?;
    if !args.yes && !interaction.confirm(&target)? {
        return Err(Report::new(PbsError::NotConfirmed));
    }
    let response = aws.call(
        &deployment.aws,
        &args.region,
        "secretsmanager",
        "put-secret-value",
        &json!({"SecretId": arn, "ClientRequestToken": token, "SecretString": payload}),
    )?;
    if response.get("ARN").and_then(Value::as_str) != Some(arn.as_str())
        || response.get("VersionId").and_then(Value::as_str) != Some(token.as_str())
    {
        return Err(Report::new(PbsError::Aws(WRITE_NOT_CONFIRMED)));
    }
    Ok(Output {
        failure: None,
        summary: format!("Stored credential version for {} in {}; no containers changed", args.bidder, args.region),
        details: vec![format!("Version / retry token: {token}"),
            "Confirm replica readiness and separately deploy consuming containers before revoking old partner credentials.".to_owned()],
        data: json!({"environment": deployment.environment, "account_id": deployment.aws.account_id,
            "region": args.region, "bidder": args.bidder, "secret_arn": arn, "version_id": token,
            "deployed": false, "partner_credential_rotated": false,
            "regions_requiring_deployment_review": binding.secrets.keys().collect::<Vec<_>>()}),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::commands::pbs::aws::tests::FakeAws;
    use crate::commands::pbs::config::tests::fixture;

    const ARN: &str = "arn:aws:secretsmanager:us-east-1:123456789012:secret:pbs/example-AbCdEf";
    const TOKEN: &str = "11111111-2222-4333-8444-555555555555";

    struct Prompt {
        approve: bool,
        notices: Vec<String>,
    }

    impl Interaction for Prompt {
        fn secret(&mut self) -> Result<String> {
            Ok("{\"api_key\":\"dummy\"}".to_owned())
        }
        fn confirm(&mut self, _target: &str) -> Result<bool> {
            Ok(self.approve)
        }
        fn notice(&mut self, text: &str) -> Result<()> {
            self.notices.push(text.to_owned());
            Ok(())
        }
    }

    fn args(path: &std::path::Path) -> SetArgs {
        SetArgs {
            bidder: "examplebidder".to_owned(),
            deployment: path.to_path_buf(),
            region: "us-east-1".to_owned(),
            file: None,
            stdin: false,
            yes: false,
            request_token: Some(Uuid::parse_str(TOKEN).expect("should parse token")),
        }
    }

    fn replies() -> Vec<Value> {
        vec![
            json!({"Account": "123456789012"}),
            json!({"ARN": ARN}),
            json!({"ARN": ARN, "VersionId": TOKEN}),
        ]
    }

    #[test]
    fn wrong_account_and_declined_confirmation_never_write() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        let aws = FakeAws::new(vec![json!({"Account": "999999999999"})]);
        let mut prompt = Prompt {
            approve: true,
            notices: vec![],
        };
        assert!(set(&args(&path), &deployment, &aws, &mut prompt).is_err());
        assert_eq!(aws.calls.borrow().len(), 1);
        let aws = FakeAws::new(replies());
        prompt.approve = false;
        assert!(set(&args(&path), &deployment, &aws, &mut prompt).is_err());
        assert_eq!(aws.calls.borrow().len(), 2);
    }

    #[test]
    fn special_characters_round_trip_without_output_or_source_file_changes() {
        let (dir, path) = fixture();
        let secret = "dummy-$\"\nline\\end";
        let original = json!({"api_key": secret}).to_string();
        let input = dir.path().join("secret.json");
        fs::write(&input, &original).expect("should write secret fixture");
        let deployment = Deployment::load(&path).expect("should load deployment");
        let mut arguments = args(&path);
        arguments.file = Some(input.clone());
        arguments.yes = true;
        let aws = FakeAws::new(replies());
        let mut prompt = Prompt {
            approve: false,
            notices: vec![],
        };
        let report =
            set(&arguments, &deployment, &aws, &mut prompt).expect("should write approved secret");
        let calls = aws.calls.borrow();
        assert_eq!(calls[2].1, "put-secret-value");
        let stored: Value = serde_json::from_str(
            calls[2].2["SecretString"]
                .as_str()
                .expect("should have encoded JSON"),
        )
        .expect("should parse payload");
        assert_eq!(stored["api_key"], secret);
        assert_eq!(calls[2].2["ClientRequestToken"], TOKEN);
        for json in [false, true] {
            let mut output = Vec::new();
            report
                .write(json, &mut output)
                .expect("should render report");
            assert!(!String::from_utf8_lossy(&output).contains("dummy-"));
        }
        assert!(!prompt.notices.join("\n").contains("dummy-"));
        assert_eq!(
            fs::read_to_string(input).expect("should read input"),
            original
        );
        assert_eq!(report.data["deployed"], false);
    }

    #[test]
    fn replicas_and_missing_confirmation_inputs_fail_closed() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        let aws = FakeAws::new(vec![
            json!({"Account": "123456789012"}),
            json!({"ARN": ARN, "PrimaryRegion": "us-west-2"}),
        ]);
        let mut prompt = Prompt {
            approve: true,
            notices: vec![],
        };
        assert!(set(&args(&path), &deployment, &aws, &mut prompt).is_err());
        assert_eq!(aws.calls.borrow().len(), 2);
        let mut arguments = args(&path);
        arguments.yes = true;
        arguments.request_token = None;
        let aws = FakeAws::new(vec![]);
        assert!(set(&arguments, &deployment, &aws, &mut prompt).is_err());
        assert!(aws.calls.borrow().is_empty());
    }

    #[test]
    fn undeclared_region_and_mismatched_secret_metadata_never_write() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        let mut arguments = args(&path);
        arguments.region = "us-west-2".to_owned();
        let aws = FakeAws::new(vec![]);
        let mut prompt = Prompt {
            approve: true,
            notices: vec![],
        };
        assert!(set(&arguments, &deployment, &aws, &mut prompt).is_err());
        assert!(aws.calls.borrow().is_empty());
        for metadata in [
            json!({"ARN": "arn:aws:secretsmanager:us-east-1:123456789012:secret:other-AbCdEf"}),
            json!({"ARN": ARN, "DeletedDate": 1234567890}),
        ] {
            let aws = FakeAws::new(vec![json!({"Account": "123456789012"}), metadata]);
            assert!(set(&args(&path), &deployment, &aws, &mut prompt).is_err());
            assert_eq!(aws.calls.borrow().len(), 2);
        }
    }

    #[test]
    fn payload_rejects_missing_extra_duplicate_and_nonstring_keys_without_leaks() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        let binding = &deployment.bindings["examplebidder"];
        for payload in [
            "{}",
            "{\"api_key\":null}",
            "{\"api_key\":\"\"}",
            "{\"api_key\":\"dummy\",\"extra\":\"NEVER_PRINT_ME\"}",
            "{\"api_key\":\"NEVER_PRINT_ME\",\"api_key\":\"second\"}",
            "NEVER_PRINT_ME",
        ] {
            let error = validate_payload(payload, binding)
                .expect_err("should reject invalid credential payload");
            assert!(!format!("{error:?}").contains("NEVER_PRINT_ME"));
        }
    }

    #[test]
    fn retries_reuse_the_same_token_and_payload_and_failures_are_reported() {
        let (_dir, path) = fixture();
        let deployment = Deployment::load(&path).expect("should load deployment");
        let mut all = replies();
        all.extend(replies());
        let aws = FakeAws::new(all);
        let mut prompt = Prompt {
            approve: true,
            notices: vec![],
        };
        for _ in 0..2 {
            set(&args(&path), &deployment, &aws, &mut prompt).expect("should submit retry");
        }
        assert_eq!(aws.calls.borrow()[2].2, aws.calls.borrow()[5].2);
        let aws = FakeAws::new(vec![
            json!({"Account": "123456789012"}),
            json!({"ARN": ARN}),
        ]);
        assert!(set(&args(&path), &deployment, &aws, &mut prompt).is_err());
    }
}
