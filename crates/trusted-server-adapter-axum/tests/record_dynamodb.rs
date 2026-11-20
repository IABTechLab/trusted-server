//! Explicit local `DynamoDB` lifecycle proof through the shared EC adapter.

#![cfg(feature = "aws-dynamodb-ec")]

use std::env;
use std::net::{Ipv4Addr, SocketAddr};
use std::panic::AssertUnwindSafe;
use std::process::{self, Command};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::config::{Credentials, Region, retry::RetryConfig};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType,
};
use edgezero_core::record_store::RecordLayout;
use edgezero_core::time::MonotonicClock;
use edgezero_store_aws::AwsPreparation;
use edgezero_store_aws::settings::{AwsPreparationLimits, DynamoDbKvLimits, DynamoDbKvSettings};
use futures::FutureExt as _;
use trusted_server_core::ec::kv_backend::{EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use trusted_server_core::ec::record_backend::{EcRecordLayout, RecordEcKvStore};
use trusted_server_core::test_support::record_ec::assert_record_ec_lifecycle;

const TEST: &str = "shared_record_ec_dynamodb_local_lifecycle";

fn endpoint() -> String {
    let value = env::var("EDGEZERO_AWS_TEST_DYNAMODB_ENDPOINT")
        .expect("should explicitly supply the task-owned DynamoDB Local endpoint");
    let address: SocketAddr = value
        .strip_prefix("http://")
        .expect("should use the local HTTP fixture")
        .parse()
        .expect("should use a literal local socket address");
    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
    assert_ne!(address.port(), 0);
    assert_eq!(value, format!("http://{address}"));
    value
}

#[test]
#[ignore = "requires explicit DynamoDB Local; no hosted AWS qualification"]
fn shared_record_ec_dynamodb_local_lifecycle() {
    let endpoint = endpoint();
    if env::var("TS_RECORD_DYNAMODB_CHILD").as_deref() != Ok(TEST) {
        let status = Command::new(env::current_exe().expect("should locate test executable"))
            .args(["--ignored", "--exact", TEST, "--nocapture"])
            .env_clear()
            .env(
                "TMPDIR",
                env::var_os("TMPDIR").expect("should use physical task TMPDIR"),
            )
            .env("TS_RECORD_DYNAMODB_CHILD", TEST)
            .env("EDGEZERO_AWS_TEST_DYNAMODB_ENDPOINT", &endpoint)
            .env("AWS_ENDPOINT_URL_DYNAMODB", &endpoint)
            .env("AWS_ENDPOINT_URL", &endpoint)
            .env("AWS_ACCESS_KEY_ID", "DUMMYLOCAL")
            .env("AWS_SECRET_ACCESS_KEY", "DUMMYLOCAL")
            .env("AWS_REGION", "us-east-1")
            .env("AWS_DEFAULT_REGION", "us-east-1")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_ACCOUNT_ID_ENDPOINT_MODE", "disabled")
            .status()
            .expect("should run isolated local record test");
        assert!(
            status.success(),
            "should pass isolated dummy-credential child"
        );
        return;
    }
    assert_eq!(env::var("AWS_ACCESS_KEY_ID").as_deref(), Ok("DUMMYLOCAL"));
    assert_eq!(
        env::var("AWS_ENDPOINT_URL_DYNAMODB").as_deref(),
        Ok(endpoint.as_str())
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should create local test runtime")
        .block_on(run());
}

async fn run() {
    let config = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            "DUMMYLOCAL",
            "DUMMYLOCAL",
            None,
            None,
            "local-record-proof",
        ))
        .endpoint_url(endpoint())
        .load()
        .await;
    let client = Client::from_conf(
        aws_sdk_dynamodb::Config::from(&config)
            .to_builder()
            .retry_config(RetryConfig::standard().with_max_attempts(1))
            .build(),
    );
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("should read fixture time")
        .as_nanos();
    let table = format!("ts-record-ec-{}-{nanos}", process::id());
    client
        .create_table()
        .table_name(&table)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("pk")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .expect("should define partition key"),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("sk")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .expect("should define sort key"),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("pk")
                .key_type(KeyType::Hash)
                .build()
                .expect("should define partition schema"),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("sk")
                .key_type(KeyType::Range)
                .build()
                .expect("should define sort schema"),
        )
        .send()
        .await
        .expect("should create isolated record table");
    let result = AssertUnwindSafe(async {
        let first = prepared(&table).await;
        let second = prepared(&table).await;
        let key = "race.Ab1234";
        let write = EcKvWrite {
            body: "first",
            metadata: "{}",
            ttl: Duration::from_secs(60),
            mode: EcKvWriteMode::Add,
        };
        let (one, two) = futures::join!(first.insert(key, write), second.insert(key, write));
        let outcomes = [
            one.expect("should complete first Add"),
            two.expect("should complete second Add"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == EcKvWriteOutcome::Written)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == EcKvWriteOutcome::PreconditionFailed)
                .count(),
            1
        );
        let row = first
            .lookup(key)
            .await
            .expect("should read Add winner")
            .expect("should retain winner");
        let cas = EcKvWrite {
            mode: EcKvWriteMode::IfGenerationMatch(row.generation),
            ..write
        };
        let (one, two) = futures::join!(first.insert(key, cas), second.insert(key, cas));
        let outcomes = [
            one.expect("should complete first CAS"),
            two.expect("should complete second CAS"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == EcKvWriteOutcome::Written)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == EcKvWriteOutcome::PreconditionFailed)
                .count(),
            1
        );
        first.delete(key).await.expect("should clean race key");
        for index in 0_u32..105_u32 {
            first
                .insert(&format!("pages.{index:06}"), write)
                .await
                .expect("should seed grouped records");
        }
        let pages = first
            .list_keys_with_prefix("pages.", 200)
            .await
            .expect("should continue through allocator-only native pages");
        assert_eq!(pages.len(), 105);
        let capped = first
            .list_keys_with_prefix("pages.", 100)
            .await
            .expect("should continue after an allocator-only native page");
        assert_eq!(capped.len(), 100);
        assert_record_ec_lifecycle(&first).await;
    })
    .catch_unwind()
    .await;
    client
        .delete_table()
        .table_name(&table)
        .send()
        .await
        .expect("should remove owned local table");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn prepared(table: &str) -> RecordEcKvStore {
    let mut session =
        AwsPreparation::new(MonotonicClock::default(), AwsPreparationLimits::default())
            .expect("should create record preparation session");
    let handle = session
        .prepare_dynamodb_records(
            &DynamoDbKvSettings {
                region: "us-east-1".to_owned(),
                table: table.to_owned(),
                namespace: "ts-ec".to_owned(),
                limits: DynamoDbKvLimits::default(),
            },
            &RecordLayout::new(EcRecordLayout).expect("should declare EC key layout"),
        )
        .await
        .expect("should prepare actual EdgeZero DynamoDB records");
    RecordEcKvStore::new("trusted_server_kv".to_owned(), &handle)
        .expect("should acquire shared EC backend")
}
